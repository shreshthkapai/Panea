use std::{
    any::Any,
    collections::{BTreeSet, HashMap},
    error::Error,
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use config_core::{
    AppConfig, ClipboardConfig, CommandBlockStyle, ConfigDiagnostic, ConfigDiagnosticSeverity,
    ConfigPlatform, DecorationStrategyConfig, InputOutputGroupingStyle, LinuxBackendConfig,
    LogLevel, MuxLayoutConfig, MuxSplitAxisConfig, MuxTransportConfig, NotificationConfig,
    PasteConfig, PerformanceConfig, PerformanceOverlayDetail, PerformanceOverlayPosition,
    PerformanceProfile, PresentModePreference, PromptDecorationStyle, ReloadPlan,
    ReloadableSection, ShellIntegrationActivationConfig, ShellProfile, ShellProfileKind,
    SshAuthMethod, SshKnownHostsPolicy, SshProfile, WindowModeConfig,
};
use diagnostics::{PerformanceBudget, PerformanceOverlay};
use font_system::{CellMetrics, FontConfig as RuntimeFontConfig, FontSource, FontSystem};
use mux::{
    LogicalRect, MuxAction, MuxModel, PaneId, PaneLayout, PaneRestore, RestoreSnapshot,
    SessionSpec, SessionStatus, SessionTransportKind, SplitAxis, SplitTree, TabId, TabRestore,
    TerminalGridSize, WindowRestore, WorkspaceRestore,
};
use platform_core::{
    DecorationMode, InputEvent, KeyEvent, KeyModifiers, KeyState, LinuxWindowBackend, MouseButton,
    MouseEvent, MouseEventKind, NotificationProvider, NotificationRequest, NotificationUrgency,
    PowerSource, PowerState, PowerStateProvider, UrlOpener, WindowAction, WindowMode,
};
use platform_winit::{
    ClipboardBridge, DesktopNotificationProvider, DesktopPowerMonitor, DesktopUrlOpener,
    DesktopWindow, InputTranslator, WindowSettings, apply_window_mode_with_decoration,
    create_event_loop, platform_capabilities,
};
use render_core::{
    CellPosition, CursorVisual, OverlayKind, OverlayPrimitive, RenderCell, RenderCellStyle,
    RenderColor, RenderCursorShape, RenderGrid, RenderInstrumentation, RenderOffset, RenderRect,
    RenderScene, SelectionVisual,
};
use render_wgpu::{
    AnimatedCursorImageCache, AnimatedCursorImageRequest, AnimatedCursorImageRuntime,
    AnimatedCursorImageStatus, CursorAnimationRuntime, CursorAnimationSettings, CursorBlinkRuntime,
    CursorVectorCache, CursorVectorRequest, CursorVectorRuntime, CursorVectorStatus, DamageTracker,
    FrameDecision, FrameScheduler, GpuTerminalRenderer, PresentMode, RendererError,
    RendererOptions, RetainedDamageStatus,
};
use security::{
    HostKeyTrustAction, HostKeyTrustReason, HostKeyTrustRequest, HostTrustProvider,
    KeychainBackedSecretProvider, KeychainProvider, KeychainProviderCapability,
    SecretPromptProvider, SecretPromptResponse, SecretRequest, SecretString,
};
use security::{
    Osc52ClipboardDecision, Osc52ClipboardPolicy, Osc52ClipboardRequest as SecurityOsc52Request,
    Osc52ClipboardTarget, PlatformKeychainProvider, approve_osc52_clipboard_write,
    evaluate_osc52_clipboard_write,
};
use semantics::detect_url_hints;
use semantics::{
    BufferPosition, CommandStatus, IntegrationMode, RemoteMetadata, SemanticAction,
    SemanticActionResult, SemanticMetadata, SemanticRegionKind, SemanticSpan,
    SemanticTimelineStore, TerminalTextProvider,
};
use shell_integration::{
    HeuristicCommandDetector, IntegrationActivation, SemanticEscapeParser,
    ShellIntegrationActivationAction, ShellIntegrationActivationPlan, ShellIntegrationPolicy,
    ShellIntegrationRuntimeMode, ShellKind, detect_shell_kind, remote_install_plan,
};
use term_core::{
    CellAttributes, ClipboardTarget, Color, CursorShape, GridPosition, KeypadKey,
    Osc52ClipboardRequest, Selection, SelectionKind, TerminalCore, TerminalKey,
    TerminalKeyModifiers, TerminalMode, TerminalSize as CoreTerminalSize, encode_terminal_key,
};
use term_parser::TerminalEmulator;
use transport_core::{
    TerminalSize as TransportSize, TerminalTransport, TransportOutput, TransportResult,
    TransportState, TransportWakeHandle,
};
use transport_pty::{LocalPtyTransport, LocalShellKind, LocalShellProfile};
use transport_ssh::{SshConnectionProfile, SshTransport};
use unicode_segmentation::UnicodeSegmentation;
use winit::{
    event::{Event, WindowEvent},
    event_loop::ControlFlow,
};

pub mod fullscreen_chrome;

pub fn main_entry() {
    if std::env::args().nth(1).as_deref() == Some("gui-smoke") {
        std::process::exit(run_gui_smoke_cli());
    }
    if let Some(code) = run_cli() {
        std::process::exit(code);
    }

    if let Err(error) = run(None) {
        eprintln!("panea desktop failed: {error}");
        std::process::exit(1);
    }
}

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
        "usage: panea gui-smoke [--startup|--terminal-io] [--hold-ms <ms>] [--json] [--timeout-ms <ms>]"
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuiSmokeMode {
    FirstFrame,
    Startup,
    TerminalIo,
}

const GUI_SMOKE_MARKER: &str = "PANEAE2E_OUTPUT";

fn run_gui_smoke_cli() -> i32 {
    let args = std::env::args().skip(2).collect::<Vec<_>>();
    let json = args.iter().any(|arg| arg == "--json");
    let mut timeout = Duration::from_secs(10);
    let mut mode = GuiSmokeMode::FirstFrame;
    let mut hold_after_success = Duration::ZERO;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {}
            "--startup" => mode = GuiSmokeMode::Startup,
            "--terminal-io" => mode = GuiSmokeMode::TerminalIo,
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
    let started = Instant::now();
    let result = run(Some(GuiSmokeOptions {
        timeout,
        completed: Arc::clone(&completed),
        mode,
        hold_after_success,
    }));
    let passed = result.is_ok() && completed.load(Ordering::Acquire);
    if json {
        println!(
            "{{\"name\":\"gui-smoke\",\"status\":\"{}\",\"duration_ms\":{},\"milestone\":\"{}\"}}",
            if passed { "passed" } else { "failed" },
            started.elapsed().as_millis(),
            match mode {
                GuiSmokeMode::FirstFrame => "window_renderer_session_first_frame",
                GuiSmokeMode::Startup => "single_shell_prompt_rendered_without_input",
                GuiSmokeMode::TerminalIo => "shell_prompt_input_output_rendered",
            }
        );
    }
    if let Err(error) = result {
        eprintln!("gui smoke failed: {error}");
    } else if !passed {
        eprintln!("gui smoke timed out before its required render milestone");
    }
    i32::from(!passed)
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

    while Instant::now() < deadline {
        match transport.poll_output() {
            Ok(poll) => {
                if poll
                    .bytes
                    .windows(b"\x1b[6n".len())
                    .any(|window| window == b"\x1b[6n")
                {
                    let _ = transport.write_input(b"\x1b[1;1R");
                }
                output.extend(poll.bytes);
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

struct LoadedDesktopConfig {
    config: AppConfig,
    diagnostics: Vec<ConfigDiagnostic>,
    source: String,
    asset_base_dir: Option<PathBuf>,
    watcher: Option<DesktopConfigWatcher>,
}

enum DesktopConfigWatcher {
    Toml(config_toml::ConfigWatcher),
    Programmable(config_lua::ProgrammableConfigWatcher),
}

enum DesktopConfigWatchEvent {
    Unchanged,
    Pending {
        path: Option<PathBuf>,
    },
    Reloaded {
        config: Box<AppConfig>,
        diagnostics: Vec<ConfigDiagnostic>,
    },
    Failed {
        path: Option<PathBuf>,
        error: String,
    },
}

impl DesktopConfigWatcher {
    fn poll(&mut self) -> DesktopConfigWatchEvent {
        match self {
            Self::Toml(watcher) => match watcher.poll() {
                config_toml::ConfigWatchEvent::Unchanged => DesktopConfigWatchEvent::Unchanged,
                config_toml::ConfigWatchEvent::Pending { path } => {
                    DesktopConfigWatchEvent::Pending { path }
                }
                config_toml::ConfigWatchEvent::Reloaded(loaded) => {
                    DesktopConfigWatchEvent::Reloaded {
                        config: Box::new(loaded.config),
                        diagnostics: loaded.diagnostics,
                    }
                }
                config_toml::ConfigWatchEvent::Failed { path, error } => {
                    DesktopConfigWatchEvent::Failed {
                        path,
                        error: error.to_string(),
                    }
                }
            },
            Self::Programmable(watcher) => match watcher.poll() {
                config_lua::ProgrammableConfigWatchEvent::Unchanged => {
                    DesktopConfigWatchEvent::Unchanged
                }
                config_lua::ProgrammableConfigWatchEvent::Pending { path } => {
                    DesktopConfigWatchEvent::Pending { path: Some(path) }
                }
                config_lua::ProgrammableConfigWatchEvent::Reloaded(loaded) => {
                    DesktopConfigWatchEvent::Reloaded {
                        config: Box::new(loaded.config),
                        diagnostics: loaded.diagnostics,
                    }
                }
                config_lua::ProgrammableConfigWatchEvent::Failed { path, error } => {
                    DesktopConfigWatchEvent::Failed {
                        path: Some(path),
                        error: error.to_string(),
                    }
                }
            },
        }
    }
}

fn load_desktop_config() -> Result<LoadedDesktopConfig, Box<dyn Error>> {
    let platform = config_core::ConfigPlatform::current();

    if let Some(path) = std::env::var_os("PANEA_CONFIG").map(PathBuf::from) {
        if config_lua::is_programmable_config_path(&path) {
            let loaded = config_lua::load_path(path.clone(), true, platform)?;
            return Ok(LoadedDesktopConfig {
                config: loaded.config,
                diagnostics: loaded.diagnostics,
                source: format!("explicit:{}", path.display()),
                asset_base_dir: path.parent().map(Path::to_path_buf),
                watcher: Some(DesktopConfigWatcher::Programmable(
                    config_lua::ProgrammableConfigWatcher::new(path, platform),
                )),
            });
        }

        let options = config_toml::ConfigLoadOptions {
            explicit_path: Some(path),
            platform,
        };
        let loaded = config_toml::load(options.clone())?;
        return Ok(LoadedDesktopConfig {
            source: config_source_text(&loaded.source),
            asset_base_dir: config_source_path(&loaded.source)
                .and_then(Path::parent)
                .map(Path::to_path_buf),
            config: loaded.config,
            diagnostics: loaded.diagnostics,
            watcher: Some(DesktopConfigWatcher::Toml(config_toml::ConfigWatcher::new(
                options,
            ))),
        });
    }

    let toml_exists = config_toml::candidate_paths_for_current_platform()
        .iter()
        .any(|path| path.exists());
    if toml_exists {
        let options = config_toml::ConfigLoadOptions {
            explicit_path: None,
            platform,
        };
        let loaded = config_toml::load(options.clone())?;
        return Ok(LoadedDesktopConfig {
            source: config_source_text(&loaded.source),
            asset_base_dir: config_source_path(&loaded.source)
                .and_then(Path::parent)
                .map(Path::to_path_buf),
            config: loaded.config,
            diagnostics: loaded.diagnostics,
            watcher: Some(DesktopConfigWatcher::Toml(config_toml::ConfigWatcher::new(
                options,
            ))),
        });
    }

    if let Some(path) = config_lua::candidate_paths_for_current_platform()
        .into_iter()
        .find(|path| path.exists())
    {
        let loaded = config_lua::load_path(path.clone(), false, platform)?;
        return Ok(LoadedDesktopConfig {
            config: loaded.config,
            diagnostics: loaded.diagnostics,
            source: path.display().to_string(),
            asset_base_dir: path.parent().map(Path::to_path_buf),
            watcher: Some(DesktopConfigWatcher::Programmable(
                config_lua::ProgrammableConfigWatcher::new(path, platform),
            )),
        });
    }

    let options = config_toml::ConfigLoadOptions {
        explicit_path: None,
        platform,
    };
    let loaded = config_toml::load(options.clone())?;
    Ok(LoadedDesktopConfig {
        source: config_source_text(&loaded.source),
        asset_base_dir: config_source_path(&loaded.source)
            .and_then(Path::parent)
            .map(Path::to_path_buf),
        config: loaded.config,
        diagnostics: loaded.diagnostics,
        watcher: Some(DesktopConfigWatcher::Toml(config_toml::ConfigWatcher::new(
            options,
        ))),
    })
}

fn doctor_input() -> diagnostics::DoctorInput {
    match load_desktop_config() {
        Ok(loaded) => {
            let runtime = doctor_runtime_snapshot(&loaded.config, "loaded");
            diagnostics::DoctorInput {
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                config_source: loaded.source,
                config: loaded.config,
                config_diagnostics: loaded.diagnostics,
                platform: diagnostics::PlatformSnapshot::detect(),
                runtime,
                recent_errors: Vec::new(),
            }
        }
        Err(error) => {
            let config = AppConfig::default();
            let runtime = doctor_runtime_snapshot(&config, &error.to_string());
            diagnostics::DoctorInput {
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                config_source: "unloaded".to_owned(),
                config,
                config_diagnostics: Vec::new(),
                platform: diagnostics::PlatformSnapshot::detect(),
                runtime,
                recent_errors: vec![format!("config load failed: {error}")],
            }
        }
    }
}

fn config_source_text(source: &config_toml::ConfigSource) -> String {
    match source {
        config_toml::ConfigSource::Default => "default".to_owned(),
        config_toml::ConfigSource::File(path) => path.display().to_string(),
        config_toml::ConfigSource::ExplicitFile(path) => {
            format!("explicit:{}", path.display())
        }
    }
}

fn config_source_path(source: &config_toml::ConfigSource) -> Option<&Path> {
    match source {
        config_toml::ConfigSource::Default => None,
        config_toml::ConfigSource::File(path) | config_toml::ConfigSource::ExplicitFile(path) => {
            Some(path)
        }
    }
}

fn doctor_runtime_snapshot(
    config: &AppConfig,
    config_parse_status: &str,
) -> diagnostics::DoctorRuntimeSnapshot {
    let gpu_probe = pollster::block_on(render_wgpu::probe_gpu_adapter());
    let clipboard = ClipboardBridge::new();
    let clipboard_diagnostic = clipboard.last_diagnostic().clone();
    let notification_provider = DesktopNotificationProvider::new(config.notifications.enabled);
    let notification_diagnostic = notification_provider.diagnostic();
    let keychain = PlatformKeychainProvider::for_current_platform();
    let keychain_capability = keychain.capability();
    let performance_overlay_ui = PerformanceOverlayUiState::new(&config.diagnostics);

    diagnostics::DoctorRuntimeSnapshot {
        renderer_backend: gpu_probe.as_ref().map_or_else(
            || "wgpu adapter not detected".to_owned(),
            |probe| format!("wgpu {}", probe.backend),
        ),
        gpu_adapter: gpu_probe
            .as_ref()
            .map(|probe| format!("{} ({})", probe.adapter, probe.device_type)),
        gpu_features: gpu_probe
            .as_ref()
            .map_or_else(Vec::new, |probe| probe.features.clone()),
        window_backend: Some(window_backend_label(config)),
        x11_wayland_status: Some(x11_wayland_status()),
        dpi_scale: None,
        font_discovery: font_discovery_label(config),
        config_parse_status: config_parse_status.to_owned(),
        shell_integration_status: shell_integration_config_status(config),
        performance_overlay_status: performance_overlay_ui.diagnostic(),
        clipboard_provider: format!(
            "arboard system clipboard {:?}: {}",
            clipboard_diagnostic.availability,
            clipboard_diagnostic
                .message
                .as_deref()
                .unwrap_or("provider initialized")
        ),
        notification_provider: format!(
            "{:?} {:?}: {}",
            notification_diagnostic.backend,
            notification_diagnostic.availability,
            notification_diagnostic.message
        ),
        keychain_provider: format!(
            "{:?} available={} secure={} persistent={} ({})",
            keychain_capability.backend,
            keychain_capability.available,
            keychain_capability.secure_storage,
            keychain_capability.persistent,
            keychain_capability.message
        ),
        pty_backend: pty_backend_label(),
        ssh_provider_status: format!(
            "ssh2 transport; interactive host trust and credential prompts enabled; native keychain available={}",
            keychain_capability.available
        ),
    }
}

fn shell_integration_config_status(config: &AppConfig) -> String {
    if !config.shell_integration.enabled
        || matches!(
            config.shell_integration.activation,
            ShellIntegrationActivationConfig::Disabled
        )
    {
        return "disabled by config".to_owned();
    }
    let remote_profiles = config
        .ssh_profiles
        .iter()
        .filter(|profile| profile.shell_integration)
        .count();
    if matches!(
        config.shell_integration.activation,
        ShellIntegrationActivationConfig::Heuristic
    ) {
        return format!(
            "heuristic low-confidence mode; runtime shell/cwd/exit metadata unavailable; remote_profiles={remote_profiles}"
        );
    }
    format!(
        "configured {:?}; no active session during doctor; remote_profiles={} remain inactive until markers are observed",
        config.shell_integration.activation, remote_profiles
    )
}

fn window_backend_label(config: &AppConfig) -> String {
    if cfg!(windows) {
        "winit/windows".to_owned()
    } else if cfg!(target_os = "macos") {
        "winit/macos".to_owned()
    } else if cfg!(target_os = "linux") {
        let detected = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "wayland"
        } else if std::env::var_os("DISPLAY").is_some() {
            "x11"
        } else {
            "unavailable"
        };
        format!(
            "winit/linux requested={:?} detected={detected}",
            config.window.linux_backend
        )
    } else {
        "winit/unknown".to_owned()
    }
}

fn x11_wayland_status() -> String {
    if !cfg!(target_os = "linux") {
        return "n/a on this platform".to_owned();
    }

    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".to_owned());
    let wayland = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "unset".to_owned());
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| "unset".to_owned());
    format!("session={session} wayland_display={wayland} display={display}")
}

fn pty_backend_label() -> String {
    if cfg!(windows) {
        "portable-pty ConPTY".to_owned()
    } else if cfg!(unix) {
        "portable-pty Unix PTY".to_owned()
    } else {
        "portable-pty unknown backend".to_owned()
    }
}

fn font_discovery_label(config: &AppConfig) -> String {
    let fonts = FontSystem::new(font_config(&config.font));
    fonts
        .diagnostics()
        .into_iter()
        .map(|diagnostic| {
            let source = match &diagnostic.source {
                FontSource::File(path) => format!("file:{}", path.display()),
                FontSource::Memory => "memory".to_owned(),
                FontSource::Unresolved => "unresolved".to_owned(),
            };
            format!("{}:{}={source}", diagnostic.role, diagnostic.family)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn run(gui_smoke: Option<GuiSmokeOptions>) -> Result<(), Box<dyn Error>> {
    let startup_probe_started = gui_smoke.as_ref().map(|_| Instant::now());
    if gui_smoke.is_some() {
        eprintln!("gui-smoke milestone=config-load-start");
    }
    let loaded_config = load_desktop_config()?;
    log_config_diagnostics(&loaded_config.diagnostics);
    let mut config = loaded_config.config;
    let mut configured_performance = config.performance.clone();
    let mut power_monitor =
        DesktopPowerMonitor::with_enabled(config.performance.disable_expensive_effects_on_battery);
    let startup_power_state = power_monitor.power_state();
    apply_power_policy(
        &mut config.performance,
        &configured_performance,
        startup_power_state.state,
    );
    log_power_policy(&config.performance, &startup_power_state);
    let cursor_asset_base_dir = loaded_config.asset_base_dir;
    let mut config_watcher = loaded_config.watcher;
    let _ssh_session_profiles: Vec<SshConnectionProfile> = config
        .ssh_profiles
        .iter()
        .map(ssh_connection_profile)
        .collect();
    let settings = window_settings(&config);
    let event_loop = create_event_loop(settings.linux_backend)?;
    let transport_waker = TransportWakeHandle::new({
        let event_loop_proxy = event_loop.create_proxy();
        move || {
            let _ = event_loop_proxy.send_event(());
        }
    });
    let desktop_window = DesktopWindow::create(&event_loop, &settings)?;
    if gui_smoke.is_some() {
        eprintln!(
            "gui-smoke milestone=window-created elapsed_ms={}",
            startup_probe_started.map_or(0, |started| started.elapsed().as_millis())
        );
    }
    if let Some(fallback) = desktop_window.diagnostics().window_mode.fallback.as_ref() {
        eprintln!(
            "platform fallback [{}]: requested={} effective={} reason={}",
            fallback.feature, fallback.requested, fallback.effective, fallback.reason
        );
    }
    if let Some(fallback) = desktop_window.diagnostics().decoration.fallback.as_ref() {
        eprintln!(
            "platform fallback [{}]: requested={} effective={} reason={}",
            fallback.feature, fallback.requested, fallback.effective, fallback.reason
        );
    }
    if let Some(fallback) = desktop_window
        .diagnostics()
        .linux
        .as_ref()
        .and_then(|diagnostic| diagnostic.fallback.as_ref())
    {
        eprintln!(
            "platform fallback [{}]: requested={} effective={} reason={}",
            fallback.feature, fallback.requested, fallback.effective, fallback.reason
        );
    }
    let window = desktop_window.window();
    let capabilities = platform_capabilities(&event_loop, &window);
    let _diagnostics =
        DesktopDiagnosticsPlaceholder::new(desktop_window.diagnostics().clone(), capabilities);
    let mut input_translator = InputTranslator::new();
    let mut clipboard = ClipboardBridge::new();
    let mut notification_provider = DesktopNotificationProvider::new(config.notifications.enabled);
    let mut url_opener = DesktopUrlOpener::new();
    let mut current_window_mode = desktop_window.diagnostics().window_mode.effective;
    let decoration_mode = map_decoration_mode(config.window.decoration_strategy);
    let mut clipboard_config = config.clipboard.clone();
    let mut paste_config = config.paste.clone();
    let mut osc52_policy = osc52_policy(&clipboard_config);

    let mut dpi_scale_factor = window.scale_factor();
    let mut fonts = FontSystem::new_with_scale_factor(font_config(&config.font), dpi_scale_factor);
    let metrics = fonts.cell_metrics()?;
    // Start the transport as soon as cell metrics are available. PTY startup
    // and initial shell output can then overlap GPU adapter/device creation.
    let mut surface_size = window.inner_size();
    let mut mux_runtime = MuxRuntime::new(
        &config,
        metrics,
        surface_size.width,
        surface_size.height,
        transport_waker,
    );
    if gui_smoke.is_some() {
        eprintln!(
            "gui-smoke milestone=session-created elapsed_ms={}",
            startup_probe_started.map_or(0, |started| started.elapsed().as_millis())
        );
    }
    let mut renderer = pollster::block_on(GpuTerminalRenderer::new(
        Arc::clone(&window),
        renderer_options(&config),
    ))?;
    if let Err(error) = renderer.present_startup_background() {
        eprintln!(
            "renderer startup background fallback: {error}; revealing the window for normal first-frame rendering"
        );
    }
    if config.renderer.damage_tracking {
        let retained_status = renderer.retained_damage_status();
        if retained_status != RetainedDamageStatus::Enabled {
            eprintln!(
                "renderer fallback: retained damage presentation is {retained_status}; using event-driven full-frame GPU batches"
            );
        }
    }
    if gui_smoke.is_some() {
        eprintln!(
            "gui-smoke milestone=renderer-created elapsed_ms={}",
            startup_probe_started.map_or(0, |started| started.elapsed().as_millis())
        );
    }
    if config.window.opacity < 1.0 && !renderer.transparency_active() {
        eprintln!(
            "window opacity fallback: GPU/window backend exposes only opaque composition; rendering remains fully opaque"
        );
    }
    let mut scheduler = FrameScheduler::new();
    let mut damage_tracker = DamageTracker::new();
    let mut performance_overlay_ui = PerformanceOverlayUiState::new(&config.diagnostics);
    let mut performance_overlay = PerformanceOverlay::new(performance_overlay_ui.enabled, "wgpu");
    update_performance_overlay_context(
        &mut performance_overlay,
        &config,
        startup_power_state.state,
    );
    let mut performance_budget = performance_budget_from_config(&config);
    let mut cursor_animator = CursorAnimationRuntime::new();
    let mut cursor_blink = CursorBlinkRuntime::new();
    let mut window_focused = true;
    let mut pointer_visible = true;
    let mut pending_terminal_resize = PendingTerminalResize::default();
    let mut cursor_image_cache = AnimatedCursorImageCache::new();
    let mut cursor_image_runtime = AnimatedCursorImageRuntime::new();
    let mut cursor_image_status_reported: Option<String> = None;
    let mut cursor_vector_cache = CursorVectorCache::new();
    let mut cursor_vector_runtime = CursorVectorRuntime::new();
    let mut cursor_vector_status_reported: Option<String> = None;
    request_cursor_image_if_enabled(
        &mut cursor_image_cache,
        &config,
        cursor_asset_base_dir.as_deref(),
    );
    request_cursor_vector_if_enabled(
        &mut cursor_vector_cache,
        &config,
        cursor_asset_base_dir.as_deref(),
    );

    input_translator.arm_initial_focus_handoff();
    window.set_visible(true);
    scheduler.terminal_content_changed();
    let gui_smoke_deadline = gui_smoke
        .as_ref()
        .map(|smoke| Instant::now() + smoke.timeout);
    let gui_smoke_mode = gui_smoke.as_ref().map(|smoke| smoke.mode);
    let gui_smoke_hold = gui_smoke
        .as_ref()
        .map_or(Duration::ZERO, |smoke| smoke.hold_after_success);
    let gui_smoke_completed = gui_smoke.map(|smoke| smoke.completed);
    let gui_smoke_result = gui_smoke_completed.clone();
    let mut gui_smoke_command_sent = false;
    let mut gui_smoke_startup_prompt_observed_at = None;
    let mut gui_smoke_startup_validated = false;
    let mut gui_smoke_success_presented = false;
    let mut gui_smoke_hold_until = None;

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Wait);

        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::RedrawRequested => {
                    let metrics = fonts.cell_metrics().ok();
                    let observed_size = window.inner_size();
                    if surface_size_is_renderable(observed_size) && observed_size != surface_size {
                        surface_size = observed_size;
                        renderer.resize(surface_size.width, surface_size.height);
                        pending_terminal_resize.queue(surface_size);
                        damage_tracker.request_full_redraw();
                    }
                    let mut scene = scene_from_mux(
                        &mux_runtime,
                        metrics,
                        &config,
                        Some(&mut cursor_animator),
                        Some(&mut cursor_image_runtime),
                        Some(&mut cursor_vector_runtime),
                        CursorPresentation {
                            blink_visible: cursor_blink.visible(),
                            window_focused,
                        },
                    );
                    if let Some(metrics) = metrics {
                        append_performance_overlay(
                            &mut scene,
                            &performance_overlay,
                            &performance_overlay_ui,
                            performance_budget,
                            metrics,
                        );
                        if let Some(cursor) = scene.cursor {
                            let x = scene.content_offset.x.max(0) as f64
                                + f64::from(cursor.position.col) * f64::from(metrics.cell_width);
                            let y = scene.content_offset.y.max(0) as f64
                                + cursor.position.row.max(0) as f64
                                    * f64::from(metrics.cell_height);
                            window.set_ime_cursor_area(
                                winit::dpi::PhysicalPosition::new(x, y),
                                winit::dpi::PhysicalSize::new(
                                    f64::from(metrics.cell_width),
                                    f64::from(metrics.cell_height),
                                ),
                            );
                        }
                        scene.damage_regions = damage_tracker.update(&scene, metrics);
                    }
                    let idle_wakeups = scheduler.take_idle_wakeups();
                    match catch_unwind(AssertUnwindSafe(|| {
                        renderer.render_scene(&scene, &mut fonts)
                    })) {
                        Ok(Ok(())) => {
                            let mut instrumentation = renderer.last_instrumentation();
                            instrumentation.idle_wakeups = idle_wakeups;
                            if performance_overlay.is_enabled() {
                                mux_runtime.populate_performance_sample(&mut instrumentation);
                            }
                            let had_performance_sample = performance_overlay.latest().is_some();
                            performance_overlay.record(instrumentation);
                            if performance_overlay.is_enabled() && !had_performance_sample {
                                scheduler.terminal_content_changed();
                                window.request_redraw();
                            }
                            if matches!(config.diagnostics.log_level, LogLevel::Trace)
                                && let Some(text) =
                                    performance_overlay.render_text(performance_budget)
                            {
                                eprintln!("performance {text}");
                            }
                            if let Some(completed) = &gui_smoke_completed {
                                let milestone_reached = match gui_smoke_mode {
                                    Some(GuiSmokeMode::Startup) => gui_smoke_startup_validated,
                                    Some(GuiSmokeMode::TerminalIo) => {
                                        gui_smoke_command_sent
                                            && mux_runtime
                                                .active_visible_text()
                                                .matches(GUI_SMOKE_MARKER)
                                                .count()
                                                >= 2
                                    }
                                    Some(GuiSmokeMode::FirstFrame) | None => true,
                                };
                                if milestone_reached && !gui_smoke_success_presented {
                                    eprintln!("gui-smoke milestone=frame-presented");
                                    gui_smoke_success_presented = true;
                                    if gui_smoke_hold.is_zero() {
                                        completed.store(true, Ordering::Release);
                                        mux_runtime.shutdown_all();
                                        target.exit();
                                    } else {
                                        gui_smoke_hold_until = Some(Instant::now() + gui_smoke_hold);
                                    }
                                }
                            }
                        }
                        Ok(Err(error)) => match error {
                            RendererError::DeviceLost { reason, message } => {
                                eprintln!("render device lost ({reason:?}): {message}");
                                match pollster::block_on(renderer.recover_from_device_loss(reason))
                                {
                                    Ok(event) => {
                                        eprintln!("render recovery: {}", event.message);
                                        scheduler.terminal_content_changed();
                                        window.request_redraw();
                                    }
                                    Err(recovery_error) => {
                                        eprintln!("render recovery failed: {recovery_error}");
                                    }
                                }
                            }
                            error => {
                                eprintln!("render error: {error}");
                            }
                        },
                        Err(panic) => {
                            eprintln!("render panic boundary: {}", panic_payload(panic));
                            scheduler.terminal_content_changed();
                        }
                    }
                }
                _ => {
                    // PTY output can contain synchronous terminal queries (for
                    // example, CPR/DSR used by PowerShell's line editor). Apply
                    // pending output and send terminal responses before a host
                    // input or resize event is allowed to overtake it.
                    if mux_runtime.poll_outputs(
                        &mut clipboard,
                        &osc52_policy,
                        &clipboard_config,
                        &mut notification_provider,
                        &config.notifications,
                        window_focused,
                    ) {
                        scheduler.terminal_content_changed();
                        window.request_redraw();
                    }
                    let platform_events = match catch_unwind(AssertUnwindSafe(|| {
                        input_translator.translate_window_event(&event)
                    })) {
                        Ok(events) => events,
                        Err(panic) => {
                            eprintln!("platform event panic boundary: {}", panic_payload(panic));
                            Vec::new()
                        }
                    };
                    for platform_event in platform_events {
                        match platform_event {
                            InputEvent::CloseRequested => {
                                mux_runtime.shutdown_all();
                                target.exit();
                            }
                            InputEvent::Resized { width, height } => {
                                let resized = winit::dpi::PhysicalSize::new(width, height);
                                if !surface_size_is_renderable(resized) {
                                    continue;
                                }
                                surface_size = resized;
                                if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
                                    renderer.resize(width, height)
                                })) {
                                    eprintln!(
                                        "renderer resize panic boundary: {}",
                                        panic_payload(panic)
                                    );
                                }
                                pending_terminal_resize.queue(resized);
                                scheduler.window_resized();
                                window.request_redraw();
                            }
                            InputEvent::ScaleFactorChanged { scale_factor } => {
                                let observed_size = window.inner_size();
                                if surface_size_is_renderable(observed_size) {
                                    surface_size = observed_size;
                                }
                                renderer.resize(surface_size.width, surface_size.height);
                                dpi_scale_factor = scale_factor;
                                if fonts.set_scale_factor(scale_factor) {
                                    renderer.request_full_redraw();
                                }
                                pending_terminal_resize.queue(surface_size);
                                if matches!(config.diagnostics.log_level, LogLevel::Debug | LogLevel::Trace) {
                                    eprintln!("DPI scale changed to {scale_factor:.3}");
                                }
                                scheduler.window_resized();
                                window.request_redraw();
                            }
                            InputEvent::Key(key) => {
                                if key.state != KeyState::Pressed {
                                    continue;
                                }
                                if config.mouse.hide_cursor_when_typing && pointer_visible {
                                    window.set_cursor_visible(false);
                                    pointer_visible = false;
                                }

                                if let Some(changed) = mux_runtime.handle_modal_key(
                                    &key,
                                    &mut clipboard,
                                    &osc52_policy,
                                    &clipboard_config,
                                ) {
                                    if changed {
                                        scheduler.terminal_content_changed();
                                        window.request_redraw();
                                    }
                                    continue;
                                }

                                if let Some(action) = keybinding_action(&key, &config) {
                                    match action.as_str() {
                                        "copy" => {
                                            if clipboard_config.enabled
                                                && let Some(text) =
                                                    mux_runtime.active_selected_text()
                                            {
                                                copy_text_with_diagnostics(
                                                    &mut clipboard,
                                                    &text,
                                                    &clipboard_config,
                                                    "selection copy",
                                                );
                                            }
                                        }
                                        "paste" => {
                                            if clipboard_config.enabled
                                                && let Ok(text) = clipboard.paste_text()
                                            {
                                                cursor_animator.record_typing();
                                                if cursor_blink.record_activity() {
                                                    scheduler.cursor_blink_changed();
                                                }
                                                mux_runtime.paste_into_active(
                                                    &text,
                                                    &clipboard_config,
                                                    &paste_config,
                                                );
                                            }
                                        }
                                        "scroll_page_up" => {
                                            if mux_runtime.scroll_active_page(true) {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "scroll_page_down" => {
                                            if mux_runtime.scroll_active_page(false) {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "scroll_to_top" => {
                                            if mux_runtime.scroll_active_to_top() {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "scroll_to_bottom" => {
                                            if mux_runtime.scroll_active_to_bottom() {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "search_scrollback" => {
                                            mux_runtime.start_search();
                                            scheduler.terminal_content_changed();
                                            window.request_redraw();
                                        }
                                        "keyboard_select" => {
                                            mux_runtime.start_keyboard_selection(
                                                SelectionKind::Normal,
                                            );
                                            scheduler.terminal_content_changed();
                                            window.request_redraw();
                                        }
                                        "keyboard_select_rectangular" => {
                                            mux_runtime.start_keyboard_selection(
                                                SelectionKind::Rectangular,
                                            );
                                            scheduler.terminal_content_changed();
                                            window.request_redraw();
                                        }
                                        "jump_to_previous_command" => {
                                            if config.command_blocks.jump_actions_enabled {
                                                let _ = mux_runtime.run_semantic_action(
                                                    SemanticAction::JumpToPreviousCommand,
                                                );
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "jump_to_next_command" => {
                                            if config.command_blocks.jump_actions_enabled {
                                                let _ = mux_runtime.run_semantic_action(
                                                    SemanticAction::JumpToNextCommand,
                                                );
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "select_current_command_output" => {
                                            if config.command_blocks.copy_actions_enabled {
                                                let _ = mux_runtime.run_semantic_action(
                                                    SemanticAction::SelectCurrentCommandOutput,
                                                );
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "copy_current_command_output" => {
                                            if config.command_blocks.copy_actions_enabled
                                                && let SemanticActionResult::Text(text) = mux_runtime
                                                .run_semantic_action(
                                                    SemanticAction::CopyCurrentCommandOutput,
                                                )
                                            {
                                                copy_text_with_diagnostics(
                                                    &mut clipboard,
                                                    &text,
                                                    &clipboard_config,
                                                    "semantic command output copy",
                                                );
                                            }
                                        }
                                        "copy_command_and_output" => {
                                            if config.command_blocks.copy_actions_enabled
                                                && let SemanticActionResult::Text(text) = mux_runtime
                                                .run_semantic_action(
                                                    SemanticAction::CopyCommandAndOutput,
                                                )
                                            {
                                                copy_text_with_diagnostics(
                                                    &mut clipboard,
                                                    &text,
                                                    &clipboard_config,
                                                    "semantic command and output copy",
                                                );
                                            }
                                        }
                                        "toggle_current_command_output" => {
                                            if mux_runtime.toggle_current_command_output(&config) {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "reconnect_session" => {
                                            if mux_runtime.reconnect_active(&config, metrics) {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            }
                                        }
                                        "toggle_performance_overlay" => {
                                            performance_overlay_ui.toggle();
                                            performance_overlay.set_enabled(
                                                performance_overlay_ui.enabled,
                                            );
                                            let power_state = power_monitor.power_state();
                                            update_performance_overlay_context(
                                                &mut performance_overlay,
                                                &config,
                                                power_state.state,
                                            );
                                            scheduler.terminal_content_changed();
                                            window.request_redraw();
                                        }
                                        "toggle_fullscreen" => {
                                            current_window_mode = if matches!(
                                                current_window_mode,
                                                WindowMode::Windowed
                                            ) {
                                                WindowMode::BorderlessFullscreen
                                            } else {
                                                WindowMode::Windowed
                                            };
                                            current_window_mode = apply_window_mode_logged(
                                                &window,
                                                current_window_mode,
                                                decoration_mode,
                                            );
                                            scheduler.window_resized();
                                            window.request_redraw();
                                        }
                                        "restore_window_decorations" => {
                                            current_window_mode = WindowMode::Windowed;
                                            current_window_mode = apply_window_mode_logged(
                                                &window,
                                                current_window_mode,
                                                decoration_mode,
                                            );
                                            scheduler.window_resized();
                                            window.request_redraw();
                                        }
                                        "toggle_frameless" => {
                                            current_window_mode = if matches!(
                                                current_window_mode,
                                                WindowMode::FramelessWindowed
                                            ) {
                                                WindowMode::Windowed
                                            } else {
                                                WindowMode::FramelessWindowed
                                            };
                                            current_window_mode = apply_window_mode_logged(
                                                &window,
                                                current_window_mode,
                                                decoration_mode,
                                            );
                                            scheduler.window_resized();
                                            window.request_redraw();
                                        }
                                        "close_window" => {
                                            mux_runtime.shutdown_all();
                                            target.exit();
                                        }
                                        "open_command_palette_later" => {
                                            eprintln!(
                                                "command palette action is reserved for a later phase"
                                            );
                                        }
                                        _ => {
                                            if mux_runtime.handle_profile_mux_action(
                                                &action,
                                                &config,
                                                metrics,
                                                surface_size.width,
                                                surface_size.height,
                                            ) {
                                                scheduler.terminal_content_changed();
                                                window.request_redraw();
                                            } else if let Some(action) = MuxAction::named(&action) {
                                                if mux_runtime.handle_mux_action(
                                                    action,
                                                    &config,
                                                    metrics,
                                                    surface_size.width,
                                                    surface_size.height,
                                                ) {
                                                    scheduler.terminal_content_changed();
                                                    window.request_redraw();
                                                }
                                            } else {
                                                eprintln!("unhandled keybinding action: {action}");
                                            }
                                        }
                                    }
                                } else if let Some(bytes) = mux_runtime.input_bytes(&key) {
                                    cursor_animator.record_typing();
                                    if cursor_blink.record_activity() {
                                        scheduler.cursor_blink_changed();
                                    }
                                    mux_runtime.write_active(&bytes);
                                }
                            }
                            InputEvent::Mouse(mouse) => {
                                if !pointer_visible {
                                    window.set_cursor_visible(true);
                                    pointer_visible = true;
                                }
                                if let Ok(metrics) = fonts.cell_metrics() {
                                    if handle_performance_overlay_mouse(
                                        mouse,
                                        &performance_overlay,
                                        &mut performance_overlay_ui,
                                        performance_budget,
                                        metrics,
                                        mux_runtime.surface_cols,
                                        mux_runtime.surface_rows,
                                        &config,
                                    ) {
                                        performance_overlay
                                            .set_enabled(performance_overlay_ui.enabled);
                                        scheduler.terminal_content_changed();
                                        window.request_redraw();
                                        continue;
                                    }
                                    let outcome = mux_runtime.handle_mouse(
                                        mouse,
                                        metrics,
                                        &config,
                                        &clipboard_config,
                                        &paste_config,
                                        &mut clipboard,
                                    );
                                    if let Some(url) = outcome.open_url
                                        && let Err(diagnostic) = url_opener.open_url(&url)
                                    {
                                        eprintln!("URL action failed: {diagnostic:?}");
                                    }
                                    if outcome.changed {
                                        scheduler.terminal_content_changed();
                                        window.request_redraw();
                                    }
                                }
                            }
                            InputEvent::Ime(platform_core::ImeEvent::Commit { text }) => {
                                let _ = mux_runtime.update_active_ime_preedit(String::new());
                                if mux_runtime.append_modal_text(&text)
                                    || mux_runtime.append_search_text(&text)
                                {
                                    scheduler.terminal_content_changed();
                                    window.request_redraw();
                                } else {
                                    cursor_animator.record_typing();
                                    if cursor_blink.record_activity() {
                                        scheduler.cursor_blink_changed();
                                    }
                                    mux_runtime.write_active(text.as_bytes());
                                }
                            }
                            InputEvent::Ime(platform_core::ImeEvent::Preedit { text }) => {
                                if mux_runtime.update_active_ime_preedit(text) {
                                    scheduler.terminal_content_changed();
                                    window.request_redraw();
                                }
                            }
                            InputEvent::Ime(platform_core::ImeEvent::Enabled) => {}
                            InputEvent::Ime(platform_core::ImeEvent::Disabled) => {
                                if mux_runtime.update_active_ime_preedit(String::new()) {
                                    scheduler.terminal_content_changed();
                                    window.request_redraw();
                                }
                            }
                            InputEvent::Focused(focused) => {
                                window_focused = focused;
                                if cursor_blink.record_activity() {
                                    scheduler.cursor_blink_changed();
                                }
                                mux_runtime.send_focus_event(focused);
                                scheduler.cursor_blink_changed();
                                window.request_redraw();
                            }
                            InputEvent::WindowAction(action) => match action {
                                WindowAction::ToggleFullscreen => {
                                    current_window_mode =
                                        if matches!(current_window_mode, WindowMode::Windowed) {
                                            WindowMode::BorderlessFullscreen
                                        } else {
                                            WindowMode::Windowed
                                        };
                                    current_window_mode = apply_window_mode_logged(
                                        &window,
                                        current_window_mode,
                                        decoration_mode,
                                    );
                                    scheduler.window_resized();
                                    window.request_redraw();
                                }
                                WindowAction::RestoreWindowDecorations => {
                                    current_window_mode = WindowMode::Windowed;
                                    current_window_mode = apply_window_mode_logged(
                                        &window,
                                        current_window_mode,
                                        decoration_mode,
                                    );
                                    scheduler.window_resized();
                                    window.request_redraw();
                                }
                                WindowAction::ToggleFrameless => {
                                    current_window_mode = if matches!(
                                        current_window_mode,
                                        WindowMode::FramelessWindowed
                                    ) {
                                        WindowMode::Windowed
                                    } else {
                                        WindowMode::FramelessWindowed
                                    };
                                    current_window_mode = apply_window_mode_logged(
                                        &window,
                                        current_window_mode,
                                        decoration_mode,
                                    );
                                    scheduler.window_resized();
                                    window.request_redraw();
                                }
                                WindowAction::CloseWindow => {
                                    mux_runtime.shutdown_all();
                                    target.exit();
                                }
                                WindowAction::OpenCommandPaletteLater => {
                                    eprintln!(
                                        "command palette action is reserved for a later phase"
                                    );
                                }
                            },
                        }
                    }
                }
            },
            Event::UserEvent(()) => {
                // Local PTY readers wake the event loop when bytes become
                // available. Process that wake immediately instead of waiting
                // for AboutToWait, where queued keyboard events could otherwise
                // be handled first.
                if mux_runtime.poll_outputs(
                    &mut clipboard,
                    &osc52_policy,
                    &clipboard_config,
                    &mut notification_provider,
                    &config.notifications,
                    window_focused,
                ) {
                    scheduler.terminal_content_changed();
                    window.request_redraw();
                }
            }
            Event::AboutToWait => {
                let now = Instant::now();
                if let Some(size) = pending_terminal_resize.take_due(now) {
                    if let Ok(metrics) = fonts.cell_metrics() {
                        mux_runtime.resize_all(size.width, size.height, metrics, &config);
                        damage_tracker.request_full_redraw();
                        scheduler.window_resized();
                        window.request_redraw();
                    }
                }
                if gui_smoke_hold_until.is_some_and(|deadline| Instant::now() >= deadline) {
                    if let Some(completed) = &gui_smoke_completed {
                        completed.store(true, Ordering::Release);
                    }
                    mux_runtime.shutdown_all();
                    target.exit();
                    return;
                }
                if !gui_smoke_success_presented
                    && gui_smoke_deadline.is_some_and(|deadline| Instant::now() >= deadline)
                {
                    eprintln!("gui-smoke milestone=timeout");
                    if matches!(
                        gui_smoke_mode,
                        Some(GuiSmokeMode::Startup | GuiSmokeMode::TerminalIo)
                    ) {
                        let preview = mux_runtime.active_visible_text();
                        eprintln!(
                            "gui-smoke terminal-preview={:?} command-sent={gui_smoke_command_sent}",
                            preview.chars().take(1024).collect::<String>()
                        );
                    }
                    mux_runtime.shutdown_all();
                    target.exit();
                    return;
                }
                if let Some(config_watcher) = config_watcher.as_mut() {
                    match config_watcher.poll() {
                        DesktopConfigWatchEvent::Unchanged => {}
                        DesktopConfigWatchEvent::Pending { path } => {
                            if matches!(
                                config.diagnostics.log_level,
                                LogLevel::Debug | LogLevel::Trace
                            ) {
                                eprintln!(
                                    "config reload pending{}",
                                    path.as_ref()
                                        .map(|path| format!(" for {}", path.display()))
                                        .unwrap_or_default()
                                );
                            }
                        }
                        DesktopConfigWatchEvent::Reloaded {
                            config: loaded,
                            diagnostics,
                        } => {
                            let loaded = *loaded;
                            let next_configured_performance = loaded.performance.clone();
                            log_config_diagnostics(&diagnostics);
                            let plan = config.reload_plan_from(&loaded);
                            log_reload_plan(&plan);
                            match apply_live_config_reload(
                                &mut config,
                                loaded,
                                &plan,
                                &mut fonts,
                                &mut clipboard_config,
                                &mut paste_config,
                                &mut osc52_policy,
                                &mut notification_provider,
                                &mut performance_overlay,
                                &mut performance_overlay_ui,
                                &mut performance_budget,
                                &mut renderer,
                                &window,
                                dpi_scale_factor,
                            ) {
                                Ok(reloaded) => {
                                    configured_performance = next_configured_performance;
                                    power_monitor.set_enabled(
                                        configured_performance
                                            .disable_expensive_effects_on_battery,
                                    );
                                    let power_state = power_monitor.power_state();
                                    apply_power_policy(
                                        &mut config.performance,
                                        &configured_performance,
                                        power_state.state,
                                    );
                                    renderer.set_glyph_cache_capacity(
                                        config.performance.glyph_cache_entries,
                                    );
                                    performance_budget = performance_budget_from_config(&config);
                                    update_performance_overlay_context(
                                        &mut performance_overlay,
                                        &config,
                                        power_state.state,
                                    );
                                    request_cursor_image_if_enabled(
                                        &mut cursor_image_cache,
                                        &config,
                                        cursor_asset_base_dir.as_deref(),
                                    );
                                    cursor_image_status_reported = None;
                                    request_cursor_vector_if_enabled(
                                        &mut cursor_vector_cache,
                                        &config,
                                        cursor_asset_base_dir.as_deref(),
                                    );
                                    cursor_vector_status_reported = None;
                                    if reloaded {
                                        if let Ok(metrics) = fonts.cell_metrics() {
                                            mux_runtime.resize_all(
                                                surface_size.width,
                                                surface_size.height,
                                                metrics,
                                                &config,
                                            );
                                        }
                                        scheduler.terminal_content_changed();
                                        window.request_redraw();
                                    }
                                }
                                Err(message) => {
                                    eprintln!(
                                        "config reload rejected: {message}; keeping previous valid config"
                                    );
                                }
                            }
                        }
                        DesktopConfigWatchEvent::Failed { path, error } => {
                            eprintln!(
                                "config reload failed{}: {error}; keeping previous valid config",
                                path.as_ref()
                                    .map(|path| format!(" for {}", path.display()))
                                    .unwrap_or_default()
                            );
                        }
                    }
                }

                if power_monitor.refresh_if_due() {
                    let power_state = power_monitor.power_state();
                    apply_power_policy(
                        &mut config.performance,
                        &configured_performance,
                        power_state.state,
                    );
                    renderer.set_glyph_cache_capacity(config.performance.glyph_cache_entries);
                    performance_budget = performance_budget_from_config(&config);
                    request_cursor_image_if_enabled(
                        &mut cursor_image_cache,
                        &config,
                        cursor_asset_base_dir.as_deref(),
                    );
                    cursor_image_status_reported = None;
                    request_cursor_vector_if_enabled(
                        &mut cursor_vector_cache,
                        &config,
                        cursor_asset_base_dir.as_deref(),
                    );
                    cursor_vector_status_reported = None;
                    update_performance_overlay_context(
                        &mut performance_overlay,
                        &config,
                        power_state.state,
                    );
                    log_power_policy(&config.performance, &power_state);
                    scheduler.animation_changed();
                    window.request_redraw();
                }

                if mux_runtime.poll_outputs(
                    &mut clipboard,
                    &osc52_policy,
                    &clipboard_config,
                    &mut notification_provider,
                    &config.notifications,
                    window_focused,
                ) {
                    scheduler.terminal_content_changed();
                }

                if gui_smoke_mode == Some(GuiSmokeMode::TerminalIo)
                    && !gui_smoke_command_sent
                    && shell_prompt_visible(&mux_runtime.active_visible_text())
                {
                    mux_runtime.write_active(
                        format!("echo {GUI_SMOKE_MARKER}\r").as_bytes(),
                    );
                    gui_smoke_command_sent = true;
                    eprintln!("gui-smoke milestone=prompt-observed-input-sent");
                }

                if gui_smoke_mode == Some(GuiSmokeMode::Startup)
                    && !gui_smoke_startup_validated
                {
                    let visible = mux_runtime.active_visible_text();
                    if shell_prompt_visible(&visible) {
                        let observed_at = gui_smoke_startup_prompt_observed_at
                            .get_or_insert_with(Instant::now);
                        let settled_at = *observed_at + Duration::from_millis(500);
                        if Instant::now() >= settled_at {
                            let prompt_count = shell_prompt_line_count(&visible);
                            eprintln!(
                                "gui-smoke startup-prompt-count={prompt_count} terminal-preview={:?}",
                                visible.chars().take(1024).collect::<String>()
                            );
                            if prompt_count != 1 {
                                eprintln!(
                                    "gui-smoke startup failed: expected exactly one prompt without user input"
                                );
                                mux_runtime.shutdown_all();
                                target.exit();
                                return;
                            }
                            gui_smoke_startup_validated = true;
                            scheduler.terminal_content_changed();
                            window.request_redraw();
                        } else {
                            // The shared wake-deadline calculation below includes
                            // this settle deadline without overwriting animation,
                            // transport, hold, or overall smoke deadlines.
                        }
                    }
                }

                match cursor_image_cache.poll() {
                    AnimatedCursorImageStatus::Ready(image) => {
                        if cursor_image_runtime.set_image(&image) {
                            scheduler.animation_changed();
                            window.request_redraw();
                        }
                        let key = format!("ready:{}", image.path.display());
                        if cursor_image_status_reported.as_deref() != Some(&key) {
                            for warning in image.warnings {
                                eprintln!("cursor image warning: {warning}");
                            }
                            cursor_image_status_reported = Some(key);
                        }
                    }
                    AnimatedCursorImageStatus::Failed { path, message } => {
                        if cursor_image_runtime.clear() {
                            scheduler.animation_changed();
                            window.request_redraw();
                        }
                        let key = format!("failed:{}:{message}", path.display());
                        if cursor_image_status_reported.as_deref() != Some(&key) {
                            eprintln!("cursor image {} failed: {message}", path.display());
                            cursor_image_status_reported = Some(key);
                        }
                    }
                    AnimatedCursorImageStatus::Disabled => {
                        if cursor_image_runtime.clear() {
                            scheduler.animation_changed();
                            window.request_redraw();
                        }
                    }
                    AnimatedCursorImageStatus::Loading { .. } => {}
                }

                match cursor_vector_cache.poll() {
                    CursorVectorStatus::Ready(vector) => {
                        if cursor_vector_runtime.set_vector(&vector) {
                            scheduler.terminal_content_changed();
                            window.request_redraw();
                        }
                    }
                    CursorVectorStatus::Failed { path, message } => {
                        if cursor_vector_runtime.clear() {
                            scheduler.terminal_content_changed();
                            window.request_redraw();
                        }
                        let key = format!("{}:{message}", path.display());
                        if cursor_vector_status_reported.as_deref() != Some(&key) {
                            eprintln!("cursor vector {} failed: {message}", path.display());
                            cursor_vector_status_reported = Some(key);
                        }
                    }
                    CursorVectorStatus::Disabled => {
                        if cursor_vector_runtime.clear() {
                            scheduler.terminal_content_changed();
                            window.request_redraw();
                        }
                    }
                    CursorVectorStatus::Loading { .. } => {}
                }

                let cursor_settings = cursor_animation_settings(&config);
                let blink_enabled = window_focused
                    && config.cursor.blink
                    && mux_runtime.active_cursor_blinks();
                if cursor_blink.update(
                    blink_enabled,
                    Duration::from_millis(u64::from(config.cursor.blink_interval_ms)),
                ) {
                    scheduler.cursor_blink_changed();
                }
                let animation_delay = cursor_animator.next_frame_after(cursor_settings);
                let cursor_image_delay = cursor_image_runtime.next_frame_after();
                let blink_delay = cursor_blink.next_frame_after();
                let power_delay = power_monitor.next_refresh_after();
                // SSH currently exposes non-blocking reads without a native
                // readiness callback. Keep that backend responsive with a
                // bounded fallback; local PTYs wake this event loop directly.
                let transport_poll_delay = mux_runtime
                    .requires_periodic_transport_poll()
                    .then_some(Duration::from_millis(8));
                let next_delay = [
                    animation_delay,
                    cursor_image_delay,
                    blink_delay,
                    power_delay,
                    transport_poll_delay,
                ]
                .into_iter()
                .flatten()
                .min();
                let mut next_wake = next_delay.map(|delay| Instant::now() + delay);
                if next_delay.is_some()
                    && (animation_delay.is_some() || cursor_image_delay.is_some())
                {
                    scheduler.animation_changed();
                }
                if gui_smoke_mode == Some(GuiSmokeMode::Startup)
                    && !gui_smoke_startup_validated
                    && let Some(observed_at) = gui_smoke_startup_prompt_observed_at
                {
                    retain_earliest_deadline(
                        &mut next_wake,
                        observed_at + Duration::from_millis(500),
                    );
                }
                if let Some(deadline) = gui_smoke_deadline {
                    retain_earliest_deadline(&mut next_wake, deadline);
                }
                if let Some(deadline) = gui_smoke_hold_until {
                    retain_earliest_deadline(&mut next_wake, deadline);
                }
                if let Some(deadline) = pending_terminal_resize.deadline() {
                    retain_earliest_deadline(&mut next_wake, deadline);
                }
                if let Some(deadline) = next_wake {
                    target.set_control_flow(ControlFlow::WaitUntil(deadline));
                }

                if matches!(scheduler.next_frame(), FrameDecision::FrameNeeded(_)) {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    })?;

    if gui_smoke_result.is_some() {
        eprintln!("gui-smoke milestone=event-loop-exited");
    }

    Ok(())
}

fn retain_earliest_deadline(current: &mut Option<Instant>, candidate: Instant) {
    if current.is_none_or(|deadline| candidate < deadline) {
        *current = Some(candidate);
    }
}

fn log_config_diagnostics(diagnostics: &[config_core::ConfigDiagnostic]) {
    for diagnostic in diagnostics {
        let level = match diagnostic.severity {
            ConfigDiagnosticSeverity::Error => "error",
            ConfigDiagnosticSeverity::Warning => "warning",
        };
        eprintln!(
            "config {level} at {}: {}",
            diagnostic.path, diagnostic.message
        );
    }
}

fn log_reload_plan(plan: &ReloadPlan) {
    if !plan.live.is_empty() {
        eprintln!("config reload live sections: {:?}", plan.live);
    }
    for change in &plan.restart_required {
        eprintln!(
            "config reload restart required for {}: {}",
            change.path, change.reason
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_live_config_reload(
    current: &mut AppConfig,
    next: AppConfig,
    plan: &ReloadPlan,
    fonts: &mut FontSystem,
    clipboard_config: &mut ClipboardConfig,
    paste_config: &mut PasteConfig,
    runtime_osc52_policy: &mut Osc52ClipboardPolicy,
    notification_provider: &mut DesktopNotificationProvider,
    performance_overlay: &mut PerformanceOverlay,
    performance_overlay_ui: &mut PerformanceOverlayUiState,
    runtime_performance_budget: &mut PerformanceBudget,
    renderer: &mut GpuTerminalRenderer,
    window: &winit::window::Window,
    dpi_scale_factor: f64,
) -> Result<bool, String> {
    if plan.live.is_empty() {
        return Ok(false);
    }

    if plan.live.contains(&ReloadableSection::Font) {
        let mut reloaded_fonts =
            FontSystem::new_with_scale_factor(font_config(&next.font), dpi_scale_factor);
        reloaded_fonts
            .cell_metrics()
            .map_err(|error| format!("font reload failed: {error}"))?;
        *fonts = reloaded_fonts;
    }

    for section in &plan.live {
        match section {
            ReloadableSection::Colors => {
                current.colors = next.colors.clone();
                renderer.set_background(render_color(current.colors.background));
            }
            ReloadableSection::Cursor => current.cursor = next.cursor.clone(),
            ReloadableSection::Diagnostics => {
                current.diagnostics = next.diagnostics.clone();
                performance_overlay_ui.apply_config(&current.diagnostics);
                performance_overlay.set_enabled(performance_overlay_ui.enabled);
            }
            ReloadableSection::Font => current.font = next.font.clone(),
            ReloadableSection::Input => {
                current.mouse = next.mouse.clone();
                current.clipboard = next.clipboard.clone();
                current.paste = next.paste.clone();
                *clipboard_config = current.clipboard.clone();
                *paste_config = current.paste.clone();
                *runtime_osc52_policy = osc52_policy(clipboard_config);
            }
            ReloadableSection::Keybindings => current.keyboard = next.keyboard.clone(),
            ReloadableSection::Mux => current.mux = next.mux.clone(),
            ReloadableSection::Notifications => {
                current.notifications = next.notifications.clone();
                notification_provider.set_enabled(current.notifications.enabled);
            }
            ReloadableSection::Performance => {
                renderer.set_glyph_cache_capacity(next.performance.glyph_cache_entries);
                current.performance = next.performance.clone();
                *runtime_performance_budget = performance_budget_from_config(current);
            }
            ReloadableSection::VisualSemantics => {
                current.visual_theme = next.visual_theme.clone();
                current.command_blocks = next.command_blocks.clone();
                current.prompt_decorations = next.prompt_decorations.clone();
                current.shell_integration = next.shell_integration.clone();
            }
            ReloadableSection::WindowChrome => {
                current.window.fullscreen_titlebar = next.window.fullscreen_titlebar.clone();
                renderer.request_full_redraw();
            }
            ReloadableSection::WindowPadding => {
                current.window.padding_x = next.window.padding_x;
                current.window.padding_y = next.window.padding_y;
                current.window.margin_x = next.window.margin_x;
                current.window.margin_y = next.window.margin_y;
                renderer.request_full_redraw();
            }
            ReloadableSection::WindowTitle => {
                current.window.title = next.window.title.clone();
                window.set_title(&current.window.title);
            }
        }
    }

    eprintln!("config reload applied live sections without restarting sessions");
    Ok(true)
}

fn font_config(config: &config_core::FontConfig) -> RuntimeFontConfig {
    RuntimeFontConfig {
        family: config.family.clone(),
        fallback_families: config.fallback_families.clone(),
        size: config.size as f32,
        line_height: config.line_height as f32,
        ligatures: config.ligatures,
    }
}

fn apply_power_policy(
    effective: &mut PerformanceConfig,
    configured: &PerformanceConfig,
    power: PowerState,
) {
    *effective = configured.clone();
    if !power.is_on_battery() || !configured.disable_expensive_effects_on_battery {
        return;
    }

    let mut battery = PerformanceConfig::default();
    battery.apply_profile(PerformanceProfile::BatterySaver);
    effective.frame_rate_limit = Some(
        effective
            .frame_rate_limit
            .unwrap_or(u16::MAX)
            .min(battery.frame_rate_limit.unwrap_or(30)),
    );
    effective.glyph_cache_entries = effective
        .glyph_cache_entries
        .min(battery.glyph_cache_entries);
    effective.max_animation_fps = effective.max_animation_fps.min(battery.max_animation_fps);
    effective.max_active_animations = effective
        .max_active_animations
        .min(battery.max_active_animations);
    effective.max_animated_region_pixels = effective
        .max_animated_region_pixels
        .min(battery.max_animated_region_pixels);
}

fn update_performance_overlay_context(
    overlay: &mut PerformanceOverlay,
    config: &AppConfig,
    power: PowerState,
) {
    if !overlay.is_enabled() {
        return;
    }
    overlay.set_runtime_context(
        performance_profile_label(config.performance.profile),
        power_source_label(power.source),
    );
}

const fn performance_profile_label(profile: PerformanceProfile) -> &'static str {
    match profile {
        PerformanceProfile::MaximumPerformance => "maximum_performance",
        PerformanceProfile::Balanced => "balanced",
        PerformanceProfile::Visual => "visual",
        PerformanceProfile::BatterySaver => "battery_saver",
    }
}

fn log_power_policy(config: &PerformanceConfig, diagnostic: &platform_core::PowerStateDiagnostic) {
    if let Some(message) = diagnostic.message.as_deref() {
        eprintln!("power diagnostics: {message}");
    }
    if diagnostic.state.is_on_battery() && config.disable_expensive_effects_on_battery {
        eprintln!(
            "performance power policy: battery caps active (charge={:?}%, fps={:?}, animations={}, pixels={})",
            diagnostic.state.charge_percent,
            config.frame_rate_limit,
            config.max_active_animations,
            config.max_animated_region_pixels
        );
    }
}

const fn power_source_label(source: PowerSource) -> &'static str {
    match source {
        PowerSource::Ac => "ac",
        PowerSource::Battery => "battery",
        PowerSource::Unknown => "unknown",
    }
}

fn renderer_options(config: &AppConfig) -> RendererOptions {
    RendererOptions {
        present_mode: match config.renderer.present_mode {
            PresentModePreference::Immediate => PresentMode::Immediate,
            PresentModePreference::Auto
            | PresentModePreference::Fifo
            | PresentModePreference::Mailbox => PresentMode::Vsync,
        },
        damage_tracking: config.renderer.damage_tracking,
        gpu_timestamps: config.renderer.gpu_timestamps,
        transparent: config.window.opacity < 1.0,
        glyph_cache_entries: config.performance.glyph_cache_entries,
        background: render_color(config.colors.background),
    }
}

fn cursor_animation_settings(config: &AppConfig) -> CursorAnimationSettings {
    let fps = config
        .performance
        .frame_rate_limit
        .map_or(config.performance.max_animation_fps, |limit| {
            limit.min(config.performance.max_animation_fps)
        });
    CursorAnimationSettings {
        enabled: config.cursor.animations_enabled,
        smooth_movement: config.cursor.smooth_movement,
        typing_pulse: config.cursor.typing_pulse,
        typing_stretch: config.cursor.typing_stretch,
        trail: config.cursor.trail,
        blink_easing: config.cursor.blink_easing,
        short_lived_glow: config.cursor.short_lived_glow,
        shadow: config.cursor.shadow,
        fps,
        max_active_animations: config.performance.max_active_animations,
        max_animated_region_pixels: config.performance.max_animated_region_pixels,
    }
}

fn request_cursor_image_if_enabled(
    cache: &mut AnimatedCursorImageCache,
    config: &AppConfig,
    config_base_dir: Option<&Path>,
) {
    if !config.cursor.image.enabled || config.performance.max_active_animations == 0 {
        cache.disable();
        return;
    }

    let path = resolve_cursor_image_path(&config.cursor.image.path, config_base_dir);
    cache.request(AnimatedCursorImageRequest {
        path,
        fps: config
            .cursor
            .image
            .fps
            .min(config.performance.max_animation_fps)
            .max(1),
        max_size_kb: config.performance.max_cursor_asset_size_kb,
        warn_if_expensive: config.cursor.image.warn_if_expensive,
    });
}

fn request_cursor_vector_if_enabled(
    cache: &mut CursorVectorCache,
    config: &AppConfig,
    config_base_dir: Option<&Path>,
) {
    if !config.cursor.vector.enabled {
        cache.disable();
        return;
    }
    cache.request(CursorVectorRequest {
        path: resolve_cursor_image_path(&config.cursor.vector.path, config_base_dir),
        max_size_kb: config.performance.max_cursor_asset_size_kb,
    });
}

fn resolve_cursor_image_path(configured: &str, config_base_dir: Option<&Path>) -> PathBuf {
    let configured_path = expand_home_path(Path::new(configured));
    if configured_path.is_relative() {
        config_base_dir.map_or(configured_path.clone(), |base| base.join(&configured_path))
    } else {
        configured_path
    }
}

fn expand_home_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(relative) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) else {
        return path.to_path_buf();
    };
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(
            || path.to_path_buf(),
            |home| PathBuf::from(home).join(relative),
        )
}

fn performance_budget_from_config(config: &AppConfig) -> PerformanceBudget {
    PerformanceBudget {
        max_frame_time: Duration::from_millis(u64::from(config.performance.max_frame_time_ms)),
        ..PerformanceBudget::default()
    }
}

fn spawn_session_transport(
    config: &AppConfig,
    spec: &SessionSpec,
    size: TransportSize,
    output_waker: &TransportWakeHandle,
) -> transport_core::TransportResult<InitialTransport> {
    match spec.transport {
        SessionTransportKind::LocalPty | SessionTransportKind::WindowsPseudoconsole => {
            if spec.profile_name != "default"
                && !config
                    .shell_profiles
                    .iter()
                    .any(|profile| profile.name == spec.profile_name)
            {
                return Err(transport_core::TransportError::new(format!(
                    "local shell profile '{}' does not exist",
                    spec.profile_name
                )));
            }
            let (mut profile, activation) =
                initial_local_shell_profile(config, Some(&spec.profile_name));
            if let Some(directory) = &spec.working_directory {
                profile.working_directory = Some(PathBuf::from(directory));
            }
            let mut transport = LocalPtyTransport::spawn(profile, size)?;
            transport.set_output_waker(Some(output_waker.clone()));
            Ok(InitialTransport {
                transport: PaneTransport::Local(Box::new(transport)),
                semantic_mode: semantic_mode_for_activation(&activation),
                parse_semantic_events: activation.parses_escape_sequences(),
                activation_diagnostics: activation.diagnostics,
                remote_metadata: None,
            })
        }
        SessionTransportKind::Ssh => {
            let profile = config
                .ssh_profiles
                .iter()
                .find(|profile| profile.name == spec.profile_name)
                .ok_or_else(|| {
                    transport_core::TransportError::new(format!(
                        "SSH profile '{}' does not exist",
                        spec.profile_name
                    ))
                })?;
            let semantic_mode = if !config.shell_integration.enabled || !profile.shell_integration {
                IntegrationMode::Disabled
            } else if matches!(
                config.shell_integration.activation,
                ShellIntegrationActivationConfig::Heuristic
            ) {
                IntegrationMode::Heuristic
            } else if matches!(
                config.shell_integration.activation,
                ShellIntegrationActivationConfig::Disabled
            ) {
                IntegrationMode::Disabled
            } else {
                IntegrationMode::EscapeSequences
            };
            let parse_semantic_events = semantic_mode == IntegrationMode::EscapeSequences;
            let mut connection = ssh_connection_profile(profile);
            if let Some(directory) = &spec.working_directory {
                connection.remote_working_directory = Some(directory.clone());
            }
            Ok(InitialTransport {
                transport: PaneTransport::connecting_ssh(connection, size, output_waker.clone()),
                semantic_mode,
                parse_semantic_events,
                activation_diagnostics: vec![match semantic_mode {
                    IntegrationMode::EscapeSequences => format!(
                        "SSH profile '{}' accepts remote semantic markers but remains inactive until a marker is observed; run `panea shell-integration remote-plan --shell <shell> --profile {}` for installation help",
                        profile.name, profile.name
                    ),
                    IntegrationMode::Heuristic => format!(
                        "SSH profile '{}' uses low-confidence input-boundary heuristics; exit status, prompt, and remote cwd metadata are unavailable",
                        profile.name
                    ),
                    IntegrationMode::Disabled => format!(
                        "SSH semantic integration disabled for profile '{}'",
                        profile.name
                    ),
                }],
                remote_metadata: Some(RemoteMetadata {
                    transport: Some("ssh".to_owned()),
                    remote_host: Some(profile.host.clone()),
                    remote_user: profile.username.clone(),
                    remote_current_working_directory: profile.remote_working_directory.clone(),
                }),
            })
        }
        SessionTransportKind::FutureMobileSsh => Err(transport_core::TransportError::new(
            "future mobile SSH transport cannot run in the desktop application",
        )),
    }
}

fn selected_shell_profile(config: &AppConfig) -> Option<&ShellProfile> {
    if let Some(default_shell_profile) = &config.default_shell_profile
        && let Some(profile) = config
            .shell_profiles
            .iter()
            .find(|profile| &profile.name == default_shell_profile)
    {
        return Some(profile);
    }

    config.shell_profiles.first()
}

struct InitialTransport {
    transport: PaneTransport,
    semantic_mode: IntegrationMode,
    parse_semantic_events: bool,
    activation_diagnostics: Vec<String>,
    remote_metadata: Option<RemoteMetadata>,
}

const MAX_PENDING_SSH_INPUT_BYTES: usize = 64 * 1024;

struct PendingSshTransport {
    result: Receiver<TransportResult<SshTransport>>,
    interactions: Receiver<SshInteractionRequest>,
    requested_size: TransportSize,
    pending_input: Vec<u8>,
    output_waker: TransportWakeHandle,
}

enum SshInteractionRequest {
    HostTrust {
        request: HostKeyTrustRequest,
        response: SyncSender<HostKeyTrustAction>,
    },
    Secret {
        request: SecretRequest,
        keychain: KeychainProviderCapability,
        response: SyncSender<Option<SecretPromptResponse>>,
    },
}

struct ChannelHostTrustProvider {
    requests: SyncSender<SshInteractionRequest>,
    output_waker: TransportWakeHandle,
}

impl HostTrustProvider for ChannelHostTrustProvider {
    fn decide_host_trust(
        &mut self,
        request: HostKeyTrustRequest,
    ) -> security::SecurityResult<HostKeyTrustAction> {
        let (response, result) = mpsc::sync_channel(1);
        self.requests
            .send(SshInteractionRequest::HostTrust { request, response })
            .map_err(|_| security::SecurityError::new("SSH trust prompt was cancelled"))?;
        self.output_waker.wake();
        result
            .recv()
            .map_err(|_| security::SecurityError::new("SSH trust prompt was cancelled"))
    }
}

struct ChannelSecretPromptProvider {
    requests: SyncSender<SshInteractionRequest>,
    keychain: KeychainProviderCapability,
    output_waker: TransportWakeHandle,
}

impl SecretPromptProvider for ChannelSecretPromptProvider {
    fn prompt_secret(
        &mut self,
        request: &SecretRequest,
    ) -> security::SecurityResult<Option<SecretPromptResponse>> {
        let (response, result) = mpsc::sync_channel(1);
        self.requests
            .send(SshInteractionRequest::Secret {
                request: request.clone(),
                keychain: self.keychain.clone(),
                response,
            })
            .map_err(|_| security::SecurityError::new("SSH credential prompt was cancelled"))?;
        self.output_waker.wake();
        result
            .recv()
            .map_err(|_| security::SecurityError::new("SSH credential prompt was cancelled"))
    }
}

enum PaneTransport {
    Local(Box<LocalPtyTransport>),
    ConnectingSsh(PendingSshTransport),
    Ssh(Box<SshTransport>),
    Failed { message: String, reported: bool },
}

impl PaneTransport {
    fn connecting_ssh(
        profile: SshConnectionProfile,
        size: TransportSize,
        output_waker: TransportWakeHandle,
    ) -> Self {
        let (sender, result) = mpsc::sync_channel(1);
        let (interaction_sender, interactions) = mpsc::sync_channel(1);
        let worker_waker = output_waker.clone();
        thread::spawn(move || {
            let mut trust_provider = ChannelHostTrustProvider {
                requests: interaction_sender.clone(),
                output_waker: worker_waker.clone(),
            };
            let keychain = PlatformKeychainProvider::for_current_platform();
            let prompt_provider = ChannelSecretPromptProvider {
                requests: interaction_sender,
                keychain: keychain.capability(),
                output_waker: worker_waker.clone(),
            };
            let mut secret_provider = KeychainBackedSecretProvider::new(keychain, prompt_provider);
            let transport = SshTransport::connect_with_providers(
                profile,
                size,
                &mut secret_provider,
                &mut trust_provider,
            );
            let _ = sender.send(transport);
            worker_waker.wake();
        });
        Self::ConnectingSsh(PendingSshTransport {
            result,
            interactions,
            requested_size: size,
            pending_input: Vec::new(),
            output_waker,
        })
    }

    fn take_interaction(&mut self) -> Option<SshInteractionRequest> {
        let Self::ConnectingSsh(pending) = self else {
            return None;
        };
        pending.interactions.try_recv().ok()
    }

    fn is_connected(&self) -> bool {
        matches!(self, Self::Local(_) | Self::Ssh(_))
    }

    fn promote_ssh(&mut self) -> TransportResult<()> {
        let Self::ConnectingSsh(pending) = self else {
            return Ok(());
        };
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => Err(transport_core::TransportError::new(
                "SSH connection worker stopped without a result",
            )),
        };
        match result {
            Ok(mut transport) => {
                transport.set_output_waker(Some(pending.output_waker.clone()));
                transport.resize(pending.requested_size)?;
                if !pending.pending_input.is_empty() {
                    transport.write_input(&pending.pending_input)?;
                }
                *self = Self::Ssh(Box::new(transport));
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                *self = Self::Failed {
                    message: message.clone(),
                    reported: false,
                };
                Err(transport_core::TransportError::new(message))
            }
        }
    }

    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<()> {
        self.promote_ssh()?;
        match self {
            Self::Local(transport) => transport.write_input(bytes),
            Self::Ssh(transport) => transport.write_input(bytes),
            Self::ConnectingSsh(pending) => {
                if pending.pending_input.len().saturating_add(bytes.len())
                    > MAX_PENDING_SSH_INPUT_BYTES
                {
                    return Err(transport_core::TransportError::new(
                        "SSH input queue is full while connection is pending",
                    ));
                }
                pending.pending_input.extend_from_slice(bytes);
                Ok(())
            }
            Self::Failed { message, .. } => {
                Err(transport_core::TransportError::new(message.clone()))
            }
        }
    }

    fn resize(&mut self, size: TransportSize) -> TransportResult<()> {
        self.promote_ssh()?;
        match self {
            Self::Local(transport) => transport.resize(size),
            Self::Ssh(transport) => transport.resize(size),
            Self::ConnectingSsh(pending) => {
                pending.requested_size = size;
                Ok(())
            }
            Self::Failed { message, .. } => {
                Err(transport_core::TransportError::new(message.clone()))
            }
        }
    }

    fn poll_output(&mut self) -> TransportResult<TransportOutput> {
        self.promote_ssh()?;
        match self {
            Self::Local(transport) => transport.poll_output(),
            Self::Ssh(transport) => transport.poll_output(),
            Self::ConnectingSsh(_) => Ok(TransportOutput::bytes(Vec::new())),
            Self::Failed { message, reported } => {
                if *reported {
                    Ok(TransportOutput::closed())
                } else {
                    *reported = true;
                    Err(transport_core::TransportError::new(message.clone()))
                }
            }
        }
    }

    fn shutdown(&mut self) -> TransportResult<()> {
        match self {
            Self::Local(transport) => transport.shutdown(),
            Self::Ssh(transport) => transport.shutdown(),
            Self::ConnectingSsh(_) | Self::Failed { .. } => Ok(()),
        }
    }

    fn requires_periodic_poll(&self) -> bool {
        matches!(self, Self::ConnectingSsh(_) | Self::Ssh(_))
    }
}

fn initial_local_shell_profile(
    config: &AppConfig,
    requested_profile: Option<&str>,
) -> (LocalShellProfile, ShellIntegrationActivationPlan) {
    let mut profile = requested_profile
        .and_then(|name| {
            config
                .shell_profiles
                .iter()
                .find(|profile| profile.name == name)
        })
        .or_else(|| selected_shell_profile(config))
        .map(local_shell_profile)
        .unwrap_or_else(LocalShellProfile::default_for_platform);
    let shell = shell_kind_for_local_profile(&profile);
    let policy = shell_integration_policy(config);
    let activation = shell_integration::activation_plan(&policy, &profile.name, shell);
    apply_shell_integration_activation(&mut profile, &activation);
    (profile, activation)
}

fn local_shell_profile(profile: &ShellProfile) -> LocalShellProfile {
    let profile = resolved_shell_profile(profile);
    let kind = match profile.kind {
        ShellProfileKind::Default => LocalShellKind::Default,
        ShellProfileKind::PowerShell => LocalShellKind::PowerShell,
        ShellProfileKind::Cmd => LocalShellKind::Cmd,
        ShellProfileKind::Wsl => LocalShellKind::Wsl,
        ShellProfileKind::Custom => LocalShellKind::Custom,
    };
    let program = if profile.program.trim().is_empty() {
        match profile.kind {
            ShellProfileKind::PowerShell => "powershell.exe",
            ShellProfileKind::Cmd => "cmd.exe",
            ShellProfileKind::Wsl => "wsl.exe",
            ShellProfileKind::Default | ShellProfileKind::Custom => {
                if cfg!(windows) {
                    "powershell.exe"
                } else {
                    "/bin/sh"
                }
            }
        }
        .to_owned()
    } else {
        profile.program.clone()
    };

    LocalShellProfile {
        name: profile.name.clone(),
        kind,
        program,
        args: profile.args.clone(),
        env: profile.env.clone(),
        working_directory: profile.working_directory.as_ref().map(PathBuf::from),
        startup_command: profile.startup_command.clone(),
    }
}

fn resolved_shell_profile(profile: &ShellProfile) -> ShellProfile {
    let mut resolved = profile.clone();
    let override_config = match ConfigPlatform::current() {
        ConfigPlatform::MacOs => profile.platform_overrides.macos.as_ref(),
        ConfigPlatform::Windows => profile.platform_overrides.windows.as_ref(),
        ConfigPlatform::Linux => profile.platform_overrides.linux.as_ref(),
        ConfigPlatform::LinuxX11 => profile
            .platform_overrides
            .linux_x11
            .as_ref()
            .or(profile.platform_overrides.linux.as_ref()),
        ConfigPlatform::LinuxWayland => profile
            .platform_overrides
            .linux_wayland
            .as_ref()
            .or(profile.platform_overrides.linux.as_ref()),
        ConfigPlatform::Unknown => None,
    };
    if let Some(override_config) = override_config {
        if let Some(program) = &override_config.program {
            resolved.program = program.clone();
        }
        if let Some(args) = &override_config.args {
            resolved.args = args.clone();
        }
        if let Some(env) = &override_config.env {
            resolved.env.extend(env.clone());
        }
        if let Some(working_directory) = &override_config.working_directory {
            resolved.working_directory = Some(working_directory.clone());
        }
        if let Some(startup_command) = &override_config.startup_command {
            resolved.startup_command = Some(startup_command.clone());
        }
    }
    resolved
}

fn shell_integration_policy(config: &AppConfig) -> ShellIntegrationPolicy {
    let activation = if !config.shell_integration.enabled {
        IntegrationActivation::Disabled
    } else {
        match config.shell_integration.activation {
            ShellIntegrationActivationConfig::Full => IntegrationActivation::Full,
            ShellIntegrationActivationConfig::AutoDetect => IntegrationActivation::AutoDetect,
            ShellIntegrationActivationConfig::Manual => IntegrationActivation::Manual,
            ShellIntegrationActivationConfig::Heuristic => IntegrationActivation::Heuristic,
            ShellIntegrationActivationConfig::Disabled => IntegrationActivation::Disabled,
        }
    };

    ShellIntegrationPolicy {
        enabled: config.shell_integration.enabled,
        activation,
        auto_install: config.shell_integration.auto_install,
        enabled_shells: config
            .shell_integration
            .enabled_shells
            .iter()
            .map(|shell| ShellKind::parse(shell))
            .filter(|shell| *shell != ShellKind::Unknown)
            .collect(),
        disabled_profiles: config.shell_integration.disabled_shell_profiles.clone(),
        remote_instructions: config.shell_integration.remote_instructions,
    }
}

fn shell_kind_for_local_profile(profile: &LocalShellProfile) -> ShellKind {
    match profile.kind {
        LocalShellKind::PowerShell => ShellKind::PowerShell,
        LocalShellKind::Cmd => ShellKind::Cmd,
        LocalShellKind::Wsl => ShellKind::Bash,
        LocalShellKind::Default | LocalShellKind::Custom => {
            let detected = detect_shell_kind(&profile.program);
            if detected == ShellKind::Unknown && cfg!(windows) {
                ShellKind::PowerShell
            } else {
                detected
            }
        }
    }
}

fn semantic_mode_for_activation(activation: &ShellIntegrationActivationPlan) -> IntegrationMode {
    match activation.mode {
        ShellIntegrationRuntimeMode::Full | ShellIntegrationRuntimeMode::Auto => {
            IntegrationMode::EscapeSequences
        }
        ShellIntegrationRuntimeMode::Heuristic => IntegrationMode::Heuristic,
        ShellIntegrationRuntimeMode::Off => IntegrationMode::Disabled,
    }
}

fn apply_shell_integration_activation(
    profile: &mut LocalShellProfile,
    activation: &ShellIntegrationActivationPlan,
) {
    profile.env.extend(activation.environment.clone());

    if activation.action != ShellIntegrationActivationAction::InjectRuntimeScript {
        return;
    }

    if !profile.args.is_empty() {
        eprintln!(
            "shell integration fallback: profile '{}' has explicit args, runtime hook injection skipped",
            profile.name
        );
        return;
    }

    let Some(script) = activation.script.as_ref() else {
        return;
    };
    let existing_startup = profile.startup_command.take();
    let hook = combine_shell_startup(script.contents, existing_startup.as_deref());

    match activation.shell {
        ShellKind::Bash => {
            if let Ok(path) = write_runtime_shell_hook(&profile.name, "bashrc", &hook) {
                profile.args = vec![
                    "--init-file".to_owned(),
                    path.display().to_string(),
                    "-i".to_owned(),
                ];
            } else {
                profile.startup_command = existing_startup;
            }
        }
        ShellKind::Zsh => {
            if let Ok(path) = write_runtime_shell_hook(&profile.name, "zshrc", &hook)
                && let Some(directory) = path.parent()
            {
                profile.env.insert(
                    "PANEA_ORIGINAL_ZDOTDIR".to_owned(),
                    std::env::var("ZDOTDIR").unwrap_or_else(|_| {
                        std::env::var("HOME").unwrap_or_else(|_| "~".to_owned())
                    }),
                );
                profile
                    .env
                    .insert("ZDOTDIR".to_owned(), directory.display().to_string());
                profile.args = vec!["-i".to_owned()];
            } else {
                profile.startup_command = existing_startup;
            }
        }
        ShellKind::Fish => {
            profile.args = vec!["-C".to_owned(), hook];
        }
        ShellKind::PowerShell | ShellKind::Pwsh => {
            profile.args = vec![
                "-NoLogo".to_owned(),
                "-NoExit".to_owned(),
                "-Command".to_owned(),
                hook,
            ];
            profile.startup_command = None;
        }
        ShellKind::Cmd | ShellKind::Nushell | ShellKind::Unknown => {
            profile.startup_command = existing_startup;
        }
    }
}

fn combine_shell_startup(script: &str, existing_startup: Option<&str>) -> String {
    let mut combined = String::from(script);
    if let Some(existing_startup) = existing_startup
        && !existing_startup.trim().is_empty()
    {
        combined.push('\n');
        combined.push_str(existing_startup);
        combined.push('\n');
    }
    combined
}

fn write_runtime_shell_hook(
    profile_name: &str,
    file_name: &str,
    contents: &str,
) -> std::io::Result<PathBuf> {
    let directory = std::env::temp_dir()
        .join("panea-shell-integration")
        .join(std::process::id().to_string())
        .join(sanitize_file_component(profile_name));
    fs::create_dir_all(&directory)?;

    let path = match file_name {
        "zshrc" => directory.join(".zshrc"),
        "bashrc" => directory.join("panea.bashrc"),
        _ => directory.join(file_name),
    };
    let wrapped = wrap_runtime_shell_hook(file_name, contents);
    fs::write(&path, wrapped)?;
    Ok(path)
}

fn wrap_runtime_shell_hook(file_name: &str, contents: &str) -> String {
    match file_name {
        "bashrc" => {
            format!("if [ -r \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi\n{contents}\n")
        }
        "zshrc" => format!(
            "if [ -n \"$PANEA_ORIGINAL_ZDOTDIR\" ] && [ -r \"$PANEA_ORIGINAL_ZDOTDIR/.zshrc\" ]; then . \"$PANEA_ORIGINAL_ZDOTDIR/.zshrc\"; fi\n{contents}\n"
        ),
        _ => contents.to_owned(),
    }
}

fn sanitize_file_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn ssh_connection_profile(profile: &SshProfile) -> SshConnectionProfile {
    let mut connection = SshConnectionProfile::new(profile.name.clone(), profile.host.clone());
    connection.port = profile.port;
    connection.username = profile.username.clone();
    connection.auth_method = match profile.auth_method {
        SshAuthMethod::Agent => security::AuthMethod::Agent,
        SshAuthMethod::PublicKey => security::AuthMethod::PublicKey,
        SshAuthMethod::Password => security::AuthMethod::Password,
        SshAuthMethod::KeyboardInteractive => security::AuthMethod::KeyboardInteractive,
        SshAuthMethod::None => security::AuthMethod::None,
    };
    connection.identity_file = profile.identity_file.as_ref().map(PathBuf::from);
    connection.known_hosts_policy = match &profile.known_hosts_policy {
        SshKnownHostsPolicy::Ask => security::KnownHostsPolicy::Ask,
        SshKnownHostsPolicy::RequireKnown => security::KnownHostsPolicy::RequireKnown,
        SshKnownHostsPolicy::TrustOnFirstUse => security::KnownHostsPolicy::TrustOnFirstUse,
        SshKnownHostsPolicy::PinFingerprint { sha256 } => {
            security::KnownHostsPolicy::PinFingerprint {
                sha256: sha256.clone(),
            }
        }
    };
    connection.remote_command = profile.remote_command.clone();
    connection.remote_working_directory = profile.remote_working_directory.clone();
    connection.shell_integration = profile.shell_integration;
    connection.agent_forwarding = profile.agent_forwarding;
    connection.proxy_jump = profile.proxy_jump.clone();
    connection
}

fn cols_for_width(width: u32, metrics: CellMetrics) -> u16 {
    ((width as f32 / metrics.cell_width).floor() as u16).max(1)
}

fn horizontal_content_inset(config: &AppConfig) -> u32 {
    u32::from(config.window.padding_x).saturating_add(u32::from(config.window.margin_x))
}

fn vertical_content_inset(config: &AppConfig) -> u32 {
    u32::from(config.window.padding_y).saturating_add(u32::from(config.window.margin_y))
}

fn content_extent(extent: u32, inset: u32) -> u32 {
    extent.saturating_sub(inset.saturating_mul(2)).max(1)
}

fn rows_for_height(height: u32, metrics: CellMetrics) -> u16 {
    ((height as f32 / metrics.cell_height).floor() as u16).max(1)
}

fn terminal_key(event: &KeyEvent) -> Option<TerminalKey> {
    if event.state != KeyState::Pressed {
        return None;
    }

    if let Some(keypad) = event.physical_key.as_deref().and_then(keypad_key) {
        return Some(TerminalKey::Keypad(keypad));
    }

    let key = match event.logical_key.as_str() {
        "Enter" => TerminalKey::Enter,
        "Backspace" => TerminalKey::Backspace,
        "Tab" => TerminalKey::Tab,
        "Escape" => TerminalKey::Escape,
        "ArrowUp" => TerminalKey::Up,
        "ArrowDown" => TerminalKey::Down,
        "ArrowLeft" => TerminalKey::Left,
        "ArrowRight" => TerminalKey::Right,
        "Home" => TerminalKey::Home,
        "End" => TerminalKey::End,
        "Insert" => TerminalKey::Insert,
        "Delete" => TerminalKey::Delete,
        "PageUp" => TerminalKey::PageUp,
        "PageDown" => TerminalKey::PageDown,
        logical if logical.len() > 1 && logical.starts_with('F') => {
            TerminalKey::Function(logical[1..].parse().ok()?)
        }
        _ => TerminalKey::Character(terminal_character_text(event)?),
    };
    Some(key)
}

fn terminal_character_text(event: &KeyEvent) -> Option<String> {
    if event.modifiers.ctrl && !event.modifiers.alt_graph {
        let logical = event.logical_key.as_str();
        if logical.chars().count() == 1 && logical.chars().next().is_some_and(|ch| !ch.is_control())
        {
            return Some(logical.to_owned());
        }
        return None;
    }

    event
        .text
        .as_ref()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
        .cloned()
}

fn keypad_key(physical_key: &str) -> Option<KeypadKey> {
    let name = physical_key
        .strip_prefix("Code(")
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(physical_key);
    match name {
        "Numpad0" => Some(KeypadKey::Digit(0)),
        "Numpad1" => Some(KeypadKey::Digit(1)),
        "Numpad2" => Some(KeypadKey::Digit(2)),
        "Numpad3" => Some(KeypadKey::Digit(3)),
        "Numpad4" => Some(KeypadKey::Digit(4)),
        "Numpad5" => Some(KeypadKey::Digit(5)),
        "Numpad6" => Some(KeypadKey::Digit(6)),
        "Numpad7" => Some(KeypadKey::Digit(7)),
        "Numpad8" => Some(KeypadKey::Digit(8)),
        "Numpad9" => Some(KeypadKey::Digit(9)),
        "NumpadDecimal" => Some(KeypadKey::Decimal),
        "NumpadDivide" => Some(KeypadKey::Divide),
        "NumpadMultiply" => Some(KeypadKey::Multiply),
        "NumpadSubtract" => Some(KeypadKey::Subtract),
        "NumpadAdd" => Some(KeypadKey::Add),
        "NumpadEnter" => Some(KeypadKey::Enter),
        "NumpadEqual" => Some(KeypadKey::Equal),
        _ => None,
    }
}

fn terminal_modifiers(modifiers: KeyModifiers) -> TerminalKeyModifiers {
    TerminalKeyModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        ctrl: modifiers.ctrl,
        super_key: modifiers.super_key,
        alt_graph: modifiers.alt_graph,
    }
}

fn keybinding_action(event: &KeyEvent, config: &AppConfig) -> Option<String> {
    if event.state != KeyState::Pressed {
        return None;
    }
    let event_key = canonical_key_event(event);
    config
        .keyboard
        .keybindings
        .iter()
        .find(|binding| canonical_key_spec(&binding.keys) == event_key)
        .map(|binding| binding.action.clone())
}

fn mousebinding_action(event: &MouseEvent, config: &config_core::MouseConfig) -> Option<String> {
    let event_gesture = canonical_mouse_event(event)?;
    config
        .bindings
        .iter()
        .find(|binding| canonical_mouse_spec(&binding.gesture).as_deref() == Some(&event_gesture))
        .map(|binding| binding.action.trim().to_ascii_lowercase())
}

fn canonical_mouse_event(event: &MouseEvent) -> Option<String> {
    let name = match event.kind {
        MouseEventKind::Pressed(button) => format!("{}press", mouse_button_name(button)?),
        MouseEventKind::Released(button) => format!("{}release", mouse_button_name(button)?),
        MouseEventKind::Wheel {
            delta_x: _,
            delta_y,
        } if delta_y > 0.0 => "wheelup".to_owned(),
        MouseEventKind::Wheel {
            delta_x: _,
            delta_y,
        } if delta_y < 0.0 => "wheeldown".to_owned(),
        MouseEventKind::Wheel {
            delta_x,
            delta_y: _,
        } if delta_x > 0.0 => "wheelright".to_owned(),
        MouseEventKind::Wheel {
            delta_x,
            delta_y: _,
        } if delta_x < 0.0 => "wheelleft".to_owned(),
        MouseEventKind::Moved | MouseEventKind::Wheel { .. } => return None,
    };
    Some(canonical_mouse_parts(event.modifiers, &name))
}

fn canonical_mouse_spec(spec: &str) -> Option<String> {
    let mut modifiers = KeyModifiers::default();
    let mut event = None;
    for part in spec.split('+') {
        let normalized = part.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "alt" | "option" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "super" | "cmd" | "command" | "meta" => modifiers.super_key = true,
            _ if event.is_none() => event = Some(normalized),
            _ => return None,
        }
    }
    event.map(|event| canonical_mouse_parts(modifiers, &event))
}

fn canonical_mouse_parts(modifiers: KeyModifiers, event: &str) -> String {
    let mut parts = Vec::new();
    if modifiers.ctrl {
        parts.push("ctrl");
    }
    if modifiers.alt {
        parts.push("alt");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    if modifiers.super_key {
        parts.push("super");
    }
    parts.push(event);
    parts.join("+")
}

fn mouse_button_name(button: MouseButton) -> Option<&'static str> {
    match button {
        MouseButton::Left => Some("left"),
        MouseButton::Middle => Some("middle"),
        MouseButton::Right => Some("right"),
        MouseButton::Back => Some("back"),
        MouseButton::Forward => Some("forward"),
        MouseButton::Other(_) => None,
    }
}

fn canonical_key_event(event: &KeyEvent) -> String {
    let mut parts = Vec::new();
    if event.modifiers.ctrl {
        parts.push("ctrl".to_owned());
    }
    if event.modifiers.alt {
        parts.push("alt".to_owned());
    }
    if event.modifiers.shift {
        parts.push("shift".to_owned());
    }
    if event.modifiers.super_key {
        parts.push("super".to_owned());
    }
    parts.push(canonical_key_name(&event.logical_key));
    parts.join("+")
}

fn canonical_key_spec(spec: &str) -> String {
    let mut modifiers = BTreeSet::new();
    let mut key = String::new();
    for part in spec.split('+') {
        match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => {
                modifiers.insert("ctrl");
            }
            "alt" | "option" => {
                modifiers.insert("alt");
            }
            "shift" => {
                modifiers.insert("shift");
            }
            "super" | "cmd" | "command" | "meta" => {
                modifiers.insert("super");
            }
            other if !other.is_empty() => key = canonical_key_name(other),
            _ => {}
        }
    }

    let mut parts = modifiers.into_iter().collect::<Vec<_>>();
    parts.push(key.as_str());
    parts.join("+")
}

fn canonical_key_name(key: &str) -> String {
    match key.trim().to_ascii_lowercase().as_str() {
        "pagedown" | "page_down" | "page down" => "pagedown".to_owned(),
        "pageup" | "page_up" | "page up" => "pageup".to_owned(),
        "arrowleft" | "left" => "left".to_owned(),
        "arrowright" | "right" => "right".to_owned(),
        "arrowup" | "up" => "up".to_owned(),
        "arrowdown" | "down" => "down".to_owned(),
        " " | "space" => "space".to_owned(),
        other => other.to_owned(),
    }
}

fn paste_bytes(
    text: &str,
    clipboard: &ClipboardConfig,
    legacy_paste: &PasteConfig,
    bracketed_mode: bool,
) -> Vec<u8> {
    let mut text = if clipboard.paste_protection && legacy_paste.normalize_newlines {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_owned()
    };

    if clipboard.paste_protection && legacy_paste.strip_control_characters {
        text.retain(|ch| ch == '\n' || ch == '\t' || !ch.is_control());
    }

    let mut bytes = Vec::new();
    if bracketed_mode && clipboard.bracketed_paste {
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
    } else {
        bytes.extend_from_slice(text.as_bytes());
    }

    bytes
}

fn should_middle_click_paste(
    mouse: &MouseEvent,
    modes: &BTreeSet<TerminalMode>,
    config: &ClipboardConfig,
) -> bool {
    config.enabled
        && config.middle_click_paste
        && !mouse_reporting_enabled(modes)
        && matches!(mouse.kind, MouseEventKind::Pressed(MouseButton::Middle))
}

fn paste_for_middle_click(
    clipboard: &mut ClipboardBridge,
    config: &ClipboardConfig,
) -> Result<String, platform_core::ClipboardDiagnostic> {
    if cfg!(target_os = "linux") && config.prefer_primary_selection_on_linux {
        match clipboard.paste_primary_text() {
            Ok(text) => return Ok(text),
            Err(diagnostic) => {
                eprintln!(
                    "Linux primary selection paste unavailable; falling back to system clipboard: {diagnostic:?}"
                );
            }
        }
    }
    clipboard.paste_text()
}

fn mouse_reporting_enabled(modes: &BTreeSet<TerminalMode>) -> bool {
    modes.contains(&TerminalMode::MouseReporting)
        || modes.contains(&TerminalMode::MouseCellMotion)
        || modes.contains(&TerminalMode::MouseAllMotion)
}

fn focus_report_bytes(focused: bool, modes: &BTreeSet<TerminalMode>) -> Option<&'static [u8]> {
    if !modes.contains(&TerminalMode::FocusEvents) {
        return None;
    }
    Some(if focused { b"\x1b[I" } else { b"\x1b[O" })
}

fn copy_text_with_diagnostics(
    clipboard: &mut ClipboardBridge,
    text: &str,
    config: &ClipboardConfig,
    source: &str,
) {
    match clipboard.copy_text(text) {
        Ok(()) if config.log_operations => {
            eprintln!(
                "clipboard {source}: wrote {} bytes to system clipboard",
                text.len()
            );
        }
        Ok(()) => {}
        Err(diagnostic) => eprintln!("clipboard {source} failed: {diagnostic:?}"),
    }
}

fn process_pending_clipboard_requests(
    terminal: &mut TerminalEmulator,
    clipboard: &mut ClipboardBridge,
    policy: &Osc52ClipboardPolicy,
    config: &ClipboardConfig,
    session_is_remote: bool,
    pending_prompt: &mut Option<Osc52PromptState>,
) {
    if !config.enabled {
        let dropped = terminal.state_mut().take_pending_clipboard_requests();
        if config.log_operations && !dropped.is_empty() {
            eprintln!(
                "clipboard osc52: dropped {} request(s) because clipboard is disabled",
                dropped.len()
            );
        }
        return;
    }

    for request in terminal.state_mut().take_pending_clipboard_requests() {
        let security_request = security_osc52_request(request, session_is_remote);
        match evaluate_osc52_clipboard_write(&security_request, policy) {
            Osc52ClipboardDecision::Allow { text, bytes } => {
                copy_osc52_text_with_diagnostics(
                    clipboard,
                    &text,
                    config,
                    security_request.target,
                    "OSC 52",
                );
                if config.log_operations {
                    eprintln!("clipboard OSC 52: accepted {bytes} byte request");
                }
            }
            Osc52ClipboardDecision::PromptRequired { reason, bytes } => {
                if pending_prompt.is_none() {
                    *pending_prompt = Some(Osc52PromptState {
                        request: security_request,
                        reason,
                        bytes,
                    });
                    eprintln!(
                        "clipboard OSC 52: remote write is waiting for explicit confirmation"
                    );
                } else {
                    eprintln!(
                        "clipboard OSC 52 denied: another remote clipboard decision is already pending"
                    );
                }
            }
            Osc52ClipboardDecision::Deny { reason } => {
                if config.log_operations {
                    eprintln!("clipboard OSC 52 denied: {reason}");
                }
            }
        }
    }
}

fn copy_osc52_text_with_diagnostics(
    clipboard: &mut ClipboardBridge,
    text: &str,
    config: &ClipboardConfig,
    target: Osc52ClipboardTarget,
    source: &str,
) {
    if matches!(target, Osc52ClipboardTarget::PrimarySelection) {
        match clipboard.copy_primary_text(text) {
            Ok(()) => {
                if config.log_operations {
                    eprintln!(
                        "clipboard {source}: wrote {} bytes to primary selection",
                        text.len()
                    );
                }
                return;
            }
            Err(diagnostic) => eprintln!(
                "clipboard {source}: primary selection unavailable; falling back to system clipboard: {diagnostic:?}"
            ),
        }
    }
    copy_text_with_diagnostics(clipboard, text, config, source);
}

fn security_osc52_request(request: Osc52ClipboardRequest, remote: bool) -> SecurityOsc52Request {
    SecurityOsc52Request {
        target: security_clipboard_target(request.target),
        payload_base64: request.payload_base64,
        remote,
    }
}

fn security_clipboard_target(target: ClipboardTarget) -> Osc52ClipboardTarget {
    match target {
        ClipboardTarget::Clipboard => Osc52ClipboardTarget::Clipboard,
        ClipboardTarget::PrimarySelection => Osc52ClipboardTarget::PrimarySelection,
        ClipboardTarget::Select => Osc52ClipboardTarget::Select,
        ClipboardTarget::Unknown(ch) => Osc52ClipboardTarget::Unknown(ch),
    }
}

fn osc52_policy(config: &ClipboardConfig) -> Osc52ClipboardPolicy {
    Osc52ClipboardPolicy {
        enabled: config.osc52.enabled,
        allow_local: config.osc52.allow_local,
        allow_remote: config.osc52.allow_remote,
        max_bytes: config.osc52.max_bytes,
        confirm_remote_writes: config.osc52.confirm_remote_writes,
    }
}

fn shutdown_transport(transport: Option<&mut PaneTransport>) {
    if let Some(transport) = transport {
        match catch_unwind(AssertUnwindSafe(|| transport.shutdown())) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("transport shutdown error: {error}"),
            Err(panic) => eprintln!(
                "transport shutdown panic boundary: {}",
                panic_payload(panic)
            ),
        }
    }
}

trait TerminalInputSink {
    fn write_terminal_bytes(&mut self, bytes: &[u8]) -> TransportResult<()>;
}

impl TerminalInputSink for PaneTransport {
    fn write_terminal_bytes(&mut self, bytes: &[u8]) -> TransportResult<()> {
        self.write_input(bytes)
    }
}

#[cfg(test)]
impl TerminalInputSink for LocalPtyTransport {
    fn write_terminal_bytes(&mut self, bytes: &[u8]) -> TransportResult<()> {
        self.write_input(bytes)
    }
}

fn flush_terminal_responses<T>(terminal: &mut TerminalEmulator, transport: &mut T)
where
    T: TerminalInputSink + ?Sized,
{
    let responses = terminal.state_mut().take_pending_output();
    if !responses.is_empty() {
        write_transport_input(transport, &responses);
    }
}

fn write_terminal_input<T>(terminal: &mut TerminalEmulator, transport: &mut T, bytes: &[u8])
where
    T: TerminalInputSink + ?Sized,
{
    flush_terminal_responses(terminal, transport);
    write_transport_input(transport, bytes);
}

fn write_transport_input<T>(transport: &mut T, bytes: &[u8])
where
    T: TerminalInputSink + ?Sized,
{
    match catch_unwind(AssertUnwindSafe(|| transport.write_terminal_bytes(bytes))) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("transport input error: {error}"),
        Err(panic) => eprintln!("transport input panic boundary: {}", panic_payload(panic)),
    }
}

fn panic_payload(panic: Box<dyn Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn window_settings(config: &AppConfig) -> WindowSettings {
    WindowSettings {
        title: config.window.title.clone(),
        initial_width: config.window.initial_width,
        initial_height: config.window.initial_height,
        visible_on_create: false,
        mode: map_window_mode(config.window.mode),
        linux_backend: map_linux_backend(config.window.linux_backend),
        decoration_mode: map_decoration_mode(config.window.decoration_strategy),
        opacity: config.window.opacity,
        icon: panea_window_icon(),
    }
}

fn panea_window_icon() -> Option<platform_winit::WindowIcon> {
    static ICON: std::sync::OnceLock<Option<platform_winit::WindowIcon>> =
        std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let bitmap = panea_brand_bitmap()?;
        match platform_winit::WindowIcon::from_rgba(
            bitmap.pixels.as_ref().to_vec(),
            bitmap.width,
            bitmap.height,
        ) {
            Ok(icon) => Some(icon),
            Err(error) => {
                eprintln!("window icon fallback: invalid Panea icon: {error}");
                None
            }
        }
    })
    .clone()
}

#[derive(Debug)]
struct PaneaBrandBitmap {
    pixels: Arc<[u8]>,
    width: u32,
    height: u32,
}

fn panea_brand_bitmap() -> Option<&'static PaneaBrandBitmap> {
    const PANEA_ICON_PNG: &[u8] =
        include_bytes!("../../../crates/assets/branding/generated/panea-icon-128.png");
    static BITMAP: std::sync::OnceLock<Option<PaneaBrandBitmap>> = std::sync::OnceLock::new();
    BITMAP
        .get_or_init(|| match image::load_from_memory(PANEA_ICON_PNG) {
            Ok(decoded) => {
                let decoded = decoded.into_rgba8();
                let (width, height) = decoded.dimensions();
                Some(PaneaBrandBitmap {
                    pixels: Arc::from(decoded.into_raw()),
                    width,
                    height,
                })
            }
            Err(error) => {
                eprintln!("window icon fallback: failed to decode Panea icon: {error}");
                None
            }
        })
        .as_ref()
}

fn apply_window_mode_logged(
    window: &winit::window::Window,
    requested: WindowMode,
    decoration: DecorationMode,
) -> WindowMode {
    let diagnostic = apply_window_mode_with_decoration(window, requested, decoration);
    log_window_mode_diagnostic(&diagnostic);
    diagnostic.effective
}

fn log_window_mode_diagnostic(diagnostic: &platform_core::WindowModeDiagnostic) {
    if let Some(fallback) = diagnostic.fallback.as_ref() {
        eprintln!(
            "platform fallback [{}]: requested={} effective={} reason={}",
            fallback.feature, fallback.requested, fallback.effective, fallback.reason
        );
    }
}

fn map_window_mode(mode: WindowModeConfig) -> WindowMode {
    match mode {
        WindowModeConfig::Windowed => WindowMode::Windowed,
        WindowModeConfig::Maximized => WindowMode::Maximized,
        WindowModeConfig::Fullscreen => WindowMode::Fullscreen,
        WindowModeConfig::BorderlessFullscreen => WindowMode::BorderlessFullscreen,
        WindowModeConfig::FramelessWindowed => WindowMode::FramelessWindowed,
        WindowModeConfig::FramelessFullscreen => WindowMode::FramelessFullscreen,
    }
}

fn map_linux_backend(backend: LinuxBackendConfig) -> LinuxWindowBackend {
    match backend {
        LinuxBackendConfig::Auto => LinuxWindowBackend::Auto,
        LinuxBackendConfig::X11 => LinuxWindowBackend::X11,
        LinuxBackendConfig::Wayland => LinuxWindowBackend::Wayland,
    }
}

fn map_decoration_mode(mode: DecorationStrategyConfig) -> DecorationMode {
    match mode {
        DecorationStrategyConfig::Auto => DecorationMode::Auto,
        DecorationStrategyConfig::Native => DecorationMode::Native,
        DecorationStrategyConfig::ClientSide => DecorationMode::ClientSide,
        DecorationStrategyConfig::Custom => DecorationMode::Custom,
        DecorationStrategyConfig::None => DecorationMode::None,
        DecorationStrategyConfig::FallbackDecorated => DecorationMode::FallbackDecorated,
    }
}

struct DesktopDiagnosticsPlaceholder {
    _window: platform_winit::DesktopWindowDiagnostics,
    _capabilities: platform_core::PlatformCapabilities,
}

impl DesktopDiagnosticsPlaceholder {
    fn new(
        window: platform_winit::DesktopWindowDiagnostics,
        capabilities: platform_core::PlatformCapabilities,
    ) -> Self {
        Self {
            _window: window,
            _capabilities: capabilities,
        }
    }
}

fn mux_state_path() -> PathBuf {
    if cfg!(target_os = "windows") {
        return std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Panea")
            .join("mux-state.json");
    }
    if cfg!(target_os = "macos") {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Library")
            .join("Application Support")
            .join("Panea")
            .join("mux-state.json");
    }
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        })
        .unwrap_or_else(std::env::temp_dir)
        .join("panea")
        .join("mux-state.json")
}

fn desktop_ui_state_path() -> PathBuf {
    mux_state_path()
        .parent()
        .map_or_else(std::env::temp_dir, Path::to_path_buf)
        .join("ui-state.json")
}

#[derive(Debug, Clone)]
struct PerformanceOverlayUiState {
    enabled: bool,
    position: PerformanceOverlayPosition,
    detail: PerformanceOverlayDetail,
    menu_open: bool,
    persist: bool,
    loaded_from_state: bool,
    state_path: PathBuf,
}

impl PerformanceOverlayUiState {
    fn new(config: &config_core::DiagnosticsConfig) -> Self {
        let state_path = desktop_ui_state_path();
        let mut state = Self {
            enabled: config.performance_overlay,
            position: config.performance_overlay_position,
            detail: config.performance_overlay_detail,
            menu_open: false,
            persist: config.persist_performance_overlay,
            loaded_from_state: false,
            state_path,
        };
        if state.persist {
            state.load();
        }
        state
    }

    fn load(&mut self) {
        let Ok(contents) = fs::read_to_string(&self.state_path) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
            eprintln!(
                "performance overlay preference ignored: {} is invalid JSON",
                self.state_path.display()
            );
            return;
        };
        if let Some(enabled) = value.get("enabled").and_then(serde_json::Value::as_bool) {
            self.enabled = enabled;
        }
        if let Some(position) = value.get("position").and_then(serde_json::Value::as_str) {
            self.position = match position {
                "top_left" => PerformanceOverlayPosition::TopLeft,
                "bottom_left" => PerformanceOverlayPosition::BottomLeft,
                "bottom_right" => PerformanceOverlayPosition::BottomRight,
                _ => PerformanceOverlayPosition::TopRight,
            };
        }
        if let Some(detail) = value.get("detail").and_then(serde_json::Value::as_str) {
            self.detail = if detail == "detailed" {
                PerformanceOverlayDetail::Detailed
            } else {
                PerformanceOverlayDetail::Compact
            };
        }
        self.loaded_from_state = true;
    }

    fn apply_config(&mut self, config: &config_core::DiagnosticsConfig) {
        self.persist = config.persist_performance_overlay;
        if !self.persist || !self.loaded_from_state {
            self.enabled = config.performance_overlay;
            self.position = config.performance_overlay_position;
            self.detail = config.performance_overlay_detail;
        }
        if !self.enabled {
            self.menu_open = false;
        }
    }

    fn toggle(&mut self) {
        self.enabled = !self.enabled;
        self.menu_open = false;
        self.persist();
    }

    fn cycle_detail(&mut self) {
        self.detail = match self.detail {
            PerformanceOverlayDetail::Compact => PerformanceOverlayDetail::Detailed,
            PerformanceOverlayDetail::Detailed => PerformanceOverlayDetail::Compact,
        };
        self.persist();
    }

    fn cycle_position(&mut self) {
        self.position = match self.position {
            PerformanceOverlayPosition::TopLeft => PerformanceOverlayPosition::TopRight,
            PerformanceOverlayPosition::TopRight => PerformanceOverlayPosition::BottomRight,
            PerformanceOverlayPosition::BottomRight => PerformanceOverlayPosition::BottomLeft,
            PerformanceOverlayPosition::BottomLeft => PerformanceOverlayPosition::TopLeft,
        };
        self.persist();
    }

    fn hide(&mut self) {
        self.enabled = false;
        self.menu_open = false;
        self.persist();
    }

    fn persist(&mut self) {
        if !self.persist {
            return;
        }
        let Some(parent) = self.state_path.parent() else {
            return;
        };
        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!("performance overlay preference directory failed: {error}");
            return;
        }
        let value = serde_json::json!({
            "enabled": self.enabled,
            "position": performance_overlay_position_name(self.position),
            "detail": performance_overlay_detail_name(self.detail),
        });
        if let Err(error) = serde_json::to_string_pretty(&value)
            .map_err(|error| error.to_string())
            .and_then(|json| fs::write(&self.state_path, json).map_err(|error| error.to_string()))
        {
            eprintln!("performance overlay preference save failed: {error}");
            return;
        }
        self.loaded_from_state = true;
    }

    fn diagnostic(&self) -> String {
        format!(
            "enabled={} position={} detail={} persistence={} source={}",
            self.enabled,
            performance_overlay_position_name(self.position),
            performance_overlay_detail_name(self.detail),
            self.persist,
            if self.loaded_from_state {
                "runtime preference"
            } else {
                "config"
            }
        )
    }
}

const fn performance_overlay_position_name(position: PerformanceOverlayPosition) -> &'static str {
    match position {
        PerformanceOverlayPosition::TopLeft => "top_left",
        PerformanceOverlayPosition::TopRight => "top_right",
        PerformanceOverlayPosition::BottomLeft => "bottom_left",
        PerformanceOverlayPosition::BottomRight => "bottom_right",
    }
}

const fn performance_overlay_detail_name(detail: PerformanceOverlayDetail) -> &'static str {
    match detail {
        PerformanceOverlayDetail::Compact => "compact",
        PerformanceOverlayDetail::Detailed => "detailed",
    }
}

