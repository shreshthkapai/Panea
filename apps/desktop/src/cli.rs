// Desktop command-line entrypoints and bounded smoke commands.

fn run_cli() -> Option<i32> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let command = args.first()?;

    match command.as_str() {
        "doctor" => Some(run_doctor_cli(&args[1..])),
        "shell-smoke" => Some(run_shell_smoke_cli(&args[1..])),
        "shell-integration" => Some(run_shell_integration_cli(&args[1..])),
        "help" | "--help" | "-h" => {
            print_cli_help();
            Some(0)
        }
        _ => None,
    }
}

fn print_cli_help() {
    eprintln!(
        "usage: panea doctor [window|renderer|config|shell|ssh|fonts|clipboard|notifications] [--json]"
    );
    eprintln!("usage: panea shell-smoke [--json] [--timeout-ms <ms>]");
    eprintln!(
        "usage: panea gui-smoke [--startup|--input-echo|--terminal-io] [--backend <auto|vulkan|metal|dx12|gl>] [--hold-ms <ms>] [--json] [--timeout-ms <ms>]"
    );
    eprintln!(
        "usage: panea shell-integration export --shell <bash|zsh|fish|powershell> --output <path>"
    );
    eprintln!("usage: panea shell-integration remote-plan --shell <shell> [--profile <name>]");
}

#[derive(Debug, Clone)]
struct GuiSmokeOptions {
    timeout: Duration,
    completed: Arc<AtomicBool>,
    mode: GuiSmokeMode,
    hold_after_success: Duration,
    renderer_backend_override: Option<config_core::RendererBackendPreference>,
    report: Arc<Mutex<GuiSmokeReport>>,
}

#[derive(Debug, Clone, Default)]
struct GuiSmokeReport {
    power_source: Option<&'static str>,
    charge_percent: Option<u8>,
    config_loaded: Option<Duration>,
    window_created: Option<Duration>,
    fonts_ready: Option<Duration>,
    session_created: Option<Duration>,
    renderer_initialized: Option<Duration>,
    startup_background_presented: Option<Duration>,
    renderer_created: Option<Duration>,
    first_scene_preparation: Option<Duration>,
    first_render_submission: Option<Duration>,
    prompt_observed: Option<Duration>,
    input_sent: Option<Duration>,
    input_observed: Option<Duration>,
    success_frame_presented: Option<Duration>,
    renderer: Option<render_wgpu::RendererStartupDiagnostics>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuiSmokeMode {
    FirstFrame,
    Startup,
    InputEcho,
    TerminalIo,
}

const GUI_SMOKE_MARKER: &str = "PANEAE2E_OUTPUT";
const GUI_INPUT_ECHO_MARKER: &str = "PANEAE2E_INPUT";
const GUI_INPUT_SETTLE_DELAY: Duration = Duration::from_millis(100);

fn run_gui_smoke_cli() -> i32 {
    let args = std::env::args().skip(2).collect::<Vec<_>>();
    let json = args.iter().any(|arg| arg == "--json");
    let mut timeout = Duration::from_secs(10);
    let mut mode = GuiSmokeMode::FirstFrame;
    let mut hold_after_success = Duration::ZERO;
    let mut renderer_backend_override = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {}
            "--startup" => mode = GuiSmokeMode::Startup,
            "--input-echo" => mode = GuiSmokeMode::InputEcho,
            "--terminal-io" => mode = GuiSmokeMode::TerminalIo,
            "--backend" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    eprintln!("--backend requires a value");
                    return 2;
                };
                let Some(backend) = parse_gui_smoke_backend(value) else {
                    eprintln!("invalid --backend value: {value}");
                    return 2;
                };
                renderer_backend_override = Some(backend);
            }
            "--hold-ms" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    eprintln!("--hold-ms requires a value");
                    return 2;
                };
                let Ok(millis) = value.parse::<u64>() else {
                    eprintln!("invalid --hold-ms value: {value}");
                    return 2;
                };
                if millis > 30_000 {
                    eprintln!("--hold-ms must not exceed 30000");
                    return 2;
                }
                hold_after_success = Duration::from_millis(millis);
            }
            "--timeout-ms" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    eprintln!("--timeout-ms requires a value");
                    return 2;
                };
                let Ok(millis) = value.parse::<u64>() else {
                    eprintln!("invalid --timeout-ms value: {value}");
                    return 2;
                };
                if millis < 1000 {
                    eprintln!("--timeout-ms must be at least 1000");
                    return 2;
                }
                timeout = Duration::from_millis(millis);
            }
            other => {
                eprintln!("unknown gui-smoke option: {other}");
                return 2;
            }
        }
        index += 1;
    }

    let completed = Arc::new(AtomicBool::new(false));
    let report = Arc::new(Mutex::new(GuiSmokeReport::default()));
    let started = Instant::now();
    let result = run(Some(GuiSmokeOptions {
        timeout,
        completed: Arc::clone(&completed),
        mode,
        hold_after_success,
        renderer_backend_override,
        report: Arc::clone(&report),
    }));
    let passed = result.is_ok() && completed.load(Ordering::Acquire);
    if json {
        let report = report
            .lock()
            .map_or_else(|_| GuiSmokeReport::default(), |report| report.clone());
        println!(
            "{}",
            gui_smoke_json(passed, mode, started.elapsed(), &report)
        );
    }
    if let Err(error) = result {
        eprintln!("gui smoke failed: {error}");
    } else if !passed {
        eprintln!("gui smoke timed out before its required render milestone");
    }
    i32::from(!passed)
}

