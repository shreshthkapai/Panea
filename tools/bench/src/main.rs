use std::{
    env,
    error::Error,
    process::ExitCode,
    time::{Duration, Instant},
};

use config_core::PerformanceProfile;
use diagnostics::{
    PerformanceBudget, PerformanceOverlay, evaluate_feature_cost, evaluate_performance_gate,
};
use font_system::{FontConfig, FontSystem};
use render_core::{
    AnimationHandle, AnimationKind, CellPosition, CursorVisual, FeatureCostSample, OptionalFeature,
    OptionalFeatureCostMode, OverlayKind, OverlayPrimitive, RenderCell, RenderCellStyle,
    RenderColor, RenderCursorShape, RenderGrid, RenderInstrumentation, RenderRect, RenderScene,
};
use render_wgpu::TerminalRasterizer;
use term_core::{
    CellAttributes, Color, CursorShape, TerminalCore, TerminalSize as CoreTerminalSize,
};
use term_parser::TerminalEmulator;

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 36;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let command = args.first().map_or("help", String::as_str);

    match command {
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "all" => run_all(),
        "profiles" => {
            print_profiles();
            Ok(())
        }
        "render-grid" => render_grid(),
        "render-full-ascii" => batch_scene_bench(
            "render-full-ascii",
            generated_scene(
                DEFAULT_COLS,
                DEFAULT_ROWS,
                OptionalFeatureCostMode::Disabled,
            ),
            10,
        ),
        "render-mixed-unicode" => batch_scene_bench(
            "render-mixed-unicode",
            unicode_scene(DEFAULT_COLS, DEFAULT_ROWS, false),
            10,
        ),
        "render-emoji-heavy" => batch_scene_bench(
            "render-emoji-heavy",
            unicode_scene(DEFAULT_COLS, DEFAULT_ROWS, true),
            10,
        ),
        "render-fast-scrolling" => {
            parse_and_render("render-fast-scrolling", large_log_fixture(8_000), 4)
        }
        "render-large-scrollback-viewport" => batch_scene_bench(
            "render-large-scrollback-viewport",
            generated_scene(180, 64, OptionalFeatureCostMode::Disabled),
            5,
        ),
        "render-many-panes" => render_many_panes(),
        "render-cursor-animation" => cursor_animation_cost(),
        "render-command-blocks" => render_command_blocks(),
        "cat-large-file" => parse_fixture("cat-large-file", large_log_fixture(20_000), 1),
        "color-heavy" => parse_and_render("color-heavy", color_heavy_fixture(4_000), 2),
        "scrollback" => parse_fixture("scrollback", large_log_fixture(50_000), 1),
        "resize" => resize_bench(),
        "input-latency" => input_latency(),
        "unicode" => parse_and_render("unicode", unicode_fixture(4_000), 2),
        "alternate-screen" => {
            parse_fixture("alternate-screen", alternate_screen_fixture(2_000), 20)
        }
        "cursor-animation" => cursor_animation_cost(),
        "command-blocks" => render_command_blocks(),
        other => Err(format!("unknown benchmark '{other}'").into()),
    }
}

fn print_help() {
    println!(
        "usage: cargo xtask bench <all|profiles|render-grid|render-full-ascii|render-mixed-unicode|render-emoji-heavy|render-fast-scrolling|render-large-scrollback-viewport|render-many-panes|render-cursor-animation|render-command-blocks|cat-large-file|color-heavy|scrollback|resize|input-latency|unicode|alternate-screen|cursor-animation|command-blocks>"
    );
}