fn initial_mux_model(config: &AppConfig, state_path: &PathBuf) -> MuxModel {
    let fallback = session_spec_for_config(config);
    if config.mux.restore_sessions {
        match fs::read_to_string(state_path) {
            Ok(contents) => match serde_json::from_str::<RestoreSnapshot>(&contents)
                .map_err(|error| error.to_string())
                .and_then(|snapshot| {
                    MuxModel::from_restore_snapshot(&snapshot, fallback.clone())
                        .map_err(|error| error.to_string())
                }) {
                Ok(model) => return model,
                Err(error) => eprintln!(
                    "mux restore fallback: {} could not be restored: {error}",
                    state_path.display()
                ),
            },
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => eprintln!(
                "mux restore fallback: {} could not be read: {error}",
                state_path.display()
            ),
            Err(_) => {}
        }
    }

    if let Some(snapshot) = startup_mux_snapshot(config) {
        match MuxModel::from_restore_snapshot(&snapshot, fallback.clone()) {
            Ok(model) => return model,
            Err(error) => eprintln!("startup mux layout rejected: {error}"),
        }
    }

    let mut model = MuxModel::new(fallback);
    model.active_workspace_mut().name = config.mux.default_workspace.clone();
    model
}

fn startup_mux_snapshot(config: &AppConfig) -> Option<RestoreSnapshot> {
    if config.mux.startup_workspaces.is_empty() {
        return None;
    }
    let mut next_pane_id = 1u64;
    let workspaces = config
        .mux
        .startup_workspaces
        .iter()
        .map(|workspace| WorkspaceRestore {
            name: workspace.name.clone(),
            windows: vec![WindowRestore {
                active_tab_name: workspace.tabs.first().map(|tab| tab.name.clone()),
                tabs: workspace
                    .tabs
                    .iter()
                    .map(|tab| {
                        let mut panes = Vec::new();
                        let layout =
                            startup_layout_tree(&tab.layout, &mut next_pane_id, &mut panes);
                        TabRestore {
                            name: tab.name.clone(),
                            active_pane: layout
                                .first_pane()
                                .expect("validated startup layout contains a pane"),
                            layout,
                            panes,
                        }
                    })
                    .collect(),
            }],
        })
        .collect();
    Some(RestoreSnapshot { workspaces })
}

