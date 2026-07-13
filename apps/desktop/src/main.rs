use std::{
    any::Any,
    collections::{BTreeSet, HashMap},
    error::Error,
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use config_core::{
    AppConfig, ClipboardConfig, CommandBlockStyle, ConfigDiagnostic, ConfigDiagnosticSeverity,
    ConfigPlatform, DecorationStrategyConfig, InputOutputGroupingStyle, LinuxBackendConfig,
    LogLevel, MuxLayoutConfig, MuxSplitAxisConfig, MuxTransportConfig, PasteConfig,
    PresentModePreference, PromptDecorationStyle, ReloadPlan, ReloadableSection,
    ShellIntegrationActivationConfig, ShellProfile, ShellProfileKind, SshAuthMethod,
    SshKnownHostsPolicy, SshProfile, WindowModeConfig,
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
    MouseEvent, MouseEventKind, UrlOpener, WindowAction, WindowMode,
};
use platform_winit::{
    ClipboardBridge, DesktopUrlOpener, DesktopWindow, InputTranslator, WindowSettings,
    apply_window_mode, platform_capabilities,
};
use render_core::{
    CellPosition, CursorVisual, OverlayKind, OverlayPrimitive, RenderCell, RenderCellStyle,
    RenderColor, RenderCursorShape, RenderGrid, RenderInstrumentation, RenderOffset, RenderRect,
    RenderScene, SelectionVisual,
};
use render_wgpu::{
    AnimatedCursorImageCache, AnimatedCursorImageRequest, AnimatedCursorImageStatus,
    CursorAnimationRuntime, CursorAnimationSettings, CursorBlinkRuntime, DamageTracker,
    FrameDecision, FrameScheduler, GpuTerminalRenderer, PresentMode, RendererError,
    RendererOptions,
};
use security::KeychainProvider;
use security::{
    Osc52ClipboardDecision, Osc52ClipboardPolicy, Osc52ClipboardRequest as SecurityOsc52Request,
    Osc52ClipboardTarget, PlatformKeychainProvider, evaluate_osc52_clipboard_write,
};
use semantics::detect_url_hints;
use semantics::{
    BufferPosition, CommandStatus, IntegrationMode, RemoteMetadata, SemanticAction,
    SemanticActionResult, SemanticMetadata, SemanticRegionKind, SemanticSpan,
    SemanticTimelineStore, TerminalTextProvider,
};
use shell_integration::{
    IntegrationActivation, SemanticEscapeParser, ShellIntegrationActivationAction,
    ShellIntegrationActivationPlan, ShellIntegrationPolicy, ShellIntegrationRuntimeMode, ShellKind,
    detect_shell_kind,
};
use term_core::{
    CellAttributes, ClipboardTarget, Color, CursorShape, GridPosition, KeypadKey,
    Osc52ClipboardRequest, Selection, SelectionKind, TerminalCore, TerminalKey,
    TerminalKeyModifiers, TerminalMode, TerminalSize as CoreTerminalSize, encode_terminal_key,
};
use term_parser::TerminalEmulator;
use transport_core::{
    TerminalSize as TransportSize, TerminalTransport, TransportOutput, TransportResult,
    TransportState,
};
use transport_pty::{LocalPtyTransport, LocalShellKind, LocalShellProfile};
use transport_ssh::{SshConnectionProfile, SshTransport};
use unicode_segmentation::UnicodeSegmentation;
use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
};

fn main() {
    if let Some(code) = run_cli() {
        std::process::exit(code);
    }

    if let Err(error) = run() {
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
        "help" | "--help" | "-h" => {
            print_cli_help();
            Some(0)
        }
        _ => None,
    }
}

