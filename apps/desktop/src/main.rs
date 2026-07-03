use std::{
    any::Any,
    collections::{BTreeSet, HashMap},
    error::Error,
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use config_core::{
    AppConfig, ClipboardConfig, CommandBlockStyle, ConfigDiagnosticSeverity,
    DecorationStrategyConfig, InputOutputGroupingStyle, LinuxBackendConfig, LogLevel, PasteConfig,
    PresentModePreference, PromptDecorationStyle, ReloadPlan, ReloadableSection,
    ShellIntegrationActivationConfig, ShellProfile, ShellProfileKind, SshAuthMethod,
    SshKnownHostsPolicy, SshProfile, WindowModeConfig,
};
use diagnostics::{PerformanceBudget, PerformanceOverlay};
use font_system::{CellMetrics, FontConfig as RuntimeFontConfig, FontSource, FontSystem};
use mux::{
    LogicalRect, MuxAction, MuxModel, PaneId, PaneLayout, SessionSpec, SessionStatus, SplitAxis,
    TerminalGridSize,
};
use platform_core::{
    DecorationMode, InputEvent, KeyEvent, KeyModifiers, KeyState, LinuxWindowBackend, MouseButton,
    MouseEvent, MouseEventKind, WindowAction, WindowMode,
};
use platform_winit::{
    ClipboardBridge, DesktopWindow, InputTranslator, WindowSettings, apply_window_mode,
    platform_capabilities,
};
use render_core::{
    CellPosition, CursorVisual, OverlayKind, OverlayPrimitive, RenderCell, RenderCellStyle,
    RenderColor, RenderCursorShape, RenderGrid, RenderInstrumentation, RenderRect, RenderScene,
};
use render_wgpu::{
    AnimatedCursorImageCache, AnimatedCursorImageRequest, AnimatedCursorImageStatus,
    CursorAnimationRuntime, CursorAnimationSettings, FrameDecision, FrameScheduler,
    GpuTerminalRenderer, PresentMode, RendererError, RendererOptions,
};
use security::KeychainProvider;
use security::{
    Osc52ClipboardDecision, Osc52ClipboardPolicy, Osc52ClipboardRequest as SecurityOsc52Request,
    Osc52ClipboardTarget, PlatformKeychainProvider, evaluate_osc52_clipboard_write,
};
use semantics::detect_url_hints;
use semantics::{
    BufferPosition, CommandStatus, IntegrationMode, SemanticMetadata, SemanticRegionKind,
    SemanticTimelineStore,
};
use shell_integration::{
    IntegrationActivation, SemanticEscapeParser, ShellIntegrationActivationAction,
    ShellIntegrationActivationPlan, ShellIntegrationPolicy, ShellIntegrationRuntimeMode, ShellKind,
    detect_shell_kind,
};
use term_core::{
    CellAttributes, ClipboardTarget, Color, CursorShape, Osc52ClipboardRequest, TerminalCore,
    TerminalMode, TerminalSize as CoreTerminalSize,
};
use term_parser::TerminalEmulator;
use transport_core::{TerminalSize as TransportSize, TerminalTransport};
use transport_pty::{LocalPtyTransport, LocalShellKind, LocalShellProfile};
use transport_ssh::SshConnectionProfile;
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
        "help" | "--help" | "-h" => {
            print_cli_help();
            Some(0)
        }
        _ => None,
    }
}