fn startup_layout_tree(
    layout: &MuxLayoutConfig,
    next_pane_id: &mut u64,
    panes: &mut Vec<PaneRestore>,
) -> SplitTree {
    match layout {
        MuxLayoutConfig::Pane {
            profile,
            transport,
            working_directory,
        } => {
            let pane_id = PaneId(*next_pane_id);
            *next_pane_id = next_pane_id.saturating_add(1);
            panes.push(PaneRestore {
                pane_id,
                session_profile: profile.clone(),
                transport: match transport {
                    MuxTransportConfig::Local => {
                        if cfg!(windows) {
                            SessionTransportKind::WindowsPseudoconsole
                        } else {
                            SessionTransportKind::LocalPty
                        }
                    }
                    MuxTransportConfig::Ssh => SessionTransportKind::Ssh,
                },
                working_directory: working_directory.clone(),
            });
            SplitTree::Pane(pane_id)
        }
        MuxLayoutConfig::Split {
            axis,
            ratio,
            first,
            second,
        } => SplitTree::Split {
            axis: match axis {
                MuxSplitAxisConfig::Horizontal => SplitAxis::Horizontal,
                MuxSplitAxisConfig::Vertical => SplitAxis::Vertical,
            },
            children: vec![
                startup_layout_tree(first, next_pane_id, panes),
                startup_layout_tree(second, next_pane_id, panes),
            ],
            ratios: vec![*ratio, 1.0 - *ratio],
        },
    }
}