fn print_cli_help() {
    eprintln!("usage: panea doctor [window|renderer|config|shell|ssh|fonts|clipboard] [--json]");
    eprintln!("usage: panea shell-smoke [--json] [--timeout-ms <ms>]");
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
            "unknown doctor topic; expected window, renderer, config, shell, ssh, fonts, clipboard, platform, or performance"
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

fn doctor_runtime_snapshot(
    config: &AppConfig,
    config_parse_status: &str,
) -> diagnostics::DoctorRuntimeSnapshot {
    let gpu_probe = pollster::block_on(render_wgpu::probe_gpu_adapter());
    let clipboard = ClipboardBridge::new();
    let clipboard_diagnostic = clipboard.last_diagnostic().clone();
    let keychain = PlatformKeychainProvider::for_current_platform();
    let keychain_capability = keychain.capability();

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
        window_backend: Some(window_backend_label()),
        x11_wayland_status: Some(x11_wayland_status()),
        dpi_scale: None,
        font_discovery: font_discovery_label(config),
        config_parse_status: config_parse_status.to_owned(),
        shell_integration_status: "no active terminal session during doctor".to_owned(),
        clipboard_provider: format!(
            "arboard system clipboard {:?}: {}",
            clipboard_diagnostic.availability,
            clipboard_diagnostic
                .message
                .as_deref()
                .unwrap_or("provider initialized")
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
        ssh_provider_status: "ssh2 transport backend configured; host trust is explicit".to_owned(),
    }
}

fn window_backend_label() -> String {
    if cfg!(windows) {
        "winit/windows".to_owned()
    } else if cfg!(target_os = "macos") {
        "winit/macos".to_owned()
    } else if cfg!(target_os = "linux") {
        std::env::var("WINIT_UNIX_BACKEND")
            .map(|backend| format!("winit/linux requested={backend}"))
            .unwrap_or_else(|_| "winit/linux auto".to_owned())
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

fn run() -> Result<(), Box<dyn Error>> {
    let loaded_config = load_desktop_config()?;
    log_config_diagnostics(&loaded_config.diagnostics);
    let mut config = loaded_config.config;
    let mut config_watcher = loaded_config.watcher;
    let _ssh_session_profiles: Vec<SshConnectionProfile> = config
        .ssh_profiles
        .iter()
        .map(ssh_connection_profile)
        .collect();
    let event_loop = EventLoop::new()?;
    let desktop_window = DesktopWindow::create(&event_loop, &window_settings(&config))?;
    let window = desktop_window.window();
    let capabilities = platform_capabilities(&event_loop, &window);
    let _diagnostics =
        DesktopDiagnosticsPlaceholder::new(desktop_window.diagnostics().clone(), capabilities);
    let mut input_translator = InputTranslator::new();
    let mut clipboard = ClipboardBridge::new();
    let mut url_opener = DesktopUrlOpener::new();
    let mut current_window_mode = map_window_mode(config.window.mode);
    let mut clipboard_config = config.clipboard.clone();
    let mut paste_config = config.paste.clone();
    let mut osc52_policy = osc52_policy(&clipboard_config);

    let mut fonts = FontSystem::new(font_config(&config.font));
    let metrics = fonts.cell_metrics()?;
    let mut renderer = pollster::block_on(GpuTerminalRenderer::new(
        Arc::clone(&window),
        renderer_options(&config),
    ))?;
    if config.window.opacity < 1.0 && !renderer.transparency_active() {
        eprintln!(
            "window opacity fallback: GPU/window backend exposes only opaque composition; rendering remains fully opaque"
        );
    }
    let mut scheduler = FrameScheduler::new();
    let mut damage_tracker = DamageTracker::new();
    let mut performance_overlay =
        PerformanceOverlay::new(config.diagnostics.performance_overlay, "wgpu");
    let mut performance_budget = performance_budget(&config);
    let mut cursor_animator = CursorAnimationRuntime::new();
    let mut cursor_blink = CursorBlinkRuntime::new();
    let mut window_focused = true;
    let mut pointer_visible = true;
    let mut cursor_image_cache = AnimatedCursorImageCache::new();
    let mut cursor_image_status_reported: Option<String> = None;
    request_cursor_image_if_enabled(&mut cursor_image_cache, &config);
    let mut surface_size = window.inner_size();
    let mut mux_runtime =
        MuxRuntime::new(&config, metrics, surface_size.width, surface_size.height);

    scheduler.terminal_content_changed();

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Wait);

        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::RedrawRequested => {
                    let metrics = fonts.cell_metrics().ok();
                    let mut scene = scene_from_mux(
                        &mux_runtime,
                        metrics,
                        &config,
                        Some(&mut cursor_animator),
                        CursorPresentation {
                            blink_visible: cursor_blink.visible(),
                            window_focused,
                        },
                    );
                    if let Some(metrics) = metrics {
                        append_performance_overlay(
                            &mut scene,
                            &performance_overlay,
                            performance_budget,
                            metrics,
                        );
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
                            performance_overlay.record(instrumentation);
                            if matches!(config.diagnostics.log_level, LogLevel::Trace)
                                && let Some(text) =
                                    performance_overlay.render_text(performance_budget)
                            {
                                eprintln!("performance {text}");
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
                                surface_size = winit::dpi::PhysicalSize::new(width, height);
                                if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
                                    renderer.resize(width, height)
                                })) {
                                    eprintln!(
                                        "renderer resize panic boundary: {}",
                                        panic_payload(panic)
                                    );
                                }
                                if let Ok(metrics) = fonts.cell_metrics() {
                                    mux_runtime.resize_all(width, height, metrics, &config);
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

                                if let Some(changed) = mux_runtime.handle_modal_key(&key) {
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
                                        "toggle_fullscreen" => {
                                            current_window_mode = if matches!(
                                                current_window_mode,
                                                WindowMode::Windowed
                                            ) {
                                                WindowMode::BorderlessFullscreen
                                            } else {
                                                WindowMode::Windowed
                                            };
                                            let _ = apply_window_mode(&window, current_window_mode);
                                        }
                                        "restore_window_decorations" => {
                                            current_window_mode = WindowMode::Windowed;
                                            let _ = apply_window_mode(&window, current_window_mode);
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
                                            let _ = apply_window_mode(&window, current_window_mode);
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
                                if mux_runtime.append_search_text(&text) {
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
                                    let _ = apply_window_mode(&window, current_window_mode);
                                }
                                WindowAction::RestoreWindowDecorations => {
                                    current_window_mode = WindowMode::Windowed;
                                    let _ = apply_window_mode(&window, current_window_mode);
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
                                    let _ = apply_window_mode(&window, current_window_mode);
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
                            _ => {}
                        }
                    }
                }
            },
            Event::AboutToWait => {
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
                                &mut performance_overlay,
                                &mut performance_budget,
                                &mut renderer,
                                &window,
                            ) {
                                Ok(reloaded) => {
                                    request_cursor_image_if_enabled(
                                        &mut cursor_image_cache,
                                        &config,
                                    );
                                    cursor_image_status_reported = None;
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

                if mux_runtime.poll_outputs(&mut clipboard, &osc52_policy, &clipboard_config) {
                    scheduler.terminal_content_changed();
                }

                match cursor_image_cache.poll() {
                    AnimatedCursorImageStatus::Ready(image) => {
                        let key = format!("ready:{}", image.path.display());
                        if cursor_image_status_reported.as_deref() != Some(&key) {
                            for warning in image.warnings {
                                eprintln!("cursor image warning: {warning}");
                            }
                            cursor_image_status_reported = Some(key);
                        }
                    }
                    AnimatedCursorImageStatus::Failed { path, message } => {
                        let key = format!("failed:{}:{message}", path.display());
                        if cursor_image_status_reported.as_deref() != Some(&key) {
                            eprintln!("cursor image {} failed: {message}", path.display());
                            cursor_image_status_reported = Some(key);
                        }
                    }
                    AnimatedCursorImageStatus::Disabled
                    | AnimatedCursorImageStatus::Loading { .. } => {}
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
                let blink_delay = cursor_blink.next_frame_after();
                let next_delay = match (animation_delay, blink_delay) {
                    (Some(animation), Some(blink)) => Some(animation.min(blink)),
                    (Some(delay), None) | (None, Some(delay)) => Some(delay),
                    (None, None) => None,
                };
                if let Some(delay) = next_delay {
                    if animation_delay.is_some() {
                        scheduler.animation_changed();
                    }
                    target.set_control_flow(ControlFlow::WaitUntil(Instant::now() + delay));
                }

                if matches!(scheduler.next_frame(), FrameDecision::FrameNeeded(_)) {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    })?;

    Ok(())
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
    performance_overlay: &mut PerformanceOverlay,
    runtime_performance_budget: &mut PerformanceBudget,
    renderer: &mut GpuTerminalRenderer,
    window: &winit::window::Window,
) -> Result<bool, String> {
    if plan.live.is_empty() {
        return Ok(false);
    }

    if plan.live.contains(&ReloadableSection::Font) {
        let mut reloaded_fonts = FontSystem::new(font_config(&next.font));
        reloaded_fonts
            .cell_metrics()
            .map_err(|error| format!("font reload failed: {error}"))?;
        *fonts = reloaded_fonts;
    }

    for section in &plan.live {
        match section {
            ReloadableSection::Colors => current.colors = next.colors.clone(),
            ReloadableSection::Cursor => current.cursor = next.cursor.clone(),
            ReloadableSection::Diagnostics => {
                current.diagnostics = next.diagnostics.clone();
                performance_overlay.set_enabled(current.diagnostics.performance_overlay);
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
            ReloadableSection::Performance => {
                renderer.set_glyph_cache_capacity(next.performance.glyph_cache_entries);
                current.performance = next.performance.clone();
                *runtime_performance_budget = performance_budget(current);
            }
            ReloadableSection::VisualSemantics => {
                current.visual_theme = next.visual_theme.clone();
                current.command_blocks = next.command_blocks.clone();
                current.prompt_decorations = next.prompt_decorations.clone();
                current.shell_integration = next.shell_integration.clone();
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
        fps,
    }
}

fn request_cursor_image_if_enabled(cache: &mut AnimatedCursorImageCache, config: &AppConfig) {
    if !config.cursor.image.enabled {
        cache.disable();
        return;
    }

    cache.request(AnimatedCursorImageRequest {
        path: PathBuf::from(&config.cursor.image.path),
        fps: config.cursor.image.fps,
        max_size_kb: config.performance.max_cursor_asset_size_kb,
        warn_if_expensive: config.cursor.image.warn_if_expensive,
    });
}

fn performance_budget(config: &AppConfig) -> PerformanceBudget {
    PerformanceBudget {
        max_frame_time: Duration::from_millis(u64::from(config.performance.max_frame_time_ms)),
        ..PerformanceBudget::default()
    }
}

fn spawn_session_transport(
    config: &AppConfig,
    spec: &SessionSpec,
    size: TransportSize,
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
            let transport = LocalPtyTransport::spawn(profile, size)?;
            Ok(InitialTransport {
                transport: PaneTransport::Local(transport),
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
            let parse_semantic_events = config.shell_integration.enabled
                && profile.shell_integration
                && !matches!(
                    config.shell_integration.activation,
                    ShellIntegrationActivationConfig::Disabled
                );
            let mut connection = ssh_connection_profile(profile);
            if let Some(directory) = &spec.working_directory {
                connection.remote_working_directory = Some(directory.clone());
            }
            Ok(InitialTransport {
                transport: PaneTransport::connecting_ssh(connection, size),
                semantic_mode: if parse_semantic_events {
                    IntegrationMode::EscapeSequences
                } else {
                    IntegrationMode::Disabled
                },
                parse_semantic_events,
                activation_diagnostics: vec![if parse_semantic_events {
                    format!(
                        "SSH profile '{}' accepts remote semantic markers; remote hooks must be installed",
                        profile.name
                    )
                } else {
                    format!(
                        "SSH semantic integration disabled for profile '{}'",
                        profile.name
                    )
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
    requested_size: TransportSize,
    pending_input: Vec<u8>,
}

enum PaneTransport {
    Local(LocalPtyTransport),
    ConnectingSsh(PendingSshTransport),
    Ssh(SshTransport),
    Failed { message: String, reported: bool },
}

impl PaneTransport {
    fn connecting_ssh(profile: SshConnectionProfile, size: TransportSize) -> Self {
        let (sender, result) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let transport = SshTransport::connect(profile, size);
            let _ = sender.send(transport);
        });
        Self::ConnectingSsh(PendingSshTransport {
            result,
            requested_size: size,
            pending_input: Vec::new(),
        })
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
                transport.resize(pending.requested_size)?;
                if !pending.pending_input.is_empty() {
                    transport.write_input(&pending.pending_input)?;
                }
                *self = Self::Ssh(transport);
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
        _ => TerminalKey::Character(
            event
                .text
                .as_ref()
                .filter(|text| !text.is_empty())
                .unwrap_or(&event.logical_key)
                .clone(),
        ),
    };
    Some(key)
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
        match evaluate_osc52_clipboard_write(
            &security_osc52_request(request, session_is_remote),
            policy,
        ) {
            Osc52ClipboardDecision::Allow { text, bytes } => {
                copy_text_with_diagnostics(clipboard, &text, config, "OSC 52");
                if config.log_operations {
                    eprintln!("clipboard OSC 52: accepted {bytes} byte request");
                }
            }
            Osc52ClipboardDecision::PromptRequired { reason } => {
                eprintln!("clipboard OSC 52 blocked pending UI confirmation: {reason}");
            }
            Osc52ClipboardDecision::Deny { reason } => {
                if config.log_operations {
                    eprintln!("clipboard OSC 52 denied: {reason}");
                }
            }
        }
    }
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

fn flush_terminal_responses(terminal: &mut TerminalEmulator, transport: &mut PaneTransport) {
    let responses = terminal.state_mut().take_pending_output();
    if !responses.is_empty() {
        write_transport_input(transport, &responses);
    }
}

fn write_transport_input(transport: &mut PaneTransport, bytes: &[u8]) {
    match catch_unwind(AssertUnwindSafe(|| transport.write_input(bytes))) {
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
        mode: map_window_mode(config.window.mode),
        linux_backend: map_linux_backend(config.window.linux_backend),
        decoration_mode: map_decoration_mode(config.window.decoration_strategy),
        opacity: config.window.opacity,
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
}

impl MuxRuntime {
    fn new(config: &AppConfig, metrics: CellMetrics, width: u32, height: u32) -> Self {
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
            let pane = PaneRuntime::new(config, &spec, initial_size, metrics);
            let status = if pane.transport.is_some() {
                SessionStatus::Running
            } else {
                SessionStatus::Failed {
                    message: "transport failed to start".to_owned(),
                }
            };
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
        let pane = PaneRuntime::new(config, &spec, size, metrics);
        mark_session_status(&mut self.model, pane_id, SessionStatus::Running);
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

    fn handle_modal_key(&mut self, event: &KeyEvent) -> Option<bool> {
        self.active_pane_mut()?.handle_modal_key(event)
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
        if let Some(tab_id) = self.tab_at_mouse(mouse, metrics, config) {
            if matches!(mouse.kind, MouseEventKind::Pressed(MouseButton::Left)) {
                if self.model.switch_tab(tab_id).is_ok() {
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
            let width = config
                .mux
                .tab_title_format
                .replace("{index}", &(index + 1).to_string())
                .replace("{title}", &tab.name)
                .replace("{workspace}", &workspace.name)
                .chars()
                .count()
                .saturating_add(2);
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
            if poll.error {
                status_updates.push((
                    *pane_id,
                    SessionStatus::Failed {
                        message: "transport error; see pane output and diagnostics".to_owned(),
                    },
                ));
            } else if poll.closed {
                status_updates.push((*pane_id, SessionStatus::Exited { exit_code: None }));
            }
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
    parse_semantic_events: bool,
    remote_session: bool,
    transport: Option<PaneTransport>,
    mouse_protocol: MouseProtocolState,
    selection_anchor: Option<GridPosition>,
    selection_kind: SelectionKind,
    keyboard_selection: Option<KeyboardSelection>,
    search: PaneSearch,
    /// Per-command presentation override. `true` is collapsed, `false` keeps
    /// an otherwise auto-collapsed block expanded. Raw terminal data is untouched.
    command_output_collapsed: HashMap<u64, bool>,
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
    ) -> Self {
        let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(size.cols, size.rows));
        let transport_size = terminal_transport_size(size, metrics);
        let mut semantic_timeline = SemanticTimelineStore::new();
        let (transport, parse_semantic_events) =
            match spawn_session_transport(config, spec, transport_size) {
                Ok(initial) => {
                    semantic_timeline.set_integration_mode(initial.semantic_mode);
                    if let Some(metadata) = initial.remote_metadata {
                        semantic_timeline.apply_event(
                            semantics::SemanticEvent::RemoteMetadataChanged {
                                position: BufferPosition::new(0, 0),
                                metadata,
                            },
                        );
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

        Self {
            terminal,
            semantic_parser: SemanticEscapeParser::new(),
            semantic_timeline,
            parse_semantic_events,
            remote_session: matches!(spec.transport, SessionTransportKind::Ssh),
            transport,
            mouse_protocol: MouseProtocolState::default(),
            selection_anchor: None,
            selection_kind: SelectionKind::Normal,
            keyboard_selection: None,
            search: PaneSearch::default(),
            command_output_collapsed: HashMap::new(),
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

    fn handle_modal_key(&mut self, event: &KeyEvent) -> Option<bool> {
        if self.search.input_active {
            return Some(self.handle_search_key(event));
        }
        if self.keyboard_selection.is_some() {
            return Some(self.handle_keyboard_selection_key(event));
        }
        None
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
        let _ = self
            .terminal
            .resize(CoreTerminalSize::new(size.cols, size.rows));
        if let Some(transport) = self.transport.as_mut() {
            resize_transport(transport, terminal_transport_size(size, metrics));
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        if let Some(transport) = self.transport.as_mut() {
            write_transport_input(transport, bytes);
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
        for _ in 0..64 {
            let output = match catch_unwind(AssertUnwindSafe(|| transport.poll_output())) {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    eprintln!("transport poll error: {error}");
                    let _ = self
                        .terminal
                        .apply_bytes(format!("\r\ntransport error: {error}\r\n").as_bytes());
                    stats.content_changed = true;
                    stats.error = true;
                    break;
                }
                Err(panic) => {
                    eprintln!("transport poll panic boundary: {}", panic_payload(panic));
                    break;
                }
            };
            if output.bytes.is_empty() && output.lifecycle.is_empty() && !output.closed {
                break;
            }

            if !output.bytes.is_empty() {
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
                        self.semantic_timeline.apply_event(if self.remote_session {
                            event.in_remote_session()
                        } else {
                            event
                        });
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
                );
                flush_terminal_responses(&mut self.terminal, transport);
                stats.content_changed = true;
            }
            if output.closed {
                stats.content_changed = true;
                stats.closed = true;
                break;
            }
        }
        stats
    }

    fn shutdown(&mut self) {
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

fn scene_from_mux(
    runtime: &MuxRuntime,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    cursor_animator: Option<&mut CursorAnimationRuntime>,
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
        if let Some(cursor_animator) = cursor_animator {
            cursor_animator.populate_scene(&mut scene, metrics, cursor_animation_settings(config));
        }
    }

    scene
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
    budget: PerformanceBudget,
    metrics: CellMetrics,
) {
    let Some(lines) = overlay.render_lines(budget) else {
        return;
    };
    let cols = scene.grid.columns.max(1);
    let max_chars = usize::from(cols.saturating_sub(4).max(20));
    let line_height = metrics.cell_height.ceil().max(14.0) as u32;
    let padding = 6u32;
    let panel_width = ((max_chars as f32 * metrics.cell_width).ceil() as u32)
        .saturating_add(padding.saturating_mul(2));
    let x = ((f32::from(cols) * metrics.cell_width).ceil() as i32)
        .saturating_sub(panel_width as i32)
        .saturating_sub(8)
        .max(0);
    let mut y = 8i32;

    for line in lines.into_iter().take(4) {
        let label = truncate_overlay_label(&line, max_chars);
        scene.semantic_overlays.push(OverlayPrimitive {
            kind: OverlayKind::PerformanceOverlay,
            bounds: RenderRect {
                x,
                y,
                width: panel_width,
                height: line_height.saturating_add(4),
            },
            color: RenderColor {
                red: 10,
                green: 14,
                blue: 20,
                alpha: 210,
            },
            border_color: Some(RenderColor {
                red: 90,
                green: 104,
                blue: 122,
                alpha: 180,
            }),
            border_width_px: 1,
            corner_radius_px: 4,
            z_index: 1000,
            label: Some(label),
            label_color: None,
        });
        y = y.saturating_add(line_height as i32 + 5);
    }
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

    fn test_pane(cols: u16, rows: u16) -> PaneRuntime {
        PaneRuntime {
            terminal: TerminalEmulator::new(CoreTerminalSize::new(cols, rows)),
            semantic_parser: SemanticEscapeParser::new(),
            semantic_timeline: SemanticTimelineStore::new(),
            parse_semantic_events: false,
            remote_session: false,
            transport: None,
            mouse_protocol: MouseProtocolState::default(),
            selection_anchor: None,
            selection_kind: SelectionKind::Normal,
            keyboard_selection: None,
            search: PaneSearch::default(),
            command_output_collapsed: HashMap::new(),
        }
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

    fn run_interactive_shell_activation_smoke(profile: LocalShellProfile) {
        let marker = b"panea-runtime-integration-smoke";
        let mut transport = LocalPtyTransport::spawn(profile, smoke_size()).expect("spawn shell");
        let mut parser = SemanticEscapeParser::new();
        let mut events = Vec::new();
        let mut bytes = Vec::new();
        let mut command_sent = false;
        let deadline = Instant::now() + Duration::from_secs(5);

        while Instant::now() < deadline {
            let output = transport.poll_output().expect("poll shell output");
            if !output.bytes.is_empty() {
                answer_real_shell_terminal_queries(&mut transport, &output.bytes);
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
        let mut transport = match LocalPtyTransport::spawn(profile.clone(), smoke_size()) {
            Ok(transport) => transport,
            Err(error) => {
                eprintln!(
                    "skipping real {shell:?} semantic smoke because spawn failed for {}: {error}",
                    profile.program
                );
                return;
            }
        };
        let mut parser = SemanticEscapeParser::new();
        let mut bytes = Vec::new();
        let mut events = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);

        while std::time::Instant::now() < deadline {
            let output = transport.poll_output().expect("poll shell output");
            if !output.bytes.is_empty() {
                answer_real_shell_terminal_queries(&mut transport, &output.bytes);
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

    fn answer_real_shell_terminal_queries(transport: &mut LocalPtyTransport, bytes: &[u8]) {
        if bytes
            .windows(b"\x1b[6n".len())
            .any(|window| window == b"\x1b[6n")
        {
            let _ = transport.write_input(b"\x1b[1;1R");
        }
    }
}
