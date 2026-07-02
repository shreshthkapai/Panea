use std::{error::Error, sync::Arc};

use font_system::{CellMetrics, FontConfig, FontSystem};
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
    dpi::LogicalSize,
    event::{ElementState, Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::WindowBuilder,
};

const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 32;

fn main() {
    if let Err(error) = run() {
        eprintln!("panea desktop failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("Panea")
            .with_inner_size(LogicalSize::new(960.0, 560.0))
            .build(&event_loop)?,
    );

    let mut fonts = FontSystem::new(FontConfig::default());
    let metrics = fonts.cell_metrics()?;
    let initial_size = terminal_size_for_window(DEFAULT_COLS, DEFAULT_ROWS, metrics);
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(DEFAULT_COLS, DEFAULT_ROWS));
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
                WindowEvent::CloseRequested => {
                    if let Some(transport) = transport.as_mut() {
                        let _ = transport.shutdown();
                    }
                    target.exit();
                }
                WindowEvent::Resized(size) => {
                    renderer.resize(size.width, size.height);
                    if let Ok(metrics) = fonts.cell_metrics() {
                        let cols = cols_for_width(size.width, metrics).max(1);
                        let rows = rows_for_height(size.height, metrics).max(1);
                        let core_size = CoreTerminalSize::new(cols, rows);
                        let transport_size =
                            TransportSize::new(cols, rows, size.width, size.height);
                        let _ = terminal.resize(core_size);
                        if let Some(transport) = transport.as_mut() {
                            let _ = transport.resize(transport_size);
                        }
                    }
                    scheduler.window_resized();
                    window.request_redraw();
                }
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed =>
                {
                    if let Some(transport) = transport.as_mut()
                        && let Some(bytes) = input_bytes(&event)
                    {
                        let _ = transport.write_input(&bytes);
                    }
                }
                WindowEvent::RedrawRequested => {
                    let scene = scene_from_terminal(&terminal);
                    if let Err(error) = renderer.render_scene(&scene, &mut fonts) {
                        eprintln!("render error: {error}");
                    }
                }
                _ => {}
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

fn input_bytes(event: &winit::event::KeyEvent) -> Option<Vec<u8>> {
    match &event.logical_key {
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(vec![0x08]),
        Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Character(text) if !text.is_empty() => Some(text.as_bytes().to_vec()),
        _ => event.text.as_ref().map(|text| text.as_bytes().to_vec()),
    }
}

fn contains_cpr_query(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"\x1b[6n")
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