fn run_all() -> Result<(), Box<dyn Error>> {
    print_profiles();
    render_grid()?;
    batch_scene_bench(
        "render-full-ascii",
        generated_scene(
            DEFAULT_COLS,
            DEFAULT_ROWS,
            OptionalFeatureCostMode::Disabled,
        ),
        10,
    )?;
    batch_scene_bench(
        "render-mixed-unicode",
        unicode_scene(DEFAULT_COLS, DEFAULT_ROWS, false),
        10,
    )?;
    batch_scene_bench(
        "render-emoji-heavy",
        unicode_scene(DEFAULT_COLS, DEFAULT_ROWS, true),
        10,
    )?;
    parse_fixture("cat-large-file", large_log_fixture(20_000), 1)?;
    parse_and_render("color-heavy", color_heavy_fixture(4_000), 2)?;
    parse_fixture("scrollback", large_log_fixture(50_000), 1)?;
    resize_bench()?;
    input_latency()?;
    parse_and_render("unicode", unicode_fixture(4_000), 2)?;
    parse_fixture("alternate-screen", alternate_screen_fixture(2_000), 20)?;
    cursor_animation_cost()?;
    render_many_panes()?;
    render_command_blocks()?;
    Ok(())
}

fn print_profiles() {
    for profile in [
        PerformanceProfile::MaximumPerformance,
        PerformanceProfile::Balanced,
        PerformanceProfile::Visual,
        PerformanceProfile::BatterySaver,
    ] {
        let settings = ProfileSettings::for_profile(profile);
        println!(
            "profile={:?} frame_budget={:?} frame_rate_limit={:?} animations={} expensive_warnings={}",
            profile,
            settings.budget.max_frame_time,
            settings.frame_rate_limit,
            settings.animations_enabled,
            settings.expensive_effect_warnings
        );
    }
}

fn render_grid() -> Result<(), Box<dyn Error>> {
    let scene = generated_scene(
        DEFAULT_COLS,
        DEFAULT_ROWS,
        OptionalFeatureCostMode::Disabled,
    );
    let mut fonts = FontSystem::new(FontConfig::default());
    let mut rasterizer = TerminalRasterizer::default();
    let started = Instant::now();
    let iterations = 5;
    let mut last = RenderInstrumentation::default();

    for _ in 0..iterations {
        last = rasterizer
            .rasterize_instrumented(&scene, &mut fonts)?
            .instrumentation;
    }

    print_result(BenchmarkResult {
        name: "render-grid",
        iterations,
        bytes: scene.grid.cells.len(),
        elapsed: started.elapsed(),
        instrumentation: last,
    });
    Ok(())
}

fn batch_scene_bench(
    name: &'static str,
    scene: RenderScene,
    iterations: u64,
) -> Result<(), Box<dyn Error>> {
    let mut fonts = FontSystem::new(FontConfig::default());
    let mut rasterizer = TerminalRasterizer::default();
    let started = Instant::now();
    let mut last = RenderInstrumentation::default();

    for _ in 0..iterations {
        last = rasterizer
            .prepare_batches(&scene, &mut fonts)?
            .instrumentation;
    }

    print_result(BenchmarkResult {
        name,
        iterations,
        bytes: scene.grid.cells.len(),
        elapsed: started.elapsed(),
        instrumentation: last,
    });
    Ok(())
}

fn parse_fixture(
    name: &'static str,
    fixture: Vec<u8>,
    iterations: u64,
) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(DEFAULT_COLS, DEFAULT_ROWS));

    for _ in 0..iterations {
        terminal.apply_bytes(&fixture)?;
    }

    let elapsed = started.elapsed();
    print_result(BenchmarkResult {
        name,
        iterations,
        bytes: fixture.len() * iterations as usize,
        elapsed,
        instrumentation: RenderInstrumentation::default(),
    });
    Ok(())
}

fn parse_and_render(
    name: &'static str,
    fixture: Vec<u8>,
    iterations: u64,
) -> Result<(), Box<dyn Error>> {
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(DEFAULT_COLS, DEFAULT_ROWS));
    let mut fonts = FontSystem::new(FontConfig::default());
    let mut rasterizer = TerminalRasterizer::default();
    let started = Instant::now();
    let mut last = RenderInstrumentation::default();

    for _ in 0..iterations {
        terminal.apply_bytes(&fixture)?;
        let scene = scene_from_terminal(&terminal);
        last = rasterizer
            .rasterize_instrumented(&scene, &mut fonts)?
            .instrumentation;
    }

    print_result(BenchmarkResult {
        name,
        iterations,
        bytes: fixture.len() * iterations as usize,
        elapsed: started.elapsed(),
        instrumentation: last,
    });
    Ok(())
}