fn parse_gui_smoke_backend(value: &str) -> Option<config_core::RendererBackendPreference> {
    match value {
        "auto" => Some(config_core::RendererBackendPreference::Auto),
        "vulkan" => Some(config_core::RendererBackendPreference::Vulkan),
        "metal" => Some(config_core::RendererBackendPreference::Metal),
        "dx12" => Some(config_core::RendererBackendPreference::Dx12),
        "gl" => Some(config_core::RendererBackendPreference::Gl),
        _ => None,
    }
}

fn gui_smoke_json(
    passed: bool,
    mode: GuiSmokeMode,
    duration: Duration,
    report: &GuiSmokeReport,
) -> serde_json::Value {
    let renderer = report.renderer.as_ref().map(|renderer| {
        let timings = renderer.timings;
        serde_json::json!({
            "requested_backend": renderer.requested_backend.as_str(),
            "effective_backend": renderer.effective_backend,
            "adapter": renderer.adapter,
            "instance_and_surface_us": duration_micros(timings.instance_and_surface),
            "adapter_request_us": duration_micros(timings.adapter_request),
            "device_request_us": duration_micros(timings.device_request),
            "surface_configuration_us": duration_micros(timings.surface_configuration),
            "pipeline_creation_us": duration_micros(timings.pipeline_creation),
            "accounted_us": duration_micros(timings.accounted()),
            "total_us": duration_micros(timings.total),
        })
    });
    serde_json::json!({
        "name": "gui-smoke",
        "status": if passed { "passed" } else { "failed" },
        "power_source": report.power_source,
        "charge_percent": report.charge_percent,
        "duration_ms": duration_millis(duration),
        "milestone": match mode {
            GuiSmokeMode::FirstFrame => "window_renderer_session_first_frame",
            GuiSmokeMode::Startup => "single_shell_prompt_rendered_without_input",
            GuiSmokeMode::InputEcho => "prompt_input_echo_presented",
            GuiSmokeMode::TerminalIo => "shell_prompt_input_output_rendered",
        },
        "window_created_us": report.window_created.map(duration_micros),
        "config_loaded_us": report.config_loaded.map(duration_micros),
        "fonts_ready_us": report.fonts_ready.map(duration_micros),
        "session_created_us": report.session_created.map(duration_micros),
        "renderer_initialized_us": report.renderer_initialized.map(duration_micros),
        "startup_background_presented_us": report
            .startup_background_presented
            .map(duration_micros),
        "startup_background_present_us": report
            .renderer_initialized
            .zip(report.startup_background_presented)
            .map(|(initialized, presented)| {
                duration_micros(presented.saturating_sub(initialized))
            }),
        "renderer_created_us": report.renderer_created.map(duration_micros),
        "first_scene_preparation_us": report.first_scene_preparation.map(duration_micros),
        "first_render_submission_us": report.first_render_submission.map(duration_micros),
        "prompt_observed_us": report.prompt_observed.map(duration_micros),
        "input_sent_us": report.input_sent.map(duration_micros),
        "input_observed_us": report.input_observed.map(duration_micros),
        "success_frame_presented_us": report.success_frame_presented.map(duration_micros),
        "input_to_output_us": report
            .input_sent
            .zip(report.input_observed)
            .map(|(sent, observed)| duration_micros(observed.saturating_sub(sent))),
        "output_to_present_us": report
            .input_observed
            .zip(report.success_frame_presented)
            .map(|(observed, presented)| duration_micros(presented.saturating_sub(observed))),
        "input_to_present_us": report
            .input_sent
            .zip(report.success_frame_presented)
            .map(|(sent, presented)| duration_micros(presented.saturating_sub(sent))),
        "renderer": renderer,
    })
}