fn pane_session_specs(model: &MuxModel) -> Vec<(PaneId, SessionSpec)> {
    let mut specs = Vec::new();
    for workspace in model.workspaces.values() {
        for window in &workspace.windows {
            for tab in &window.tabs {
                for pane in tab.panes.values() {
                    if let Some(session) = tab.sessions.get(&pane.session_id) {
                        specs.push((pane.id, session.spec.clone()));
                    }
                }
            }
        }
    }
    specs
}

struct MuxRuntime {
    model: MuxModel,
    panes: HashMap<PaneId, PaneRuntime>,
    surface_cols: u16,
    surface_rows: u16,
    performance: RuntimePerformanceCounters,
    restore_sessions: bool,
    state_path: PathBuf,
    drag: Option<MuxDragState>,
    output_waker: TransportWakeHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MuxDragState {
    Tab { source: TabId, target: TabId },
    Pane { source: PaneId, target: PaneId },
}

impl MuxRuntime {
    fn new(
        config: &AppConfig,
        metrics: CellMetrics,
        width: u32,
        height: u32,
        output_waker: TransportWakeHandle,
    ) -> Self {
        let state_path = mux_state_path();
        let mut model = initial_mux_model(config, &state_path);

        let surface_cols = cols_for_width(
            content_extent(width, horizontal_content_inset(config)),
            metrics,
        )
        .max(1);
        let surface_rows = rows_for_height(
            content_extent(height, vertical_content_inset(config)),
            metrics,
        )
        .max(1);
        let initial_size = TerminalGridSize::new(
            surface_cols,
            surface_rows
                .saturating_sub(tab_bar_rows(&model, config))
                .max(1),
        );
        let mut panes = HashMap::new();
        for (pane_id, spec) in pane_session_specs(&model) {
            let pane = PaneRuntime::new(config, &spec, initial_size, metrics, output_waker.clone());
            let status = session_status_for_pane(&pane);
            panes.insert(pane_id, pane);
            mark_session_status(&mut model, pane_id, status);
        }

        let mut runtime = Self {
            model,
            panes,
            surface_cols,
            surface_rows,
            performance: RuntimePerformanceCounters::new(),
            restore_sessions: config.mux.restore_sessions,
            state_path,
            drag: None,
            output_waker,
        };
        runtime.resize_all(width, height, metrics, config);
        runtime
    }

    fn resize_all(&mut self, width: u32, height: u32, metrics: CellMetrics, config: &AppConfig) {
        self.surface_cols = cols_for_width(
            content_extent(width, horizontal_content_inset(config)),
            metrics,
        )
        .max(1);
        self.surface_rows = rows_for_height(
            content_extent(height, vertical_content_inset(config)),
            metrics,
        )
        .max(1);
        self.resize_active_tab(metrics, config);
    }

    fn resize_active_tab(&mut self, metrics: CellMetrics, config: &AppConfig) {
        let layouts = self.active_layouts(config);
        for layout in layouts {
            if let Some(pane) = self.panes.get_mut(&layout.pane_id) {
                pane.resize(layout.terminal_size, metrics);
            }
            if let Some(model_pane) = self.model.active_tab_mut().panes.get_mut(&layout.pane_id) {
                model_pane.last_size = Some(layout.terminal_size);
            }
        }
    }

    fn handle_mux_action(
        &mut self,
        action: MuxAction,
        config: &AppConfig,
        metrics: CellMetrics,
        width: u32,
        height: u32,
    ) -> bool {
        if !config.mux.enabled {
            eprintln!("mux action ignored because mux.enabled is false");
            return false;
        }

        let result = match action {
            MuxAction::NewTab => {
                self.new_tab_with_spec(
                    config,
                    session_spec_for_config(config),
                    metrics,
                    width,
                    height,
                );
                Ok(())
            }
            MuxAction::NewWorkspace => {
                let number = self.model.workspaces.len() + 1;
                let workspace_id = self.model.new_workspace(
                    format!("workspace {number}"),
                    session_spec_for_config(config),
                );
                let pane_id = self.model.active_tab().active_pane;
                self.insert_runtime_for_pane(pane_id, config, metrics);
                self.resize_all(width, height, metrics, config);
                debug_assert_eq!(self.model.active_workspace, workspace_id);
                Ok(())
            }
            MuxAction::CloseWorkspace => self.close_active_workspace(metrics, config),
            MuxAction::NextWorkspace => self.switch_relative_workspace(1, metrics, config),
            MuxAction::PreviousWorkspace => self.switch_relative_workspace(-1, metrics, config),
            MuxAction::CloseTab => self.close_active_tab(metrics, config),
            MuxAction::NextTab => self.switch_relative_tab(1, metrics, config),
            MuxAction::PreviousTab => self.switch_relative_tab(-1, metrics, config),
            MuxAction::RenameTab { name } => {
                let tab_id = self.model.active_tab().id;
                let name = if name.trim().is_empty() {
                    format!("tab {}", tab_id.0)
                } else {
                    name
                };
                self.model.rename_tab(tab_id, name)
            }
            MuxAction::MoveTab { target_index } => self
                .model
                .move_tab(self.model.active_tab().id, target_index),
            MuxAction::SplitHorizontal => {
                self.split_active(SplitAxis::Horizontal, config, metrics, width, height);
                Ok(())
            }
            MuxAction::SplitVertical => {
                self.split_active(SplitAxis::Vertical, config, metrics, width, height);
                Ok(())
            }
            MuxAction::ClosePane => self.close_active_pane(metrics, config),
            MuxAction::FocusDirection(direction) => {
                self.model.focus_direction(direction).map(|_| {
                    self.sync_active_tab_title();
                    self.resize_active_tab(metrics, config);
                })
            }
            MuxAction::ResizePane(direction) => self
                .model
                .resize_active_pane(direction, config.mux.pane_resize_step as f32)
                .map(|_| self.resize_active_tab(metrics, config)),
            MuxAction::ZoomPane => {
                self.model.toggle_zoom_active_pane();
                self.resize_active_tab(metrics, config);
                Ok(())
            }
            MuxAction::MovePane(direction) | MuxAction::SwapPaneDirection(direction) => self
                .model
                .move_active_pane(direction)
                .map(|_| self.resize_active_tab(metrics, config)),
            MuxAction::SwapPane { other } => self
                .model
                .swap_panes(self.model.active_tab().active_pane, other)
                .map(|_| self.resize_active_tab(metrics, config)),
        };

        if let Err(error) = result {
            eprintln!("mux action failed: {error}");
            return false;
        }
        true
    }

    fn new_tab_with_spec(
        &mut self,
        config: &AppConfig,
        spec: SessionSpec,
        metrics: CellMetrics,
        width: u32,
        height: u32,
    ) {
        let tab_number = self.model.active_workspace().active_window().tabs.len() + 1;
        match self.model.new_tab(tab_number.to_string(), spec) {
            Ok(_) => {
                let pane_id = self.model.active_tab().active_pane;
                self.insert_runtime_for_pane(pane_id, config, metrics);
                self.resize_all(width, height, metrics, config);
            }
            Err(error) => eprintln!("mux new tab failed: {error}"),
        }
    }

    fn split_active(
        &mut self,
        axis: SplitAxis,
        config: &AppConfig,
        metrics: CellMetrics,
        width: u32,
        height: u32,
    ) {
        self.split_active_with_spec(
            axis,
            session_spec_for_config(config),
            config,
            metrics,
            width,
            height,
        );
    }

    fn split_active_with_spec(
        &mut self,
        axis: SplitAxis,
        spec: SessionSpec,
        config: &AppConfig,
        metrics: CellMetrics,
        width: u32,
        height: u32,
    ) {
        match self.model.split_active_pane(axis, spec) {
            Ok(pane_id) => {
                self.insert_runtime_for_pane(pane_id, config, metrics);
                self.resize_all(width, height, metrics, config);
            }
            Err(error) => eprintln!("mux split failed: {error}"),
        }
    }

    fn insert_runtime_for_pane(
        &mut self,
        pane_id: PaneId,
        config: &AppConfig,
        metrics: CellMetrics,
    ) {
        let size = self
            .active_layouts(config)
            .into_iter()
            .find(|layout| layout.pane_id == pane_id)
            .map(|layout| layout.terminal_size)
            .unwrap_or_else(|| TerminalGridSize::new(self.surface_cols, self.surface_rows));
        let spec = match self.model.session_for_pane(pane_id) {
            Ok(session) => session.spec.clone(),
            Err(error) => {
                eprintln!("mux pane runtime could not find session: {error}");
                return;
            }
        };
        let pane = PaneRuntime::new(config, &spec, size, metrics, self.output_waker.clone());
        mark_session_status(&mut self.model, pane_id, session_status_for_pane(&pane));
        self.panes.insert(pane_id, pane);
    }

    fn close_active_tab(&mut self, metrics: CellMetrics, config: &AppConfig) -> mux::MuxResult<()> {
        let tab_id = self.model.active_tab().id;
        let pane_ids = self
            .model
            .active_tab()
            .panes
            .keys()
            .copied()
            .collect::<Vec<_>>();
        self.model.close_tab(tab_id)?;
        for pane_id in pane_ids {
            if let Some(mut pane) = self.panes.remove(&pane_id) {
                pane.shutdown();
            }
        }
        self.resize_active_tab(metrics, config);
        Ok(())
    }