fn resize_bench() -> Result<(), Box<dyn Error>> {
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(100, 30));
    terminal.apply_bytes(&large_log_fixture(2_000))?;
    let sizes = [
        CoreTerminalSize::new(80, 24),
        CoreTerminalSize::new(132, 40),
        CoreTerminalSize::new(100, 30),
        CoreTerminalSize::new(160, 48),
    ];
    let iterations = 24_u64;
    let started = Instant::now();

    for index in 0..iterations {
        terminal.resize(sizes[index as usize % sizes.len()])?;
    }

    print_result(BenchmarkResult {
        name: "resize",
        iterations,
        bytes: 0,
        elapsed: started.elapsed(),
        instrumentation: RenderInstrumentation::default(),
    });
    Ok(())
}

fn input_latency() -> Result<(), Box<dyn Error>> {
    let mut terminal = TerminalEmulator::new(CoreTerminalSize::new(DEFAULT_COLS, DEFAULT_ROWS));
    let iterations = 20_000;
    let started = Instant::now();

    for _ in 0..iterations {
        terminal.apply_bytes(b"x")?;
    }

    let elapsed = started.elapsed();
    let average = elapsed / iterations as u32;
    println!(
        "bench=input-latency iterations={iterations} total={elapsed:?} average_per_input={average:?}"
    );
    Ok(())
}

fn cursor_animation_cost() -> Result<(), Box<dyn Error>> {
    let mut fonts = FontSystem::new(FontConfig::default());
    let mut rasterizer = TerminalRasterizer::default();
    let disabled = FeatureCostSample {
        feature: OptionalFeature::CursorAnimation,
        mode: OptionalFeatureCostMode::Disabled,
        instrumentation: RenderInstrumentation::default(),
    };
    let disabled_report = evaluate_feature_cost(&disabled);
    println!(
        "feature=cursor-animation mode=disabled passed={} warnings={}",
        disabled_report.passed,
        disabled_report.warnings.len()
    );

    for mode in [
        OptionalFeatureCostMode::EnabledDefault,
        OptionalFeatureCostMode::EnabledHeavy,
    ] {
        let mut scene = generated_scene(DEFAULT_COLS, DEFAULT_ROWS, mode);
        scene.damage_regions = scene
            .animations
            .iter()
            .map(|animation| animation.affected_region)
            .collect();
        let instrumentation = rasterizer
            .prepare_batches(&scene, &mut fonts)?
            .instrumentation;
        let report = evaluate_performance_gate(instrumentation, PerformanceBudget::default());
        println!(
            "feature=cursor-animation mode={mode:?} frame={:?} animations={} draw_calls={} passed={}",
            instrumentation.frame_time,
            instrumentation.animated_region_count,
            instrumentation.draw_call_count,
            report.passed
        );
    }

    Ok(())
}

fn render_many_panes() -> Result<(), Box<dyn Error>> {
    let mut fonts = FontSystem::new(FontConfig::default());
    let mut rasterizer = TerminalRasterizer::default();
    let scenes = (0..8)
        .map(|index| {
            generated_scene(
                80 + (index % 3) * 10,
                24 + (index % 2) * 8,
                OptionalFeatureCostMode::Disabled,
            )
        })
        .collect::<Vec<_>>();
    let started = Instant::now();
    let iterations = 4_u64;
    let mut last = RenderInstrumentation::default();

    for _ in 0..iterations {
        for scene in &scenes {
            last = rasterizer
                .prepare_batches(scene, &mut fonts)?
                .instrumentation;
        }
    }

    print_result(BenchmarkResult {
        name: "render-many-panes",
        iterations,
        bytes: scenes.iter().map(|scene| scene.grid.cells.len()).sum(),
        elapsed: started.elapsed(),
        instrumentation: last,
    });
    Ok(())
}