fn duration_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn run_shell_integration_cli(args: &[String]) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("shell-integration requires export or remote-plan");
        return 2;
    };
    let mut shell = None;
    let mut output = None;
    let mut profile = "remote".to_owned();
    let mut index = 1;
    while index < args.len() {
        let option = args[index].as_str();
        index += 1;
        let Some(value) = args.get(index) else {
            eprintln!("{option} requires a value");
            return 2;
        };
        match option {
            "--shell" => shell = Some(ShellKind::parse(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--profile" => profile = value.clone(),
            _ => {
                eprintln!("unknown shell-integration option: {option}");
                return 2;
            }
        }
        index += 1;
    }
    let Some(shell) = shell.filter(|shell| *shell != ShellKind::Unknown) else {
        eprintln!("--shell must name bash, zsh, fish, powershell, or pwsh");
        return 2;
    };

    match command {
        "export" => {
            let Some(output) = output else {
                eprintln!("shell-integration export requires --output <path>");
                return 2;
            };
            let Some(script) = shell_integration::script_for_shell(shell) else {
                eprintln!("Panea has no integration hook for {shell:?}");
                return 2;
            };
            if let Some(parent) = output
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                && let Err(error) = fs::create_dir_all(parent)
            {
                eprintln!("could not create {}: {error}", parent.display());
                return 1;
            }
            match fs::write(&output, script.contents) {
                Ok(()) => {
                    println!("exported reviewed Panea hook to {}", output.display());
                    0
                }
                Err(error) => {
                    eprintln!("could not write {}: {error}", output.display());
                    1
                }
            }
        }
        "remote-plan" => match remote_install_plan(shell) {
            Some(plan) => {
                println!("{}", plan.render(&profile));
                0
            }
            None => {
                eprintln!("Panea has no remote integration plan for {shell:?}");
                2
            }
        },
        _ => {
            eprintln!("unknown shell-integration command: {command}");
            2
        }
    }
}

fn run_doctor_cli(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let topic_arg = args
        .iter()
        .find(|arg| arg.as_str() != "--json")
        .map(String::as_str);
    let topic = topic_arg.map_or(
        Some(diagnostics::DoctorTopic::All),
        diagnostics::DoctorTopic::parse,
    );
    let Some(topic) = topic else {
        eprintln!(
            "unknown doctor topic; expected window, renderer, config, shell, ssh, fonts, clipboard, notifications, platform, or performance"
        );
        return 2;
    };

    let input = doctor_input();
    let report = diagnostics::doctor_report(&input, topic);
    if json {
        println!("{}", report.render_json());
    } else {
        println!("{}", report.render_text());
    }
    0
}

fn run_shell_smoke_cli(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let mut timeout = Duration::from_secs(5);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {}
            "--timeout-ms" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    eprintln!("--timeout-ms requires a value");
                    return 2;
                };
                let Ok(millis) = value.parse::<u64>() else {
                    eprintln!("invalid --timeout-ms value: {value}");
                    return 2;
                };
                if millis < 500 {
                    eprintln!("--timeout-ms must be at least 500");
                    return 2;
                }
                timeout = Duration::from_millis(millis);
            }
            other => {
                eprintln!("unknown shell-smoke option: {other}");
                return 2;
            }
        }
        index += 1;
    }

    let started = Instant::now();
    let result = match load_desktop_config() {
        Ok(loaded) => run_headless_shell_smoke(&loaded.config, timeout),
        Err(error) => ShellSmokeResult {
            passed: false,
            duration: started.elapsed(),
            marker_observed: false,
            bytes_received: 0,
            preview: String::new(),
            detail: format!("config load failed: {error}"),
            diagnostics: Vec::new(),
        },
    };

    if json {
        println!("{}", result.render_json());
    } else {
        println!("{}", result.render_text());
    }

    if result.passed { 0 } else { 1 }
}

