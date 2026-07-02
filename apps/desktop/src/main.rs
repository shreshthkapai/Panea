use std::{error::Error, sync::Arc};

use config_core::{AppConfig, DecorationStrategyConfig, LinuxBackendConfig, WindowModeConfig};
use font_system::{CellMetrics, FontConfig, FontSystem};
use platform_core::{
    DecorationMode, InputEvent, KeyEvent, KeyState, LinuxWindowBackend, WindowAction, WindowMode,
};
use platform_winit::{
    ClipboardBridge, DesktopWindow, InputTranslator, WindowSettings, apply_window_mode,
    platform_capabilities,
};
use render_core::{
    CellPosition, CursorVisual, RenderCell, RenderCellStyle, RenderColor, RenderCursorShape,
    RenderGrid, RenderScene,
};
use render_wgpu::{FrameDecision, FrameScheduler, GpuTerminalRenderer, RendererOptions};
use term_core::{
    CellAttributes, Color, CursorShape, TerminalCore, TerminalSize as CoreTerminalSize,
};
use term_parser::TerminalEmulator;
use transport_core::{TerminalSize as TransportSize, TerminalTransport};
use transport_pty::LocalPtyTransport;
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
    let config = AppConfig::default();
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

    let mut fonts = FontSystem::new(FontConfig::default());
    let metrics = fonts.cell_metrics()?;
    let initial_size = terminal_size_for_window(config.window.columns, config.window.rows, metrics);
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(
        config.window.columns,
        config.window.rows,
    ));
    let mut renderer = pollster::block_on(GpuTerminalRenderer::new(
        Arc::clone(&window),
        RendererOptions::default(),
    ))?;
    let mut scheduler = FrameScheduler::new();
    let mut transport = match LocalPtyTransport::spawn_default(initial_size) {
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
                    let scene = scene_from_terminal(&terminal);
                    if let Err(error) = renderer.render_scene(&scene, &mut fonts) {
                        eprintln!("render error: {error}");
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
                                            let _ = transport.write_input(text.as_bytes());
                                        }
                                    } else if let Some(bytes) = input_bytes(&key) {
                                        let _ = transport.write_input(&bytes);
                                    }
                                }
                            }
                            InputEvent::Ime(platform_core::ImeEvent::Commit { text }) => {
                                if let Some(transport) = transport.as_mut() {
                                    let _ = transport.write_input(text.as_bytes());
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

                        if contains_cpr_query(&output.bytes) {
                            let _ = transport.write_input(b"\x1b[1;1R");
                        }
                        if !output.bytes.is_empty() {
                            let _ = terminal.apply_bytes(&output.bytes);
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

fn shutdown_transport(transport: Option<&mut LocalPtyTransport>) {
    if let Some(transport) = transport {
        let _ = transport.shutdown();
    }
}

fn contains_cpr_query(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"\x1b[6n")
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

fn scene_from_terminal(terminal: &TerminalEmulator) -> RenderScene {
    let visible = terminal.visible_grid();
    let cursor = terminal.cursor_state();
    let mut cells = Vec::with_capacity(visible.cells.len());
    let cols = visible.viewport.size.cols;

    for (index, cell) in visible.cells.iter().enumerate() {
        let row = (index / usize::from(cols)) as i64;
        let col = (index % usize::from(cols)) as u16;
        let (foreground, background) = colors_for_attributes(cell.attributes);
        cells.push(RenderCell {
            position: CellPosition { row, col },
            text: cell.text.clone(),
            foreground,
            background,
            style: style_for_attributes(cell.attributes),
        });
    }

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
            color: RenderColor::rgb(235, 235, 235),
            visible: cursor.visible,
        }),
        ..RenderScene::default()
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

fn colors_for_attributes(attributes: CellAttributes) -> (RenderColor, RenderColor) {
    let mut foreground = color_or_default(attributes.foreground, RenderColor::rgb(230, 230, 230));
    let mut background = color_or_default(attributes.background, RenderColor::rgb(12, 12, 12));

    if attributes.inverse {
        std::mem::swap(&mut foreground, &mut background);
    }

    (foreground, background)
}

fn color_or_default(color: Option<Color>, default: RenderColor) -> RenderColor {
    match color {
        Some(Color::Rgb { red, green, blue }) => RenderColor::rgb(red, green, blue),
        Some(Color::Indexed(index)) => ansi_color(index),
        Some(Color::DefaultForeground | Color::DefaultBackground) | None => default,
    }
}

fn ansi_color(index: u8) -> RenderColor {
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

    PALETTE
        .get(usize::from(index.min(15)))
        .copied()
        .unwrap_or(PALETTE[7])
}