    fn switch_relative_tab(
        &mut self,
        delta: isize,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> mux::MuxResult<()> {
        let window = self.model.active_workspace().active_window();
        let active_index = window
            .tabs
            .iter()
            .position(|tab| tab.id == window.active_tab)
            .ok_or(mux::MuxError::TabNotFound(window.active_tab))?;
        let next_index = (active_index as isize + delta).rem_euclid(window.tabs.len() as isize);
        let next_tab = window.tabs[next_index as usize].id;
        self.model.switch_tab(next_tab)?;
        self.resize_active_tab(metrics, config);
        Ok(())
    }

    fn switch_relative_workspace(
        &mut self,
        delta: isize,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> mux::MuxResult<()> {
        let workspace_ids = self.model.workspaces.keys().copied().collect::<Vec<_>>();
        let active_index = workspace_ids
            .iter()
            .position(|id| *id == self.model.active_workspace)
            .ok_or(mux::MuxError::WorkspaceNotFound(
                self.model.active_workspace,
            ))?;
        let next_index =
            (active_index as isize + delta).rem_euclid(workspace_ids.len() as isize) as usize;
        self.model.switch_workspace(workspace_ids[next_index])?;
        self.resize_active_tab(metrics, config);
        Ok(())
    }

    fn close_active_workspace(
        &mut self,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> mux::MuxResult<()> {
        let workspace_id = self.model.active_workspace;
        let pane_ids = self
            .model
            .active_workspace()
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .flat_map(|tab| tab.panes.keys().copied())
            .collect::<Vec<_>>();
        self.model.close_workspace(workspace_id)?;
        for pane_id in pane_ids {
            if let Some(mut pane) = self.panes.remove(&pane_id) {
                pane.shutdown();
            }
        }
        self.resize_active_tab(metrics, config);
        Ok(())
    }

    fn handle_profile_mux_action(
        &mut self,
        action: &str,
        config: &AppConfig,
        metrics: CellMetrics,
        width: u32,
        height: u32,
    ) -> bool {
        let Some((command, profile)) = action.split_once(':') else {
            return false;
        };
        if profile.trim().is_empty() {
            eprintln!("mux profile action requires a non-empty profile name");
            return true;
        }
        match command {
            "new_workspace" => {
                let pane_id = {
                    self.model
                        .new_workspace(profile, session_spec_for_config(config));
                    self.model.active_tab().active_pane
                };
                self.insert_runtime_for_pane(pane_id, config, metrics);
                self.resize_all(width, height, metrics, config);
                return true;
            }
            "rename_workspace" => {
                let _ = self
                    .model
                    .rename_workspace(self.model.active_workspace, profile);
                return true;
            }
            "switch_workspace" => {
                if let Some(workspace_id) = self
                    .model
                    .workspaces
                    .values()
                    .find(|workspace| workspace.name == profile)
                    .map(|workspace| workspace.id)
                {
                    let _ = self.model.switch_workspace(workspace_id);
                    self.resize_active_tab(metrics, config);
                } else {
                    eprintln!("mux workspace '{profile}' does not exist");
                }
                return true;
            }
            "rename_tab" => {
                let _ = self.model.rename_tab(self.model.active_tab().id, profile);
                return true;
            }
            _ => {}
        }
        let spec = match command {
            "new_local_tab" | "split_local_horizontal" | "split_local_vertical" => {
                local_session_spec(profile)
            }
            "new_ssh_tab" | "split_ssh_horizontal" | "split_ssh_vertical" => {
                SessionSpec::ssh(profile)
            }
            _ => return false,
        };
        match command {
            "new_local_tab" | "new_ssh_tab" => {
                self.new_tab_with_spec(config, spec, metrics, width, height)
            }
            "split_local_horizontal" | "split_ssh_horizontal" => self.split_active_with_spec(
                SplitAxis::Horizontal,
                spec,
                config,
                metrics,
                width,
                height,
            ),
            "split_local_vertical" | "split_ssh_vertical" => self.split_active_with_spec(
                SplitAxis::Vertical,
                spec,
                config,
                metrics,
                width,
                height,
            ),
            _ => unreachable!("profile mux command was validated above"),
        }
        true
    }

    fn close_active_pane(
        &mut self,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> mux::MuxResult<()> {
        let pane_id = self.model.active_tab().active_pane;
        self.model.close_pane(pane_id)?;
        if let Some(mut pane) = self.panes.remove(&pane_id) {
            pane.shutdown();
        }
        self.resize_active_tab(metrics, config);
        Ok(())
    }

    fn write_active(&mut self, bytes: &[u8]) {
        if let Some(pane) = self.active_pane_mut() {
            pane.write_input(bytes);
        }
    }

    fn input_bytes(&self, event: &KeyEvent) -> Option<Vec<u8>> {
        let key = terminal_key(event)?;
        let modes = self.active_pane()?.terminal.modes();
        encode_terminal_key(&key, terminal_modifiers(event.modifiers), &modes)
    }

    fn start_search(&mut self) {
        if let Some(pane) = self.active_pane_mut() {
            pane.search.start();
        }
    }

    fn append_search_text(&mut self, text: &str) -> bool {
        self.active_pane_mut()
            .is_some_and(|pane| pane.append_search_text(text))
    }

    fn append_modal_text(&mut self, text: &str) -> bool {
        self.active_pane_mut()
            .is_some_and(|pane| pane.append_modal_text(text))
    }

    fn update_active_ime_preedit(&mut self, text: String) -> bool {
        self.active_pane_mut()
            .is_some_and(|pane| pane.update_ime_preedit(text))
    }

    fn reconnect_active(&mut self, config: &AppConfig, metrics: CellMetrics) -> bool {
        let pane_id = self.model.active_tab().active_pane;
        let reconnected = self
            .panes
            .get_mut(&pane_id)
            .is_some_and(|pane| pane.reconnect(config, metrics));
        if let Some(pane) = self.panes.get(&pane_id) {
            mark_session_status(&mut self.model, pane_id, session_status_for_pane(pane));
        }
        reconnected
    }

    fn start_keyboard_selection(&mut self, kind: SelectionKind) {
        if let Some(pane) = self.active_pane_mut() {
            pane.start_keyboard_selection(kind);
        }
    }

    fn run_semantic_action(&mut self, action: SemanticAction) -> SemanticActionResult {
        self.active_pane_mut()
            .map_or(SemanticActionResult::Noop, |pane| {
                pane.run_semantic_action(action)
            })
    }

    fn toggle_current_command_output(&mut self, config: &AppConfig) -> bool {
        self.active_pane_mut()
            .is_some_and(|pane| pane.toggle_current_command_output(config))
    }

    fn handle_modal_key(
        &mut self,
        event: &KeyEvent,
        clipboard: &mut ClipboardBridge,
        policy: &Osc52ClipboardPolicy,
        clipboard_config: &ClipboardConfig,
    ) -> Option<bool> {
        self.active_pane_mut()?
            .handle_modal_key(event, clipboard, policy, clipboard_config)
    }

    fn scroll_active_page(&mut self, toward_older: bool) -> bool {
        let Some(pane) = self.active_pane_mut() else {
            return false;
        };
        let rows = i64::from(pane.terminal.visible_grid().viewport.size.rows).max(1);
        pane.terminal
            .state_mut()
            .scroll_viewport(if toward_older { rows } else { -rows })
    }

    fn scroll_active_to_top(&mut self) -> bool {
        let Some(pane) = self.active_pane_mut() else {
            return false;
        };
        let lines = i64::try_from(pane.terminal.scrollback().lines.len()).unwrap_or(i64::MAX);
        pane.terminal.state_mut().scroll_viewport(lines)
    }

    fn scroll_active_to_bottom(&mut self) -> bool {
        self.active_pane_mut()
            .is_some_and(|pane| pane.terminal.state_mut().scroll_to_bottom())
    }

    fn paste_into_active(
        &mut self,
        text: &str,
        clipboard: &ClipboardConfig,
        paste_config: &PasteConfig,
    ) {
        if let Some(pane) = self.active_pane_mut() {
            let bytes = paste_bytes(
                text,
                clipboard,
                paste_config,
                pane.terminal
                    .modes()
                    .contains(&TerminalMode::BracketedPaste),
            );
            pane.write_input(&bytes);
        }
    }

    fn send_focus_event(&mut self, focused: bool) {
        if let Some(pane) = self.active_pane_mut()
            && let Some(bytes) = focus_report_bytes(focused, &pane.terminal.modes())
        {
            pane.write_input(bytes);
        }
    }

    fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        metrics: CellMetrics,
        config: &AppConfig,
        clipboard_config: &ClipboardConfig,
        paste_config: &PasteConfig,
        clipboard: &mut ClipboardBridge,
    ) -> MouseHandling {
        if let Some(outcome) = self.update_mux_drag(mouse, metrics, config) {
            return outcome;
        }
        if let Some(tab_id) = self.tab_at_mouse(mouse, metrics, config) {
            if matches!(mouse.kind, MouseEventKind::Pressed(MouseButton::Left)) {
                if self.model.switch_tab(tab_id).is_ok() {
                    if config.mux.drag_tabs {
                        self.drag = Some(MuxDragState::Tab {
                            source: tab_id,
                            target: tab_id,
                        });
                    }
                    self.resize_active_tab(metrics, config);
                    return MouseHandling {
                        changed: true,
                        open_url: None,
                    };
                }
            } else if matches!(mouse.kind, MouseEventKind::Pressed(MouseButton::Middle))
                && self.model.active_workspace().active_window().tabs.len() > 1
            {
                let pane_ids = self
                    .model
                    .active_workspace()
                    .active_window()
                    .tabs
                    .iter()
                    .find(|tab| tab.id == tab_id)
                    .map(|tab| tab.panes.keys().copied().collect::<Vec<_>>())
                    .unwrap_or_default();
                if self.model.close_tab(tab_id).is_ok() {
                    for pane_id in pane_ids {
                        if let Some(mut pane) = self.panes.remove(&pane_id) {
                            pane.shutdown();
                        }
                    }
                    self.resize_active_tab(metrics, config);
                    return MouseHandling {
                        changed: true,
                        open_url: None,
                    };
                }
            }
            return MouseHandling::default();
        }
        let Some((pane_id, local_mouse)) = self.local_mouse_event(mouse, metrics, config) else {
            return MouseHandling::default();
        };
        if config.mux.drag_panes
            && local_mouse.modifiers.ctrl
            && local_mouse.modifiers.shift
            && matches!(local_mouse.kind, MouseEventKind::Pressed(MouseButton::Left))
        {
            let _ = self.model.focus_pane(pane_id);
            self.drag = Some(MuxDragState::Pane {
                source: pane_id,
                target: pane_id,
            });
            return MouseHandling {
                changed: true,
                open_url: None,
            };
        }
        if matches!(mouse.kind, MouseEventKind::Pressed(_)) {
            let _ = self.model.focus_pane(pane_id);
            self.sync_active_tab_title();
        }
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return MouseHandling::default();
        };
        let binding_action = mousebinding_action(&local_mouse, &config.mouse);
        if binding_action.as_deref() == Some("open_url") {
            return MouseHandling {
                changed: false,
                open_url: matches!(local_mouse.kind, MouseEventKind::Released(_))
                    .then(|| pane.url_at_mouse(local_mouse, metrics))
                    .flatten(),
            };
        }
        let modes = pane.terminal.modes();
        if !local_mouse.modifiers.shift
            && let Some(bytes) = pane
                .mouse_protocol
                .report_bytes(local_mouse, metrics, &modes)
        {
            pane.write_input(&bytes);
            return MouseHandling::default();
        }

        match binding_action.as_deref() {
            Some("ignore") => return MouseHandling::default(),
            Some("paste") if matches!(local_mouse.kind, MouseEventKind::Pressed(_)) => {
                if let Ok(text) = clipboard.paste_text() {
                    let bytes = paste_bytes(&text, clipboard_config, paste_config, false);
                    pane.write_input(&bytes);
                }
                return MouseHandling::default();
            }
            Some("paste_primary") if matches!(local_mouse.kind, MouseEventKind::Pressed(_)) => {
                if let Ok(text) = paste_for_middle_click(clipboard, clipboard_config) {
                    let bytes = paste_bytes(&text, clipboard_config, paste_config, false);
                    pane.write_input(&bytes);
                }
                return MouseHandling::default();
            }
            Some("copy") if matches!(local_mouse.kind, MouseEventKind::Released(_)) => {
                if let Some(text) = pane.terminal.state().selected_text() {
                    copy_text_with_diagnostics(clipboard, &text, clipboard_config, "mouse copy");
                }
                return MouseHandling::default();
            }
            _ => {}
        }

        let mut local_mouse = local_mouse;
        if binding_action.as_deref() == Some("select_rectangular") {
            local_mouse.modifiers.alt = true;
        } else if binding_action.as_deref() == Some("select") {
            local_mouse.modifiers.alt = false;
        } else if should_middle_click_paste(&local_mouse, &modes, clipboard_config)
            && let Ok(text) = paste_for_middle_click(clipboard, clipboard_config)
        {
            let bytes = paste_bytes(&text, clipboard_config, paste_config, false);
            pane.write_input(&bytes);
            return MouseHandling::default();
        }

        let selection_completed = matches!(
            local_mouse.kind,
            MouseEventKind::Released(MouseButton::Left)
        );
        let changed = pane.handle_selection_or_scrollback(local_mouse, metrics);
        if changed
            && selection_completed
            && let Some(text) = pane.terminal.state().selected_text()
        {
            if cfg!(target_os = "linux")
                && clipboard_config.prefer_primary_selection_on_linux
                && let Err(diagnostic) = clipboard.copy_primary_text(&text)
            {
                eprintln!("Linux primary selection copy failed: {diagnostic:?}");
            }
            if clipboard_config.enabled
                && (clipboard_config.copy_on_select || config.mouse.copy_on_select)
            {
                copy_text_with_diagnostics(clipboard, &text, clipboard_config, "copy-on-select");
            }
        }
        MouseHandling {
            changed,
            open_url: None,
        }
    }

    fn update_mux_drag(
        &mut self,
        mouse: MouseEvent,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> Option<MouseHandling> {
        let drag = self.drag?;
        match mouse.kind {
            MouseEventKind::Moved => {
                let next = match drag {
                    MuxDragState::Tab { source, target: _ } => self
                        .tab_at_mouse(mouse, metrics, config)
                        .map_or(drag, |target| MuxDragState::Tab { source, target }),
                    MuxDragState::Pane { source, target: _ } => self
                        .local_mouse_event(mouse, metrics, config)
                        .map_or(drag, |(target, _)| MuxDragState::Pane { source, target }),
                };
                let changed = next != drag;
                self.drag = Some(next);
                Some(MouseHandling {
                    changed,
                    open_url: None,
                })
            }
            MouseEventKind::Released(MouseButton::Left) => {
                self.drag = None;
                let changed = match drag {
                    MuxDragState::Tab { source, target } if source != target => {
                        let target_index = self
                            .model
                            .active_workspace()
                            .active_window()
                            .tabs
                            .iter()
                            .position(|tab| tab.id == target);
                        target_index.is_some_and(|target_index| {
                            self.model.move_tab(source, target_index).is_ok()
                        })
                    }
                    MuxDragState::Pane { source, target } if source != target => self
                        .model
                        .swap_panes(source, target)
                        .map(|_| self.resize_active_tab(metrics, config))
                        .is_ok(),
                    MuxDragState::Tab { .. } | MuxDragState::Pane { .. } => true,
                };
                Some(MouseHandling {
                    changed,
                    open_url: None,
                })
            }
            MouseEventKind::Released(_) => {
                self.drag = None;
                Some(MouseHandling {
                    changed: true,
                    open_url: None,
                })
            }
            MouseEventKind::Pressed(_) | MouseEventKind::Wheel { .. } => {
                Some(MouseHandling::default())
            }
        }
    }

    fn local_mouse_event(
        &self,
        mouse: MouseEvent,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> Option<(PaneId, MouseEvent)> {
        let inset_x = f64::from(horizontal_content_inset(config));
        let inset_y = f64::from(vertical_content_inset(config));
        if mouse.x < inset_x || mouse.y < inset_y {
            return None;
        }
        let content_x = mouse.x - inset_x;
        let content_y = mouse.y - inset_y;
        let x_cells = (content_x as f32 / metrics.cell_width).floor();
        let y_cells = (content_y as f32 / metrics.cell_height).floor();
        self.active_layouts(config)
            .into_iter()
            .find(|layout| {
                x_cells >= layout.rect.x
                    && x_cells < layout.rect.x + layout.rect.width
                    && y_cells >= layout.rect.y
                    && y_cells < layout.rect.y + layout.rect.height
            })
            .map(|layout| {
                let local = MouseEvent {
                    x: (content_x as f32 - layout.rect.x * metrics.cell_width).max(0.0) as f64,
                    y: (content_y as f32 - layout.rect.y * metrics.cell_height).max(0.0) as f64,
                    ..mouse
                };
                (layout.pane_id, local)
            })
    }

    fn tab_at_mouse(
        &self,
        mouse: MouseEvent,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> Option<TabId> {
        if tab_bar_rows(&self.model, config) == 0 {
            return None;
        }
        let inset_x = f64::from(horizontal_content_inset(config));
        let inset_y = f64::from(vertical_content_inset(config));
        if mouse.x < inset_x
            || mouse.y < inset_y
            || mouse.y >= inset_y + f64::from(metrics.cell_height)
        {
            return None;
        }
        let mouse_col = ((mouse.x - inset_x) / f64::from(metrics.cell_width)).floor() as usize;
        let workspace = self.model.active_workspace();
        let window = workspace.active_window();
        let mut start = 0usize;
        for (index, tab) in window.tabs.iter().enumerate() {
            let width = formatted_tab_width(config, &workspace.name, index, tab);
            let end = start.saturating_add(width);
            if (start..end).contains(&mouse_col) {
                return Some(tab.id);
            }
            start = end;
        }
        None
    }

    fn poll_outputs(
        &mut self,
        clipboard: &mut ClipboardBridge,
        policy: &Osc52ClipboardPolicy,
        clipboard_config: &ClipboardConfig,
        notification_provider: &mut dyn NotificationProvider,
        notification_config: &NotificationConfig,
        window_focused: bool,
    ) -> bool {
        let mut content_changed = false;
        let mut status_updates = Vec::new();
        let mut metadata_updates = Vec::new();
        for (pane_id, pane) in &mut self.panes {
            let poll = pane.poll_output(clipboard, policy, clipboard_config);
            self.performance.record_pty_bytes(poll.pty_bytes);
            self.performance.record_parser_bytes(poll.parser_bytes);
            if poll.content_changed {
                content_changed = true;
            }
            notify_for_pane_transition(
                notification_provider,
                notification_config,
                window_focused,
                pane,
                poll,
            );
            status_updates.push((*pane_id, session_status_for_pane(pane)));
            metadata_updates.push((
                *pane_id,
                pane.terminal.state().title().map(ToOwned::to_owned),
                pane.semantic_timeline
                    .metadata()
                    .remote
                    .as_ref()
                    .and_then(|remote| remote.remote_current_working_directory.clone())
                    .or_else(|| {
                        pane.semantic_timeline
                            .metadata()
                            .shell
                            .current_working_directory
                            .clone()
                    }),
            ));
        }
        for (pane_id, status) in status_updates {
            mark_session_status(&mut self.model, pane_id, status);
        }
        for (pane_id, title, directory) in metadata_updates {
            if let Some(title) = title {
                let _ = self.model.update_pane_title(pane_id, title);
            }
            if let Ok(session) = self.model.session_for_pane_mut(pane_id) {
                session.current_working_directory = directory;
            }
        }
        content_changed
    }

    fn requires_periodic_transport_poll(&self) -> bool {
        self.panes.values().any(|pane| {
            pane.transport
                .as_ref()
                .is_some_and(PaneTransport::requires_periodic_poll)
        })
    }

    fn active_visible_text(&self) -> String {
        self.active_pane()
            .map(|pane| {
                let visible = pane.terminal.visible_grid();
                visible
                    .cells
                    .chunks(usize::from(visible.viewport.size.cols.max(1)))
                    .map(|cells| {
                        term_core::Line {
                            cells: cells.to_vec(),
                            hard_wrapped: false,
                        }
                        .raw_text()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    fn populate_performance_sample(&mut self, sample: &mut RenderInstrumentation) {
        let throughput = self.performance.sample_throughput();
        sample.pty_read_bytes_per_second = throughput.pty_read_bytes_per_second;
        sample.parser_bytes_per_second = throughput.parser_bytes_per_second;
        sample.scrollback_memory_bytes = self.scrollback_memory_bytes();
        sample.memory_usage_bytes = Some(
            sample
                .scrollback_memory_bytes
                .saturating_add(sample.glyphs.atlas_capacity_bytes),
        );
    }

    fn scrollback_memory_bytes(&self) -> u64 {
        self.panes
            .values()
            .map(PaneRuntime::scrollback_memory_bytes)
            .sum()
    }

    fn active_selected_text(&self) -> Option<String> {
        self.active_pane()
            .and_then(|pane| pane.terminal.state().selected_text())
    }

    fn active_cursor_blinks(&self) -> bool {
        self.active_pane()
            .is_some_and(|pane| pane.terminal.cursor_state().blinking)
    }

    fn active_pane(&self) -> Option<&PaneRuntime> {
        self.panes.get(&self.model.active_tab().active_pane)
    }

    fn active_pane_mut(&mut self) -> Option<&mut PaneRuntime> {
        let pane_id = self.model.active_tab().active_pane;
        self.panes.get_mut(&pane_id)
    }

    fn sync_active_tab_title(&mut self) {
        let pane_id = self.model.active_tab().active_pane;
        if let Some(title) = self
            .panes
            .get(&pane_id)
            .and_then(|pane| pane.terminal.state().title())
            .map(ToOwned::to_owned)
        {
            let _ = self.model.update_pane_title(pane_id, title);
        }
    }

    fn active_layouts(&self, config: &AppConfig) -> Vec<PaneLayout> {
        let tab_bar_rows = tab_bar_rows(&self.model, config);
        self.model.active_tab().layout(LogicalRect::new(
            0.0,
            f32::from(tab_bar_rows),
            f32::from(self.surface_cols),
            f32::from(self.surface_rows.saturating_sub(tab_bar_rows).max(1)),
        ))
    }

    fn shutdown_all(&mut self) {
        self.sync_session_metadata();
        if self.restore_sessions {
            if let Some(parent) = self.state_path.parent()
                && let Err(error) = fs::create_dir_all(parent)
            {
                eprintln!("mux state directory could not be created: {error}");
            }
            match serde_json::to_string_pretty(&self.model.restore_snapshot()) {
                Ok(snapshot) => {
                    if let Err(error) = fs::write(&self.state_path, snapshot) {
                        eprintln!(
                            "mux state could not be saved to {}: {error}",
                            self.state_path.display()
                        );
                    }
                }
                Err(error) => eprintln!("mux state could not be serialized: {error}"),
            }
        }
        for pane in self.panes.values_mut() {
            pane.shutdown();
        }
    }

    fn sync_session_metadata(&mut self) {
        for (pane_id, pane) in &self.panes {
            if let Ok(session) = self.model.session_for_pane_mut(*pane_id) {
                let metadata = pane.semantic_timeline.metadata();
                session.current_working_directory = metadata
                    .remote
                    .as_ref()
                    .and_then(|remote| remote.remote_current_working_directory.clone())
                    .or_else(|| metadata.shell.current_working_directory.clone());
            }
        }
    }
}

fn shell_prompt_visible(text: &str) -> bool {
    shell_prompt_line_count(text) > 0
}

fn shell_prompt_line_count(text: &str) -> usize {
    text.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| {
            let line = line.trim_end();
            (line.starts_with("PS ") && line.ends_with('>'))
                || line.ends_with('$')
                || line.ends_with('#')
                || line.ends_with('%')
                || (cfg!(windows) && line.ends_with('>'))
        })
        .count()
}

fn session_status_for_pane(pane: &PaneRuntime) -> SessionStatus {
    match &pane.connection_state {
        PaneConnectionState::Connecting => SessionStatus::Pending,
        PaneConnectionState::Connected => SessionStatus::Running,
        PaneConnectionState::Disconnected(message) if pane.remote_session => {
            SessionStatus::Failed {
                message: message.clone(),
            }
        }
        PaneConnectionState::Disconnected(_) => SessionStatus::Exited { exit_code: None },
    }
}

fn notify_for_pane_transition(
    provider: &mut dyn NotificationProvider,
    config: &NotificationConfig,
    window_focused: bool,
    pane: &PaneRuntime,
    poll: PanePollStats,
) {
    if !config.enabled || (config.only_when_unfocused && window_focused) {
        return;
    }
    let (title, body, urgency) = if poll.error && config.transport_errors {
        (
            if pane.remote_session {
                "Panea SSH transport error"
            } else {
                "Panea terminal transport error"
            },
            format!(
                "The {} session for profile '{}' stopped with an error. Open Panea for details.",
                if pane.remote_session { "SSH" } else { "local" },
                pane.session_spec.profile_name
            ),
            NotificationUrgency::Critical,
        )
    } else if poll.closed && config.session_closed {
        (
            if pane.remote_session {
                "Panea SSH session disconnected"
            } else {
                "Panea terminal session exited"
            },
            format!(
                "The {} session for profile '{}' has closed.",
                if pane.remote_session { "SSH" } else { "local" },
                pane.session_spec.profile_name
            ),
            NotificationUrgency::Normal,
        )
    } else {
        return;
    };
    if let Err(diagnostic) =
        provider.notify(NotificationRequest::new(title, body).with_urgency(urgency))
    {
        eprintln!("notification fallback: {}", diagnostic.message);
    }
}

struct PaneTextProvider<'a> {
    terminal: &'a TerminalEmulator,
}

impl TerminalTextProvider for PaneTextProvider<'_> {
    fn text_for_span(&self, span: SemanticSpan) -> Option<String> {
        Some(self.terminal.state().text_for_selection(Selection {
            start: GridPosition::new(span.start.row, span.start.col),
            end: GridPosition::new(span.end.row, span.end.col),
            kind: SelectionKind::Normal,
        }))
    }
}

struct PaneRuntime {
    terminal: TerminalEmulator,
    semantic_parser: SemanticEscapeParser,
    semantic_timeline: SemanticTimelineStore,
    heuristic_detector: Option<HeuristicCommandDetector>,
    parse_semantic_events: bool,
    remote_session: bool,
    session_spec: SessionSpec,
    last_size: TerminalGridSize,
    connection_state: PaneConnectionState,
    disconnect_notified: bool,
    ssh_prompt: Option<SshPromptState>,
    osc52_prompt: Option<Osc52PromptState>,
    ime_preedit: String,
    transport: Option<PaneTransport>,
    mouse_protocol: MouseProtocolState,
    selection_anchor: Option<GridPosition>,
    selection_kind: SelectionKind,
    keyboard_selection: Option<KeyboardSelection>,
    search: PaneSearch,
    /// Per-command presentation override. `true` is collapsed, `false` keeps
    /// an otherwise auto-collapsed block expanded. Raw terminal data is untouched.
    command_output_collapsed: HashMap<u64, bool>,
    output_waker: TransportWakeHandle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PaneConnectionState {
    Connecting,
    Connected,
    Disconnected(String),
}

enum SshPromptState {
    HostTrust {
        request: HostKeyTrustRequest,
        response: Option<SyncSender<HostKeyTrustAction>>,
    },
    Secret {
        request: SecretRequest,
        keychain: KeychainProviderCapability,
        response: Option<SyncSender<Option<SecretPromptResponse>>>,
        input: String,
        save_to_keychain: bool,
    },
}

#[derive(Debug, Clone)]
struct Osc52PromptState {
    request: SecurityOsc52Request,
    reason: String,
    bytes: usize,
}

impl SshPromptState {
    fn from_request(request: SshInteractionRequest) -> Self {
        match request {
            SshInteractionRequest::HostTrust { request, response } => Self::HostTrust {
                request,
                response: Some(response),
            },
            SshInteractionRequest::Secret {
                request,
                keychain,
                response,
            } => Self::Secret {
                request,
                keychain,
                response: Some(response),
                input: String::new(),
                save_to_keychain: false,
            },
        }
    }

    fn handle_key(&mut self, event: &KeyEvent) -> bool {
        match self {
            Self::HostTrust { request, response } => {
                let action = match event.logical_key.to_ascii_lowercase().as_str() {
                    "escape" => Some(HostKeyTrustAction::Reject),
                    "o" if request.reason == HostKeyTrustReason::UnknownHost => {
                        Some(HostKeyTrustAction::TrustOnce)
                    }
                    "s" if request.reason == HostKeyTrustReason::UnknownHost => {
                        Some(HostKeyTrustAction::TrustAndStore)
                    }
                    "r" if request.reason == HostKeyTrustReason::ChangedHostKey => {
                        Some(HostKeyTrustAction::ReplaceStoredKey)
                    }
                    _ => None,
                };
                if let Some(action) = action
                    && let Some(response) = response.take()
                {
                    let _ = response.send(action);
                }
                action.is_some()
            }
            Self::Secret {
                response,
                input,
                keychain,
                save_to_keychain,
                ..
            } => match event.logical_key.as_str() {
                "Escape" => {
                    if let Some(response) = response.take() {
                        let _ = response.send(None);
                    }
                    true
                }
                "Enter" if !input.is_empty() => {
                    if let Some(response) = response.take() {
                        let secret = SecretString::new(std::mem::take(input));
                        let response_value = if *save_to_keychain {
                            SecretPromptResponse::persistent(secret)
                        } else {
                            SecretPromptResponse::transient(secret)
                        };
                        let _ = response.send(Some(response_value));
                    }
                    true
                }
                "Tab" => {
                    if keychain.available {
                        *save_to_keychain = !*save_to_keychain;
                    }
                    false
                }
                "Backspace" => {
                    if let Some((index, _)) = input.grapheme_indices(true).next_back() {
                        input.truncate(index);
                    }
                    false
                }
                _ if !event.modifiers.ctrl
                    && !event.modifiers.super_key
                    && (!event.modifiers.alt || event.modifiers.alt_graph) =>
                {
                    if let Some(text) = event.text.as_deref() {
                        append_secret_input(input, text);
                    }
                    false
                }
                _ => false,
            },
        }
    }

    fn append_text(&mut self, text: &str) -> bool {
        let Self::Secret { input, .. } = self else {
            return false;
        };
        append_secret_input(input, text)
    }
}

fn append_secret_input(input: &mut String, text: &str) -> bool {
    const MAX_SECRET_BYTES: usize = 4096;
    if text.is_empty() || input.len().saturating_add(text.len()) > MAX_SECRET_BYTES {
        return false;
    }
    input.push_str(text);
    true
}

#[derive(Debug, Default)]
struct MouseHandling {
    changed: bool,
    open_url: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct KeyboardSelection {
    anchor: GridPosition,
    focus: GridPosition,
    kind: SelectionKind,
}

#[derive(Debug, Default)]
struct PaneSearch {
    input_active: bool,
    query: String,
    matches: Vec<Selection>,
    active_match: usize,
}

fn refresh_search_state(search: &mut PaneSearch, terminal: &mut TerminalEmulator) {
    search.matches = terminal.state().search(&search.query, false);
    search.active_match = search
        .active_match
        .min(search.matches.len().saturating_sub(1));
    if let Some(selection) = search.matches.get(search.active_match) {
        terminal.state_mut().reveal_position(selection.start);
    }
}

impl PaneSearch {
    fn start(&mut self) {
        self.input_active = true;
        self.query.clear();
        self.matches.clear();
        self.active_match = 0;
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct PanePollStats {
    content_changed: bool,
    pty_bytes: u64,
    parser_bytes: u64,
    closed: bool,
    error: bool,
}

#[derive(Debug)]
struct RuntimePerformanceCounters {
    pty_read_bytes: u64,
    parser_bytes: u64,
    sample_started: Instant,
}

#[derive(Debug, Clone, Copy, Default)]
struct RuntimeThroughputSample {
    pty_read_bytes_per_second: u64,
    parser_bytes_per_second: u64,
}

impl RuntimePerformanceCounters {
    fn new() -> Self {
        Self {
            pty_read_bytes: 0,
            parser_bytes: 0,
            sample_started: Instant::now(),
        }
    }

    fn record_pty_bytes(&mut self, bytes: u64) {
        self.pty_read_bytes = self.pty_read_bytes.saturating_add(bytes);
    }

    fn record_parser_bytes(&mut self, bytes: u64) {
        self.parser_bytes = self.parser_bytes.saturating_add(bytes);
    }

    fn sample_throughput(&mut self) -> RuntimeThroughputSample {
        let elapsed = self.sample_started.elapsed();
        if elapsed.is_zero() {
            return RuntimeThroughputSample::default();
        }

        let pty_read_bytes_per_second =
            bytes_per_second(self.pty_read_bytes, elapsed).unwrap_or(u64::MAX);
        let parser_bytes_per_second =
            bytes_per_second(self.parser_bytes, elapsed).unwrap_or(u64::MAX);
        self.pty_read_bytes = 0;
        self.parser_bytes = 0;
        self.sample_started = Instant::now();

        RuntimeThroughputSample {
            pty_read_bytes_per_second,
            parser_bytes_per_second,
        }
    }
}

fn bytes_per_second(bytes: u64, elapsed: Duration) -> Option<u64> {
    let nanos = elapsed.as_nanos();
    if nanos == 0 {
        return None;
    }
    let per_second = (u128::from(bytes) * 1_000_000_000) / nanos;
    Some(u64::try_from(per_second).unwrap_or(u64::MAX))
}

impl PaneRuntime {
    fn new(
        config: &AppConfig,
        spec: &SessionSpec,
        size: TerminalGridSize,
        metrics: CellMetrics,
        output_waker: TransportWakeHandle,
    ) -> Self {
        let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(size.cols, size.rows));
        let transport_size = terminal_transport_size(size, metrics);
        let mut semantic_timeline = SemanticTimelineStore::new();
        let (transport, parse_semantic_events) =
            match spawn_session_transport(config, spec, transport_size, &output_waker) {
                Ok(initial) => {
                    semantic_timeline.set_integration_mode(initial.semantic_mode);
                    if let Some(metadata) = initial.remote_metadata {
                        semantic_timeline.set_remote_session_metadata(metadata);
                    }
                    for diagnostic in initial.activation_diagnostics {
                        eprintln!("shell integration: {diagnostic}");
                    }
                    (Some(initial.transport), initial.parse_semantic_events)
                }
                Err(error) => {
                    let _ = terminal.apply_bytes(
                        format!("failed to start session transport: {error}\r\n").as_bytes(),
                    );
                    semantic_timeline.set_integration_mode(IntegrationMode::Disabled);
                    (None, false)
                }
            };

        let heuristic_detector = (semantic_timeline.integration_mode()
            == IntegrationMode::Heuristic)
            .then(HeuristicCommandDetector::default);
        Self {
            terminal,
            semantic_parser: SemanticEscapeParser::new(),
            semantic_timeline,
            heuristic_detector,
            parse_semantic_events,
            remote_session: matches!(spec.transport, SessionTransportKind::Ssh),
            session_spec: spec.clone(),
            last_size: size,
            connection_state: if matches!(spec.transport, SessionTransportKind::Ssh) {
                if transport.is_some() {
                    PaneConnectionState::Connecting
                } else {
                    PaneConnectionState::Disconnected("SSH transport failed to start".to_owned())
                }
            } else if transport.is_some() {
                PaneConnectionState::Connected
            } else {
                PaneConnectionState::Disconnected("transport failed to start".to_owned())
            },
            disconnect_notified: false,
            ssh_prompt: None,
            osc52_prompt: None,
            ime_preedit: String::new(),
            transport,
            mouse_protocol: MouseProtocolState::default(),
            selection_anchor: None,
            selection_kind: SelectionKind::Normal,
            keyboard_selection: None,
            search: PaneSearch::default(),
            command_output_collapsed: HashMap::new(),
            output_waker,
        }
    }

    fn toggle_current_command_output(&mut self, config: &AppConfig) -> bool {
        if !config.command_blocks.enabled {
            return false;
        }
        let cursor = self.terminal.state().cursor_buffer_position();
        let Some(block) = self
            .semantic_timeline
            .current_command(BufferPosition::new(cursor.row, cursor.col))
            .or_else(|| {
                self.semantic_timeline
                    .previous_command(BufferPosition::new(cursor.row, cursor.col))
            })
        else {
            return false;
        };
        let output_rows = self
            .semantic_timeline
            .output_span_for_command(block)
            .map_or(0, semantic_span_rows);
        if output_rows <= u32::from(config.command_blocks.collapsed_preview_lines) {
            return false;
        }
        let auto_collapsed = config.command_blocks.collapse_long_output
            && output_rows > u32::from(config.command_blocks.collapse_after_lines);
        let currently_collapsed = self
            .command_output_collapsed
            .get(&block.region_id)
            .copied()
            .unwrap_or(auto_collapsed);
        self.command_output_collapsed
            .insert(block.region_id, !currently_collapsed);
        true
    }

    fn start_keyboard_selection(&mut self, kind: SelectionKind) {
        let position = self.terminal.state().cursor_buffer_position();
        self.keyboard_selection = Some(KeyboardSelection {
            anchor: position,
            focus: position,
            kind,
        });
        self.terminal.state_mut().set_selection(Selection {
            start: position,
            end: position,
            kind,
        });
    }

    fn run_semantic_action(&mut self, action: SemanticAction) -> SemanticActionResult {
        let cursor = self.terminal.state().cursor_buffer_position();
        let position = BufferPosition::new(cursor.row, cursor.col);
        let provider = PaneTextProvider {
            terminal: &self.terminal,
        };
        let result = self
            .semantic_timeline
            .run_action(action, position, &provider);
        match result {
            SemanticActionResult::Position(position) => {
                self.terminal
                    .state_mut()
                    .reveal_position(GridPosition::new(position.row, position.col));
            }
            SemanticActionResult::Selection(span) => {
                self.terminal.state_mut().set_selection(Selection {
                    start: GridPosition::new(span.start.row, span.start.col),
                    end: GridPosition::new(span.end.row, span.end.col),
                    kind: SelectionKind::Normal,
                });
                self.terminal
                    .state_mut()
                    .reveal_position(GridPosition::new(span.start.row, span.start.col));
            }
            SemanticActionResult::Text(_) | SemanticActionResult::Noop => {}
        }
        result
    }

    fn handle_modal_key(
        &mut self,
        event: &KeyEvent,
        clipboard: &mut ClipboardBridge,
        policy: &Osc52ClipboardPolicy,
        clipboard_config: &ClipboardConfig,
    ) -> Option<bool> {
        if let Some(prompt) = self.ssh_prompt.as_mut() {
            let completed = prompt.handle_key(event);
            if completed {
                self.ssh_prompt = None;
            }
            return Some(true);
        }
        if self.osc52_prompt.is_some() {
            match event.logical_key.to_ascii_lowercase().as_str() {
                "y" => {
                    let Some(prompt) = self.osc52_prompt.take() else {
                        return Some(true);
                    };
                    match approve_osc52_clipboard_write(&prompt.request, policy) {
                        Osc52ClipboardDecision::Allow { text, bytes } => {
                            copy_osc52_text_with_diagnostics(
                                clipboard,
                                &text,
                                clipboard_config,
                                prompt.request.target,
                                "confirmed remote OSC 52",
                            );
                            eprintln!(
                                "clipboard OSC 52: user approved one remote {bytes} byte write"
                            );
                        }
                        Osc52ClipboardDecision::Deny { reason } => {
                            eprintln!(
                                "clipboard OSC 52: approved request failed policy recheck: {reason}"
                            );
                        }
                        Osc52ClipboardDecision::PromptRequired { .. } => {
                            eprintln!(
                                "clipboard OSC 52: approved request unexpectedly required another prompt"
                            );
                        }
                    }
                    return Some(true);
                }
                "n" | "escape" => {
                    self.osc52_prompt = None;
                    eprintln!("clipboard OSC 52: user denied remote clipboard write");
                    return Some(true);
                }
                _ => return Some(true),
            }
        }
        if self.search.input_active {
            return Some(self.handle_search_key(event));
        }
        if self.keyboard_selection.is_some() {
            return Some(self.handle_keyboard_selection_key(event));
        }
        None
    }

    fn append_modal_text(&mut self, text: &str) -> bool {
        self.ssh_prompt
            .as_mut()
            .is_some_and(|prompt| prompt.append_text(text))
    }

    fn update_ime_preedit(&mut self, text: String) -> bool {
        if self.ime_preedit == text {
            return false;
        }
        self.ime_preedit = text;
        true
    }

    fn append_search_text(&mut self, text: &str) -> bool {
        if !self.search.input_active || text.is_empty() {
            return false;
        }
        self.search.query.push_str(text);
        self.refresh_search();
        true
    }

    fn handle_search_key(&mut self, event: &KeyEvent) -> bool {
        match event.logical_key.as_str() {
            "Escape" => {
                self.search.input_active = false;
                self.search.matches.clear();
                true
            }
            "Backspace" => {
                if let Some((index, _)) = self.search.query.grapheme_indices(true).next_back() {
                    self.search.query.truncate(index);
                    self.refresh_search();
                }
                true
            }
            "Enter" | "ArrowDown" => {
                self.advance_search(!event.modifiers.shift);
                true
            }
            "ArrowUp" => {
                self.advance_search(false);
                true
            }
            _ if !event.modifiers.ctrl
                && !event.modifiers.super_key
                && (!event.modifiers.alt || event.modifiers.alt_graph) =>
            {
                if let Some(text) = event.text.as_deref().filter(|text| !text.is_empty()) {
                    self.search.query.push_str(text);
                    self.refresh_search();
                }
                true
            }
            _ => true,
        }
    }

    fn refresh_search(&mut self) {
        refresh_search_state(&mut self.search, &mut self.terminal);
    }

    fn advance_search(&mut self, forward: bool) {
        if self.search.matches.is_empty() {
            return;
        }
        if forward {
            self.search.active_match = (self.search.active_match + 1) % self.search.matches.len();
        } else {
            self.search.active_match = self
                .search
                .active_match
                .checked_sub(1)
                .unwrap_or(self.search.matches.len() - 1);
        }
        self.reveal_active_search_match();
    }

    fn reveal_active_search_match(&mut self) {
        if let Some(selection) = self.search.matches.get(self.search.active_match) {
            self.terminal.state_mut().reveal_position(selection.start);
        }
    }

    fn handle_keyboard_selection_key(&mut self, event: &KeyEvent) -> bool {
        if event.logical_key == "Escape" {
            self.keyboard_selection = None;
            self.terminal.state_mut().clear_selection();
            return true;
        }
        if event.logical_key == "Enter" {
            self.keyboard_selection = None;
            return true;
        }

        let Some(mut selection) = self.keyboard_selection else {
            return false;
        };
        let max_row = self.terminal.state().buffer_line_count().saturating_sub(1) as i64;
        let max_col = self
            .terminal
            .visible_grid()
            .viewport
            .size
            .cols
            .saturating_sub(1);
        let page = i64::from(self.terminal.visible_grid().viewport.size.rows).max(1);
        match event.logical_key.as_str() {
            "ArrowLeft" => selection.focus.col = selection.focus.col.saturating_sub(1),
            "ArrowRight" => {
                selection.focus.col = selection.focus.col.saturating_add(1).min(max_col)
            }
            "ArrowUp" => selection.focus.row = selection.focus.row.saturating_sub(1),
            "ArrowDown" => selection.focus.row = (selection.focus.row + 1).min(max_row),
            "Home" => selection.focus.col = 0,
            "End" => selection.focus.col = max_col,
            "PageUp" => selection.focus.row = selection.focus.row.saturating_sub(page),
            "PageDown" => selection.focus.row = (selection.focus.row + page).min(max_row),
            _ => return true,
        }
        self.keyboard_selection = Some(selection);
        self.terminal.state_mut().set_selection(Selection {
            start: selection.anchor,
            end: selection.focus,
            kind: selection.kind,
        });
        self.terminal.state_mut().reveal_position(selection.focus);
        true
    }

    fn url_at_mouse(&self, mouse: MouseEvent, metrics: CellMetrics) -> Option<String> {
        let viewport = self.terminal.visible_grid().viewport;
        let row = ((mouse.y / f64::from(metrics.cell_height)).floor() as u16)
            .min(viewport.size.rows.saturating_sub(1));
        let col = ((mouse.x / f64::from(metrics.cell_width)).floor() as u16)
            .min(viewport.size.cols.saturating_sub(1));
        visible_url_hints(&self.terminal, row)
            .into_iter()
            .find(|hint| col >= hint.start.col && col < hint.end.col)
            .map(|hint| hint.text)
    }

    fn handle_selection_or_scrollback(&mut self, mouse: MouseEvent, metrics: CellMetrics) -> bool {
        if let MouseEventKind::Wheel { delta_y, .. } = mouse.kind {
            let lines = if delta_y.abs() <= 10.0 {
                (delta_y * 3.0).round() as i64
            } else {
                (delta_y / f64::from(metrics.cell_height)).round() as i64
            };
            return self.terminal.state_mut().scroll_viewport(lines);
        }

        let visible = self.terminal.visible_grid().viewport;
        let row = ((mouse.y / f64::from(metrics.cell_height)).floor() as u16)
            .min(visible.size.rows.saturating_sub(1));
        let col = ((mouse.x / f64::from(metrics.cell_width)).floor() as u16)
            .min(visible.size.cols.saturating_sub(1));
        let position = self.terminal.state().viewport_position(row, col);

        match mouse.kind {
            MouseEventKind::Pressed(MouseButton::Left) => {
                self.selection_anchor = Some(position);
                self.selection_kind = if mouse.modifiers.alt {
                    SelectionKind::Rectangular
                } else {
                    SelectionKind::Normal
                };
                self.terminal.state_mut().set_selection(Selection {
                    start: position,
                    end: position,
                    kind: self.selection_kind,
                });
                true
            }
            MouseEventKind::Moved => {
                let Some(anchor) = self.selection_anchor else {
                    return false;
                };
                self.terminal.state_mut().set_selection(Selection {
                    start: anchor,
                    end: position,
                    kind: self.selection_kind,
                });
                true
            }
            MouseEventKind::Released(MouseButton::Left) => {
                let Some(anchor) = self.selection_anchor.take() else {
                    return false;
                };
                self.terminal.state_mut().set_selection(Selection {
                    start: anchor,
                    end: position,
                    kind: self.selection_kind,
                });
                true
            }
            _ => false,
        }
    }

    fn resize(&mut self, size: TerminalGridSize, metrics: CellMetrics) {
        self.last_size = size;
        let _ = self
            .terminal
            .resize(CoreTerminalSize::new(size.cols, size.rows));
        if let Some(transport) = self.transport.as_mut() {
            resize_transport(transport, terminal_transport_size(size, metrics));
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        if self.transport.is_some()
            && let Some(detector) = self.heuristic_detector.as_mut()
        {
            let cursor = self.terminal.state().cursor_buffer_position();
            let events = detector.observe_input(
                bytes,
                BufferPosition::new(cursor.row, cursor.col),
                self.terminal
                    .modes()
                    .contains(&TerminalMode::AlternateScreen),
                Instant::now(),
            );
            for event in events {
                self.semantic_timeline.apply_event(event);
            }
        }
        if let Some(transport) = self.transport.as_mut() {
            write_terminal_input(&mut self.terminal, transport, bytes);
        }
    }

    fn poll_output(
        &mut self,
        clipboard: &mut ClipboardBridge,
        policy: &Osc52ClipboardPolicy,
        clipboard_config: &ClipboardConfig,
    ) -> PanePollStats {
        let Some(transport) = self.transport.as_mut() else {
            return PanePollStats::default();
        };

        let mut stats = PanePollStats::default();
        if self.ssh_prompt.is_none()
            && let Some(request) = transport.take_interaction()
        {
            self.ssh_prompt = Some(SshPromptState::from_request(request));
            stats.content_changed = true;
        }
        for _ in 0..64 {
            let output = match catch_unwind(AssertUnwindSafe(|| transport.poll_output())) {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    eprintln!("transport poll error: {error}");
                    self.connection_state = PaneConnectionState::Disconnected(error.to_string());
                    let _ = self
                        .terminal
                        .apply_bytes(format!("\r\ntransport error: {error}\r\n").as_bytes());
                    stats.content_changed = true;
                    stats.error = !self.disconnect_notified;
                    self.disconnect_notified = true;
                    break;
                }
                Err(panic) => {
                    eprintln!("transport poll panic boundary: {}", panic_payload(panic));
                    break;
                }
            };
            if transport.is_connected() {
                self.connection_state = PaneConnectionState::Connected;
                self.disconnect_notified = false;
            }
            if output.bytes.is_empty() && output.lifecycle.is_empty() && !output.closed {
                break;
            }

            if !output.bytes.is_empty() {
                self.connection_state = PaneConnectionState::Connected;
                self.disconnect_notified = false;
                let byte_count = u64::try_from(output.bytes.len()).unwrap_or(u64::MAX);
                stats.pty_bytes = stats.pty_bytes.saturating_add(byte_count);
                if self.parse_semantic_events {
                    let cursor = self.terminal.cursor_state();
                    let initial_position =
                        BufferPosition::new(cursor.position.row, cursor.position.col);
                    let parsed = self.semantic_parser.parse(&output.bytes, initial_position);
                    let mut applied = 0usize;
                    for parsed in parsed {
                        let source_end = parsed.source_end.min(output.bytes.len()).max(applied);
                        if !apply_terminal_bytes(
                            &mut self.terminal,
                            &output.bytes[applied..source_end],
                        ) {
                            break;
                        }
                        applied = source_end;
                        let cursor = self.terminal.cursor_state();
                        let event = parsed.event.at_position(BufferPosition::new(
                            cursor.position.row,
                            cursor.position.col,
                        ));
                        let event = if self.remote_session {
                            self.semantic_timeline.mark_remote_integration_active();
                            event.in_remote_session()
                        } else {
                            event
                        };
                        self.semantic_timeline.apply_event(event);
                    }
                    if !apply_terminal_bytes(&mut self.terminal, &output.bytes[applied..]) {
                        break;
                    }
                } else if !apply_terminal_bytes(&mut self.terminal, &output.bytes) {
                    break;
                }
                stats.parser_bytes = stats.parser_bytes.saturating_add(byte_count);
                process_pending_clipboard_requests(
                    &mut self.terminal,
                    clipboard,
                    policy,
                    clipboard_config,
                    self.remote_session,
                    &mut self.osc52_prompt,
                );
                flush_terminal_responses(&mut self.terminal, transport);
                stats.content_changed = true;
            }
            if output.closed {
                if let Some(detector) = self.heuristic_detector.as_mut() {
                    let cursor = self.terminal.state().cursor_buffer_position();
                    for event in detector
                        .finish_session(BufferPosition::new(cursor.row, cursor.col), Instant::now())
                    {
                        self.semantic_timeline.apply_event(event);
                    }
                }
                self.connection_state = PaneConnectionState::Disconnected(if self.remote_session {
                    "SSH session disconnected".to_owned()
                } else {
                    "session exited".to_owned()
                });
                stats.content_changed = true;
                stats.closed = !self.disconnect_notified;
                self.disconnect_notified = true;
                break;
            }
        }
        stats
    }

    fn reconnect(&mut self, config: &AppConfig, metrics: CellMetrics) -> bool {
        self.shutdown();
        self.ssh_prompt = None;
        self.osc52_prompt = None;
        self.semantic_parser = SemanticEscapeParser::new();
        let transport_size = terminal_transport_size(self.last_size, metrics);
        match spawn_session_transport(
            config,
            &self.session_spec,
            transport_size,
            &self.output_waker,
        ) {
            Ok(initial) => {
                self.transport = Some(initial.transport);
                self.parse_semantic_events = initial.parse_semantic_events;
                self.semantic_timeline
                    .set_integration_mode(initial.semantic_mode);
                self.heuristic_detector = (initial.semantic_mode == IntegrationMode::Heuristic)
                    .then(HeuristicCommandDetector::default);
                if let Some(metadata) = initial.remote_metadata {
                    self.semantic_timeline.set_remote_session_metadata(metadata);
                }
                for diagnostic in initial.activation_diagnostics {
                    eprintln!("shell integration: {diagnostic}");
                }
                self.connection_state = if self.remote_session {
                    PaneConnectionState::Connecting
                } else {
                    PaneConnectionState::Connected
                };
                self.disconnect_notified = false;
                let message = if self.remote_session {
                    b"\r\n[Panea reconnecting SSH session...]\r\n".as_slice()
                } else {
                    b"\r\n[Panea restarting local session...]\r\n".as_slice()
                };
                let _ = self.terminal.apply_bytes(message);
                true
            }
            Err(error) => {
                self.connection_state = PaneConnectionState::Disconnected(error.to_string());
                let operation = if self.remote_session {
                    "SSH reconnect"
                } else {
                    "local session restart"
                };
                let _ = self
                    .terminal
                    .apply_bytes(format!("\r\n{operation} failed: {error}\r\n").as_bytes());
                false
            }
        }
    }

    fn shutdown(&mut self) {
        self.osc52_prompt = None;
        if let Some(detector) = self.heuristic_detector.as_mut() {
            let cursor = self.terminal.state().cursor_buffer_position();
            for event in
                detector.finish_session(BufferPosition::new(cursor.row, cursor.col), Instant::now())
            {
                self.semantic_timeline.apply_event(event);
            }
        }
        shutdown_transport(self.transport.as_mut());
    }

    fn scrollback_memory_bytes(&self) -> u64 {
        self.terminal
            .scrollback()
            .lines
            .iter()
            .flat_map(|line| line.cells.iter())
            .map(|cell| {
                let text_bytes = u64::try_from(cell.text.len()).unwrap_or(u64::MAX);
                text_bytes.saturating_add(16)
            })
            .sum()
    }
}

fn mark_session_status(model: &mut MuxModel, pane_id: PaneId, status: SessionStatus) {
    if let Ok(session) = model.session_for_pane_mut(pane_id) {
        session.status = status;
    }
}

fn session_spec_for_config(config: &AppConfig) -> SessionSpec {
    let profile = selected_shell_profile(config).map(resolved_shell_profile);
    let mut spec = SessionSpec::local(
        profile
            .as_ref()
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| "default".to_owned()),
    );
    if let Some(profile) = profile {
        spec.working_directory = profile.working_directory.clone();
        spec.startup_command = profile.startup_command.clone();
    }
    spec
}

fn local_session_spec(profile_name: &str) -> SessionSpec {
    SessionSpec::local(profile_name)
}

fn terminal_transport_size(size: TerminalGridSize, metrics: CellMetrics) -> TransportSize {
    TransportSize::new(
        size.cols,
        size.rows,
        (f32::from(size.cols) * metrics.cell_width).ceil() as u32,
        (f32::from(size.rows) * metrics.cell_height).ceil() as u32,
    )
}

fn resize_transport(transport: &mut PaneTransport, size: TransportSize) {
    match catch_unwind(AssertUnwindSafe(|| transport.resize(size))) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("transport resize error: {error}"),
        Err(panic) => eprintln!("transport resize panic boundary: {}", panic_payload(panic)),
    }
}

fn apply_terminal_bytes(terminal: &mut TerminalEmulator, bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    match catch_unwind(AssertUnwindSafe(|| terminal.apply_bytes(bytes))) {
        Ok(_) => true,
        Err(panic) => {
            eprintln!("terminal parser panic boundary: {}", panic_payload(panic));
            false
        }
    }
}

fn tab_bar_rows(model: &MuxModel, config: &AppConfig) -> u16 {
    if config.mux.show_tab_bar && model.active_workspace().active_window().tabs.len() > 1 {
        1
    } else {
        0
    }
}

#[derive(Debug, Clone, Copy)]
struct CursorPresentation {
    blink_visible: bool,
    window_focused: bool,
}

fn surface_size_is_renderable(size: winit::dpi::PhysicalSize<u32>) -> bool {
    size.width > 0 && size.height > 0
}

const TERMINAL_RESIZE_SETTLE: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, Copy, Default)]
struct PendingTerminalResize {
    size: Option<winit::dpi::PhysicalSize<u32>>,
    apply_at: Option<Instant>,
}

impl PendingTerminalResize {
    fn queue(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if !surface_size_is_renderable(size) {
            return;
        }
        self.size = Some(size);
        self.apply_at = Some(Instant::now() + TERMINAL_RESIZE_SETTLE);
    }

    fn deadline(self) -> Option<Instant> {
        self.apply_at
    }

    fn take_due(&mut self, now: Instant) -> Option<winit::dpi::PhysicalSize<u32>> {
        if self.apply_at.is_none_or(|deadline| now < deadline) {
            return None;
        }
        self.apply_at = None;
        self.size.take()
    }
}

fn scene_from_mux(
    runtime: &MuxRuntime,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    cursor_animator: Option<&mut CursorAnimationRuntime>,
    cursor_image_runtime: Option<&mut AnimatedCursorImageRuntime>,
    cursor_vector_runtime: Option<&mut CursorVectorRuntime>,
    cursor: CursorPresentation,
) -> RenderScene {
    let mut scene = RenderScene {
        grid: RenderGrid {
            columns: runtime.surface_cols,
            rows: runtime.surface_rows,
            cells: Vec::new(),
        },
        content_offset: RenderOffset {
            x: horizontal_content_inset(config).min(i32::MAX as u32) as i32,
            y: vertical_content_inset(config).min(i32::MAX as u32) as i32,
        },
        ..RenderScene::default()
    };
    let tab_bar_rows = tab_bar_rows(&runtime.model, config);

    if tab_bar_rows > 0 {
        append_tab_bar_cells(&mut scene, runtime, config);
    }

    let active_pane = runtime.model.active_tab().active_pane;
    for layout in runtime.active_layouts(config) {
        let Some(pane) = runtime.panes.get(&layout.pane_id) else {
            continue;
        };
        append_pane_scene(
            &mut scene,
            pane,
            layout,
            active_pane,
            metrics,
            config,
            cursor,
        );
    }

    if let Some(metrics) = metrics {
        append_pane_borders(&mut scene, runtime, tab_bar_rows, metrics, config);
        append_mux_drag_overlay(&mut scene, runtime, metrics, config);
        if let Some(cursor_animator) = cursor_animator {
            cursor_animator.populate_scene(&mut scene, metrics, cursor_animation_settings(config));
        }
        if let Some(cursor_image_runtime) = cursor_image_runtime {
            cursor_image_runtime.populate_scene(&mut scene, metrics);
        }
        if let Some(cursor_vector_runtime) = cursor_vector_runtime {
            cursor_vector_runtime.populate_scene(&mut scene, metrics);
        }
        append_active_ime_overlay(&mut scene, runtime, metrics);
        append_ssh_product_overlay(&mut scene, runtime, metrics);
    }

    scene
}

fn append_mux_drag_overlay(
    scene: &mut RenderScene,
    runtime: &MuxRuntime,
    metrics: CellMetrics,
    config: &AppConfig,
) {
    let Some(drag) = runtime.drag else {
        return;
    };
    let bounds = match drag {
        MuxDragState::Pane { target, .. } => runtime
            .active_layouts(config)
            .into_iter()
            .find(|layout| layout.pane_id == target)
            .map(|layout| rect_from_layout(layout.rect, metrics)),
        MuxDragState::Tab { target, .. } => {
            let workspace = runtime.model.active_workspace();
            let mut start = 0usize;
            workspace
                .active_window()
                .tabs
                .iter()
                .enumerate()
                .find_map(|(index, tab)| {
                    let width = formatted_tab_width(config, &workspace.name, index, tab);
                    let rect = (tab.id == target).then(|| RenderRect {
                        x: (start as f32 * metrics.cell_width).floor() as i32,
                        y: 0,
                        width: (width as f32 * metrics.cell_width).ceil() as u32,
                        height: metrics.cell_height.ceil() as u32,
                    });
                    start = start.saturating_add(width);
                    rect
                })
        }
    };
    let Some(bounds) = bounds else {
        return;
    };
    scene.semantic_overlays.push(OverlayPrimitive {
        kind: OverlayKind::DragTarget,
        bounds,
        color: RenderColor {
            red: 72,
            green: 142,
            blue: 230,
            alpha: 42,
        },
        border_color: Some(RenderColor {
            red: 112,
            green: 178,
            blue: 255,
            alpha: 245,
        }),
        border_width_px: 2,
        corner_radius_px: 3,
        z_index: 1600,
        label: None,
        label_color: None,
    });
}

fn formatted_tab_width(
    config: &AppConfig,
    workspace_name: &str,
    index: usize,
    tab: &mux::Tab,
) -> usize {
    config
        .mux
        .tab_title_format
        .replace("{index}", &(index + 1).to_string())
        .replace("{title}", &tab.name)
        .replace("{workspace}", workspace_name)
        .chars()
        .count()
        .saturating_add(2)
}

fn append_active_ime_overlay(scene: &mut RenderScene, runtime: &MuxRuntime, metrics: CellMetrics) {
    let Some(pane) = runtime.active_pane() else {
        return;
    };
    if pane.ime_preedit.is_empty() || pane.ssh_prompt.is_some() || pane.osc52_prompt.is_some() {
        return;
    }
    let Some(cursor) = scene.cursor else {
        return;
    };
    let width = ((pane.ime_preedit.chars().count().max(1) as f32 * metrics.cell_width).ceil()
        as u32)
        .saturating_add(8);
    scene.semantic_overlays.push(OverlayPrimitive {
        kind: OverlayKind::ImePreedit,
        bounds: RenderRect {
            x: scene.content_offset.x
                + (f32::from(cursor.position.col) * metrics.cell_width).floor() as i32,
            y: scene.content_offset.y
                + ((cursor.position.row + 1) as f32 * metrics.cell_height).floor() as i32,
            width,
            height: metrics.cell_height.ceil() as u32 + 4,
        },
        color: RenderColor {
            red: 24,
            green: 28,
            blue: 36,
            alpha: 245,
        },
        border_color: Some(RenderColor {
            red: 110,
            green: 170,
            blue: 255,
            alpha: 255,
        }),
        border_width_px: 1,
        corner_radius_px: 3,
        z_index: 1800,
        label: Some(pane.ime_preedit.clone()),
        label_color: None,
    });
}

fn append_ssh_product_overlay(scene: &mut RenderScene, runtime: &MuxRuntime, metrics: CellMetrics) {
    let Some(pane) = runtime.active_pane() else {
        return;
    };
    if let Some(prompt) = pane.osc52_prompt.as_ref() {
        append_centered_security_overlay(
            scene,
            osc52_prompt_lines(pane, prompt),
            metrics,
            RenderColor {
                red: 240,
                green: 96,
                blue: 96,
                alpha: 255,
            },
        );
        return;
    }
    if let Some(prompt) = pane.ssh_prompt.as_ref() {
        append_centered_security_overlay(
            scene,
            ssh_prompt_lines(prompt),
            metrics,
            RenderColor {
                red: 245,
                green: 185,
                blue: 72,
                alpha: 255,
            },
        );
        return;
    }

    let label = match &pane.connection_state {
        PaneConnectionState::Connecting => Some(format!(
            "Connecting SSH profile '{}'...",
            pane.session_spec.profile_name
        )),
        PaneConnectionState::Disconnected(message) if pane.remote_session => {
            Some(format!("SSH disconnected: {message}"))
        }
        PaneConnectionState::Connected | PaneConnectionState::Disconnected(_) => None,
    };
    let Some(label) = label else {
        return;
    };
    let width = ((label.chars().count() as f32 * metrics.cell_width).ceil() as u32)
        .saturating_add(16)
        .min((f32::from(scene.grid.columns) * metrics.cell_width).ceil() as u32);
    scene.semantic_overlays.push(OverlayPrimitive {
        kind: OverlayKind::SessionStatus,
        bounds: RenderRect {
            x: 8,
            y: ((f32::from(scene.grid.rows) * metrics.cell_height).ceil() as i32)
                .saturating_sub(metrics.cell_height.ceil() as i32)
                .saturating_sub(12),
            width,
            height: metrics.cell_height.ceil() as u32 + 6,
        },
        color: RenderColor {
            red: 22,
            green: 28,
            blue: 37,
            alpha: 240,
        },
        border_color: Some(RenderColor {
            red: 115,
            green: 134,
            blue: 160,
            alpha: 255,
        }),
        border_width_px: 1,
        corner_radius_px: 4,
        z_index: 1700,
        label: Some(label),
        label_color: None,
    });
}

fn append_centered_security_overlay(
    scene: &mut RenderScene,
    lines: Vec<String>,
    metrics: CellMetrics,
    accent: RenderColor,
) {
    let max_chars = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(20);
    let width = ((max_chars as f32 * metrics.cell_width).ceil() as u32)
        .saturating_add(24)
        .min((f32::from(scene.grid.columns) * metrics.cell_width).ceil() as u32);
    let line_height = metrics.cell_height.ceil() as u32 + 5;
    let total_height = line_height.saturating_mul(lines.len() as u32);
    let surface_width = (f32::from(scene.grid.columns) * metrics.cell_width).ceil() as i32;
    let surface_height = (f32::from(scene.grid.rows) * metrics.cell_height).ceil() as i32;
    let x = ((surface_width - width as i32) / 2).max(0);
    let mut y = ((surface_height - total_height as i32) / 2).max(0);
    for (index, line) in lines.into_iter().enumerate() {
        scene.semantic_overlays.push(OverlayPrimitive {
            kind: OverlayKind::SecurityPrompt,
            bounds: RenderRect {
                x,
                y,
                width,
                height: line_height,
            },
            color: RenderColor {
                red: 18,
                green: 22,
                blue: 29,
                alpha: 252,
            },
            border_color: (index == 0).then_some(accent),
            border_width_px: u8::from(index == 0),
            corner_radius_px: u8::from(index == 0) * 4,
            z_index: 2000,
            label: Some(line),
            label_color: None,
        });
        y = y.saturating_add(line_height as i32);
    }
}

fn osc52_prompt_lines(pane: &PaneRuntime, prompt: &Osc52PromptState) -> Vec<String> {
    let target = match prompt.request.target {
        Osc52ClipboardTarget::Clipboard | Osc52ClipboardTarget::Select => "system clipboard",
        Osc52ClipboardTarget::PrimarySelection => "primary selection",
        Osc52ClipboardTarget::Unknown(_) => "unknown target",
    };
    vec![
        "Remote clipboard write requested".to_owned(),
        format!("Session: {}", pane.session_spec.profile_name),
        format!("Target: {target}"),
        format!("Payload size: {} bytes", prompt.bytes),
        prompt.reason.clone(),
        "Y allow once   N/Esc deny".to_owned(),
    ]
}

fn ssh_prompt_lines(prompt: &SshPromptState) -> Vec<String> {
    match prompt {
        SshPromptState::HostTrust { request, .. } => {
            let mut lines = vec![
                match request.reason {
                    HostKeyTrustReason::UnknownHost => "Unknown SSH host".to_owned(),
                    HostKeyTrustReason::ChangedHostKey => {
                        "WARNING: SSH host key changed".to_owned()
                    }
                    HostKeyTrustReason::PinnedFingerprintMismatch => {
                        "BLOCKED: pinned SSH fingerprint mismatch".to_owned()
                    }
                },
                format!("Host: {}:{}", request.key.host, request.key.port),
                format!("Key: {}", request.key.algorithm),
                format!("Fingerprint: {}", request.key.sha256_fingerprint),
            ];
            if let Some(expected) = request.expected_fingerprint.as_deref() {
                lines.push(format!("Expected: {expected}"));
            }
            lines.push(match request.reason {
                HostKeyTrustReason::UnknownHost => {
                    "O trust once   S trust and store   Esc reject".to_owned()
                }
                HostKeyTrustReason::ChangedHostKey => {
                    "R replace stored key   Esc reject".to_owned()
                }
                HostKeyTrustReason::PinnedFingerprintMismatch => {
                    "Esc reject; update the pinned fingerprint in config to continue".to_owned()
                }
            });
            lines
        }
        SshPromptState::Secret {
            request,
            input,
            keychain,
            save_to_keychain,
            ..
        } => {
            let storage = if keychain.available {
                format!(
                    "Tab save to OS keychain: {}   Enter continue   Esc cancel",
                    if *save_to_keychain { "yes" } else { "no" }
                )
            } else {
                format!(
                    "OS keychain unavailable; secret stays transient ({})",
                    keychain.message
                )
            };
            vec![
                request.prompt_label(),
                format!("Secret: {}", "*".repeat(input.graphemes(true).count())),
                storage,
            ]
        }
    }
}

fn append_pane_scene(
    target: &mut RenderScene,
    pane: &PaneRuntime,
    layout: PaneLayout,
    active_pane: PaneId,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    cursor: CursorPresentation,
) {
    let mut pane_scene = scene_from_terminal(
        &pane.terminal,
        &pane.semantic_timeline,
        &pane.search,
        &pane.command_output_collapsed,
        metrics,
        config,
        cursor,
    );
    let row_offset = layout.rect.y.floor() as i64;
    let col_offset = layout.rect.x.floor() as u16;

    for mut cell in pane_scene.grid.cells.drain(..) {
        cell.position.row += row_offset;
        cell.position.col = cell.position.col.saturating_add(col_offset);
        target.grid.cells.push(cell);
    }

    for mut overlay in pane_scene.semantic_overlays.drain(..) {
        offset_rect(&mut overlay.bounds, row_offset, col_offset, metrics);
        target.semantic_overlays.push(overlay);
    }
    for mut overlay in pane_scene.search_highlights.drain(..) {
        offset_rect(&mut overlay.bounds, row_offset, col_offset, metrics);
        target.search_highlights.push(overlay);
    }
    for mut selection in pane_scene.selections.drain(..) {
        for position in &mut selection.cells {
            position.row += row_offset;
            position.col = position.col.saturating_add(col_offset);
        }
        target.selections.push(selection);
    }

    if layout.pane_id == active_pane
        && let Some(mut cursor) = pane_scene.cursor
    {
        cursor.position.row += row_offset;
        cursor.position.col = cursor.position.col.saturating_add(col_offset);
        target.cursor = Some(cursor);
    }
}

fn append_tab_bar_cells(scene: &mut RenderScene, runtime: &MuxRuntime, config: &AppConfig) {
    let window = runtime.model.active_workspace().active_window();
    let mut col = 0u16;
    for (index, tab) in window.tabs.iter().enumerate() {
        let active = tab.id == window.active_tab;
        let formatted = config
            .mux
            .tab_title_format
            .replace("{index}", &(index + 1).to_string())
            .replace("{title}", &tab.name)
            .replace("{workspace}", &runtime.model.active_workspace().name);
        let label = format!(" {formatted} ");
        for ch in label.chars() {
            if col >= runtime.surface_cols {
                return;
            }
            scene.grid.cells.push(RenderCell {
                position: CellPosition { row: 0, col },
                text: ch.to_string(),
                foreground: render_color(if active {
                    config.mux.appearance.active_tab_foreground
                } else {
                    config.mux.appearance.inactive_tab_foreground
                }),
                background: if active {
                    render_color(config.mux.appearance.active_tab_background)
                } else {
                    render_color(config.mux.appearance.inactive_tab_background)
                },
                style: RenderCellStyle {
                    bold: active,
                    ..RenderCellStyle::default()
                },
            });
            col = col.saturating_add(1);
        }
    }
    while col < runtime.surface_cols {
        scene.grid.cells.push(RenderCell {
            position: CellPosition { row: 0, col },
            text: " ".to_owned(),
            foreground: render_color(config.mux.appearance.inactive_tab_foreground),
            background: render_color(config.mux.appearance.tab_bar_background),
            style: RenderCellStyle::default(),
        });
        col = col.saturating_add(1);
    }
}

fn append_pane_borders(
    scene: &mut RenderScene,
    runtime: &MuxRuntime,
    tab_bar_rows: u16,
    metrics: CellMetrics,
    config: &AppConfig,
) {
    let active = runtime.model.active_tab().active_pane;
    let layouts = runtime.active_layouts(config);
    let show_borders = layouts.len() > 1 || tab_bar_rows > 0;
    for layout in layouts {
        let rect = rect_from_layout(layout.rect, metrics);
        let border = if layout.pane_id == active {
            render_color(config.mux.appearance.active_pane_border)
        } else {
            render_color(config.mux.appearance.inactive_pane_border)
        };
        if show_borders && config.mux.appearance.pane_border_width > 0 {
            for inset in 0..u32::from(config.mux.appearance.pane_border_width) {
                let double = inset.saturating_mul(2);
                if rect.width <= double || rect.height <= double {
                    break;
                }
                scene.decorations.push(render_core::RenderDecoration {
                    bounds: RenderRect {
                        x: rect.x.saturating_add(inset as i32),
                        y: rect.y.saturating_add(inset as i32),
                        width: rect.width - double,
                        height: rect.height - double,
                    },
                    color: RenderColor {
                        red: 0,
                        green: 0,
                        blue: 0,
                        alpha: 0,
                    },
                    border_color: Some(border),
                });
            }
        }
    }
}

fn append_performance_overlay(
    scene: &mut RenderScene,
    overlay: &PerformanceOverlay,
    ui: &PerformanceOverlayUiState,
    budget: PerformanceBudget,
    metrics: CellMetrics,
) {
    let Some((lines, metric_lines)) = performance_overlay_lines(overlay, ui, budget) else {
        return;
    };
    let layout = performance_overlay_layout(
        &lines,
        scene.grid.columns,
        scene.grid.rows,
        metrics,
        ui.position,
    );
    for (index, (line, bounds)) in lines.into_iter().zip(layout.rows).enumerate() {
        let max_chars =
            ((bounds.width.saturating_sub(14) as f32 / metrics.cell_width).floor() as usize).max(1);
        scene.semantic_overlays.push(OverlayPrimitive {
            kind: OverlayKind::PerformanceOverlay,
            bounds,
            color: if index >= metric_lines {
                RenderColor {
                    red: 24,
                    green: 31,
                    blue: 42,
                    alpha: 242,
                }
            } else {
                RenderColor {
                    red: 10,
                    green: 14,
                    blue: 20,
                    alpha: 224,
                }
            },
            border_color: Some(RenderColor {
                red: if index == 0 { 96 } else { 70 },
                green: if index == 0 { 172 } else { 82 },
                blue: if index == 0 { 238 } else { 98 },
                alpha: 210,
            }),
            border_width_px: 1,
            corner_radius_px: 4,
            z_index: 1000,
            label: Some(truncate_overlay_label(&line, max_chars)),
            label_color: None,
        });
    }
}

#[derive(Debug, Clone)]
struct PerformanceOverlayLayout {
    rows: Vec<RenderRect>,
}

fn performance_overlay_lines(
    overlay: &PerformanceOverlay,
    ui: &PerformanceOverlayUiState,
    budget: PerformanceBudget,
) -> Option<(Vec<String>, usize)> {
    let mut lines = overlay.render_lines(budget)?;
    let metric_lines = match ui.detail {
        PerformanceOverlayDetail::Compact => 2,
        PerformanceOverlayDetail::Detailed => 4,
    };
    lines.truncate(metric_lines);
    let metric_lines = lines.len();
    if ui.menu_open {
        lines.extend([
            format!("View  {}", performance_overlay_detail_name(ui.detail)),
            format!(
                "Position  {}",
                performance_overlay_position_name(ui.position)
            ),
            "Hide".to_owned(),
        ]);
    }
    Some((lines, metric_lines))
}

fn performance_overlay_layout(
    lines: &[String],
    cols: u16,
    rows: u16,
    metrics: CellMetrics,
    position: PerformanceOverlayPosition,
) -> PerformanceOverlayLayout {
    let surface_width = (f32::from(cols.max(1)) * metrics.cell_width).ceil() as u32;
    let surface_height = (f32::from(rows.max(1)) * metrics.cell_height).ceil() as u32;
    let max_chars = usize::from(cols.saturating_sub(4).clamp(12, 72));
    let content_chars = lines
        .iter()
        .map(|line| line.chars().count().min(max_chars))
        .max()
        .unwrap_or(12)
        .max(12);
    let padding = 7u32;
    let width = ((content_chars as f32 * metrics.cell_width).ceil() as u32)
        .saturating_add(padding * 2)
        .min(surface_width.saturating_sub(16).max(1));
    let row_height = metrics.cell_height.ceil().max(14.0) as u32 + 5;
    let gap = 3u32;
    let total_height = row_height
        .saturating_mul(lines.len() as u32)
        .saturating_add(gap.saturating_mul(lines.len().saturating_sub(1) as u32));
    let left = matches!(
        position,
        PerformanceOverlayPosition::TopLeft | PerformanceOverlayPosition::BottomLeft
    );
    let top = matches!(
        position,
        PerformanceOverlayPosition::TopLeft | PerformanceOverlayPosition::TopRight
    );
    let x = if left {
        8
    } else {
        surface_width.saturating_sub(width).saturating_sub(8) as i32
    };
    let start_y = if top {
        8
    } else {
        surface_height
            .saturating_sub(total_height)
            .saturating_sub(8) as i32
    };
    let rows = lines
        .iter()
        .enumerate()
        .map(|(index, _)| RenderRect {
            x,
            y: start_y.saturating_add((index as u32 * (row_height + gap)) as i32),
            width,
            height: row_height,
        })
        .collect();
    PerformanceOverlayLayout { rows }
}

#[allow(clippy::too_many_arguments)]
fn handle_performance_overlay_mouse(
    mouse: MouseEvent,
    overlay: &PerformanceOverlay,
    ui: &mut PerformanceOverlayUiState,
    budget: PerformanceBudget,
    metrics: CellMetrics,
    cols: u16,
    rows: u16,
    config: &AppConfig,
) -> bool {
    if !ui.enabled || !matches!(mouse.kind, MouseEventKind::Pressed(MouseButton::Left)) {
        return false;
    }
    let Some((lines, metric_lines)) = performance_overlay_lines(overlay, ui, budget) else {
        return false;
    };
    let layout = performance_overlay_layout(&lines, cols, rows, metrics, ui.position);
    let x = mouse.x - f64::from(horizontal_content_inset(config));
    let y = mouse.y - f64::from(vertical_content_inset(config));
    let Some(index) = layout
        .rows
        .iter()
        .position(|rect| point_in_rect(x, y, *rect))
    else {
        return false;
    };
    if index < metric_lines {
        ui.menu_open = !ui.menu_open;
    } else {
        match index - metric_lines {
            0 => ui.cycle_detail(),
            1 => ui.cycle_position(),
            2 => ui.hide(),
            _ => return false,
        }
    }
    true
}

fn point_in_rect(x: f64, y: f64, rect: RenderRect) -> bool {
    x >= f64::from(rect.x)
        && y >= f64::from(rect.y)
        && x < f64::from(rect.x) + f64::from(rect.width)
        && y < f64::from(rect.y) + f64::from(rect.height)
}

fn truncate_overlay_label(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }

    let keep = max_chars.saturating_sub(1);
    let mut output = text.chars().take(keep).collect::<String>();
    output.push('~');
    output
}

fn rect_from_layout(rect: LogicalRect, metrics: CellMetrics) -> RenderRect {
    RenderRect {
        x: (rect.x * metrics.cell_width).floor() as i32,
        y: (rect.y * metrics.cell_height).floor() as i32,
        width: (rect.width * metrics.cell_width).ceil() as u32,
        height: (rect.height * metrics.cell_height).ceil() as u32,
    }
}

fn offset_rect(
    rect: &mut RenderRect,
    row_offset: i64,
    col_offset: u16,
    metrics: Option<CellMetrics>,
) {
    if let Some(metrics) = metrics {
        rect.x += (f32::from(col_offset) * metrics.cell_width).floor() as i32;
        rect.y += (row_offset as f32 * metrics.cell_height).floor() as i32;
    }
}

fn scene_from_terminal(
    terminal: &TerminalEmulator,
    semantic_timeline: &SemanticTimelineStore,
    search: &PaneSearch,
    command_output_collapsed: &HashMap<u64, bool>,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    presentation: CursorPresentation,
) -> RenderScene {
    let visible = terminal.visible_grid();
    let cursor = terminal.cursor_state();
    let modes = terminal.modes();
    let configured_cursor_shape =
        resolved_cursor_shape(config, cursor.shape, &modes, presentation.window_focused);
    let cursor_visible = cursor.visible
        && terminal.state().viewport_offset() == 0
        && (!presentation.window_focused || presentation.blink_visible);
    let selection = terminal.selection_state();
    let mut cells = Vec::with_capacity(visible.cells.len());
    let cols = visible.viewport.size.cols;

    for (index, cell) in visible.cells.iter().enumerate() {
        let row = (index / usize::from(cols)) as i64;
        let col = (index % usize::from(cols)) as u16;
        let (mut foreground, background) = colors_for_attributes(cell.attributes, config);
        let position = CellPosition { row, col };
        if config.colors.selection_foreground.is_some_and(|_| {
            selection.is_some_and(|selection| {
                selection_contains(selection, visible.viewport.origin_row + row, col)
            })
        }) {
            foreground = render_color(
                config
                    .colors
                    .selection_foreground
                    .expect("selection foreground was checked"),
            );
        }
        if cursor_visible
            && row == cursor.position.row
            && col == cursor.position.col
            && matches!(
                configured_cursor_shape,
                RenderCursorShape::Block
                    | RenderCursorShape::Custom
                    | RenderCursorShape::CustomStaticShape
            )
            && let Some(cursor_text) = config.colors.cursor_text
        {
            foreground = render_color(cursor_text);
        }
        cells.push(RenderCell {
            position,
            text: cell.text.clone(),
            foreground,
            background,
            style: style_for_attributes(cell.attributes),
        });
    }

    let semantic_overlays = metrics.map_or_else(Vec::new, |metrics| {
        let mut overlays = url_hint_overlays(terminal, visible.viewport.size.rows, metrics);
        overlays.extend(semantic_visual_overlays(
            semantic_timeline,
            command_output_collapsed,
            terminal.modes().contains(&TerminalMode::AlternateScreen),
            SemanticOverlayViewport {
                origin_row: visible.viewport.origin_row,
                rows: visible.viewport.size.rows,
                cols: visible.viewport.size.cols,
                metrics,
            },
            config,
        ));
        overlays
    });
    let search_highlights = metrics.map_or_else(Vec::new, |metrics| {
        search_overlays(search, visible.viewport, metrics, config)
    });

    let selections = selection_visual(terminal, visible.viewport, config)
        .into_iter()
        .collect();
    RenderScene {
        grid: RenderGrid {
            columns: visible.viewport.size.cols,
            rows: visible.viewport.size.rows,
            cells,
        },
        cursor: Some(CursorVisual {
            position: CellPosition {
                row: cursor.position.row,
                col: cursor.position.col,
            },
            shape: configured_cursor_shape,
            color: render_color(if presentation.window_focused {
                config.cursor.color.unwrap_or(config.colors.cursor)
            } else {
                config
                    .cursor
                    .inactive_color
                    .or(config.cursor.color)
                    .unwrap_or(config.colors.cursor)
            }),
            visible: cursor_visible,
            thickness_percent: (config.cursor.thickness.clamp(0.05, 1.0) * 100.0).round() as u8,
            corner_radius_px: cursor_radius_px(config, metrics),
            inactive: !presentation.window_focused,
        }),
        semantic_overlays,
        search_highlights,
        selections,
        ..RenderScene::default()
    }
}

fn selection_contains(selection: Selection, row: i64, col: u16) -> bool {
    let (start, end) = if selection.start <= selection.end {
        (selection.start, selection.end)
    } else {
        (selection.end, selection.start)
    };
    if row < start.row || row > end.row {
        return false;
    }
    match selection.kind {
        SelectionKind::Rectangular => {
            col >= start.col.min(end.col) && col <= start.col.max(end.col)
        }
        SelectionKind::Normal if start.row == end.row => col >= start.col && col <= end.col,
        SelectionKind::Normal if row == start.row => col >= start.col,
        SelectionKind::Normal if row == end.row => col <= end.col,
        SelectionKind::Normal => true,
    }
}

fn resolved_cursor_shape(
    config: &AppConfig,
    terminal_shape: CursorShape,
    modes: &BTreeSet<TerminalMode>,
    focused: bool,
) -> RenderCursorShape {
    if !focused {
        return render_cursor_shape(config.cursor.inactive_shape);
    }
    let mode = if modes.contains(&TerminalMode::AlternateScreen) {
        Some("alternate_screen")
    } else if modes.contains(&TerminalMode::Insert) {
        Some("insert")
    } else if modes.contains(&TerminalMode::ApplicationCursorKeys) {
        Some("application_cursor")
    } else if modes.contains(&TerminalMode::ApplicationKeypad) {
        Some("application_keypad")
    } else {
        Some("normal")
    };
    if let Some(shape) = mode.and_then(|mode| {
        config
            .cursor
            .mode_specific_styles
            .iter()
            .find(|(configured, _)| configured.eq_ignore_ascii_case(mode))
            .map(|(_, shape)| shape)
    }) {
        return render_cursor_shape(*shape);
    }
    match terminal_shape {
        CursorShape::Beam => RenderCursorShape::Beam,
        CursorShape::Underline => RenderCursorShape::Underline,
        CursorShape::Block => render_cursor_shape(config.cursor.shape),
    }
}

fn search_overlays(
    search: &PaneSearch,
    viewport: term_core::Viewport,
    metrics: CellMetrics,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    let mut overlays = Vec::new();

    for (index, selection) in search.matches.iter().enumerate() {
        let (start, end) = if selection.start <= selection.end {
            (selection.start, selection.end)
        } else {
            (selection.end, selection.start)
        };
        for visible_row in 0..viewport.size.rows {
            let row = viewport.origin_row + i64::from(visible_row);
            if row < start.row || row > end.row {
                continue;
            }
            let start_col = if row == start.row { start.col } else { 0 };
            let end_col = if row == end.row {
                end.col
            } else {
                viewport.size.cols.saturating_sub(1)
            };
            overlays.push(OverlayPrimitive {
                kind: OverlayKind::SearchHighlight,
                bounds: RenderRect {
                    x: (f32::from(start_col) * metrics.cell_width).floor() as i32,
                    y: (f32::from(visible_row) * metrics.cell_height).floor() as i32,
                    width: (f32::from(end_col.saturating_sub(start_col).saturating_add(1))
                        * metrics.cell_width)
                        .ceil() as u32,
                    height: metrics.cell_height.ceil() as u32,
                },
                color: if index == search.active_match {
                    render_color(config.colors.selection_background)
                } else {
                    RenderColor {
                        red: 240,
                        green: 190,
                        blue: 50,
                        alpha: 90,
                    }
                },
                border_color: None,
                border_width_px: 0,
                corner_radius_px: 1,
                z_index: 12,
                label: None,
                label_color: None,
            });
        }
    }

    if search.input_active {
        let status = if search.matches.is_empty() {
            "0/0".to_owned()
        } else {
            format!("{}/{}", search.active_match + 1, search.matches.len())
        };
        let panel_cols = usize::from(viewport.size.cols).clamp(1, 42);
        let label = truncate_overlay_label(
            &format!("Find: {}  {status}", search.query),
            panel_cols.saturating_sub(2).max(1),
        );
        overlays.push(OverlayPrimitive {
            kind: OverlayKind::SearchHighlight,
            bounds: RenderRect {
                x: 6,
                y: 6,
                width: (metrics.cell_width * panel_cols as f32).ceil() as u32,
                height: (metrics.cell_height + 8.0).ceil() as u32,
            },
            color: RenderColor {
                red: 20,
                green: 24,
                blue: 30,
                alpha: 235,
            },
            border_color: Some(render_color(config.colors.selection_background)),
            border_width_px: 1,
            corner_radius_px: 4,
            z_index: 100,
            label: Some(label),
            label_color: None,
        });
    }
    overlays
}

fn selection_visual(
    terminal: &TerminalEmulator,
    viewport: term_core::Viewport,
    config: &AppConfig,
) -> Option<SelectionVisual> {
    let selection = terminal.selection_state()?;
    let (start, end) = if selection.start <= selection.end {
        (selection.start, selection.end)
    } else {
        (selection.end, selection.start)
    };
    let mut cells = Vec::new();
    for visible_row in 0..viewport.size.rows {
        let absolute_row = viewport.origin_row + i64::from(visible_row);
        if absolute_row < start.row || absolute_row > end.row {
            continue;
        }
        for col in 0..viewport.size.cols {
            let selected = match selection.kind {
                SelectionKind::Rectangular => {
                    col >= start.col.min(end.col) && col <= start.col.max(end.col)
                }
                SelectionKind::Normal if start.row == end.row => col >= start.col && col <= end.col,
                SelectionKind::Normal if absolute_row == start.row => col >= start.col,
                SelectionKind::Normal if absolute_row == end.row => col <= end.col,
                SelectionKind::Normal => true,
            };
            if selected {
                cells.push(CellPosition {
                    row: i64::from(visible_row),
                    col,
                });
            }
        }
    }

    (!cells.is_empty()).then_some(SelectionVisual {
        cells,
        color: render_color(config.colors.selection_background),
    })
}

fn render_cursor_shape(shape: config_core::CursorShape) -> RenderCursorShape {
    match shape {
        config_core::CursorShape::Block => RenderCursorShape::Block,
        config_core::CursorShape::Beam => RenderCursorShape::Beam,
        config_core::CursorShape::Underline => RenderCursorShape::Underline,
        config_core::CursorShape::HollowBlock => RenderCursorShape::HollowBlock,
        config_core::CursorShape::Custom => RenderCursorShape::Custom,
        config_core::CursorShape::CustomStaticShape => RenderCursorShape::CustomStaticShape,
    }
}

fn cursor_radius_px(config: &AppConfig, metrics: Option<CellMetrics>) -> u8 {
    let cell_edge = metrics.map_or(16.0, |metrics| metrics.cell_width.min(metrics.cell_height));
    let radius = config.cursor.corner_radius.clamp(0.0, 0.5) * f64::from(cell_edge);
    radius.round() as u8
}

fn url_hint_overlays(
    terminal: &TerminalEmulator,
    rows: u16,
    metrics: CellMetrics,
) -> Vec<OverlayPrimitive> {
    (0..rows)
        .flat_map(|row| visible_url_hints(terminal, row))
        .map(|hint| OverlayPrimitive {
            kind: OverlayKind::Semantic,
            bounds: RenderRect {
                x: (f32::from(hint.start.col) * metrics.cell_width).floor() as i32,
                y: (hint.start.row.max(0) as f32 * metrics.cell_height).floor() as i32,
                width: (f32::from(hint.end.col.saturating_sub(hint.start.col)) * metrics.cell_width)
                    .ceil() as u32,
                height: metrics.cell_height.ceil() as u32,
            },
            color: RenderColor {
                red: 80,
                green: 150,
                blue: 255,
                alpha: 64,
            },
            border_color: None,
            border_width_px: 0,
            corner_radius_px: 2,
            z_index: 10,
            label: Some(hint.text),
            label_color: None,
        })
        .collect()
}

fn visible_url_hints(terminal: &TerminalEmulator, row: u16) -> Vec<semantics::DetectedHint> {
    let Some(line) = terminal.state().visible_line(row) else {
        return Vec::new();
    };
    let text = line.raw_text();
    let mut hints = detect_url_hints([(i64::from(row), text.as_str())]);
    for hint in &mut hints {
        hint.start.col = line_column_for_char_offset(line, usize::from(hint.start.col));
        hint.end.col = line_column_for_char_offset(line, usize::from(hint.end.col));
    }
    hints
}

fn line_column_for_char_offset(line: &term_core::Line, offset: usize) -> u16 {
    let mut chars = 0usize;
    let mut col = 0u16;
    for cell in &line.cells {
        if cell.wide_continuation {
            continue;
        }
        if offset <= chars {
            return col;
        }
        let next_chars = chars.saturating_add(cell.text.chars().count());
        if offset < next_chars {
            return col;
        }
        chars = next_chars;
        col = col.saturating_add(u16::from(cell.width.max(1)));
    }
    col
}

#[derive(Debug, Clone, Copy)]
struct SemanticOverlayViewport {
    origin_row: i64,
    rows: u16,
    cols: u16,
    metrics: CellMetrics,
}

fn semantic_visual_overlays(
    semantic_timeline: &SemanticTimelineStore,
    command_output_collapsed: &HashMap<u64, bool>,
    alternate_screen_active: bool,
    viewport: SemanticOverlayViewport,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    let mut overlays = Vec::new();

    if config.prompt_decorations.enabled
        && (!alternate_screen_active || config.prompt_decorations.allow_in_alternate_screen)
    {
        overlays.extend(prompt_decoration_overlays(
            semantic_timeline,
            viewport,
            config,
        ));
    }
    if config.command_blocks.enabled
        && (!alternate_screen_active || config.command_blocks.allow_in_alternate_screen)
    {
        overlays.extend(command_block_overlays(
            semantic_timeline,
            command_output_collapsed,
            viewport,
            config,
        ));
    }

    overlays
}

fn prompt_decoration_overlays(
    semantic_timeline: &SemanticTimelineStore,
    viewport: SemanticOverlayViewport,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    semantic_timeline
        .regions()
        .iter()
        .filter(|region| region.kind == SemanticRegionKind::Prompt)
        .filter_map(|region| {
            let span = region.span()?;
            let raw_bounds = row_overlay_bounds(span.start.row, span.end.row, viewport)?;
            let previous_status = semantic_timeline
                .previous_command(span.start)
                .map(|block| &block.status);
            let status_color = previous_status
                .map(|status| command_status_color(status, config))
                .unwrap_or_else(|| render_color(config.visual_theme.borders.color));
            let bounds = prompt_decoration_bounds(raw_bounds, viewport.metrics, config);
            let color = if config.prompt_decorations.style
                == PromptDecorationStyle::MinimalSeparator
                && config.prompt_decorations.show_previous_status_accent
                && previous_status.is_some()
            {
                status_color
            } else {
                prompt_decoration_color(config)
            };
            Some(OverlayPrimitive {
                kind: OverlayKind::PromptDecoration,
                bounds,
                color,
                border_color: match config.prompt_decorations.style {
                    PromptDecorationStyle::MinimalSeparator => None,
                    PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => {
                        Some(if config.prompt_decorations.show_previous_status_accent {
                            status_color
                        } else {
                            render_color(config.visual_theme.borders.color)
                        })
                    }
                },
                border_width_px: config.visual_theme.borders.width_px,
                corner_radius_px: match config.prompt_decorations.style {
                    PromptDecorationStyle::MinimalSeparator => 0,
                    PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => {
                        config.visual_theme.borders.radius_px
                    }
                },
                z_index: 20,
                label: prompt_badge_label(&region.metadata, previous_status, config),
                label_color: Some(render_color(config.visual_theme.badge_foreground)),
            })
        })
        .collect()
}

fn prompt_decoration_bounds(
    bounds: RenderRect,
    metrics: CellMetrics,
    config: &AppConfig,
) -> RenderRect {
    let mut bounds = inset_overlay_bounds(bounds, prompt_overlay_padding(config));
    match config.prompt_decorations.style {
        PromptDecorationStyle::MinimalSeparator => {
            let height = u32::from(config.visual_theme.borders.width_px.max(1));
            bounds.y = bounds
                .y
                .saturating_add(bounds.height.saturating_sub(height) as i32);
            bounds.height = height;
        }
        PromptDecorationStyle::RoundedBox => {}
        PromptDecorationStyle::PillHeader => {
            bounds.height = (metrics.cell_height * 0.86).ceil().max(12.0) as u32;
        }
    }
    bounds
}

fn command_block_overlays(
    semantic_timeline: &SemanticTimelineStore,
    command_output_collapsed: &HashMap<u64, bool>,
    viewport: SemanticOverlayViewport,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    let mut overlays = Vec::new();

    if config.command_blocks.style == CommandBlockStyle::Traditional {
        return overlays;
    }

    for block in semantic_timeline.command_blocks() {
        let Some(span) = semantic_timeline.command_span(block) else {
            continue;
        };
        let Some(raw_bounds) = row_overlay_bounds(span.start.row, span.end.row, viewport) else {
            continue;
        };

        let bounds = margin_overlay_bounds(raw_bounds, config.visual_theme.spacing.block_margin_px);
        let decoration_bounds = command_block_bounds(bounds, viewport.metrics, config);
        let metadata = semantic_timeline
            .command_metadata(block)
            .unwrap_or_else(|| semantic_timeline.metadata());
        let status_color = command_status_color(&block.status, config);
        overlays.push(OverlayPrimitive {
            kind: OverlayKind::CommandBlock,
            bounds: decoration_bounds,
            color: command_block_fill(config),
            border_color: Some(status_color),
            border_width_px: config.visual_theme.borders.width_px,
            corner_radius_px: command_block_corner_radius(config),
            z_index: 15,
            label: None,
            label_color: None,
        });

        if config.command_blocks.separate_prompt_input_output {
            append_input_output_group_overlays(
                &mut overlays,
                semantic_timeline,
                [
                    (block.input_region_id, "input"),
                    (block.output_region_id, "output"),
                ],
                viewport,
                config,
            );
        }

        append_command_badges(
            &mut overlays,
            bounds,
            block,
            metadata,
            status_color,
            viewport.metrics,
            config,
        );

        append_collapsed_output_overlay(
            &mut overlays,
            semantic_timeline,
            block,
            command_output_collapsed,
            viewport,
            config,
        );
    }

    overlays
}

fn append_collapsed_output_overlay(
    overlays: &mut Vec<OverlayPrimitive>,
    timeline: &SemanticTimelineStore,
    block: &semantics::CommandBlock,
    collapsed_overrides: &HashMap<u64, bool>,
    viewport: SemanticOverlayViewport,
    config: &AppConfig,
) {
    let Some(span) = timeline.output_span_for_command(block) else {
        return;
    };
    let output_rows = semantic_span_rows(span);
    let auto_collapsed = config.command_blocks.collapse_long_output
        && output_rows > u32::from(config.command_blocks.collapse_after_lines);
    if !collapsed_overrides
        .get(&block.region_id)
        .copied()
        .unwrap_or(auto_collapsed)
    {
        return;
    }

    let preview_rows = i64::from(config.command_blocks.collapsed_preview_lines);
    let hidden_start = span.start.row.saturating_add(preview_rows);
    if hidden_start >= span.end.row {
        return;
    }
    let Some(bounds) = row_overlay_bounds(hidden_start, span.end.row, viewport) else {
        return;
    };
    let hidden_rows = span.end.row.saturating_sub(hidden_start);
    overlays.push(OverlayPrimitive {
        kind: OverlayKind::ContentMask,
        bounds: inset_overlay_bounds(bounds, command_block_padding(config)),
        color: RenderColor {
            alpha: u8::MAX,
            ..render_color(config.colors.background)
        },
        border_color: Some(render_color(config.visual_theme.borders.color)),
        border_width_px: config.visual_theme.borders.width_px,
        corner_radius_px: command_block_corner_radius(config),
        z_index: 40,
        label: Some(format!("{hidden_rows} output lines collapsed")),
        label_color: Some(render_color(config.visual_theme.badge_foreground)),
    });
}

fn semantic_span_rows(span: SemanticSpan) -> u32 {
    u32::try_from(span.end.row.saturating_sub(span.start.row).max(0)).unwrap_or(u32::MAX)
}

fn append_input_output_group_overlays(
    overlays: &mut Vec<OverlayPrimitive>,
    semantic_timeline: &SemanticTimelineStore,
    regions: [(Option<u64>, &'static str); 2],
    viewport: SemanticOverlayViewport,
    config: &AppConfig,
) {
    if config.visual_theme.grouping_style == InputOutputGroupingStyle::Traditional {
        return;
    }
    for (region_id, label) in regions {
        let Some(region_id) = region_id else {
            continue;
        };
        let Some(span) = semantic_timeline
            .regions()
            .iter()
            .find(|region| region.id == region_id)
            .and_then(|region| region.span())
        else {
            continue;
        };
        let Some(bounds) = row_overlay_bounds(span.start.row, span.end.row, viewport) else {
            continue;
        };
        let bounds = input_output_group_bounds(
            inset_overlay_bounds(bounds, input_output_group_padding(config)),
            viewport.metrics,
            config,
        );
        overlays.push(OverlayPrimitive {
            kind: OverlayKind::InputOutputGroup,
            bounds,
            color: input_output_group_color(label, config),
            border_color: None,
            border_width_px: 0,
            corner_radius_px: input_output_group_radius(config),
            z_index: 16,
            label: matches!(
                config.visual_theme.grouping_style,
                InputOutputGroupingStyle::MinimalHeaders
            )
            .then(|| label.to_owned()),
            label_color: None,
        });
    }
}

fn append_command_badges(
    overlays: &mut Vec<OverlayPrimitive>,
    bounds: RenderRect,
    block: &semantics::CommandBlock,
    metadata: &SemanticMetadata,
    status_color: RenderColor,
    metrics: CellMetrics,
    config: &AppConfig,
) {
    let labels = command_badge_labels(block, metadata, config);
    if labels.is_empty() {
        return;
    }

    let gap = i32::from(config.visual_theme.spacing.badge_gap_px.max(2));
    let badge_height = (metrics.cell_height * 0.72).ceil().max(12.0) as u32;
    let padding = i32::from(config.visual_theme.spacing.block_padding_px.max(3));
    let mut right = bounds.x + bounds.width as i32 - padding;
    let y = bounds.y + padding.min(bounds.height.saturating_sub(badge_height) as i32);

    for label in labels.into_iter().rev() {
        let width = badge_width(&label, metrics, config);
        if width == 0 || right - width as i32 <= bounds.x {
            continue;
        }
        right -= width as i32;
        overlays.push(OverlayPrimitive {
            kind: OverlayKind::Badge,
            bounds: RenderRect {
                x: right,
                y,
                width,
                height: badge_height,
            },
            color: badge_color(&label, status_color, config),
            border_color: None,
            border_width_px: 0,
            corner_radius_px: config.visual_theme.borders.radius_px.min(8),
            z_index: 35,
            label: Some(label),
            label_color: Some(render_color(config.visual_theme.badge_foreground)),
        });
        right -= gap;
    }
}

fn row_overlay_bounds(
    start_row: i64,
    end_row: i64,
    viewport: SemanticOverlayViewport,
) -> Option<RenderRect> {
    let start_row = start_row.saturating_sub(viewport.origin_row);
    let end_row = end_row.saturating_sub(viewport.origin_row);
    if end_row < 0 || start_row >= i64::from(viewport.rows) {
        return None;
    }

    let start = start_row.max(0);
    let end = end_row.max(start + 1).min(i64::from(viewport.rows));
    Some(RenderRect {
        x: 0,
        y: (start as f32 * viewport.metrics.cell_height).floor() as i32,
        width: (f32::from(viewport.cols) * viewport.metrics.cell_width).ceil() as u32,
        height: ((end - start) as f32 * viewport.metrics.cell_height).ceil() as u32,
    })
}

fn inset_overlay_bounds(bounds: RenderRect, padding_px: u8) -> RenderRect {
    let padding = u32::from(padding_px);
    if padding == 0 || bounds.width <= padding.saturating_mul(2) {
        return bounds;
    }

    RenderRect {
        x: bounds.x + padding as i32,
        y: bounds.y,
        width: bounds.width.saturating_sub(padding.saturating_mul(2)),
        height: bounds.height,
    }
}

fn margin_overlay_bounds(bounds: RenderRect, margin_px: u8) -> RenderRect {
    let margin = u32::from(margin_px);
    if margin == 0 {
        return bounds;
    }
    let horizontal = margin.saturating_mul(2).min(bounds.width);
    let vertical = margin.saturating_mul(2).min(bounds.height);
    RenderRect {
        x: bounds.x.saturating_add(margin as i32),
        y: bounds.y.saturating_add(margin as i32),
        width: bounds.width.saturating_sub(horizontal),
        height: bounds.height.saturating_sub(vertical),
    }
}

fn command_block_bounds(
    mut bounds: RenderRect,
    metrics: CellMetrics,
    config: &AppConfig,
) -> RenderRect {
    match config.command_blocks.style {
        CommandBlockStyle::Traditional
        | CommandBlockStyle::Card
        | CommandBlockStyle::Split
        | CommandBlockStyle::CustomTheme => bounds,
        CommandBlockStyle::Subtle => {
            let height = u32::from(config.visual_theme.borders.width_px.max(1));
            bounds.y = bounds
                .y
                .saturating_add(bounds.height.saturating_sub(height) as i32);
            bounds.height = height;
            bounds
        }
        CommandBlockStyle::MinimalHeader => {
            bounds.height = metrics.cell_height.ceil().max(1.0) as u32;
            bounds
        }
    }
}

fn input_output_group_bounds(
    mut bounds: RenderRect,
    metrics: CellMetrics,
    config: &AppConfig,
) -> RenderRect {
    match config.visual_theme.grouping_style {
        InputOutputGroupingStyle::Traditional
        | InputOutputGroupingStyle::CommandCards
        | InputOutputGroupingStyle::InputOutputSplit
        | InputOutputGroupingStyle::CustomTheme => bounds,
        InputOutputGroupingStyle::SubtleSeparators => {
            bounds.height = u32::from(config.visual_theme.borders.width_px.max(1));
            bounds
        }
        InputOutputGroupingStyle::MinimalHeaders => {
            bounds.height = metrics.cell_height.ceil().max(1.0) as u32;
            bounds
        }
    }
}

fn prompt_overlay_padding(config: &AppConfig) -> u8 {
    match config.prompt_decorations.style {
        PromptDecorationStyle::MinimalSeparator => 0,
        PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => {
            config.visual_theme.spacing.block_padding_px / 2
        }
    }
}

fn command_block_padding(config: &AppConfig) -> u8 {
    match config.command_blocks.style {
        CommandBlockStyle::Traditional => 0,
        CommandBlockStyle::Subtle => config.visual_theme.spacing.block_padding_px / 2,
        CommandBlockStyle::Card
        | CommandBlockStyle::Split
        | CommandBlockStyle::MinimalHeader
        | CommandBlockStyle::CustomTheme => config.visual_theme.spacing.block_padding_px,
    }
}

fn input_output_group_padding(config: &AppConfig) -> u8 {
    match config.visual_theme.grouping_style {
        InputOutputGroupingStyle::Traditional => 0,
        InputOutputGroupingStyle::SubtleSeparators | InputOutputGroupingStyle::MinimalHeaders => {
            config.visual_theme.spacing.block_padding_px / 2
        }
        InputOutputGroupingStyle::CommandCards
        | InputOutputGroupingStyle::InputOutputSplit
        | InputOutputGroupingStyle::CustomTheme => config.visual_theme.spacing.block_padding_px,
    }
}

fn command_block_corner_radius(config: &AppConfig) -> u8 {
    match config.command_blocks.style {
        CommandBlockStyle::Traditional => 0,
        CommandBlockStyle::Subtle | CommandBlockStyle::MinimalHeader => 2,
        CommandBlockStyle::Card | CommandBlockStyle::Split | CommandBlockStyle::CustomTheme => {
            config.visual_theme.borders.radius_px
        }
    }
}

fn input_output_group_radius(config: &AppConfig) -> u8 {
    match config.visual_theme.grouping_style {
        InputOutputGroupingStyle::Traditional
        | InputOutputGroupingStyle::SubtleSeparators
        | InputOutputGroupingStyle::MinimalHeaders => 0,
        InputOutputGroupingStyle::CommandCards
        | InputOutputGroupingStyle::InputOutputSplit
        | InputOutputGroupingStyle::CustomTheme => config.visual_theme.borders.radius_px / 2,
    }
}

fn prompt_decoration_color(config: &AppConfig) -> RenderColor {
    match config.prompt_decorations.style {
        PromptDecorationStyle::MinimalSeparator => RenderColor {
            alpha: config.visual_theme.borders.color.alpha.max(96),
            ..render_color(config.visual_theme.borders.color)
        },
        PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => {
            render_color(config.visual_theme.prompt_background)
        }
    }
}

fn command_block_fill(config: &AppConfig) -> RenderColor {
    match config.command_blocks.style {
        CommandBlockStyle::Traditional => RenderColor {
            alpha: 0,
            ..render_color(config.colors.background)
        },
        CommandBlockStyle::Subtle => RenderColor {
            alpha: 20,
            ..render_color(config.visual_theme.command_background)
        },
        CommandBlockStyle::Card
        | CommandBlockStyle::Split
        | CommandBlockStyle::MinimalHeader
        | CommandBlockStyle::CustomTheme => render_color(config.visual_theme.command_background),
    }
}

fn input_output_group_color(label: &str, config: &AppConfig) -> RenderColor {
    let alpha = match config.visual_theme.grouping_style {
        InputOutputGroupingStyle::Traditional => 0,
        InputOutputGroupingStyle::SubtleSeparators => 18,
        InputOutputGroupingStyle::CommandCards => 34,
        InputOutputGroupingStyle::InputOutputSplit => 28,
        InputOutputGroupingStyle::MinimalHeaders => 22,
        InputOutputGroupingStyle::CustomTheme => 36,
    };
    match label {
        "input" => RenderColor {
            alpha,
            ..render_color(config.visual_theme.input_background)
        },
        _ => RenderColor {
            alpha,
            ..render_color(config.visual_theme.output_background)
        },
    }
}

fn prompt_badge_label(
    metadata: &SemanticMetadata,
    previous_status: Option<&CommandStatus>,
    config: &AppConfig,
) -> Option<String> {
    if config.prompt_decorations.style == PromptDecorationStyle::MinimalSeparator {
        return None;
    }
    let mut badges = Vec::new();
    if (config.prompt_decorations.show_shell_badge || config.visual_theme.badges.shell)
        && let Some(shell) = metadata.shell.shell.as_deref()
    {
        badges.push(truncate_badge_text(shell, 20));
    }
    if (config.prompt_decorations.show_current_directory
        || config.visual_theme.badges.current_directory)
        && let Some(cwd) = metadata
            .remote
            .as_ref()
            .and_then(|remote| remote.remote_current_working_directory.as_deref())
            .or(metadata.shell.current_working_directory.as_deref())
    {
        badges.push(compact_path_label(cwd));
    }
    if (config.prompt_decorations.show_remote_host || config.visual_theme.badges.remote)
        && let Some(remote) = metadata.remote.as_ref()
        && let Some(host) = remote.remote_host.as_deref()
    {
        badges.push(remote.remote_user.as_ref().map_or_else(
            || truncate_badge_text(host, 28),
            |user| truncate_badge_text(&format!("{user}@{host}"), 28),
        ));
    }
    if (config.prompt_decorations.show_admin_badge || config.visual_theme.badges.admin)
        && semantic_attribute_is_true(metadata, "elevated")
    {
        badges.push("admin".to_owned());
    }
    if config.prompt_decorations.show_previous_status_accent
        && let Some(status) = previous_status.and_then(command_status_label)
    {
        badges.push(status);
    }
    (!badges.is_empty()).then(|| truncate_badge_text(&badges.join(" "), 72))
}

fn semantic_attribute_is_true(metadata: &SemanticMetadata, key: &str) -> bool {
    metadata.attributes.iter().any(|(candidate, value)| {
        candidate.eq_ignore_ascii_case(key)
            && matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes")
    })
}

fn command_status_color(status: &CommandStatus, config: &AppConfig) -> RenderColor {
    match status {
        CommandStatus::Code(0) => render_color(config.visual_theme.success_color),
        CommandStatus::Code(_) | CommandStatus::Signal(_) => {
            render_color(config.visual_theme.error_color)
        }
        CommandStatus::Running | CommandStatus::Unknown => {
            render_color(config.visual_theme.borders.color)
        }
    }
}

fn command_badge_labels(
    block: &semantics::CommandBlock,
    metadata: &SemanticMetadata,
    config: &AppConfig,
) -> Vec<String> {
    let mut labels = Vec::new();
    if (config.command_blocks.show_exit_status || config.visual_theme.badges.status)
        && let Some(label) = command_status_label(&block.status)
    {
        labels.push(label);
    }
    if config.command_blocks.show_duration
        && let Some(duration) = block.duration
    {
        labels.push(format_duration_badge(duration));
    }
    if (config.command_blocks.show_current_directory
        || config.visual_theme.badges.current_directory)
        && let Some(cwd) = metadata
            .shell
            .current_working_directory
            .as_ref()
            .or_else(|| {
                metadata
                    .remote
                    .as_ref()
                    .and_then(|remote| remote.remote_current_working_directory.as_ref())
            })
    {
        labels.push(format!("cwd {}", compact_path_label(cwd)));
    }
    if (config.command_blocks.show_shell_host
        || config.visual_theme.badges.shell
        || config.visual_theme.badges.remote)
        && let Some(label) = shell_host_badge_label(metadata)
    {
        labels.push(label);
    }
    if config.visual_theme.badges.admin && semantic_attribute_is_true(metadata, "elevated") {
        labels.push("admin".to_owned());
    }
    labels
}

fn command_status_label(status: &CommandStatus) -> Option<String> {
    match status {
        CommandStatus::Code(0) => Some("ok".to_owned()),
        CommandStatus::Code(status) => Some(format!("exit {status}")),
        CommandStatus::Signal(signal) => Some(format!("signal {signal}")),
        CommandStatus::Running | CommandStatus::Unknown => None,
    }
}

fn format_duration_badge(duration: Duration) -> String {
    if duration.as_millis() < 1_000 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{:.1}s", duration.as_secs_f32())
    }
}

fn compact_path_label(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let Some(last) = trimmed.rsplit(['/', '\\']).find(|part| !part.is_empty()) else {
        return path.to_owned();
    };
    truncate_badge_text(last, 20)
}

fn shell_host_badge_label(metadata: &SemanticMetadata) -> Option<String> {
    if let Some(remote) = &metadata.remote
        && let Some(host) = &remote.remote_host
    {
        if let Some(user) = &remote.remote_user {
            return Some(truncate_badge_text(&format!("{user}@{host}"), 28));
        }
        return Some(truncate_badge_text(host, 28));
    }
    metadata
        .shell
        .shell
        .as_ref()
        .map(|shell| truncate_badge_text(shell, 20))
}

fn truncate_badge_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut out = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        out.push('~');
    }
    out
}

fn badge_width(label: &str, metrics: CellMetrics, config: &AppConfig) -> u32 {
    let text_width = label.chars().count() as f32 * metrics.cell_width * 0.62;
    let padding = f32::from(config.visual_theme.spacing.badge_gap_px.max(4)) * 2.0;
    (text_width + padding).ceil().max(16.0) as u32
}

fn badge_color(label: &str, status_color: RenderColor, config: &AppConfig) -> RenderColor {
    if label == "ok" || label.starts_with("exit ") || label.starts_with("signal ") {
        return RenderColor {
            alpha: 148,
            ..status_color
        };
    }
    render_color(config.visual_theme.badge_background)
}

fn style_for_attributes(attributes: CellAttributes) -> RenderCellStyle {
    RenderCellStyle {
        bold: attributes.bold,
        italic: attributes.italic,
        underline: attributes.underline,
        strikethrough: attributes.strikethrough,
    }
}

fn colors_for_attributes(
    attributes: CellAttributes,
    config: &AppConfig,
) -> (RenderColor, RenderColor) {
    let mut foreground = color_or_default(
        attributes.foreground,
        render_color(config.colors.foreground),
        config,
    );
    let mut background = color_or_default(
        attributes.background,
        render_color(config.colors.background),
        config,
    );
    if attributes.inverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    background.alpha = ((f64::from(background.alpha) * config.window.opacity)
        .round()
        .clamp(0.0, 255.0)) as u8;

    (foreground, background)
}

fn color_or_default(color: Option<Color>, default: RenderColor, config: &AppConfig) -> RenderColor {
    match color {
        Some(Color::Rgb { red, green, blue }) => RenderColor::rgb(red, green, blue),
        Some(Color::Indexed(index)) => ansi_color(index, config),
        Some(Color::DefaultForeground | Color::DefaultBackground) | None => default,
    }
}

fn render_color(color: config_core::RgbaColor) -> RenderColor {
    RenderColor {
        red: color.red,
        green: color.green,
        blue: color.blue,
        alpha: color.alpha,
    }
}

#[derive(Debug, Default)]
struct MouseProtocolState {
    pressed_button: Option<MouseButton>,
}

impl MouseProtocolState {
    fn report_bytes(
        &mut self,
        event: MouseEvent,
        metrics: CellMetrics,
        modes: &BTreeSet<TerminalMode>,
    ) -> Option<Vec<u8>> {
        let enabled = modes.contains(&TerminalMode::MouseReporting)
            || modes.contains(&TerminalMode::MouseCellMotion)
            || modes.contains(&TerminalMode::MouseAllMotion);
        if !enabled {
            return None;
        }

        let col = ((event.x / f64::from(metrics.cell_width)).floor() as u16).saturating_add(1);
        let row = ((event.y / f64::from(metrics.cell_height)).floor() as u16).saturating_add(1);

        let report = match event.kind {
            MouseEventKind::Pressed(button) => {
                self.pressed_button = Some(button);
                MouseReport {
                    button_code: mouse_button_code(button)?,
                    pressed: true,
                    motion: false,
                    row,
                    col,
                    modifiers: event.modifiers,
                }
            }
            MouseEventKind::Released(button) => {
                self.pressed_button = None;
                MouseReport {
                    button_code: mouse_button_code(button)?,
                    pressed: false,
                    motion: false,
                    row,
                    col,
                    modifiers: event.modifiers,
                }
            }
            MouseEventKind::Moved => {
                if modes.contains(&TerminalMode::MouseAllMotion) {
                    MouseReport {
                        button_code: self.pressed_button.and_then(mouse_button_code).unwrap_or(3),
                        pressed: self.pressed_button.is_some(),
                        motion: true,
                        row,
                        col,
                        modifiers: event.modifiers,
                    }
                } else if modes.contains(&TerminalMode::MouseCellMotion)
                    && self.pressed_button.is_some()
                {
                    MouseReport {
                        button_code: self.pressed_button.and_then(mouse_button_code)?,
                        pressed: true,
                        motion: true,
                        row,
                        col,
                        modifiers: event.modifiers,
                    }
                } else {
                    return None;
                }
            }
            MouseEventKind::Wheel { delta_x, delta_y } => {
                let button_code = if delta_y > 0.0 {
                    64
                } else if delta_y < 0.0 {
                    65
                } else if delta_x > 0.0 {
                    66
                } else if delta_x < 0.0 {
                    67
                } else {
                    return None;
                };
                MouseReport {
                    button_code,
                    pressed: true,
                    motion: false,
                    row,
                    col,
                    modifiers: event.modifiers,
                }
            }
        };

        Some(encode_mouse_report(
            report,
            modes.contains(&TerminalMode::SgrMouse),
        ))
    }
}

#[derive(Debug, Clone, Copy)]
struct MouseReport {
    button_code: u16,
    pressed: bool,
    motion: bool,
    row: u16,
    col: u16,
    modifiers: KeyModifiers,
}

fn mouse_button_code(button: MouseButton) -> Option<u16> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => None,
    }
}

fn encode_mouse_report(report: MouseReport, sgr: bool) -> Vec<u8> {
    let mut button_code = report.button_code;
    if report.motion {
        button_code += 32;
    }
    if report.modifiers.shift {
        button_code += 4;
    }
    if report.modifiers.alt {
        button_code += 8;
    }
    if report.modifiers.ctrl {
        button_code += 16;
    }

    if sgr {
        let suffix = if report.pressed { 'M' } else { 'm' };
        format!(
            "\x1b[<{};{};{}{}",
            button_code, report.col, report.row, suffix
        )
        .into_bytes()
    } else {
        let legacy_code = if report.pressed { button_code } else { 3 };
        vec![
            0x1b,
            b'[',
            b'M',
            encode_legacy_mouse_coord(legacy_code),
            encode_legacy_mouse_coord(report.col),
            encode_legacy_mouse_coord(report.row),
        ]
    }
}

fn encode_legacy_mouse_coord(value: u16) -> u8 {
    value.saturating_add(32).min(255) as u8
}

fn ansi_color(index: u8, config: &AppConfig) -> RenderColor {
    const PALETTE: [RenderColor; 16] = [
        RenderColor::rgb(12, 12, 12),
        RenderColor::rgb(197, 15, 31),
        RenderColor::rgb(19, 161, 14),
        RenderColor::rgb(193, 156, 0),
        RenderColor::rgb(0, 55, 218),
        RenderColor::rgb(136, 23, 152),
        RenderColor::rgb(58, 150, 221),
        RenderColor::rgb(204, 204, 204),
        RenderColor::rgb(118, 118, 118),
        RenderColor::rgb(231, 72, 86),
        RenderColor::rgb(22, 198, 12),
        RenderColor::rgb(249, 241, 165),
        RenderColor::rgb(59, 120, 255),
        RenderColor::rgb(180, 0, 158),
        RenderColor::rgb(97, 214, 214),
        RenderColor::rgb(242, 242, 242),
    ];

    if index < 16 {
        return config
            .colors
            .palette
            .get(usize::from(index))
            .copied()
            .map(render_color)
            .or_else(|| PALETTE.get(usize::from(index)).copied())
            .unwrap_or(PALETTE[7]);
    }
    if index < 232 {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let cube = index - 16;
        return RenderColor::rgb(
            LEVELS[usize::from(cube / 36)],
            LEVELS[usize::from((cube / 6) % 6)],
            LEVELS[usize::from(cube % 6)],
        );
    }
    let gray = 8u8.saturating_add((index - 232).saturating_mul(10));
    RenderColor::rgb(gray, gray, gray)
}

#[cfg(test)]
mod tests {
    use super::*;
    use semantics::{SemanticEventKind, SemanticTimeline};

    #[test]
    fn desktop_startup_uses_configured_background_before_window_reveal() {
        let mut config = AppConfig::default();
        config.colors.background = config_core::RgbaColor {
            red: 17,
            green: 34,
            blue: 51,
            alpha: 255,
        };

        let settings = window_settings(&config);
        assert!(!settings.visible_on_create);
        assert!(settings.icon.is_some());
        assert_eq!(
            renderer_options(&config).background,
            RenderColor::rgb(17, 34, 51)
        );
    }

    #[derive(Debug, Default)]
    struct RecordingNotificationProvider {
        requests: Vec<NotificationRequest>,
    }

    impl NotificationProvider for RecordingNotificationProvider {
        fn notify(
            &mut self,
            request: NotificationRequest,
        ) -> Result<(), platform_core::NotificationDiagnostic> {
            self.requests.push(request);
            Ok(())
        }

        fn diagnostic(&self) -> platform_core::NotificationDiagnostic {
            platform_core::NotificationDiagnostic {
                backend: platform_core::NotificationBackend::Unsupported,
                availability: platform_core::NotificationAvailability::Available,
                message: "test provider".to_owned(),
            }
        }
    }

    #[derive(Debug, Default)]
    struct RecordingInputSink {
        writes: Vec<Vec<u8>>,
    }

    impl TerminalInputSink for RecordingInputSink {
        fn write_terminal_bytes(&mut self, bytes: &[u8]) -> TransportResult<()> {
            self.writes.push(bytes.to_vec());
            Ok(())
        }
    }

    #[test]
    fn terminal_protocol_responses_are_written_before_user_input() {
        let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(80, 24));
        terminal
            .apply_bytes(b"\x1b[4;8H\x1b[6n")
            .expect("apply cursor-position query");
        let mut sink = RecordingInputSink::default();

        write_terminal_input(&mut terminal, &mut sink, b"typed");

        assert_eq!(sink.writes, vec![b"\x1b[4;8R".to_vec(), b"typed".to_vec()]);
        assert!(terminal.state().pending_output().is_empty());
    }

    #[test]
    fn battery_policy_is_bounded_and_reversible() {
        let mut configured = PerformanceConfig::default();
        configured.apply_profile(PerformanceProfile::Visual);
        let mut effective = configured.clone();
        apply_power_policy(
            &mut effective,
            &configured,
            PowerState {
                source: PowerSource::Battery,
                battery_count: 1,
                charge_percent: Some(50),
            },
        );

        assert!(effective.max_animation_fps <= 30);
        assert!(effective.max_active_animations <= 2);
        assert!(effective.glyph_cache_entries <= 4096);

        apply_power_policy(
            &mut effective,
            &configured,
            PowerState {
                source: PowerSource::Ac,
                battery_count: 1,
                charge_percent: Some(51),
            },
        );
        assert_eq!(effective, configured);
    }

    #[test]
    fn disabled_battery_adaptation_preserves_configured_profile() {
        let configured = PerformanceConfig {
            disable_expensive_effects_on_battery: false,
            ..PerformanceConfig::default()
        };
        let mut effective = configured.clone();
        apply_power_policy(
            &mut effective,
            &configured,
            PowerState {
                source: PowerSource::Battery,
                battery_count: 1,
                charge_percent: None,
            },
        );
        assert_eq!(effective, configured);
    }

    fn mouse_event(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            x: 0.0,
            y: 0.0,
            modifiers: KeyModifiers::default(),
        }
    }