fn print_cli_help() {
    eprintln!("usage: panea doctor [window|renderer|config|shell|ssh|fonts|clipboard] [--json]");
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

fn doctor_input() -> diagnostics::DoctorInput {
    let config_load_options = config_toml::ConfigLoadOptions::default();
    match config_toml::load(config_load_options) {
        Ok(loaded) => {
            let runtime = doctor_runtime_snapshot(&loaded.config, "loaded");
            diagnostics::DoctorInput {
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                config_source: config_source_text(&loaded.source),
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
    let chain = fonts.resolve_fallback_chain();
    let mut labels = Vec::new();
    labels.push(format_font_descriptor("primary", &chain.primary));
    labels.extend(
        chain
            .fallbacks
            .iter()
            .map(|fallback| format_font_descriptor("fallback", fallback)),
    );
    labels.join("; ")
}

fn format_font_descriptor(role: &str, descriptor: &font_system::FontDescriptor) -> String {
    let source = match &descriptor.source {
        FontSource::File(path) => format!("file:{}", path.display()),
        FontSource::Memory => "memory".to_owned(),
        FontSource::Unresolved => "unresolved".to_owned(),
    };
    format!("{role}:{}={source}", descriptor.family)
}

fn run() -> Result<(), Box<dyn Error>> {
    let config_load_options = config_toml::ConfigLoadOptions::default();
    let loaded_config = config_toml::load(config_load_options.clone())?;
    log_config_diagnostics(&loaded_config.diagnostics);
    let mut config = loaded_config.config;
    let mut config_watcher = config_toml::ConfigWatcher::new(config_load_options);
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
    let mut scheduler = FrameScheduler::new();
    let mut performance_overlay =
        PerformanceOverlay::new(config.diagnostics.performance_overlay, "wgpu");
    let mut performance_budget = performance_budget(&config);
    let mut cursor_animator = CursorAnimationRuntime::new();
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
                    );
                    if let Some(metrics) = metrics {
                        append_performance_overlay(
                            &mut scene,
                            &performance_overlay,
                            performance_budget,
                            metrics,
                        );
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
                                                mux_runtime.paste_into_active(
                                                    &text,
                                                    &clipboard_config,
                                                    &paste_config,
                                                );
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
                                            if let Some(action) = MuxAction::named(&action) {
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
                                } else if let Some(bytes) = input_bytes(&key) {
                                    cursor_animator.record_typing();
                                    mux_runtime.write_active(&bytes);
                                }
                            }
                            InputEvent::Mouse(mouse) => {
                                if let Ok(metrics) = fonts.cell_metrics() {
                                    mux_runtime.handle_mouse(
                                        mouse,
                                        metrics,
                                        &config,
                                        &clipboard_config,
                                        &paste_config,
                                        &mut clipboard,
                                    );
                                }
                            }
                            InputEvent::Ime(platform_core::ImeEvent::Commit { text }) => {
                                cursor_animator.record_typing();
                                mux_runtime.write_active(text.as_bytes());
                            }
                            InputEvent::Focused(focused) => {
                                mux_runtime.send_focus_event(focused);
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
                match config_watcher.poll() {
                    config_toml::ConfigWatchEvent::Unchanged => {}
                    config_toml::ConfigWatchEvent::Pending { path } => {
                        if matches!(config.diagnostics.log_level, LogLevel::Debug | LogLevel::Trace)
                        {
                            eprintln!(
                                "config reload pending{}",
                                path.as_ref()
                                    .map(|path| format!(" for {}", path.display()))
                                    .unwrap_or_default()
                            );
                        }
                    }
                    config_toml::ConfigWatchEvent::Reloaded(loaded) => {
                        let loaded = *loaded;
                        log_config_diagnostics(&loaded.diagnostics);
                        let plan = config.reload_plan_from(&loaded.config);
                        log_reload_plan(&plan);
                        match apply_live_config_reload(
                            &mut config,
                            loaded.config,
                            &plan,
                            &mut fonts,
                            &mut clipboard_config,
                            &mut paste_config,
                            &mut osc52_policy,
                            &mut performance_overlay,
                            &mut performance_budget,
                            &window,
                        ) {
                            Ok(reloaded) => {
                                request_cursor_image_if_enabled(&mut cursor_image_cache, &config);
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
                    config_toml::ConfigWatchEvent::Failed { path, error } => {
                        eprintln!(
                            "config reload failed{}: {error}; keeping previous valid config",
                            path.as_ref()
                                .map(|path| format!(" for {}", path.display()))
                                .unwrap_or_default()
                        );
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
                if let Some(delay) = cursor_animator.next_frame_after(cursor_settings) {
                    scheduler.animation_changed();
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
    }
}

fn cursor_animation_settings(config: &AppConfig) -> CursorAnimationSettings {
    CursorAnimationSettings {
        enabled: config.cursor.animations_enabled,
        smooth_movement: config.cursor.smooth_movement,
        typing_pulse: config.cursor.typing_pulse,
        typing_stretch: config.cursor.typing_stretch,
        trail: config.cursor.trail,
        blink_easing: config.cursor.blink_easing,
        short_lived_glow: config.cursor.short_lived_glow,
        fps: config.performance.max_animation_fps,
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

fn spawn_initial_transport(
    config: &AppConfig,
    size: TransportSize,
) -> transport_core::TransportResult<InitialTransport> {
    let (profile, activation) = initial_local_shell_profile(config);
    let parse_semantic_events = activation.parses_escape_sequences();
    let semantic_mode = semantic_mode_for_activation(&activation);

    let transport = LocalPtyTransport::spawn(profile, size)?;
    Ok(InitialTransport {
        transport,
        semantic_mode,
        parse_semantic_events,
        activation_diagnostics: activation.diagnostics,
    })
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
    transport: LocalPtyTransport,
    semantic_mode: IntegrationMode,
    parse_semantic_events: bool,
    activation_diagnostics: Vec<String>,
}

fn initial_local_shell_profile(
    config: &AppConfig,
) -> (LocalShellProfile, ShellIntegrationActivationPlan) {
    let mut profile = selected_shell_profile(config)
        .map(local_shell_profile)
        .unwrap_or_else(LocalShellProfile::default_for_platform);
    let shell = shell_kind_for_local_profile(&profile);
    let policy = shell_integration_policy(config);
    let activation = shell_integration::activation_plan(&policy, &profile.name, shell);
    apply_shell_integration_activation(&mut profile, &activation);
    (profile, activation)
}

fn local_shell_profile(profile: &ShellProfile) -> LocalShellProfile {
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
            profile.startup_command = Some(hook);
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

fn rows_for_height(height: u32, metrics: CellMetrics) -> u16 {
    ((height as f32 / metrics.cell_height).floor() as u16).max(1)
}

fn input_bytes(event: &KeyEvent) -> Option<Vec<u8>> {
    if event.state != KeyState::Pressed
        || event.modifiers.ctrl
        || event.modifiers.alt
        || event.modifiers.super_key
    {
        return None;
    }

    match event.logical_key.as_str() {
        "Enter" => Some(b"\r".to_vec()),
        "Backspace" => Some(vec![0x08]),
        "Tab" => Some(b"\t".to_vec()),
        "Escape" => Some(vec![0x1b]),
        _ => event
            .text
            .as_ref()
            .filter(|text| !text.is_empty())
            .map(|text| text.as_bytes().to_vec()),
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

fn mouse_reporting_enabled(modes: &BTreeSet<TerminalMode>) -> bool {
    modes.contains(&TerminalMode::MouseReporting)
        || modes.contains(&TerminalMode::MouseCellMotion)
        || modes.contains(&TerminalMode::MouseAllMotion)
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

fn shutdown_transport(transport: Option<&mut LocalPtyTransport>) {
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

fn flush_terminal_responses(terminal: &mut TerminalEmulator, transport: &mut LocalPtyTransport) {
    let responses = terminal.state_mut().take_pending_output();
    if !responses.is_empty() {
        write_transport_input(transport, &responses);
    }
}

fn write_transport_input(transport: &mut LocalPtyTransport, bytes: &[u8]) {
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

struct MuxRuntime {
    model: MuxModel,
    panes: HashMap<PaneId, PaneRuntime>,
    surface_cols: u16,
    surface_rows: u16,
    performance: RuntimePerformanceCounters,
}

impl MuxRuntime {
    fn new(config: &AppConfig, metrics: CellMetrics, width: u32, height: u32) -> Self {
        let mut model = MuxModel::new(session_spec_for_config(config));
        model.active_workspace_mut().name = config.mux.default_workspace.clone();

        let surface_cols = cols_for_width(width, metrics).max(1);
        let surface_rows = rows_for_height(height, metrics).max(1);
        let pane_id = model.active_tab().active_pane;
        let pane_size = TerminalGridSize::new(
            surface_cols,
            surface_rows
                .saturating_sub(tab_bar_rows(&model, config))
                .max(1),
        );
        let pane = PaneRuntime::new(config, pane_size, metrics);
        let mut panes = HashMap::new();
        panes.insert(pane_id, pane);
        mark_session_status(&mut model, pane_id, SessionStatus::Running);

        let mut runtime = Self {
            model,
            panes,
            surface_cols,
            surface_rows,
            performance: RuntimePerformanceCounters::new(),
        };
        runtime.resize_all(width, height, metrics, config);
        runtime
    }

    fn resize_all(&mut self, width: u32, height: u32, metrics: CellMetrics, config: &AppConfig) {
        self.surface_cols = cols_for_width(width, metrics).max(1);
        self.surface_rows = rows_for_height(height, metrics).max(1);
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
                self.new_tab(config, metrics, width, height);
                Ok(())
            }
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
            MuxAction::FocusDirection(direction) => self
                .model
                .focus_direction(direction)
                .map(|_| self.resize_active_tab(metrics, config)),
            MuxAction::ResizePane(direction) => self
                .model
                .resize_active_pane(direction, config.mux.pane_resize_step as f32)
                .map(|_| self.resize_active_tab(metrics, config)),
            MuxAction::ZoomPane => {
                self.model.toggle_zoom_active_pane();
                self.resize_active_tab(metrics, config);
                Ok(())
            }
            MuxAction::MovePane => {
                eprintln!("mux move_pane is reserved for layout persistence after pane drag UI");
                Ok(())
            }
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

    fn new_tab(&mut self, config: &AppConfig, metrics: CellMetrics, width: u32, height: u32) {
        let tab_number = self.model.active_workspace().active_window().tabs.len() + 1;
        match self
            .model
            .new_tab(tab_number.to_string(), session_spec_for_config(config))
        {
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
        match self
            .model
            .split_active_pane(axis, session_spec_for_config(config))
        {
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
        let pane = PaneRuntime::new(config, size, metrics);
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
            && pane.terminal.modes().contains(&TerminalMode::FocusEvents)
        {
            let bytes = if focused { b"\x1b[I" } else { b"\x1b[O" };
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
    ) {
        let Some((pane_id, local_mouse)) = self.local_mouse_event(mouse, metrics, config) else {
            return;
        };
        if matches!(mouse.kind, MouseEventKind::Pressed(_)) {
            let _ = self.model.focus_pane(pane_id);
        }
        let Some(pane) = self.panes.get_mut(&pane_id) else {
            return;
        };
        let modes = pane.terminal.modes();
        if let Some(bytes) = pane
            .mouse_protocol
            .report_bytes(local_mouse, metrics, &modes)
        {
            pane.write_input(&bytes);
        } else if should_middle_click_paste(&local_mouse, &modes, clipboard_config)
            && let Ok(text) = clipboard.paste_text()
        {
            let bytes = paste_bytes(&text, clipboard_config, paste_config, false);
            pane.write_input(&bytes);
        }
    }

    fn local_mouse_event(
        &self,
        mouse: MouseEvent,
        metrics: CellMetrics,
        config: &AppConfig,
    ) -> Option<(PaneId, MouseEvent)> {
        let x_cells = (mouse.x as f32 / metrics.cell_width).floor();
        let y_cells = (mouse.y as f32 / metrics.cell_height).floor();
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
                    x: (mouse.x as f32 - layout.rect.x * metrics.cell_width).max(0.0) as f64,
                    y: (mouse.y as f32 - layout.rect.y * metrics.cell_height).max(0.0) as f64,
                    ..mouse
                };
                (layout.pane_id, local)
            })
    }

    fn poll_outputs(
        &mut self,
        clipboard: &mut ClipboardBridge,
        policy: &Osc52ClipboardPolicy,
        clipboard_config: &ClipboardConfig,
    ) -> bool {
        let mut content_changed = false;
        for pane in self.panes.values_mut() {
            let poll = pane.poll_output(clipboard, policy, clipboard_config);
            self.performance.record_pty_bytes(poll.pty_bytes);
            self.performance.record_parser_bytes(poll.parser_bytes);
            if poll.content_changed {
                content_changed = true;
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

    fn active_pane(&self) -> Option<&PaneRuntime> {
        self.panes.get(&self.model.active_tab().active_pane)
    }

    fn active_pane_mut(&mut self) -> Option<&mut PaneRuntime> {
        let pane_id = self.model.active_tab().active_pane;
        self.panes.get_mut(&pane_id)
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
        for pane in self.panes.values_mut() {
            pane.shutdown();
        }
    }
}

struct PaneRuntime {
    terminal: TerminalEmulator,
    semantic_parser: SemanticEscapeParser,
    semantic_timeline: SemanticTimelineStore,
    parse_semantic_events: bool,
    transport: Option<LocalPtyTransport>,
    mouse_protocol: MouseProtocolState,
}

#[derive(Debug, Clone, Copy, Default)]
struct PanePollStats {
    content_changed: bool,
    pty_bytes: u64,
    parser_bytes: u64,
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
    fn new(config: &AppConfig, size: TerminalGridSize, metrics: CellMetrics) -> Self {
        let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(size.cols, size.rows));
        let transport_size = terminal_transport_size(size, metrics);
        let mut semantic_timeline = SemanticTimelineStore::new();
        let (transport, parse_semantic_events) =
            match spawn_initial_transport(config, transport_size) {
                Ok(initial) => {
                    semantic_timeline.set_integration_mode(initial.semantic_mode);
                    for diagnostic in initial.activation_diagnostics {
                        eprintln!("shell integration: {diagnostic}");
                    }
                    (Some(initial.transport), initial.parse_semantic_events)
                }
                Err(error) => {
                    let _ = terminal.apply_bytes(
                        format!("failed to spawn local shell: {error}\r\n").as_bytes(),
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
            transport,
            mouse_protocol: MouseProtocolState::default(),
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
                    break;
                }
                Err(panic) => {
                    eprintln!("transport poll panic boundary: {}", panic_payload(panic));
                    break;
                }
            };
            if output.bytes.is_empty() && output.lifecycle.is_empty() {
                break;
            }

            if !output.bytes.is_empty() {
                let byte_count = u64::try_from(output.bytes.len()).unwrap_or(u64::MAX);
                stats.pty_bytes = stats.pty_bytes.saturating_add(byte_count);
                let cursor = self.terminal.cursor_state();
                let semantic_position =
                    BufferPosition::new(cursor.position.row, cursor.position.col);
                if self.parse_semantic_events {
                    for parsed in self.semantic_parser.parse(&output.bytes, semantic_position) {
                        self.semantic_timeline.apply_event(parsed.event);
                    }
                }
                if let Err(panic) = catch_unwind(AssertUnwindSafe(|| {
                    let _ = self.terminal.apply_bytes(&output.bytes);
                })) {
                    eprintln!("terminal parser panic boundary: {}", panic_payload(panic));
                    break;
                }
                stats.parser_bytes = stats.parser_bytes.saturating_add(byte_count);
                process_pending_clipboard_requests(
                    &mut self.terminal,
                    clipboard,
                    policy,
                    clipboard_config,
                    false,
                );
                flush_terminal_responses(&mut self.terminal, transport);
                stats.content_changed = true;
            }
            if output.closed {
                stats.content_changed = true;
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
    let Some(session_id) = model
        .active_tab()
        .panes
        .get(&pane_id)
        .map(|pane| pane.session_id)
    else {
        return;
    };
    if let Some(session) = model.active_tab_mut().sessions.get_mut(&session_id) {
        session.status = status;
    }
}

fn session_spec_for_config(config: &AppConfig) -> SessionSpec {
    let profile = selected_shell_profile(config);
    let mut spec = SessionSpec::local(
        profile
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| "default".to_owned()),
    );
    if let Some(profile) = profile {
        spec.working_directory = profile.working_directory.clone();
        spec.startup_command = profile.startup_command.clone();
    }
    spec
}

fn terminal_transport_size(size: TerminalGridSize, metrics: CellMetrics) -> TransportSize {
    TransportSize::new(
        size.cols,
        size.rows,
        (f32::from(size.cols) * metrics.cell_width).ceil() as u32,
        (f32::from(size.rows) * metrics.cell_height).ceil() as u32,
    )
}

fn resize_transport(transport: &mut LocalPtyTransport, size: TransportSize) {
    match catch_unwind(AssertUnwindSafe(|| transport.resize(size))) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("transport resize error: {error}"),
        Err(panic) => eprintln!("transport resize panic boundary: {}", panic_payload(panic)),
    }
}

fn tab_bar_rows(model: &MuxModel, config: &AppConfig) -> u16 {
    if config.mux.show_tab_bar && model.active_workspace().active_window().tabs.len() > 1 {
        1
    } else {
        0
    }
}

fn scene_from_mux(
    runtime: &MuxRuntime,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    cursor_animator: Option<&mut CursorAnimationRuntime>,
) -> RenderScene {
    let mut scene = RenderScene {
        grid: RenderGrid {
            columns: runtime.surface_cols,
            rows: runtime.surface_rows,
            cells: Vec::new(),
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
        append_pane_scene(&mut scene, pane, layout, active_pane, metrics, config);
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
) {
    let mut pane_scene =
        scene_from_terminal(&pane.terminal, &pane.semantic_timeline, metrics, config);
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
    for tab in &window.tabs {
        let active = tab.id == window.active_tab;
        let label = if active {
            format!(" [{}] ", tab.name)
        } else {
            format!(" {} ", tab.name)
        };
        for ch in label.chars() {
            if col >= runtime.surface_cols {
                return;
            }
            scene.grid.cells.push(RenderCell {
                position: CellPosition { row: 0, col },
                text: ch.to_string(),
                foreground: render_color(config.colors.foreground),
                background: if active {
                    render_color(config.colors.selection_background)
                } else {
                    render_color(config.colors.background)
                },
                style: RenderCellStyle {
                    bold: active,
                    ..RenderCellStyle::default()
                },
            });
            col = col.saturating_add(1);
        }
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
            render_color(config.colors.cursor)
        } else {
            RenderColor {
                red: 120,
                green: 130,
                blue: 145,
                alpha: 120,
            }
        };
        if show_borders {
            scene.decorations.push(render_core::RenderDecoration {
                bounds: rect,
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
            corner_radius_px: 4,
            z_index: 1000,
            label: Some(label),
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
    metrics: Option<CellMetrics>,
    config: &AppConfig,
) -> RenderScene {
    let visible = terminal.visible_grid();
    let cursor = terminal.cursor_state();
    let mut cells = Vec::with_capacity(visible.cells.len());
    let cols = visible.viewport.size.cols;

    for (index, cell) in visible.cells.iter().enumerate() {
        let row = (index / usize::from(cols)) as i64;
        let col = (index % usize::from(cols)) as u16;
        let (foreground, background) = colors_for_attributes(cell.attributes, config);
        cells.push(RenderCell {
            position: CellPosition { row, col },
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
            terminal.modes().contains(&TerminalMode::AlternateScreen),
            visible.viewport.size.rows,
            visible.viewport.size.cols,
            metrics,
            config,
        ));
        overlays
    });

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
            shape: match cursor.shape {
                CursorShape::Block => render_cursor_shape(config.cursor.shape),
                CursorShape::Beam => RenderCursorShape::Beam,
                CursorShape::Underline => RenderCursorShape::Underline,
            },
            color: render_color(config.cursor.color.unwrap_or(config.colors.cursor)),
            visible: cursor.visible,
            thickness_percent: (config.cursor.thickness.clamp(0.05, 1.0) * 100.0).round() as u8,
            corner_radius_px: cursor_radius_px(config),
            inactive: false,
        }),
        semantic_overlays,
        ..RenderScene::default()
    }
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

fn cursor_radius_px(config: &AppConfig) -> u8 {
    let radius = config.cursor.corner_radius.clamp(0.0, 0.5) * 16.0;
    radius.round() as u8
}

fn url_hint_overlays(
    terminal: &TerminalEmulator,
    rows: u16,
    metrics: CellMetrics,
) -> Vec<OverlayPrimitive> {
    let mut lines = Vec::new();
    for row in 0..rows {
        if let Some(line) = terminal.state().line(row) {
            lines.push((i64::from(row), line.raw_text()));
        }
    }

    let borrowed = lines
        .iter()
        .map(|(row, text)| (*row, text.as_str()))
        .collect::<Vec<_>>();

    detect_url_hints(borrowed)
        .into_iter()
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
            corner_radius_px: 2,
            z_index: 10,
            label: Some(hint.text),
        })
        .collect()
}

fn semantic_visual_overlays(
    semantic_timeline: &SemanticTimelineStore,
    alternate_screen_active: bool,
    rows: u16,
    cols: u16,
    metrics: CellMetrics,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    let mut overlays = Vec::new();

    if config.prompt_decorations.enabled
        && (!alternate_screen_active || config.prompt_decorations.allow_in_alternate_screen)
    {
        overlays.extend(prompt_decoration_overlays(
            semantic_timeline,
            rows,
            cols,
            metrics,
            config,
        ));
    }
    if config.command_blocks.enabled
        && (!alternate_screen_active || config.command_blocks.allow_in_alternate_screen)
    {
        overlays.extend(command_block_overlays(
            semantic_timeline,
            rows,
            cols,
            metrics,
            config,
        ));
    }

    overlays
}

fn prompt_decoration_overlays(
    semantic_timeline: &SemanticTimelineStore,
    rows: u16,
    cols: u16,
    metrics: CellMetrics,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    semantic_timeline
        .regions()
        .iter()
        .filter(|region| region.kind == SemanticRegionKind::Prompt)
        .filter_map(|region| region.span())
        .filter_map(|span| row_overlay_bounds(span.start.row, span.end.row, rows, cols, metrics))
        .map(|bounds| inset_overlay_bounds(bounds, prompt_overlay_padding(config)))
        .map(|bounds| OverlayPrimitive {
            kind: OverlayKind::PromptDecoration,
            bounds,
            color: prompt_decoration_color(config),
            border_color: match config.prompt_decorations.style {
                PromptDecorationStyle::MinimalSeparator => None,
                PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => {
                    Some(render_color(config.visual_theme.borders.color))
                }
            },
            corner_radius_px: match config.prompt_decorations.style {
                PromptDecorationStyle::MinimalSeparator => 0,
                PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => {
                    config.visual_theme.borders.radius_px
                }
            },
            z_index: 20,
            label: prompt_badge_label(config),
        })
        .collect()
}

fn command_block_overlays(
    semantic_timeline: &SemanticTimelineStore,
    rows: u16,
    cols: u16,
    metrics: CellMetrics,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    let mut overlays = Vec::new();

    for block in semantic_timeline.command_blocks() {
        let Some(span) = semantic_timeline.command_span(block) else {
            continue;
        };
        let Some(raw_bounds) =
            row_overlay_bounds(span.start.row, span.end.row, rows, cols, metrics)
        else {
            continue;
        };

        let bounds = inset_overlay_bounds(raw_bounds, command_block_padding(config));
        let metadata = semantic_timeline
            .command_metadata(block)
            .unwrap_or_else(|| semantic_timeline.metadata());
        let status_color = command_status_color(&block.status, config);
        overlays.push(OverlayPrimitive {
            kind: OverlayKind::CommandBlock,
            bounds,
            color: command_block_fill(config),
            border_color: Some(status_color),
            corner_radius_px: command_block_corner_radius(config),
            z_index: 15,
            label: None,
        });

        if config.command_blocks.separate_prompt_input_output {
            append_input_output_group_overlays(
                &mut overlays,
                semantic_timeline,
                [
                    (block.input_region_id, "input"),
                    (block.output_region_id, "output"),
                ],
                rows,
                cols,
                metrics,
                config,
            );
        }

        append_command_badges(
            &mut overlays,
            bounds,
            block,
            metadata,
            status_color,
            metrics,
            config,
        );
    }

    overlays
}

fn append_input_output_group_overlays(
    overlays: &mut Vec<OverlayPrimitive>,
    semantic_timeline: &SemanticTimelineStore,
    regions: [(Option<u64>, &'static str); 2],
    rows: u16,
    cols: u16,
    metrics: CellMetrics,
    config: &AppConfig,
) {
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
        let Some(bounds) = row_overlay_bounds(span.start.row, span.end.row, rows, cols, metrics)
        else {
            continue;
        };
        overlays.push(OverlayPrimitive {
            kind: OverlayKind::InputOutputGroup,
            bounds: inset_overlay_bounds(bounds, input_output_group_padding(config)),
            color: input_output_group_color(label, config),
            border_color: None,
            corner_radius_px: input_output_group_radius(config),
            z_index: 16,
            label: None,
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
            color: badge_color(&label, status_color),
            border_color: None,
            corner_radius_px: config.visual_theme.borders.radius_px.min(8),
            z_index: 35,
            label: Some(label),
        });
        right -= gap;
    }
}

fn row_overlay_bounds(
    start_row: i64,
    end_row: i64,
    rows: u16,
    cols: u16,
    metrics: CellMetrics,
) -> Option<RenderRect> {
    if end_row < 0 || start_row >= i64::from(rows) {
        return None;
    }

    let start = start_row.max(0);
    let end = end_row.max(start + 1).min(i64::from(rows));
    Some(RenderRect {
        x: 0,
        y: (start as f32 * metrics.cell_height).floor() as i32,
        width: (f32::from(cols) * metrics.cell_width).ceil() as u32,
        height: ((end - start) as f32 * metrics.cell_height).ceil() as u32,
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
            red: 180,
            green: 190,
            blue: 205,
            alpha: 36,
        },
        PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => RenderColor {
            red: 80,
            green: 150,
            blue: 255,
            alpha: 28,
        },
    }
}

fn command_block_fill(config: &AppConfig) -> RenderColor {
    match config.command_blocks.style {
        CommandBlockStyle::Subtle => RenderColor {
            red: 220,
            green: 225,
            blue: 235,
            alpha: 20,
        },
        CommandBlockStyle::Card
        | CommandBlockStyle::Split
        | CommandBlockStyle::MinimalHeader
        | CommandBlockStyle::CustomTheme => RenderColor {
            red: 38,
            green: 44,
            blue: 52,
            alpha: 82,
        },
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
            red: 80,
            green: 150,
            blue: 255,
            alpha,
        },
        _ => RenderColor {
            red: 180,
            green: 190,
            blue: 205,
            alpha,
        },
    }
}

fn prompt_badge_label(config: &AppConfig) -> Option<String> {
    let mut badges = Vec::new();
    if config.prompt_decorations.show_shell_badge || config.visual_theme.badges.shell {
        badges.push("shell");
    }
    if config.prompt_decorations.show_current_directory
        || config.visual_theme.badges.current_directory
    {
        badges.push("cwd");
    }
    if config.prompt_decorations.show_remote_host || config.visual_theme.badges.remote {
        badges.push("remote");
    }
    (!badges.is_empty()).then(|| badges.join(" "))
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
    if config.command_blocks.show_exit_status
        && let Some(label) = command_status_label(&block.status)
    {
        labels.push(label);
    }
    if config.command_blocks.show_duration
        && let Some(duration) = block.duration
    {
        labels.push(format_duration_badge(duration));
    }
    if config.command_blocks.show_current_directory
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
    if config.command_blocks.show_shell_host
        && let Some(label) = shell_host_badge_label(metadata)
    {
        labels.push(label);
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

fn badge_color(label: &str, status_color: RenderColor) -> RenderColor {
    if label == "ok" || label.starts_with("exit ") || label.starts_with("signal ") {
        return RenderColor {
            alpha: 148,
            ..status_color
        };
    }
    RenderColor {
        red: 32,
        green: 38,
        blue: 46,
        alpha: 156,
    }
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

    config
        .colors
        .palette
        .get(usize::from(index.min(15)))
        .copied()
        .map(render_color)
        .or_else(|| PALETTE.get(usize::from(index.min(15))).copied())
        .unwrap_or(PALETTE[7])
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
    fn mux_layout_reserves_tab_bar_only_when_configured_and_needed() {
        let mut config = AppConfig::default();
        let mut model = MuxModel::new(SessionSpec::local("default"));
        model
            .new_tab("2", SessionSpec::local("default"))
            .expect("new tab");
        let runtime = MuxRuntime {
            model,
            panes: HashMap::new(),
            surface_cols: 100,
            surface_rows: 30,
            performance: RuntimePerformanceCounters::new(),
        };

        let layout = runtime.active_layouts(&config);
        assert_eq!(layout[0].rect.y, 1.0);
        assert_eq!(layout[0].terminal_size.rows, 29);

        config.mux.show_tab_bar = false;
        let layout = runtime.active_layouts(&config);
        assert_eq!(layout[0].rect.y, 0.0);
        assert_eq!(layout[0].terminal_size.rows, 30);
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

        let (profile, activation) = initial_local_shell_profile(&config);

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

        let (profile, activation) = initial_local_shell_profile(&config);

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

        let (profile, activation) = initial_local_shell_profile(&config);

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
    fn command_block_overlays_include_groups_and_metadata_badges() {
        let timeline = command_timeline();
        let mut config = AppConfig::default();
        config.command_blocks.enabled = true;
        config.command_blocks.style = CommandBlockStyle::Card;
        config.visual_theme.grouping_style = InputOutputGroupingStyle::InputOutputSplit;

        let overlays = semantic_visual_overlays(&timeline, false, 10, 80, test_metrics(), &config);

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

        let overlays = semantic_visual_overlays(&timeline, true, 10, 80, test_metrics(), &config);
        assert!(overlays.is_empty());

        config.command_blocks.allow_in_alternate_screen = true;
        let overlays = semantic_visual_overlays(&timeline, true, 10, 80, test_metrics(), &config);
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

        let overlays = semantic_visual_overlays(&timeline, false, 10, 80, test_metrics(), &config);

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
