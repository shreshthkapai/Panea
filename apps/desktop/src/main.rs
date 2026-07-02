use std::{collections::BTreeSet, error::Error, path::PathBuf, sync::Arc, time::Duration};

use config_core::{
    AppConfig, ConfigDiagnosticSeverity, DecorationStrategyConfig, LinuxBackendConfig, PasteConfig,
    PresentModePreference, ShellProfile, ShellProfileKind, WindowModeConfig,
};
use diagnostics::{PerformanceBudget, PerformanceOverlay};
use font_system::{CellMetrics, FontConfig as RuntimeFontConfig, FontSystem};
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
    RenderColor, RenderCursorShape, RenderGrid, RenderRect, RenderScene,
};
use render_wgpu::{
    FrameDecision, FrameScheduler, GpuTerminalRenderer, PresentMode, RendererOptions,
};
use semantics::detect_url_hints;
use term_core::{
    CellAttributes, Color, CursorShape, TerminalCore, TerminalMode,
    TerminalSize as CoreTerminalSize,
};
use term_parser::TerminalEmulator;
use transport_core::{TerminalSize as TransportSize, TerminalTransport};
use transport_pty::{LocalPtyTransport, LocalShellKind, LocalShellProfile};
use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
};

fn main() {
    if let Err(error) = run() {
        eprintln!("panea desktop failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let loaded_config = config_toml::load(config_toml::ConfigLoadOptions::default())?;
    for diagnostic in &loaded_config.diagnostics {
        let level = match diagnostic.severity {
            ConfigDiagnosticSeverity::Error => "error",
            ConfigDiagnosticSeverity::Warning => "warning",
        };
        eprintln!(
            "config {level} at {}: {}",
            diagnostic.path, diagnostic.message
        );
    }
    let config = loaded_config.config;
    let event_loop = EventLoop::new()?;
    let desktop_window = DesktopWindow::create(&event_loop, &window_settings(&config))?;
    let window = desktop_window.window();
    let capabilities = platform_capabilities(&event_loop, &window);
    let _diagnostics =
        DesktopDiagnosticsPlaceholder::new(desktop_window.diagnostics().clone(), capabilities);
    let _session_manager = SessionManagerPlaceholder;
    let mut input_translator = InputTranslator::new();
    let mut clipboard = ClipboardBridge::new();
    let mut current_window_mode = map_window_mode(config.window.mode);
    let paste_config = config.paste.clone();
    let mut mouse_protocol = MouseProtocolState::default();

    let mut fonts = FontSystem::new(font_config(&config.font));
    let metrics = fonts.cell_metrics()?;
    let initial_size = terminal_size_for_window(config.window.columns, config.window.rows, metrics);
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(
        config.window.columns,
        config.window.rows,
    ));
    let mut renderer = pollster::block_on(GpuTerminalRenderer::new(
        Arc::clone(&window),
        renderer_options(&config),
    ))?;
    let mut scheduler = FrameScheduler::new();
    let mut performance_overlay =
        PerformanceOverlay::new(config.diagnostics.performance_overlay, "wgpu");
    let performance_budget = performance_budget(&config);
    let mut transport = match spawn_initial_transport(&config, initial_size) {
        Ok(transport) => Some(transport),
        Err(error) => {
            terminal.apply_bytes(format!("failed to spawn local shell: {error}\r\n").as_bytes())?;
            scheduler.terminal_content_changed();
            None
        }
    };

    scheduler.terminal_content_changed();

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Wait);

        match event {
            Event::WindowEvent { event, window_id } if window_id == window.id() => match event {
                WindowEvent::RedrawRequested => {
                    let metrics = fonts.cell_metrics().ok();
                    let scene = scene_from_terminal(&terminal, metrics, &config);
                    let idle_wakeups = scheduler.take_idle_wakeups();
                    match renderer.render_scene(&scene, &mut fonts) {
                        Ok(()) => {
                            let mut instrumentation = renderer.last_instrumentation();
                            instrumentation.idle_wakeups = idle_wakeups;
                            performance_overlay.record(instrumentation);
                            if let Some(text) = performance_overlay.render_text(performance_budget)
                            {
                                eprintln!("performance {text}");
                            }
                        }
                        Err(error) => {
                            eprintln!("render error: {error}");
                        }
                    }
                }
                _ => {
                    let platform_events = input_translator.translate_window_event(&event);
                    for platform_event in platform_events {
                        match platform_event {
                            InputEvent::CloseRequested => {
                                shutdown_transport(transport.as_mut());
                                target.exit();
                            }
                            InputEvent::Resized { width, height } => {
                                renderer.resize(width, height);
                                if let Ok(metrics) = fonts.cell_metrics() {
                                    let cols = cols_for_width(width, metrics).max(1);
                                    let rows = rows_for_height(height, metrics).max(1);
                                    let core_size = CoreTerminalSize::new(cols, rows);
                                    let transport_size =
                                        TransportSize::new(cols, rows, width, height);
                                    let _ = terminal.resize(core_size);
                                    if let Some(transport) = transport.as_mut() {
                                        let _ = transport.resize(transport_size);
                                    }
                                }
                                scheduler.window_resized();
                                window.request_redraw();
                            }
                            InputEvent::Key(key) => {
                                if is_copy_shortcut(&key) {
                                    if let Some(text) = terminal.state().selected_text() {
                                        let _ = clipboard.copy_text(&text);
                                    }
                                } else if let Some(transport) = transport.as_mut() {
                                    if is_paste_shortcut(&key) {
                                        if let Ok(text) = clipboard.paste_text() {
                                            let bytes = paste_bytes(
                                                &text,
                                                &paste_config,
                                                terminal
                                                    .modes()
                                                    .contains(&TerminalMode::BracketedPaste),
                                            );
                                            let _ = transport.write_input(&bytes);
                                        }
                                    } else if let Some(bytes) = input_bytes(&key) {
                                        let _ = transport.write_input(&bytes);
                                    }
                                }
                            }
                            InputEvent::Mouse(mouse) => {
                                if let Some(transport) = transport.as_mut()
                                    && let Ok(metrics) = fonts.cell_metrics()
                                {
                                    let modes = terminal.modes();
                                    if let Some(bytes) =
                                        mouse_protocol.report_bytes(mouse, metrics, &modes)
                                    {
                                        let _ = transport.write_input(&bytes);
                                    }
                                }
                            }
                            InputEvent::Ime(platform_core::ImeEvent::Commit { text }) => {
                                if let Some(transport) = transport.as_mut() {
                                    let _ = transport.write_input(text.as_bytes());
                                }
                            }
                            InputEvent::Focused(focused) => {
                                if terminal.modes().contains(&TerminalMode::FocusEvents)
                                    && let Some(transport) = transport.as_mut()
                                {
                                    let bytes = if focused { b"\x1b[I" } else { b"\x1b[O" };
                                    let _ = transport.write_input(bytes);
                                }
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
                                    shutdown_transport(transport.as_mut());
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
                if let Some(transport) = transport.as_mut() {
                    let mut content_changed = false;
                    for _ in 0..64 {
                        let Ok(output) = transport.poll_output() else {
                            break;
                        };
                        if output.bytes.is_empty() && output.lifecycle.is_empty() {
                            break;
                        }

                        if !output.bytes.is_empty() {
                            let _ = terminal.apply_bytes(&output.bytes);
                            flush_terminal_responses(&mut terminal, transport);
                            content_changed = true;
                        }
                        if output.closed {
                            content_changed = true;
                            break;
                        }
                    }

                    if content_changed {
                        scheduler.terminal_content_changed();
                    }
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
    }
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
) -> transport_core::TransportResult<LocalPtyTransport> {
    let Some(profile) = selected_shell_profile(config) else {
        return LocalPtyTransport::spawn_default(size);
    };

    LocalPtyTransport::spawn(local_shell_profile(profile), size)
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

fn terminal_size_for_window(cols: u16, rows: u16, metrics: CellMetrics) -> TransportSize {
    TransportSize::new(
        cols,
        rows,
        (f32::from(cols) * metrics.cell_width).ceil() as u32,
        (f32::from(rows) * metrics.cell_height).ceil() as u32,
    )
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

fn is_paste_shortcut(event: &KeyEvent) -> bool {
    event.state == KeyState::Pressed
        && event.modifiers.ctrl
        && event.modifiers.shift
        && event.logical_key.eq_ignore_ascii_case("v")
}

fn is_copy_shortcut(event: &KeyEvent) -> bool {
    event.state == KeyState::Pressed
        && event.modifiers.ctrl
        && event.modifiers.shift
        && event.logical_key.eq_ignore_ascii_case("c")
}

fn paste_bytes(text: &str, config: &PasteConfig, bracketed_mode: bool) -> Vec<u8> {
    let mut text = if config.normalize_newlines {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_owned()
    };

    if config.strip_control_characters {
        text.retain(|ch| ch == '\n' || ch == '\t' || !ch.is_control());
    }

    let mut bytes = Vec::new();
    if bracketed_mode && config.bracketed_paste {
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
    } else {
        bytes.extend_from_slice(text.as_bytes());
    }

    bytes
}

fn shutdown_transport(transport: Option<&mut LocalPtyTransport>) {
    if let Some(transport) = transport {
        let _ = transport.shutdown();
    }
}

fn flush_terminal_responses(terminal: &mut TerminalEmulator, transport: &mut LocalPtyTransport) {
    let responses = terminal.state_mut().take_pending_output();
    if !responses.is_empty() {
        let _ = transport.write_input(&responses);
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

struct SessionManagerPlaceholder;

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

fn scene_from_terminal(
    terminal: &TerminalEmulator,
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
        url_hint_overlays(terminal, visible.viewport.size.rows, metrics)
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
                CursorShape::Block => RenderCursorShape::Block,
                CursorShape::Beam => RenderCursorShape::Beam,
                CursorShape::Underline => RenderCursorShape::Underline,
            },
            color: render_color(config.colors.cursor),
            visible: cursor.visible,
        }),
        semantic_overlays,
        ..RenderScene::default()
    }
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
            label: Some(hint.text),
        })
        .collect()
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