    fn key_event(logical_key: &str, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            physical_key: None,
            logical_key: logical_key.to_owned(),
            text: None,
            state: KeyState::Pressed,
            modifiers,
            repeat: false,
        }
    }

    fn text_key_event(logical_key: &str, text: &str, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            physical_key: None,
            logical_key: logical_key.to_owned(),
            text: Some(text.to_owned()),
            state: KeyState::Pressed,
            modifiers,
            repeat: false,
        }
    }

    fn test_transport_waker() -> TransportWakeHandle {
        TransportWakeHandle::new(|| {})
    }

    #[test]
    fn terminal_input_ignores_modifier_and_unknown_named_keys() {
        for key in [
            "Alt",
            "AltGraph",
            "Control",
            "Shift",
            "Super",
            "CapsLock",
            "NumLock",
            "PrintScreen",
            "Pause",
            "Unidentified",
        ] {
            assert_eq!(terminal_key(&key_event(key, KeyModifiers::default())), None);
        }
    }

    #[test]
    fn terminal_input_uses_text_only_for_printable_character_events() {
        assert_eq!(
            terminal_key(&text_key_event("a", "a", KeyModifiers::default())),
            Some(TerminalKey::Character("a".to_owned()))
        );
        assert_eq!(
            terminal_key(&text_key_event("Dead(Acute)", "", KeyModifiers::default())),
            None
        );
    }

    #[test]
    fn terminal_input_encodes_ctrl_from_logical_key_not_control_text() {
        let modifiers = KeyModifiers {
            ctrl: true,
            ..KeyModifiers::default()
        };
        let event = text_key_event("a", "\u{1}", modifiers);
        assert_eq!(
            terminal_key(&event),
            Some(TerminalKey::Character("a".to_owned()))
        );
        assert_eq!(
            encode_terminal_key(
                &terminal_key(&event).expect("Ctrl+A terminal key"),
                terminal_modifiers(event.modifiers),
                &BTreeSet::new(),
            ),
            Some(vec![0x01])
        );
    }

    #[test]
    fn gui_terminal_io_smoke_recognizes_common_cross_platform_prompts() {
        assert!(shell_prompt_visible(
            "Windows PowerShell\nPS C:\\Users\\panea>"
        ));
        assert_eq!(
            shell_prompt_line_count(
                "Windows PowerShell\nPS C:\\Users\\panea>\nPS C:\\Users\\panea>"
            ),
            2
        );
        assert!(shell_prompt_visible(
            "Windows PowerShell\nPS C:\\Users\\panea>\n\n\n\n\n"
        ));
        assert!(shell_prompt_visible("panea@host:~$"));
        assert!(shell_prompt_visible("root@host:/#"));
        assert!(shell_prompt_visible("host%"));
        assert!(!shell_prompt_visible(
            "Copyright (C) Microsoft Corporation."
        ));
    }

    fn test_pane(cols: u16, rows: u16) -> PaneRuntime {
        PaneRuntime {
            terminal: TerminalEmulator::new(CoreTerminalSize::new(cols, rows)),
            semantic_parser: SemanticEscapeParser::new(),
            semantic_timeline: SemanticTimelineStore::new(),
            heuristic_detector: None,
            parse_semantic_events: false,
            remote_session: false,
            session_spec: SessionSpec::local("default"),
            last_size: TerminalGridSize::new(cols, rows),
            connection_state: PaneConnectionState::Disconnected("test".to_owned()),
            disconnect_notified: false,
            ssh_prompt: None,
            osc52_prompt: None,
            ime_preedit: String::new(),
            transport: None,
            mouse_protocol: MouseProtocolState::default(),
            selection_anchor: None,
            selection_kind: SelectionKind::Normal,
            keyboard_selection: None,
            search: PaneSearch::default(),
            command_output_collapsed: HashMap::new(),
            output_waker: test_transport_waker(),
        }
    }

    #[test]
    fn remote_osc52_prompt_never_displays_clipboard_contents() {
        let mut pane = test_pane(80, 24);
        pane.remote_session = true;
        pane.session_spec = SessionSpec::ssh("prod");
        let prompt = Osc52PromptState {
            request: SecurityOsc52Request {
                target: Osc52ClipboardTarget::Clipboard,
                payload_base64: "c2VjcmV0LWNsaXBib2FyZA==".to_owned(),
                remote: true,
            },
            reason: "explicit confirmation required".to_owned(),
            bytes: 16,
        };

        let lines = osc52_prompt_lines(&pane, &prompt).join("\n");

        assert!(lines.contains("prod"));
        assert!(lines.contains("16 bytes"));
        assert!(!lines.contains("secret-clipboard"));
        assert!(!lines.contains(&prompt.request.payload_base64));
    }

    #[test]
    fn session_notifications_are_background_only_by_default() {
        let mut pane = test_pane(80, 24);
        pane.remote_session = true;
        pane.session_spec = SessionSpec::ssh("prod");
        let config = NotificationConfig::default();
        let poll = PanePollStats {
            closed: true,
            ..PanePollStats::default()
        };
        let mut provider = RecordingNotificationProvider::default();

        notify_for_pane_transition(&mut provider, &config, true, &pane, poll);
        assert!(provider.requests.is_empty());

        notify_for_pane_transition(&mut provider, &config, false, &pane, poll);
        assert_eq!(provider.requests.len(), 1);
        assert!(provider.requests[0].title.contains("SSH"));
        assert!(provider.requests[0].body.contains("prod"));
    }

    fn text_key(text: &str) -> KeyEvent {
        KeyEvent {
            physical_key: None,
            logical_key: text.to_owned(),
            text: Some(text.to_owned()),
            state: KeyState::Pressed,
            modifiers: KeyModifiers::default(),
            repeat: false,
        }
    }

    #[test]
    fn unknown_ssh_host_prompt_requires_explicit_trust_action() {
        let key = security::HostKey::from_raw("host.example", 22, "ssh-ed25519", b"key");
        let request = HostKeyTrustRequest::unknown(key, "explicit decision required");
        let (response, decision) = mpsc::sync_channel(1);
        let mut prompt = SshPromptState::HostTrust {
            request,
            response: Some(response),
        };

        assert!(!prompt.handle_key(&text_key("x")));
        assert!(matches!(decision.try_recv(), Err(TryRecvError::Empty)));
        assert!(prompt.handle_key(&text_key("s")));
        assert_eq!(decision.recv().unwrap(), HostKeyTrustAction::TrustAndStore);
    }

    #[test]
    fn ssh_secret_prompt_masks_and_returns_persistence_intent() {
        let request = SecretRequest::SshPassword {
            profile: "prod".to_owned(),
            host: "host.example".to_owned(),
            username: "alice".to_owned(),
        };
        let (response, result) = mpsc::sync_channel(1);
        let mut prompt = SshPromptState::Secret {
            request,
            keychain: KeychainProviderCapability {
                platform: security::SecurityPlatform::Windows,
                backend: security::KeychainBackend::WindowsCredentialManager,
                available: true,
                persistent: true,
                secure_storage: true,
                message: "available".to_owned(),
            },
            response: Some(response),
            input: String::new(),
            save_to_keychain: false,
        };

        assert!(!prompt.handle_key(&text_key("secret")));
        let rendered = ssh_prompt_lines(&prompt).join("\n");
        assert!(rendered.contains("******"));
        assert!(!rendered.contains("Secret: secret"));
        assert!(!prompt.handle_key(&key_event("Tab", KeyModifiers::default())));
        assert!(prompt.handle_key(&key_event("Enter", KeyModifiers::default())));
        let response = result.recv().unwrap().expect("secret response");
        assert!(response.save_to_keychain);
        assert_eq!(response.secret.expose(), "secret");
    }

    fn smoke_size() -> TransportSize {
        TransportSize::new(80, 24, 640, 384)
    }

    #[test]
    fn paste_protection_normalizes_and_strips_controls() {
        let clipboard = ClipboardConfig::default();
        let paste = PasteConfig::default();

        let bytes = paste_bytes("a\r\nb\u{7}c", &clipboard, &paste, false);

        assert_eq!(String::from_utf8(bytes).unwrap(), "a\nbc");
    }

    #[test]
    fn bracketed_paste_wraps_only_when_terminal_mode_is_enabled() {
        let clipboard = ClipboardConfig::default();
        let paste = PasteConfig::default();

        let bytes = paste_bytes("panea", &clipboard, &paste, true);

        assert_eq!(bytes, b"\x1b[200~panea\x1b[201~");
    }

    #[test]
    fn middle_click_paste_is_suppressed_when_mouse_reporting_is_active() {
        let mouse = mouse_event(MouseEventKind::Pressed(MouseButton::Middle));
        let mut modes = BTreeSet::new();

        assert!(should_middle_click_paste(
            &mouse,
            &modes,
            &ClipboardConfig::default()
        ));

        modes.insert(TerminalMode::MouseReporting);
        assert!(!should_middle_click_paste(
            &mouse,
            &modes,
            &ClipboardConfig::default()
        ));
    }

    #[test]
    fn focus_reports_are_emitted_only_when_requested() {
        let mut modes = BTreeSet::new();
        assert_eq!(focus_report_bytes(true, &modes), None);

        modes.insert(TerminalMode::FocusEvents);
        assert_eq!(focus_report_bytes(true, &modes), Some(b"\x1b[I".as_slice()));
        assert_eq!(
            focus_report_bytes(false, &modes),
            Some(b"\x1b[O".as_slice())
        );
    }

    #[test]
    fn osc52_policy_mapping_keeps_remote_denied_by_default() {
        let policy = osc52_policy(&ClipboardConfig::default());
        let request = SecurityOsc52Request {
            target: Osc52ClipboardTarget::Clipboard,
            payload_base64: "cGFuZWE=".to_owned(),
            remote: true,
        };

        let decision = evaluate_osc52_clipboard_write(&request, &policy);

        assert!(
            matches!(decision, Osc52ClipboardDecision::Deny { reason } if reason.contains("remote"))
        );
    }

    #[test]
    fn configured_keybindings_drive_mux_actions() {
        let config = AppConfig::default();
        let event = key_event(
            "T",
            KeyModifiers {
                ctrl: true,
                shift: true,
                ..KeyModifiers::default()
            },
        );

        assert_eq!(
            keybinding_action(&event, &config).as_deref(),
            Some("new_tab")
        );
        assert_eq!(
            canonical_key_spec("Shift+Ctrl+T"),
            canonical_key_event(&event)
        );
    }

    #[test]
    fn desktop_key_mapping_preserves_terminal_protocol_keys() {
        let mut event = key_event("ArrowUp", KeyModifiers::default());
        assert_eq!(terminal_key(&event), Some(TerminalKey::Up));

        event.logical_key = "F12".to_owned();
        assert_eq!(terminal_key(&event), Some(TerminalKey::Function(12)));

        event.physical_key = Some("Code(NumpadEnter)".to_owned());
        event.logical_key = "Enter".to_owned();
        assert_eq!(
            terminal_key(&event),
            Some(TerminalKey::Keypad(KeypadKey::Enter))
        );
    }

    #[test]
    fn selection_visual_projects_only_visible_selected_cells() {
        let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(4, 2));
        terminal
            .apply_bytes(b"abc\r\ndef\r\nghi")
            .expect("terminal input");
        terminal.state_mut().scroll_viewport(1);
        terminal.state_mut().set_selection(Selection::normal(
            GridPosition::new(0, 2),
            GridPosition::new(1, 1),
        ));
        let viewport = terminal.visible_grid().viewport;

        let visual =
            selection_visual(&terminal, viewport, &AppConfig::default()).expect("selection visual");

        assert_eq!(
            visual.cells,
            vec![
                CellPosition { row: 0, col: 2 },
                CellPosition { row: 0, col: 3 },
                CellPosition { row: 1, col: 0 },
                CellPosition { row: 1, col: 1 },
            ]
        );
    }

    #[test]
    fn sgr_mouse_reports_press_drag_release_and_modifiers() {
        let mut protocol = MouseProtocolState::default();
        let mut modes = BTreeSet::from([TerminalMode::MouseCellMotion, TerminalMode::SgrMouse]);
        let metrics = test_metrics();
        let mut press = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
        press.x = 15.0;
        press.y = 25.0;
        press.modifiers.shift = true;
        assert_eq!(
            protocol.report_bytes(press, metrics, &modes),
            Some(b"\x1b[<4;2;2M".to_vec())
        );

        let mut drag = press;
        drag.kind = MouseEventKind::Moved;
        assert_eq!(
            protocol.report_bytes(drag, metrics, &modes),
            Some(b"\x1b[<36;2;2M".to_vec())
        );

        let mut release = press;
        release.kind = MouseEventKind::Released(MouseButton::Left);
        assert_eq!(
            protocol.report_bytes(release, metrics, &modes),
            Some(b"\x1b[<4;2;2m".to_vec())
        );

        modes.clear();
        assert_eq!(protocol.report_bytes(press, metrics, &modes), None);
    }

    #[test]
    fn keyboard_selection_mode_extends_normal_and_rectangular_ranges() {
        let mut pane = test_pane(10, 2);
        pane.terminal.apply_bytes(b"abc").unwrap();
        pane.start_keyboard_selection(SelectionKind::Normal);

        assert!(
            pane.handle_keyboard_selection_key(&key_event("ArrowLeft", KeyModifiers::default()))
        );
        assert_eq!(pane.terminal.state().selected_text().as_deref(), Some("c"));
        assert_eq!(
            pane.terminal
                .state()
                .selection_state()
                .map(|value| value.kind),
            Some(SelectionKind::Normal)
        );

        pane.start_keyboard_selection(SelectionKind::Rectangular);
        pane.handle_keyboard_selection_key(&key_event("ArrowLeft", KeyModifiers::default()));
        assert_eq!(
            pane.terminal
                .state()
                .selection_state()
                .map(|value| value.kind),
            Some(SelectionKind::Rectangular)
        );
        pane.handle_keyboard_selection_key(&key_event("Escape", KeyModifiers::default()));
        assert!(pane.terminal.state().selection_state().is_none());
    }

    #[test]
    fn interactive_search_updates_navigates_and_closes_without_pty_input() {
        let mut pane = test_pane(20, 3);
        pane.terminal
            .apply_bytes(b"needle one\r\nneedle two")
            .unwrap();
        pane.search.start();

        assert!(pane.append_search_text("needle"));
        assert_eq!(pane.search.matches.len(), 2);
        assert_eq!(pane.search.active_match, 0);

        pane.handle_search_key(&key_event("Enter", KeyModifiers::default()));
        assert_eq!(pane.search.active_match, 1);
        pane.handle_search_key(&key_event("ArrowUp", KeyModifiers::default()));
        assert_eq!(pane.search.active_match, 0);
        pane.handle_search_key(&key_event("Escape", KeyModifiers::default()));
        assert!(!pane.search.input_active);
        assert!(pane.search.matches.is_empty());
    }

    #[test]
    fn search_overlay_and_url_hit_testing_use_visible_terminal_content() {
        let mut pane = test_pane(40, 2);
        pane.terminal
            .apply_bytes("\u{754c} https://example.com now".as_bytes())
            .unwrap();
        pane.search.start();
        pane.append_search_text("example");
        let viewport = pane.terminal.visible_grid().viewport;

        let overlays = search_overlays(
            &pane.search,
            viewport,
            test_metrics(),
            &AppConfig::default(),
        );
        assert!(
            overlays
                .iter()
                .any(|overlay| overlay.kind == OverlayKind::SearchHighlight)
        );

        let mut mouse = mouse_event(MouseEventKind::Released(MouseButton::Left));
        mouse.x = 8.0 * 10.0;
        mouse.y = 2.0;
        assert_eq!(
            pane.url_at_mouse(mouse, test_metrics()).as_deref(),
            Some("https://example.com")
        );
        assert_eq!(visible_url_hints(&pane.terminal, 0)[0].start.col, 3);
    }

    #[test]
    fn mux_layout_reserves_tab_bar_only_when_configured_and_needed() {
        let mut config = AppConfig::default();
        let mut model = MuxModel::new(SessionSpec::local("default"));
        model
            .new_tab("2", SessionSpec::local("default"))
            .expect("new tab");
        let active_tab = model.active_tab().id;
        let runtime = MuxRuntime {
            model,
            panes: HashMap::new(),
            surface_cols: 100,
            surface_rows: 30,
            performance: RuntimePerformanceCounters::new(),
            restore_sessions: false,
            state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
            drag: None,
            output_waker: test_transport_waker(),
        };

        let layout = runtime.active_layouts(&config);
        assert_eq!(layout[0].rect.y, 1.0);
        assert_eq!(layout[0].terminal_size.rows, 29);
        let mut mouse = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
        mouse.x = f64::from(horizontal_content_inset(&config)) + 8.0 * 7.0;
        mouse.y = f64::from(vertical_content_inset(&config)) + 4.0;
        assert_eq!(
            runtime.tab_at_mouse(mouse, test_metrics(), &config),
            Some(active_tab)
        );

        config.mux.show_tab_bar = false;
        let layout = runtime.active_layouts(&config);
        assert_eq!(layout[0].rect.y, 0.0);
        assert_eq!(layout[0].terminal_size.rows, 30);
    }

    #[test]
    fn tab_drag_reorders_without_replacing_session_models() {
        let config = AppConfig::default();
        let mut model = MuxModel::new(SessionSpec::local("default"));
        let first = model.active_tab().id;
        let second = model
            .new_tab("2", SessionSpec::local("default"))
            .expect("new tab");
        let mut runtime = MuxRuntime {
            model,
            panes: HashMap::new(),
            surface_cols: 100,
            surface_rows: 30,
            performance: RuntimePerformanceCounters::new(),
            restore_sessions: false,
            state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
            drag: None,
            output_waker: test_transport_waker(),
        };
        let metrics = test_metrics();
        let mut clipboard = ClipboardBridge::new();
        let mut press = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
        press.x = f64::from(horizontal_content_inset(&config)) + 4.0;
        press.y = f64::from(vertical_content_inset(&config)) + 4.0;
        assert!(
            runtime
                .handle_mouse(
                    press,
                    metrics,
                    &config,
                    &config.clipboard,
                    &config.paste,
                    &mut clipboard,
                )
                .changed
        );
        assert_eq!(
            runtime.drag,
            Some(MuxDragState::Tab {
                source: first,
                target: first
            })
        );

        let first_width = formatted_tab_width(
            &config,
            &runtime.model.active_workspace().name,
            0,
            &runtime.model.active_workspace().active_window().tabs[0],
        );
        let mut moved = press;
        moved.kind = MouseEventKind::Moved;
        moved.x = f64::from(horizontal_content_inset(&config))
            + (first_width as f64 + 1.0) * f64::from(metrics.cell_width);
        assert!(
            runtime
                .handle_mouse(
                    moved,
                    metrics,
                    &config,
                    &config.clipboard,
                    &config.paste,
                    &mut clipboard,
                )
                .changed
        );
        assert_eq!(
            runtime.drag,
            Some(MuxDragState::Tab {
                source: first,
                target: second
            })
        );

        moved.kind = MouseEventKind::Released(MouseButton::Left);
        runtime.handle_mouse(
            moved,
            metrics,
            &config,
            &config.clipboard,
            &config.paste,
            &mut clipboard,
        );
        assert_eq!(
            runtime
                .model
                .active_workspace()
                .active_window()
                .tabs
                .iter()
                .map(|tab| tab.id)
                .collect::<Vec<_>>(),
            vec![second, first]
        );
    }

    #[test]
    fn pane_drag_target_is_visual_only() {
        let config = AppConfig::default();
        let mut model = MuxModel::new(SessionSpec::local("default"));
        let source = model.active_tab().active_pane;
        let target = model
            .split_active_pane(SplitAxis::Vertical, SessionSpec::local("default"))
            .expect("split");
        let runtime = MuxRuntime {
            model,
            panes: HashMap::new(),
            surface_cols: 80,
            surface_rows: 24,
            performance: RuntimePerformanceCounters::new(),
            restore_sessions: false,
            state_path: std::env::temp_dir().join("panea-test-mux-state.json"),
            drag: Some(MuxDragState::Pane { source, target }),
            output_waker: test_transport_waker(),
        };
        let mut scene = RenderScene::default();
        append_mux_drag_overlay(&mut scene, &runtime, test_metrics(), &config);
        assert_eq!(scene.semantic_overlays.len(), 1);
        assert_eq!(scene.semantic_overlays[0].kind, OverlayKind::DragTarget);
        assert!(scene.grid.cells.is_empty());
    }

    #[test]
    fn startup_mux_snapshot_preserves_nested_local_and_ssh_transports() {
        let mut config = AppConfig::default();
        config.mux.startup_workspaces = vec![config_core::MuxWorkspaceConfig {
            name: "work".to_owned(),
            tabs: vec![config_core::MuxTabConfig {
                name: "mixed".to_owned(),
                layout: MuxLayoutConfig::Split {
                    axis: MuxSplitAxisConfig::Vertical,
                    ratio: 0.7,
                    first: Box::new(MuxLayoutConfig::Pane {
                        profile: "default".to_owned(),
                        transport: MuxTransportConfig::Local,
                        working_directory: Some("local".to_owned()),
                    }),
                    second: Box::new(MuxLayoutConfig::Pane {
                        profile: "prod".to_owned(),
                        transport: MuxTransportConfig::Ssh,
                        working_directory: Some("remote".to_owned()),
                    }),
                },
            }],
        }];

        let snapshot = startup_mux_snapshot(&config).expect("startup snapshot");
        let tab = &snapshot.workspaces[0].windows[0].tabs[0];
        assert_eq!(tab.panes.len(), 2);
        assert_eq!(
            tab.panes[0].transport,
            if cfg!(windows) {
                SessionTransportKind::WindowsPseudoconsole
            } else {
                SessionTransportKind::LocalPty
            }
        );
        assert_eq!(tab.panes[1].transport, SessionTransportKind::Ssh);
        assert_eq!(
            MuxModel::from_restore_snapshot(&snapshot, SessionSpec::local("default"))
                .expect("restore")
                .active_tab()
                .layout(LogicalRect::unit())
                .len(),
            2
        );
    }

    #[test]
    fn shell_integration_full_mode_injects_supported_runtime_hook() {
        let mut config = AppConfig {
            default_shell_profile: Some("bash".to_owned()),
            shell_profiles: vec![ShellProfile {
                name: "bash".to_owned(),
                kind: ShellProfileKind::Custom,
                program: "bash".to_owned(),
                ..ShellProfile::default()
            }],
            ..AppConfig::default()
        };
        config.shell_integration.activation = ShellIntegrationActivationConfig::Full;

        let (profile, activation) = initial_local_shell_profile(&config, None);

        assert_eq!(
            activation.action,
            ShellIntegrationActivationAction::InjectRuntimeScript
        );
        assert_eq!(
            profile
                .env
                .get("PANEA_SHELL_INTEGRATION")
                .map(String::as_str),
            Some("full")
        );
        if cfg!(windows) {
            assert!(
                profile
                    .startup_command
                    .as_deref()
                    .unwrap_or_default()
                    .contains("777")
                    || profile.args.iter().any(|arg| arg.contains("panea"))
            );
        } else {
            assert!(profile.args.iter().any(|arg| arg.contains("panea")));
        }
    }

    #[test]
    fn shell_integration_off_mode_does_not_inject_or_parse() {
        let mut config = AppConfig {
            default_shell_profile: Some("bash".to_owned()),
            shell_profiles: vec![ShellProfile {
                name: "bash".to_owned(),
                kind: ShellProfileKind::Custom,
                program: "bash".to_owned(),
                ..ShellProfile::default()
            }],
            ..AppConfig::default()
        };
        config.shell_integration.activation = ShellIntegrationActivationConfig::Disabled;

        let (profile, activation) = initial_local_shell_profile(&config, None);

        assert_eq!(
            semantic_mode_for_activation(&activation),
            IntegrationMode::Disabled
        );
        assert!(!activation.parses_escape_sequences());
        assert!(profile.args.is_empty());
        assert_eq!(
            profile
                .env
                .get("PANEA_SHELL_INTEGRATION")
                .map(String::as_str),
            Some("0")
        );
    }

    #[test]
    fn explicit_shell_args_prevent_runtime_hook_injection() {
        let mut config = AppConfig {
            default_shell_profile: Some("bash".to_owned()),
            shell_profiles: vec![ShellProfile {
                name: "bash".to_owned(),
                kind: ShellProfileKind::Custom,
                program: "bash".to_owned(),
                args: vec!["--login".to_owned()],
                ..ShellProfile::default()
            }],
            ..AppConfig::default()
        };
        config.shell_integration.activation = ShellIntegrationActivationConfig::Full;

        let (profile, activation) = initial_local_shell_profile(&config, None);

        assert_eq!(
            activation.action,
            ShellIntegrationActivationAction::InjectRuntimeScript
        );
        assert_eq!(profile.args, ["--login"]);
        assert!(profile.startup_command.is_none());
    }

    fn test_metrics() -> CellMetrics {
        CellMetrics {
            font_size: 13.0,
            cell_width: 8.0,
            cell_height: 16.0,
            ascent: 11.0,
            descent: -3.0,
            line_gap: 1.0,
        }
    }

    fn test_semantic_viewport(origin_row: i64) -> SemanticOverlayViewport {
        SemanticOverlayViewport {
            origin_row,
            rows: 10,
            cols: 80,
            metrics: test_metrics(),
        }
    }

    fn command_timeline() -> SemanticTimelineStore {
        let mut timeline = SemanticTimelineStore::new();
        timeline.apply_event(semantics::SemanticEvent::ShellMetadataChanged {
            position: BufferPosition::new(0, 0),
            metadata: semantics::ShellMetadata {
                shell: Some("pwsh".to_owned()),
                current_working_directory: Some("C:\\Users\\shres\\panea".to_owned()),
                ..semantics::ShellMetadata::default()
            },
        });
        timeline.input_started(BufferPosition::new(1, 0));
        timeline.input_ended(BufferPosition::new(1, 10));
        timeline.output_started(BufferPosition::new(2, 0));
        timeline.command_finished(
            BufferPosition::new(4, 0),
            CommandStatus::Code(0),
            Duration::from_millis(42),
        );
        timeline
    }

    #[test]
    fn semantic_navigation_selection_and_copy_use_raw_pane_text() {
        let mut pane = test_pane(40, 4);
        pane.terminal
            .apply_bytes(b"echo panea\r\npanea-output")
            .expect("terminal output");
        pane.semantic_timeline
            .input_started(BufferPosition::new(0, 0));
        pane.semantic_timeline
            .input_ended(BufferPosition::new(0, 10));
        pane.semantic_timeline
            .output_started(BufferPosition::new(1, 0));
        pane.semantic_timeline.command_finished(
            BufferPosition::new(1, 12),
            CommandStatus::Code(0),
            Duration::from_millis(5),
        );

        assert_eq!(
            pane.run_semantic_action(SemanticAction::CopyCurrentCommandOutput),
            SemanticActionResult::Text("panea-output".to_owned())
        );
        assert!(matches!(
            pane.run_semantic_action(SemanticAction::SelectCurrentCommandOutput),
            SemanticActionResult::Selection(_)
        ));
        assert_eq!(
            pane.terminal.state().selected_text().as_deref(),
            Some("panea-output")
        );
    }

    #[test]
    fn command_block_overlays_include_groups_and_metadata_badges() {
        let timeline = command_timeline();
        let mut config = AppConfig::default();
        config.command_blocks.enabled = true;
        config.command_blocks.style = CommandBlockStyle::Card;
        config.visual_theme.grouping_style = InputOutputGroupingStyle::InputOutputSplit;

        let overlays = semantic_visual_overlays(
            &timeline,
            &HashMap::new(),
            false,
            test_semantic_viewport(0),
            &config,
        );

        assert!(
            overlays
                .iter()
                .any(|overlay| overlay.kind == OverlayKind::CommandBlock)
        );
        assert!(
            overlays
                .iter()
                .any(|overlay| overlay.kind == OverlayKind::InputOutputGroup)
        );
        let labels = overlays
            .iter()
            .filter_map(|overlay| overlay.label.as_deref())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"ok"));
        assert!(labels.contains(&"42ms"));
        assert!(labels.iter().any(|label| label.starts_with("cwd ")));
        assert!(labels.contains(&"pwsh"));
        assert!(
            overlays
                .iter()
                .filter(|overlay| overlay.kind == OverlayKind::Badge)
                .all(|overlay| overlay.z_index > 20)
        );
    }

    #[test]
    fn semantic_visuals_are_suppressed_in_alternate_screen_by_default() {
        let timeline = command_timeline();
        let mut config = AppConfig::default();
        config.command_blocks.enabled = true;
        config.prompt_decorations.enabled = true;

        let overlays = semantic_visual_overlays(
            &timeline,
            &HashMap::new(),
            true,
            test_semantic_viewport(0),
            &config,
        );
        assert!(overlays.is_empty());

        config.command_blocks.allow_in_alternate_screen = true;
        let overlays = semantic_visual_overlays(
            &timeline,
            &HashMap::new(),
            true,
            test_semantic_viewport(0),
            &config,
        );
        assert!(
            overlays
                .iter()
                .any(|overlay| overlay.kind == OverlayKind::CommandBlock)
        );
        assert!(
            !overlays
                .iter()
                .any(|overlay| overlay.kind == OverlayKind::PromptDecoration)
        );
    }

    #[test]
    fn disabled_semantic_visuals_have_no_overlay_projection_cost() {
        let timeline = command_timeline();
        let config = AppConfig::default();

        let overlays = semantic_visual_overlays(
            &timeline,
            &HashMap::new(),
            false,
            test_semantic_viewport(0),
            &config,
        );

        assert!(overlays.is_empty());
    }

    #[test]
    fn prompt_overlay_uses_real_metadata_elevation_and_previous_status() {
        let mut timeline = command_timeline();
        timeline.prompt_started(
            BufferPosition::new(5, 0),
            SemanticMetadata {
                shell: semantics::ShellMetadata {
                    shell: Some("pwsh".to_owned()),
                    current_working_directory: Some("C:\\work\\panea".to_owned()),
                    ..semantics::ShellMetadata::default()
                },
                attributes: vec![("elevated".to_owned(), "true".to_owned())],
                ..SemanticMetadata::default()
            },
        );
        timeline.prompt_ended(BufferPosition::new(5, 8));
        let mut config = AppConfig::default();
        config.prompt_decorations.enabled = true;
        config.prompt_decorations.style = PromptDecorationStyle::RoundedBox;
        config.prompt_decorations.show_shell_badge = true;
        config.prompt_decorations.show_current_directory = true;
        config.prompt_decorations.show_admin_badge = true;
        config.prompt_decorations.show_previous_status_accent = true;

        let overlays = semantic_visual_overlays(
            &timeline,
            &HashMap::new(),
            false,
            test_semantic_viewport(0),
            &config,
        );
        let prompt = overlays
            .iter()
            .find(|overlay| overlay.kind == OverlayKind::PromptDecoration)
            .expect("prompt overlay");

        let label = prompt.label.as_deref().expect("prompt metadata label");
        assert!(label.contains("pwsh"));
        assert!(label.contains("panea"));
        assert!(label.contains("admin"));
        assert!(label.contains("ok"));
        assert_eq!(
            prompt.border_color,
            Some(render_color(config.visual_theme.success_color))
        );
    }

    #[test]
    fn semantic_overlays_follow_absolute_scrollback_positions() {
        let mut timeline = SemanticTimelineStore::new();
        timeline.input_started(BufferPosition::new(101, 0));
        timeline.input_ended(BufferPosition::new(101, 4));
        timeline.output_started(BufferPosition::new(102, 0));
        timeline.command_finished(
            BufferPosition::new(104, 0),
            CommandStatus::Code(0),
            Duration::from_millis(1),
        );
        let mut config = AppConfig::default();
        config.command_blocks.enabled = true;
        config.command_blocks.style = CommandBlockStyle::Card;

        let overlays = semantic_visual_overlays(
            &timeline,
            &HashMap::new(),
            false,
            test_semantic_viewport(100),
            &config,
        );
        let block = overlays
            .iter()
            .find(|overlay| overlay.kind == OverlayKind::CommandBlock)
            .expect("visible command block");
        assert!(block.bounds.y < (test_metrics().cell_height * 5.0) as i32);
    }

    #[test]
    fn collapsed_output_is_foreground_mask_and_preserves_raw_copy_text() {
        let mut pane = test_pane(40, 8);
        pane.terminal
            .apply_bytes(b"echo panea\r\none\r\ntwo\r\nthree\r\nfour")
            .expect("terminal output");
        pane.semantic_timeline
            .input_started(BufferPosition::new(0, 0));
        pane.semantic_timeline
            .input_ended(BufferPosition::new(0, 10));
        pane.semantic_timeline
            .output_started(BufferPosition::new(1, 0));
        pane.semantic_timeline.command_finished(
            BufferPosition::new(5, 0),
            CommandStatus::Code(0),
            Duration::from_millis(5),
        );
        let block_id = pane.semantic_timeline.command_blocks()[0].region_id;
        pane.command_output_collapsed.insert(block_id, true);
        let mut config = AppConfig::default();
        config.command_blocks.enabled = true;
        config.command_blocks.style = CommandBlockStyle::Card;
        config.command_blocks.collapsed_preview_lines = 1;

        let raw_before = pane.run_semantic_action(SemanticAction::CopyCurrentCommandOutput);
        let scene = scene_from_terminal(
            &pane.terminal,
            &pane.semantic_timeline,
            &pane.search,
            &pane.command_output_collapsed,
            Some(test_metrics()),
            &config,
            CursorPresentation {
                window_focused: true,
                blink_visible: true,
            },
        );
        let raw_after = pane.run_semantic_action(SemanticAction::CopyCurrentCommandOutput);

        assert_eq!(raw_before, raw_after);
        assert!(matches!(raw_after, SemanticActionResult::Text(text) if text.contains("four")));
        assert!(
            scene
                .semantic_overlays
                .iter()
                .any(|overlay| overlay.kind == OverlayKind::ContentMask
                    && overlay.color.alpha == u8::MAX)
        );
    }

    #[test]
    fn traditional_command_style_projects_no_command_visuals() {
        let timeline = command_timeline();
        let mut config = AppConfig::default();
        config.command_blocks.enabled = true;
        config.command_blocks.style = CommandBlockStyle::Traditional;

        let overlays = semantic_visual_overlays(
            &timeline,
            &HashMap::new(),
            false,
            test_semantic_viewport(0),
            &config,
        );

        assert!(overlays.is_empty());
    }

    #[test]
    fn disabled_performance_overlay_projects_no_scene_work() {
        let mut scene = RenderScene::default();
        scene.grid.columns = 80;
        scene.grid.rows = 24;
        let mut overlay = PerformanceOverlay::new(false, "test");
        overlay.record(RenderInstrumentation {
            frame_time: Duration::from_millis(16),
            ..RenderInstrumentation::default()
        });

        append_performance_overlay(
            &mut scene,
            &overlay,
            &PerformanceOverlayUiState {
                enabled: false,
                position: PerformanceOverlayPosition::TopRight,
                detail: PerformanceOverlayDetail::Compact,
                menu_open: false,
                persist: false,
                loaded_from_state: false,
                state_path: std::env::temp_dir().join("panea-test-ui-state.json"),
            },
            PerformanceBudget::default(),
            test_metrics(),
        );

        assert!(scene.semantic_overlays.is_empty());
    }

    #[test]
    fn enabled_performance_overlay_is_visual_only() {
        let mut scene = RenderScene::default();
        scene.grid.columns = 80;
        scene.grid.rows = 24;
        let mut overlay = PerformanceOverlay::new(true, "test");
        overlay.record(RenderInstrumentation {
            frame_time: Duration::from_millis(16),
            cpu_prepare_time: Duration::from_millis(4),
            draw_call_count: 3,
            ..RenderInstrumentation::default()
        });

        append_performance_overlay(
            &mut scene,
            &overlay,
            &PerformanceOverlayUiState {
                enabled: true,
                position: PerformanceOverlayPosition::TopRight,
                detail: PerformanceOverlayDetail::Compact,
                menu_open: false,
                persist: false,
                loaded_from_state: false,
                state_path: std::env::temp_dir().join("panea-test-ui-state.json"),
            },
            PerformanceBudget::default(),
            test_metrics(),
        );

        assert!(
            scene
                .semantic_overlays
                .iter()
                .all(|overlay| overlay.kind == OverlayKind::PerformanceOverlay)
        );
        assert!(scene.grid.cells.is_empty());
    }

    #[test]
    fn performance_overlay_click_menu_changes_runtime_preferences() {
        let config = AppConfig::default();
        let metrics = test_metrics();
        let mut overlay = PerformanceOverlay::new(true, "test");
        overlay.record(RenderInstrumentation {
            frame_time: Duration::from_millis(16),
            ..RenderInstrumentation::default()
        });
        let mut ui = PerformanceOverlayUiState {
            enabled: true,
            position: PerformanceOverlayPosition::TopLeft,
            detail: PerformanceOverlayDetail::Compact,
            menu_open: false,
            persist: false,
            loaded_from_state: false,
            state_path: std::env::temp_dir().join("panea-test-ui-state.json"),
        };
        let mut click = mouse_event(MouseEventKind::Pressed(MouseButton::Left));
        click.x = f64::from(horizontal_content_inset(&config)) + 12.0;
        click.y = f64::from(vertical_content_inset(&config)) + 12.0;
        assert!(handle_performance_overlay_mouse(
            click,
            &overlay,
            &mut ui,
            PerformanceBudget::default(),
            metrics,
            80,
            24,
            &config,
        ));
        assert!(ui.menu_open);

        let lines = performance_overlay_lines(&overlay, &ui, PerformanceBudget::default())
            .expect("overlay lines")
            .0;
        let layout = performance_overlay_layout(&lines, 80, 24, metrics, ui.position);
        let detail_row = layout.rows[2];
        click.x = f64::from(horizontal_content_inset(&config)) + f64::from(detail_row.x) + 2.0;
        click.y = f64::from(vertical_content_inset(&config)) + f64::from(detail_row.y) + 2.0;
        assert!(handle_performance_overlay_mouse(
            click,
            &overlay,
            &mut ui,
            PerformanceBudget::default(),
            metrics,
            80,
            24,
            &config,
        ));
        assert_eq!(ui.detail, PerformanceOverlayDetail::Detailed);
    }

    #[test]
    fn static_cursor_resolution_honors_modes_terminal_requests_and_focus() {
        let mut config = AppConfig::default();
        config.cursor.shape = config_core::CursorShape::HollowBlock;
        config
            .cursor
            .mode_specific_styles
            .insert("insert".to_owned(), config_core::CursorShape::Beam);
        let mut modes = BTreeSet::new();

        assert_eq!(
            resolved_cursor_shape(&config, CursorShape::Block, &modes, true),
            RenderCursorShape::HollowBlock
        );
        assert_eq!(
            resolved_cursor_shape(&config, CursorShape::Underline, &modes, true),
            RenderCursorShape::Underline
        );
        modes.insert(TerminalMode::Insert);
        assert_eq!(
            resolved_cursor_shape(&config, CursorShape::Block, &modes, true),
            RenderCursorShape::Beam
        );
        assert_eq!(
            resolved_cursor_shape(&config, CursorShape::Block, &modes, false),
            RenderCursorShape::HollowBlock
        );
    }

    #[test]
    fn relative_cursor_assets_resolve_from_the_config_directory() {
        let base = Path::new("portable-config");
        assert_eq!(
            resolve_cursor_image_path("assets/cursor.gif", Some(base)),
            base.join("assets/cursor.gif")
        );
        let absolute = std::env::temp_dir().join("panea-cursor.png");
        assert_eq!(
            resolve_cursor_image_path(&absolute.to_string_lossy(), Some(base)),
            absolute
        );
    }

    #[test]
    fn mouse_bindings_are_modifier_order_independent() {
        let config = config_core::MouseConfig {
            bindings: vec![config_core::MouseBinding::new(
                "Shift+Ctrl+LeftRelease",
                "copy",
            )],
            ..config_core::MouseConfig::default()
        };
        let event = MouseEvent {
            kind: MouseEventKind::Released(MouseButton::Left),
            x: 0.0,
            y: 0.0,
            modifiers: KeyModifiers {
                ctrl: true,
                shift: true,
                ..KeyModifiers::default()
            },
        };

        assert_eq!(
            mousebinding_action(&event, &config).as_deref(),
            Some("copy")
        );
    }

    #[test]
    fn indexed_color_mapping_covers_ansi_cube_and_grayscale() {
        let config = AppConfig::default();
        assert_eq!(
            ansi_color(1, &config),
            render_color(config.colors.palette[1])
        );
        assert_eq!(ansi_color(16, &config), RenderColor::rgb(0, 0, 0));
        assert_eq!(ansi_color(196, &config), RenderColor::rgb(255, 0, 0));
        assert_eq!(ansi_color(255, &config), RenderColor::rgb(238, 238, 238));
    }

    #[test]
    fn window_padding_and_margin_reduce_the_terminal_extent() {
        let mut config = AppConfig::default();
        config.window.padding_x = 8;
        config.window.margin_x = 4;
        assert_eq!(horizontal_content_inset(&config), 12);
        assert_eq!(content_extent(100, horizontal_content_inset(&config)), 76);
    }

    #[test]
    #[ignore = "spawns a real PowerShell process"]
    fn real_powershell_emits_semantic_shell_events() {
        run_real_shell_semantic_smoke(
            ShellKind::PowerShell,
            LocalShellProfile::powershell().with_args([
                "-NoLogo".to_owned(),
                "-NoProfile".to_owned(),
                "-Command".to_owned(),
                shell_integration::verification_sequence(
                    ShellKind::PowerShell,
                    "panea-shell-integration-smoke",
                )
                .expect("PowerShell verification sequence"),
            ]),
        );
    }

    #[test]
    #[ignore = "spawns an interactive PowerShell process with runtime integration"]
    fn real_powershell_runtime_activation_emits_complete_command_cycle() {
        let mut config = AppConfig {
            default_shell_profile: Some("powershell".to_owned()),
            shell_profiles: vec![ShellProfile {
                name: "powershell".to_owned(),
                kind: ShellProfileKind::PowerShell,
                program: "powershell.exe".to_owned(),
                ..ShellProfile::default()
            }],
            ..AppConfig::default()
        };
        config.shell_integration.activation = ShellIntegrationActivationConfig::Full;
        let (profile, activation) = initial_local_shell_profile(&config, None);
        assert_eq!(
            activation.action,
            ShellIntegrationActivationAction::InjectRuntimeScript
        );
        run_interactive_shell_activation_smoke(profile);
    }

    #[test]
    #[ignore = "spawns an interactive PowerShell process and waits for startup to settle"]
    fn real_default_powershell_startup_events_keep_one_prompt() {
        let config = AppConfig {
            default_shell_profile: Some("powershell".to_owned()),
            shell_profiles: vec![ShellProfile {
                name: "powershell".to_owned(),
                kind: ShellProfileKind::PowerShell,
                program: "powershell.exe".to_owned(),
                ..ShellProfile::default()
            }],
            ..AppConfig::default()
        };
        let (profile, _) = initial_local_shell_profile(&config, None);
        let initial_size = TransportSize::new(120, 36, 960, 576);
        let resized = TransportSize::new(147, 42, 1176, 672);
        let mut transport = LocalPtyTransport::spawn(profile, initial_size).expect("spawn shell");
        let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(120, 36));
        let mut raw = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut resized_after_prompt = false;
        let mut focus_reported_after_prompt = false;

        while Instant::now() < deadline {
            let output = transport.poll_output().expect("poll shell output");
            if !output.bytes.is_empty() {
                terminal
                    .apply_bytes(&output.bytes)
                    .expect("apply startup output");
                flush_terminal_responses(&mut terminal, &mut transport);
                raw.extend_from_slice(&output.bytes);
            }
            if !resized_after_prompt && raw.windows(3).any(|window| window == b"PS ") {
                terminal
                    .resize(CoreTerminalSize::new(resized.cols, resized.rows))
                    .expect("resize terminal grid");
                transport.resize(resized).expect("resize PTY");
                resized_after_prompt = true;
            }
            if resized_after_prompt && !focus_reported_after_prompt {
                transport
                    .write_input(b"\x1b[I")
                    .expect("send initial focus report");
                focus_reported_after_prompt = true;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let visible = terminal
            .visible_grid()
            .cells
            .chunks(usize::from(resized.cols))
            .map(|cells| term_core::Line {
                cells: cells.to_vec(),
                hard_wrapped: false,
            })
            .map(|line| line.raw_text())
            .collect::<Vec<_>>()
            .join("\n");
        let prompt_count = visible
            .lines()
            .filter(|line| line.trim_start().starts_with("PS ") || line.trim() == "PS>")
            .count();
        let _ = transport.shutdown();
        assert_eq!(
            prompt_count,
            1,
            "PowerShell startup events produced {prompt_count} prompts; resized_after_prompt={resized_after_prompt}; focus_reported_after_prompt={focus_reported_after_prompt}; visible={visible:?}; raw={:?}",
            String::from_utf8_lossy(&raw)
        );
    }

    #[test]
    #[ignore = "spawns an interactive PowerShell process and verifies grid/cursor coherence"]
    fn real_powershell_input_echo_keeps_grid_and_cursor_coherent() {
        let profile = LocalShellProfile::powershell();
        // Match the normal desktop launch more closely: Panea starts at a
        // modest window size, then may receive multiple grow/resize events as
        // the native window is presented or maximized.
        let size = TransportSize::new(86, 26, 944, 548);
        let mut transport = LocalPtyTransport::spawn(profile, size).expect("spawn shell");
        let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(size.cols, size.rows));
        let startup_deadline = Instant::now() + Duration::from_secs(2);
        let mut protocol_trace = Vec::new();

        while Instant::now() < startup_deadline {
            let output = transport.poll_output().expect("poll shell output");
            if !output.bytes.is_empty() {
                terminal
                    .apply_bytes(&output.bytes)
                    .expect("apply startup output");
                let responses = terminal.state_mut().take_pending_output();
                protocol_trace.push(format!(
                    "startup output={:?} cursor={:?} response={:?}",
                    String::from_utf8_lossy(&output.bytes),
                    terminal.cursor_state().position,
                    String::from_utf8_lossy(&responses)
                ));
                if !responses.is_empty() {
                    transport
                        .write_input(&responses)
                        .expect("write startup terminal response");
                }
            }
            if terminal_visible_lines(&terminal)
                .iter()
                .any(|line| line.trim_start().starts_with("PS "))
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let resized = TransportSize::new(171, 42, 1868, 868);
        terminal
            .resize(CoreTerminalSize::new(resized.cols, resized.rows))
            .expect("resize terminal grid before input");
        transport.resize(resized).expect("resize PTY before input");
        transport
            .resize(resized)
            .expect("repeat native resize event before input");
        const MARKER: &str = "panea-grid-cursor-check";
        const INPUT: &str = "Write-Output panea-grid-cursor-check";
        let mut typed = String::new();
        for character in INPUT.chars() {
            let mut encoded = [0u8; 4];
            let bytes = character.encode_utf8(&mut encoded).as_bytes();
            protocol_trace.push(format!(
                "input={character:?} cursor={:?}",
                terminal.cursor_state().position
            ));
            write_terminal_input(&mut terminal, &mut transport, bytes);
            typed.push(character);
            let visible_prefix = typed.trim_end();

            let echo_deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < echo_deadline {
                let output = transport.poll_output().expect("poll input echo");
                if !output.bytes.is_empty() {
                    terminal
                        .apply_bytes(&output.bytes)
                        .expect("apply input echo");
                    let responses = terminal.state_mut().take_pending_output();
                    protocol_trace.push(format!(
                        "echo output={:?} cursor={:?} response={:?}",
                        String::from_utf8_lossy(&output.bytes),
                        terminal.cursor_state().position,
                        String::from_utf8_lossy(&responses)
                    ));
                    if !responses.is_empty() {
                        transport
                            .write_input(&responses)
                            .expect("write input terminal response");
                    }
                }
                if terminal_visible_lines(&terminal).iter().any(|line| {
                    line.trim_start().starts_with("PS ") && line.contains(visible_prefix)
                }) {
                    break;
                }
                thread::sleep(Duration::from_millis(2));
            }

            let lines = terminal_visible_lines(&terminal);
            let input_row = lines
                .iter()
                .position(|line| {
                    line.trim_start().starts_with("PS ") && line.contains(visible_prefix)
                })
                .unwrap_or_else(|| {
                    panic!(
                        "PowerShell did not echo typed prefix {typed:?}; cursor={:?}; visible={lines:?}; trace:\n{protocol_trace}",
                        terminal.cursor_state().position,
                        protocol_trace = protocol_trace.join("\n")
                    )
                });
            assert!(
                lines[input_row].trim_start().starts_with("PS "),
                "typed prefix moved away from its prompt; prefix={typed:?}; cursor={:?}; visible={lines:?}",
                terminal.cursor_state().position
            );
        }

        let lines = terminal_visible_lines(&terminal);
        let prompt_count = lines
            .iter()
            .filter(|line| line.trim_start().starts_with("PS "))
            .count();
        assert_eq!(
            prompt_count,
            1,
            "startup/resize produced duplicate prompts before submission; cursor={:?}; visible={lines:?}",
            terminal.cursor_state().position
        );
        let input_row = lines
            .iter()
            .position(|line| line.contains(INPUT))
            .expect("typed input must be visible");
        let input_line = &lines[input_row];
        assert!(
            input_line.trim_start().starts_with("PS "),
            "input echo moved away from its prompt; cursor={:?}; visible={lines:?}",
            terminal.cursor_state().position
        );
        let input_end_col = input_line.find(INPUT).expect("input column") + INPUT.len();
        assert_eq!(
            terminal.cursor_state().position,
            GridPosition::new(input_row as i64, input_end_col as u16),
            "cursor does not follow the visible input; visible={lines:?}"
        );

        transport.write_input(b"\r\n").expect("submit input");
        let command_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < command_deadline {
            let output = transport.poll_output().expect("poll command output");
            if !output.bytes.is_empty() {
                terminal
                    .apply_bytes(&output.bytes)
                    .expect("apply command output");
                flush_terminal_responses(&mut terminal, &mut transport);
            }
            let lines = terminal_visible_lines(&terminal);
            if lines
                .iter()
                .enumerate()
                .any(|(row, line)| row > input_row && line.trim() == MARKER)
                && lines
                    .iter()
                    .enumerate()
                    .any(|(row, line)| row > input_row && line.trim_start().starts_with("PS "))
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let lines = terminal_visible_lines(&terminal);
        let output_row = lines
            .iter()
            .enumerate()
            .find_map(|(row, line)| (row > input_row && line.trim() == MARKER).then_some(row))
            .expect("command output must be below submitted input");
        let next_prompt_row = lines
            .iter()
            .enumerate()
            .find_map(|(row, line)| {
                (row > output_row && line.trim_start().starts_with("PS ")).then_some(row)
            })
            .expect("next prompt must be below command output");
        assert_eq!(
            terminal.cursor_state().position.row,
            next_prompt_row as i64,
            "cursor row must match the next visible prompt; visible={lines:?}"
        );
        let _ = transport.shutdown();
    }

    fn terminal_visible_lines(terminal: &TerminalEmulator) -> Vec<String> {
        let visible = terminal.visible_grid();
        visible
            .cells
            .chunks(usize::from(visible.viewport.size.cols.max(1)))
            .map(|cells| term_core::Line {
                cells: cells.to_vec(),
                hard_wrapped: false,
            })
            .map(|line| line.raw_text())
            .collect()
    }

    fn run_interactive_shell_activation_smoke(profile: LocalShellProfile) {
        let marker = b"panea-runtime-integration-smoke";
        let size = smoke_size();
        let mut transport = LocalPtyTransport::spawn(profile, size).expect("spawn shell");
        let mut query_terminal = TerminalEmulator::new(CoreTerminalSize::new(size.cols, size.rows));
        let mut parser = SemanticEscapeParser::new();
        let mut events = Vec::new();
        let mut bytes = Vec::new();
        let mut command_sent = false;
        let deadline = Instant::now() + Duration::from_secs(5);

        while Instant::now() < deadline {
            let output = transport.poll_output().expect("poll shell output");
            if !output.bytes.is_empty() {
                query_terminal
                    .apply_bytes(&output.bytes)
                    .expect("apply shell output for terminal queries");
                flush_terminal_responses(&mut query_terminal, &mut transport);
                for event in parser.parse(&output.bytes, BufferPosition::new(0, 0)) {
                    events.push(event.event.kind());
                }
                bytes.extend_from_slice(&output.bytes);
            }
            if !command_sent && events.contains(&SemanticEventKind::InputStarted) {
                transport
                    .write_input(b"Write-Output panea-runtime-integration-smoke\r\n")
                    .expect("write command");
                command_sent = true;
            }
            let observed_marker = bytes.windows(marker.len()).any(|window| window == marker);
            if observed_marker
                && [
                    SemanticEventKind::PromptStarted,
                    SemanticEventKind::PromptEnded,
                    SemanticEventKind::InputStarted,
                    SemanticEventKind::InputEnded,
                    SemanticEventKind::OutputStarted,
                    SemanticEventKind::OutputEnded,
                    SemanticEventKind::CommandFinished,
                    SemanticEventKind::CurrentWorkingDirectoryChanged,
                    SemanticEventKind::ShellMetadataChanged,
                ]
                .iter()
                .all(|kind| events.contains(kind))
            {
                let _ = transport.shutdown();
                return;
            }
            if output.closed {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let diagnostics = transport.diagnostics();
        let _ = transport.shutdown();
        panic!(
            "interactive shell integration did not complete; command_sent={command_sent}, bytes={}, events={events:?}, diagnostics={diagnostics:?}",
            bytes.len()
        );
    }

    #[test]
    #[ignore = "spawns a real bash process"]
    fn real_bash_emits_semantic_shell_events() {
        run_real_shell_semantic_smoke(
            ShellKind::Bash,
            LocalShellProfile::custom("bash", "bash").with_args([
                "-lc".to_owned(),
                shell_integration::verification_sequence(
                    ShellKind::Bash,
                    "panea-shell-integration-smoke",
                )
                .expect("bash verification sequence"),
            ]),
        );
    }

    #[test]
    #[ignore = "spawns a real zsh process"]
    fn real_zsh_emits_semantic_shell_events() {
        run_real_shell_semantic_smoke(
            ShellKind::Zsh,
            LocalShellProfile::custom("zsh", "zsh").with_args([
                "-lc".to_owned(),
                shell_integration::verification_sequence(
                    ShellKind::Zsh,
                    "panea-shell-integration-smoke",
                )
                .expect("zsh verification sequence"),
            ]),
        );
    }

    #[test]
    #[ignore = "spawns a real fish process"]
    fn real_fish_emits_semantic_shell_events() {
        run_real_shell_semantic_smoke(
            ShellKind::Fish,
            LocalShellProfile::custom("fish", "fish").with_args([
                "-c".to_owned(),
                shell_integration::verification_sequence(
                    ShellKind::Fish,
                    "panea-shell-integration-smoke",
                )
                .expect("fish verification sequence"),
            ]),
        );
    }

    fn run_real_shell_semantic_smoke(shell: ShellKind, profile: LocalShellProfile) {
        let marker = b"panea-shell-integration-smoke";
        let size = smoke_size();
        let mut transport = match LocalPtyTransport::spawn(profile.clone(), size) {
            Ok(transport) => transport,
            Err(error) => {
                eprintln!(
                    "skipping real {shell:?} semantic smoke because spawn failed for {}: {error}",
                    profile.program
                );
                return;
            }
        };
        let mut query_terminal = TerminalEmulator::new(CoreTerminalSize::new(size.cols, size.rows));
        let mut parser = SemanticEscapeParser::new();
        let mut bytes = Vec::new();
        let mut events = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);

        while std::time::Instant::now() < deadline {
            let output = transport.poll_output().expect("poll shell output");
            if !output.bytes.is_empty() {
                query_terminal
                    .apply_bytes(&output.bytes)
                    .expect("apply shell output for terminal queries");
                flush_terminal_responses(&mut query_terminal, &mut transport);
                events.extend(
                    parser
                        .parse(&output.bytes, BufferPosition::new(0, 0))
                        .into_iter()
                        .map(|parsed| parsed.event.kind()),
                );
                bytes.extend(output.bytes);
            }

            if bytes.windows(marker.len()).any(|window| window == marker)
                && events.contains(&SemanticEventKind::CommandFinished)
            {
                let _ = transport.shutdown();
                assert!(events.contains(&SemanticEventKind::ShellMetadataChanged));
                assert!(events.contains(&SemanticEventKind::OutputStarted));
                return;
            }

            if output.closed {
                break;
            }

            std::thread::sleep(Duration::from_millis(10));
        }

        let diagnostics = transport.diagnostics();
        let _ = transport.shutdown();
        panic!(
            "real {shell:?} semantic smoke did not observe expected events; bytes={}, events={events:?}, diagnostics={diagnostics:?}",
            bytes.len()
        );
    }

    #[test]
    fn fullscreen_chrome_hover_never_changes_native_window_mode() {
        let desktop_source = include_str!("main.rs");
        let platform_source = include_str!("../../../crates/platform-winit/src/lib.rs");
        for forbidden in [
            ["reveal_native_fullscreen", "_titlebar"].concat(),
            ["hide_native_fullscreen", "_titlebar"].concat(),
            ["NativeFullscreen", "TitlebarState"].concat(),
        ] {
            assert!(
                !desktop_source.contains(&forbidden) && !platform_source.contains(&forbidden),
                "fullscreen hover must not reconstruct native window state through {forbidden}"
            );
        }
    }
}