fn render_command_blocks() -> Result<(), Box<dyn Error>> {
    let mut scene = generated_scene(
        DEFAULT_COLS,
        DEFAULT_ROWS,
        OptionalFeatureCostMode::Disabled,
    );
    scene.semantic_overlays = (0..18)
        .map(|index| OverlayPrimitive {
            kind: OverlayKind::CommandBlock,
            bounds: RenderRect {
                x: 8,
                y: 8 + index * 22,
                width: 680,
                height: 18,
            },
            color: RenderColor {
                red: 48,
                green: 90,
                blue: 120,
                alpha: 48,
            },
            border_color: Some(RenderColor {
                red: 90,
                green: 140,
                blue: 160,
                alpha: 80,
            }),
            corner_radius_px: 2,
            z_index: 10,
            label: None,
        })
        .collect();
    batch_scene_bench("render-command-blocks", scene, 10)
}

fn generated_scene(cols: u16, rows: u16, feature_mode: OptionalFeatureCostMode) -> RenderScene {
    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));

    for row in 0..rows {
        for col in 0..cols {
            let ch = match (row + col) % 6 {
                0 => "P",
                1 => "a",
                2 => "n",
                3 => "e",
                4 => "a",
                _ => " ",
            };
            cells.push(RenderCell {
                position: CellPosition {
                    row: i64::from(row),
                    col,
                },
                text: ch.to_owned(),
                foreground: RenderColor::rgb(230, 230, 230),
                background: if row % 2 == 0 {
                    RenderColor::rgb(12, 12, 12)
                } else {
                    RenderColor::rgb(18, 18, 18)
                },
                style: RenderCellStyle {
                    bold: col % 17 == 0,
                    italic: col % 29 == 0,
                    underline: row % 9 == 0,
                    strikethrough: false,
                },
            });
        }
    }

    let animations = match feature_mode {
        OptionalFeatureCostMode::Disabled => Vec::new(),
        OptionalFeatureCostMode::EnabledDefault => vec![animation(1, 10, 10)],
        OptionalFeatureCostMode::EnabledHeavy => (0..32)
            .map(|index| animation(index, (index * 7) as i32, (index * 5) as i32))
            .collect(),
    };

    RenderScene {
        grid: RenderGrid {
            columns: cols,
            rows,
            cells,
        },
        cursor: Some(CursorVisual {
            position: CellPosition { row: 2, col: 4 },
            shape: RenderCursorShape::Block,
            color: RenderColor::rgb(255, 255, 255),
            visible: true,
            thickness_percent: 15,
            corner_radius_px: 0,
            inactive: false,
        }),
        semantic_overlays: if feature_mode == OptionalFeatureCostMode::EnabledHeavy {
            vec![OverlayPrimitive {
                kind: OverlayKind::Semantic,
                bounds: RenderRect {
                    x: 16,
                    y: 16,
                    width: 180,
                    height: 22,
                },
                color: RenderColor {
                    red: 80,
                    green: 150,
                    blue: 255,
                    alpha: 48,
                },
                border_color: None,
                corner_radius_px: 2,
                z_index: 10,
                label: Some("synthetic expensive overlay".to_owned()),
            }]
        } else {
            Vec::new()
        },
        animations,
        ..RenderScene::default()
    }
}

fn unicode_scene(cols: u16, rows: u16, emoji_heavy: bool) -> RenderScene {
    let samples = if emoji_heavy {
        [
            "P",
            "\u{1f680}",
            "\u{1f469}\u{200d}\u{1f4bb}",
            "\u{2728}",
            " ",
        ]
    } else {
        ["P", "a", "\u{7aef}", "e\u{301}", "\u{2713}"]
    };
    let mut scene = generated_scene(cols, rows, OptionalFeatureCostMode::Disabled);
    for (index, cell) in scene.grid.cells.iter_mut().enumerate() {
        cell.text = samples[index % samples.len()].to_owned();
    }
    scene
}

fn animation(id: u64, x: i32, y: i32) -> AnimationHandle {
    AnimationHandle {
        id,
        kind: AnimationKind::CursorTypingPulse,
        affected_region: RenderRect {
            x,
            y,
            width: 24,
            height: 24,
        },
        elapsed: Duration::from_millis(16),
        remaining: Some(Duration::from_millis(120)),
    }
}