#[derive(Debug, Clone)]
struct ShellSmokeResult {
    passed: bool,
    duration: Duration,
    marker_observed: bool,
    bytes_received: usize,
    preview: String,
    detail: String,
    diagnostics: Vec<String>,
}

impl ShellSmokeResult {
    fn render_text(&self) -> String {
        format!(
            "shell-smoke status={} duration_ms={} marker_observed={} bytes_received={} detail={}",
            if self.passed { "passed" } else { "failed" },
            self.duration.as_millis(),
            self.marker_observed,
            self.bytes_received,
            self.detail
        )
    }

    fn render_json(&self) -> String {
        format!(
            concat!(
                "{{",
                "\"name\":\"shell-smoke\",",
                "\"passed\":{},",
                "\"duration_ms\":{},",
                "\"marker_observed\":{},",
                "\"bytes_received\":{},",
                "\"preview\":\"{}\",",
                "\"detail\":\"{}\",",
                "\"diagnostics\":[{}]",
                "}}"
            ),
            self.passed,
            self.duration.as_millis(),
            self.marker_observed,
            self.bytes_received,
            json_escape(&self.preview),
            json_escape(&self.detail),
            self.diagnostics
                .iter()
                .map(|diagnostic| format!("\"{}\"", json_escape(diagnostic)))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}

fn run_headless_shell_smoke(config: &AppConfig, timeout: Duration) -> ShellSmokeResult {
    let started = Instant::now();
    let marker = b"panea-package-shell-smoke";
    let profile = shell_smoke_profile(config);
    let mut transport =
        match LocalPtyTransport::spawn(profile, TransportSize::new(80, 24, 640, 384)) {
            Ok(transport) => transport,
            Err(error) => {
                return ShellSmokeResult {
                    passed: false,
                    duration: started.elapsed(),
                    marker_observed: false,
                    bytes_received: 0,
                    preview: String::new(),
                    detail: format!("failed to spawn shell smoke PTY: {error}"),
                    diagnostics: Vec::new(),
                };
            }
        };

    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut saw_marker = false;
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(80, 24));

    while Instant::now() < deadline {
        match transport.poll_output() {
            Ok(poll) => {
                output.extend_from_slice(&poll.bytes);
                let responses = match terminal_responses_for_shell_smoke(&mut terminal, &poll.bytes)
                {
                    Ok(responses) => responses,
                    Err(error) => {
                        let diagnostics = format_local_pty_diagnostics(&transport.diagnostics());
                        let _ = transport.shutdown();
                        return ShellSmokeResult {
                            passed: false,
                            duration: started.elapsed(),
                            marker_observed: saw_marker,
                            bytes_received: output.len(),
                            preview: preview_smoke_bytes(&output),
                            detail: format!("terminal parser failed: {error}"),
                            diagnostics: vec![diagnostics],
                        };
                    }
                };
                if !responses.is_empty()
                    && let Err(error) = transport.write_input(&responses)
                {
                    let diagnostics = format_local_pty_diagnostics(&transport.diagnostics());
                    let _ = transport.shutdown();
                    return ShellSmokeResult {
                        passed: false,
                        duration: started.elapsed(),
                        marker_observed: saw_marker,
                        bytes_received: output.len(),
                        preview: preview_smoke_bytes(&output),
                        detail: format!("failed to write terminal response: {error}"),
                        diagnostics: vec![diagnostics],
                    };
                }
                saw_marker =
                    saw_marker || output.windows(marker.len()).any(|window| window == marker);
                let closed =
                    poll.closed || matches!(transport.state(), TransportState::Closed { .. });
                if saw_marker && closed {
                    break;
                }
            }
            Err(error) => {
                let diagnostics = format_local_pty_diagnostics(&transport.diagnostics());
                let _ = transport.shutdown();
                return ShellSmokeResult {
                    passed: false,
                    duration: started.elapsed(),
                    marker_observed: saw_marker,
                    bytes_received: output.len(),
                    preview: preview_smoke_bytes(&output),
                    detail: format!("poll failed: {error}"),
                    diagnostics: vec![diagnostics],
                };
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let before_shutdown = transport.diagnostics();
    let shutdown_result = transport.shutdown();
    let after_shutdown = transport.diagnostics();
    let diagnostics = vec![
        format_local_pty_diagnostics(&before_shutdown),
        format_local_pty_diagnostics(&after_shutdown),
    ];
    let shutdown_ok = shutdown_result.is_ok() && !after_shutdown.shutdown_timed_out;

    ShellSmokeResult {
        passed: saw_marker && shutdown_ok,
        duration: started.elapsed(),
        marker_observed: saw_marker,
        bytes_received: output.len(),
        preview: preview_smoke_bytes(&output),
        detail: if saw_marker && shutdown_ok {
            "shell emitted marker and shut down cleanly".to_owned()
        } else if !saw_marker {
            format!(
                "timed out before observing {}",
                String::from_utf8_lossy(marker)
            )
        } else {
            format!(
                "marker observed but shutdown failed: {:?}",
                shutdown_result.map_err(|error| error.to_string())
            )
        },
        diagnostics,
    }
}

fn terminal_responses_for_shell_smoke(
    terminal: &mut TerminalEmulator,
    bytes: &[u8],
) -> Result<Vec<u8>, String> {
    terminal
        .apply_bytes_and_take_pending_output(bytes)
        .map_err(|error| error.to_string())
}

fn shell_smoke_profile(config: &AppConfig) -> LocalShellProfile {
    let mut profile = selected_shell_profile(config)
        .map(local_shell_profile)
        .unwrap_or_else(LocalShellProfile::default_for_platform);
    profile.startup_command = None;
    profile
        .env
        .insert("PANEA_SHELL_SMOKE".to_owned(), "1".to_owned());

    match shell_kind_for_local_profile(&profile) {
        ShellKind::Cmd => {
            profile.kind = LocalShellKind::Cmd;
            if profile.program.trim().is_empty() {
                profile.program = "cmd.exe".to_owned();
            }
            profile.args = vec![
                "/D".to_owned(),
                "/C".to_owned(),
                "echo panea-package-shell-smoke".to_owned(),
            ];
        }
        ShellKind::PowerShell | ShellKind::Pwsh => {
            profile.kind = LocalShellKind::PowerShell;
            if profile.program.trim().is_empty() {
                profile.program = "powershell.exe".to_owned();
            }
            profile.args = vec![
                "-NoLogo".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                "Write-Output panea-package-shell-smoke".to_owned(),
            ];
        }
        ShellKind::Bash
        | ShellKind::Zsh
        | ShellKind::Fish
        | ShellKind::Nushell
        | ShellKind::Unknown => {
            if cfg!(windows) && matches!(profile.kind, LocalShellKind::Default) {
                profile.kind = LocalShellKind::PowerShell;
                profile.program = "powershell.exe".to_owned();
                profile.args = vec![
                    "-NoLogo".to_owned(),
                    "-NoProfile".to_owned(),
                    "-NonInteractive".to_owned(),
                    "-Command".to_owned(),
                    "Write-Output panea-package-shell-smoke".to_owned(),
                ];
            } else {
                if profile.program.trim().is_empty() {
                    profile.program = "sh".to_owned();
                }
                profile.args = vec![
                    "-lc".to_owned(),
                    "printf '%s\\n' panea-package-shell-smoke".to_owned(),
                ];
            }
        }
    }

    profile
}

fn preview_smoke_bytes(bytes: &[u8]) -> String {
    const LIMIT: usize = 320;
    let start = bytes.len().saturating_sub(LIMIT);
    String::from_utf8_lossy(&bytes[start..])
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn format_local_pty_diagnostics(diagnostics: &transport_pty::LocalPtyDiagnostics) -> String {
    format!(
        "command={} pid={:?} state={:?} bytes={} reads={} reader_started={} reader_stopped={} child_exited={} kill_attempted={} shutdown_timed_out={} reader_error={:?}",
        diagnostics.command,
        diagnostics.process_id,
        diagnostics.state,
        diagnostics.bytes_received,
        diagnostics.read_events,
        diagnostics.reader_started,
        diagnostics.reader_stopped,
        diagnostics.child_exited,
        diagnostics.kill_attempted,
        diagnostics.shutdown_timed_out,
        diagnostics.reader_error
    )
}

fn json_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