fn scene_from_terminal(terminal: &TerminalEmulator) -> RenderScene {
    let visible = terminal.visible_grid();
    let cursor = terminal.cursor_state();
    let cols = visible.viewport.size.cols;
    let mut cells = Vec::with_capacity(visible.cells.len());

    for (index, cell) in visible.cells.iter().enumerate() {
        let row = (index / usize::from(cols)) as i64;
        let col = (index % usize::from(cols)) as u16;
        let (foreground, background) = colors_for_attributes(cell.attributes);
        cells.push(RenderCell {
            position: CellPosition { row, col },
            text: cell.text.clone(),
            foreground,
            background,
            style: RenderCellStyle {
                bold: cell.attributes.bold,
                italic: cell.attributes.italic,
                underline: cell.attributes.underline,
                strikethrough: cell.attributes.strikethrough,
            },
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
            thickness_percent: 15,
            corner_radius_px: 0,
            inactive: false,
        }),
        ..RenderScene::default()
    }
}

fn colors_for_attributes(attributes: CellAttributes) -> (RenderColor, RenderColor) {
    let foreground = render_color(attributes.foreground, true);
    let background = render_color(attributes.background, false);

    if attributes.inverse {
        (background, foreground)
    } else {
        (foreground, background)
    }
}

fn render_color(color: Option<Color>, foreground: bool) -> RenderColor {
    match color {
        Some(Color::Rgb { red, green, blue }) => RenderColor::rgb(red, green, blue),
        Some(Color::Indexed(index)) => ansi_color(index),
        Some(Color::DefaultForeground) => RenderColor::rgb(230, 230, 230),
        Some(Color::DefaultBackground) => RenderColor::rgb(12, 12, 12),
        None if foreground => RenderColor::rgb(230, 230, 230),
        None => RenderColor::rgb(12, 12, 12),
    }
}

fn ansi_color(index: u8) -> RenderColor {
    const ANSI: [RenderColor; 16] = [
        RenderColor::rgb(0, 0, 0),
        RenderColor::rgb(205, 49, 49),
        RenderColor::rgb(13, 188, 121),
        RenderColor::rgb(229, 229, 16),
        RenderColor::rgb(36, 114, 200),
        RenderColor::rgb(188, 63, 188),
        RenderColor::rgb(17, 168, 205),
        RenderColor::rgb(229, 229, 229),
        RenderColor::rgb(102, 102, 102),
        RenderColor::rgb(241, 76, 76),
        RenderColor::rgb(35, 209, 139),
        RenderColor::rgb(245, 245, 67),
        RenderColor::rgb(59, 142, 234),
        RenderColor::rgb(214, 112, 214),
        RenderColor::rgb(41, 184, 219),
        RenderColor::rgb(255, 255, 255),
    ];

    ANSI[usize::from(index % 16)]
}

fn large_log_fixture(lines: usize) -> Vec<u8> {
    let mut out = String::with_capacity(lines * 80);
    for index in 0..lines {
        out.push_str(&format!(
            "2026-07-02T12:{:02}:{:02}Z INFO panea bench line={} component=transport status=ok\r\n",
            (index / 60) % 60,
            index % 60,
            index
        ));
    }
    out.into_bytes()
}

fn unicode_fixture(lines: usize) -> Vec<u8> {
    let samples = [
        "ASCII baseline panea",
        "wide CJK: 端末 性能 測定",
        "combining: cafe\u{301} nai\u{308}ve",
        "emoji fallback: 🚀 ✅ ⚠️",
        "\x1b[38;2;80;160;255mtruecolor\x1b[0m and \x1b[33mindexed\x1b[0m",
    ];
    let mut out = String::with_capacity(lines * 48);
    for index in 0..lines {
        out.push_str(samples[index % samples.len()]);
        out.push_str("\r\n");
    }
    out.into_bytes()
}

fn color_heavy_fixture(lines: usize) -> Vec<u8> {
    let mut out = String::with_capacity(lines * 96);
    for index in 0..lines {
        let red = (index % 255) as u8;
        let green = ((index * 3) % 255) as u8;
        let blue = ((index * 7) % 255) as u8;
        out.push_str(&format!(
            "\x1b[38;2;{red};{green};{blue}mtruecolor-{index}\x1b[0m \x1b[{}mindexed-color\x1b[0m\r\n",
            30 + (index % 8)
        ));
    }
    out.into_bytes()
}

fn alternate_screen_fixture(redraws: usize) -> Vec<u8> {
    let mut out = String::from("\x1b[?1049h\x1b[2J");
    for index in 0..redraws {
        out.push_str(&format!(
            "\x1b[Hframe={index} panea alternate screen redraw storm\r\n\x1b[32mstatus ok\x1b[0m"
        ));
    }
    out.push_str("\x1b[?1049l");
    out.into_bytes()
}

struct ProfileSettings {
    budget: PerformanceBudget,
    frame_rate_limit: Option<u16>,
    animations_enabled: bool,
    expensive_effect_warnings: bool,
}

impl ProfileSettings {
    fn for_profile(profile: PerformanceProfile) -> Self {
        match profile {
            PerformanceProfile::MaximumPerformance => Self {
                budget: PerformanceBudget {
                    max_frame_time: Duration::from_millis(8),
                    max_idle_wakeups_per_second: 1,
                    max_damage_regions: 128,
                },
                frame_rate_limit: None,
                animations_enabled: false,
                expensive_effect_warnings: true,
            },
            PerformanceProfile::Balanced => Self {
                budget: PerformanceBudget::default(),
                frame_rate_limit: None,
                animations_enabled: true,
                expensive_effect_warnings: true,
            },
            PerformanceProfile::Visual => Self {
                budget: PerformanceBudget {
                    max_frame_time: Duration::from_millis(20),
                    max_idle_wakeups_per_second: 4,
                    max_damage_regions: 384,
                },
                frame_rate_limit: None,
                animations_enabled: true,
                expensive_effect_warnings: true,
            },
            PerformanceProfile::BatterySaver => Self {
                budget: PerformanceBudget {
                    max_frame_time: Duration::from_millis(33),
                    max_idle_wakeups_per_second: 1,
                    max_damage_regions: 128,
                },
                frame_rate_limit: Some(30),
                animations_enabled: false,
                expensive_effect_warnings: true,
            },
        }
    }
}

struct BenchmarkResult {
    name: &'static str,
    iterations: u64,
    bytes: usize,
    elapsed: Duration,
    instrumentation: RenderInstrumentation,
}

fn print_result(result: BenchmarkResult) {
    let bytes_per_second = if result.elapsed.is_zero() {
        0.0
    } else {
        result.bytes as f64 / result.elapsed.as_secs_f64()
    };
    let budget = PerformanceBudget::default();
    let gate = evaluate_performance_gate(result.instrumentation, budget);
    let mut overlay = PerformanceOverlay::new(true, "batched-renderer");
    overlay.record(result.instrumentation);
    let overlay_text = overlay
        .render_text(budget)
        .unwrap_or_else(|| "overlay=unavailable".to_owned());

    println!(
        "bench={} iterations={} bytes={} elapsed={:?} bytes_per_second={:.0} gate_passed={} {}",
        result.name,
        result.iterations,
        result.bytes,
        result.elapsed,
        bytes_per_second,
        gate.passed,
        overlay_text,
    );

    for warning in gate.warnings {
        println!("bench={} warning={}", result.name, warning.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use render_wgpu::{FrameDecision, FrameScheduler};

    #[test]
    fn fixtures_are_deterministic() {
        assert_eq!(large_log_fixture(10), large_log_fixture(10));
        assert_eq!(color_heavy_fixture(10), color_heavy_fixture(10));
        assert_eq!(unicode_fixture(10), unicode_fixture(10));
    }

    #[test]
    fn scheduler_counts_idle_wakeups() {
        let mut scheduler = FrameScheduler::new();
        assert_eq!(scheduler.next_frame(), FrameDecision::NoFrameNeeded);
        assert_eq!(scheduler.idle_wakeups(), 1);
        scheduler.terminal_content_changed();
        assert!(matches!(
            scheduler.next_frame(),
            FrameDecision::FrameNeeded(_)
        ));
    }
}
