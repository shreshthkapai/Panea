use std::{
    env,
    error::Error,
    process::ExitCode,
    sync::Arc,
    time::{Duration, Instant},
};

use config_core::PerformanceProfile;
use diagnostics::{
    PerformanceBudget, PerformanceOverlay, evaluate_feature_cost, evaluate_performance_gate,
};
use font_system::{FontConfig, FontSystem};
use render_core::{
    AnimationHandle, AnimationKind, CellPosition, CursorImageAsset, CursorImageFrame,
    CursorImageVisual, CursorVisual, FeatureCostSample, OptionalFeature, OptionalFeatureCostMode,
    OverlayKind, OverlayPrimitive, RenderCell, RenderCellStyle, RenderColor, RenderCursorShape,
    RenderGrid, RenderInstrumentation, RenderRect, RenderScene, WindowChromeControlKind,
    WindowChromeControlVisual, WindowChromeVisual,
};
use render_wgpu::{DamageTracker, TerminalRasterizer};
use term_core::{
    CellAttributes, Color, CursorShape, TerminalCore, TerminalSize as CoreTerminalSize,
};
use term_parser::TerminalEmulator;

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 36;
const FULLSCREEN_CHROME_SURFACE_WIDTH: u32 = 1_920;
const FULLSCREEN_CHROME_SURFACE_HEIGHT: u32 = 1_080;
const FULLSCREEN_CHROME_HEIGHT: u32 = 36;
const FULLSCREEN_CHROME_TRANSITION: Duration = Duration::from_millis(120);
const FULLSCREEN_CHROME_FPS: u64 = 60;

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
        "render-coding-agent" => batch_scene_bench(
            "render-coding-agent",
            coding_agent_scene(DEFAULT_COLS, DEFAULT_ROWS),
            20,
        ),
        "render-partial-update" => partial_update_bench(),
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
        "fullscreen-chrome" => fullscreen_chrome_benchmark(),
        other => Err(format!("unknown benchmark '{other}'").into()),
    }
}

fn print_help() {
    println!(
        "usage: cargo xtask bench <all|profiles|render-grid|render-full-ascii|render-mixed-unicode|render-emoji-heavy|render-fast-scrolling|render-large-scrollback-viewport|render-many-panes|render-coding-agent|render-partial-update|render-cursor-animation|render-command-blocks|cat-large-file|color-heavy|scrollback|resize|input-latency|unicode|alternate-screen|cursor-animation|command-blocks|fullscreen-chrome>"
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
    batch_scene_bench(
        "render-coding-agent",
        coding_agent_scene(DEFAULT_COLS, DEFAULT_ROWS),
        20,
    )?;
    partial_update_bench()?;
    render_command_blocks()?;
    fullscreen_chrome_benchmark()?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FullscreenChromeBenchMode {
    Disabled,
    Instant,
    Smooth,
}

impl FullscreenChromeBenchMode {
    const fn name(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Instant => "instant",
            Self::Smooth => "smooth",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FullscreenChromeBenchmarkResult {
    mode: FullscreenChromeBenchMode,
    cpu_prepare: Duration,
    frames: u64,
    frame_budget: u64,
    dirty_pixels: u64,
    draw_calls: u64,
    max_damage_height: u32,
}

fn fullscreen_chrome_benchmark() -> Result<(), Box<dyn Error>> {
    let results = fullscreen_chrome_benchmark_results()?;
    print!("{}", fullscreen_chrome_benchmark_report(&results));
    Ok(())
}

fn fullscreen_chrome_benchmark_results()
-> Result<Vec<FullscreenChromeBenchmarkResult>, Box<dyn Error>> {
    [
        FullscreenChromeBenchMode::Disabled,
        FullscreenChromeBenchMode::Instant,
        FullscreenChromeBenchMode::Smooth,
    ]
    .into_iter()
    .map(fullscreen_chrome_benchmark_case)
    .collect()
}

fn fullscreen_chrome_benchmark_case(
    mode: FullscreenChromeBenchMode,
) -> Result<FullscreenChromeBenchmarkResult, Box<dyn Error>> {
    let frame_budget = fullscreen_chrome_frame_budget();
    if mode == FullscreenChromeBenchMode::Disabled {
        return Ok(FullscreenChromeBenchmarkResult {
            mode,
            cpu_prepare: Duration::ZERO,
            frames: 0,
            frame_budget,
            dirty_pixels: 0,
            draw_calls: 0,
            max_damage_height: 0,
        });
    }

    let mut fonts = FontSystem::new(FontConfig::default());
    let metrics = fonts.cell_metrics()?;
    let mut rasterizer = TerminalRasterizer::default();
    let mut damage_tracker = DamageTracker::new();
    let hidden = fullscreen_chrome_scene(None);
    let _ = damage_tracker.update(&hidden, metrics);
    let frame_count = if mode == FullscreenChromeBenchMode::Instant {
        1
    } else {
        frame_budget
    };
    let mut result = FullscreenChromeBenchmarkResult {
        mode,
        cpu_prepare: Duration::ZERO,
        frames: 0,
        frame_budget,
        dirty_pixels: 0,
        draw_calls: 0,
        max_damage_height: 0,
    };

    for frame in 1..=frame_count {
        let progress = if mode == FullscreenChromeBenchMode::Instant {
            u16::MAX
        } else {
            ((u64::from(u16::MAX) * frame) / frame_count).min(u64::from(u16::MAX)) as u16
        };
        let mut scene = fullscreen_chrome_scene(Some(progress));
        scene.damage_regions = damage_tracker
            .update(&scene, metrics)
            .into_iter()
            .filter_map(clip_fullscreen_chrome_damage)
            .collect();
        for damage in &scene.damage_regions {
            result.dirty_pixels = result
                .dirty_pixels
                .saturating_add(u64::from(damage.width) * u64::from(damage.height));
            result.max_damage_height = result.max_damage_height.max(damage.height);
        }
        let batches = rasterizer.prepare_batches(&scene, &mut fonts)?;
        result.cpu_prepare = result
            .cpu_prepare
            .saturating_add(batches.instrumentation.cpu_prepare_time);
        result.draw_calls = result
            .draw_calls
            .saturating_add(u64::from(batches.draw_call_count()));
        result.frames = result.frames.saturating_add(1);
    }

    Ok(result)
}

fn fullscreen_chrome_frame_budget() -> u64 {
    let transition_nanos = FULLSCREEN_CHROME_TRANSITION.as_nanos();
    let frames = transition_nanos
        .saturating_mul(u128::from(FULLSCREEN_CHROME_FPS))
        .div_ceil(1_000_000_000);
    u64::try_from(frames).unwrap_or(u64::MAX).saturating_add(1)
}

fn fullscreen_chrome_scene(progress: Option<u16>) -> RenderScene {
    let mut scene = RenderScene {
        grid: RenderGrid {
            columns: 1,
            rows: 1,
            cells: Vec::new(),
        },
        ..RenderScene::default()
    };
    let Some(progress) = progress else {
        return scene;
    };
    let visible_height = (u64::from(FULLSCREEN_CHROME_HEIGHT) * u64::from(progress))
        .div_ceil(u64::from(u16::MAX))
        .min(u64::from(FULLSCREEN_CHROME_HEIGHT)) as u32;
    let y = visible_height as i32 - FULLSCREEN_CHROME_HEIGHT as i32;
    let control_width = FULLSCREEN_CHROME_HEIGHT;
    let controls = [
        (WindowChromeControlKind::Minimize, 3u32),
        (WindowChromeControlKind::LeaveFullscreen, 2u32),
        (WindowChromeControlKind::Close, 1u32),
    ]
    .into_iter()
    .map(|(kind, slot)| WindowChromeControlVisual {
        kind,
        bounds: RenderRect {
            x: FULLSCREEN_CHROME_SURFACE_WIDTH.saturating_sub(control_width.saturating_mul(slot))
                as i32,
            y,
            width: control_width,
            height: FULLSCREEN_CHROME_HEIGHT,
        },
        hovered: false,
        pressed: false,
    })
    .collect();
    scene.window_chrome = Some(WindowChromeVisual {
        bounds: RenderRect {
            x: 0,
            y,
            width: FULLSCREEN_CHROME_SURFACE_WIDTH,
            height: FULLSCREEN_CHROME_HEIGHT,
        },
        opacity: progress,
        title: "Panea".to_owned(),
        show_logo: true,
        controls,
    });
    scene
}

fn clip_fullscreen_chrome_damage(region: RenderRect) -> Option<RenderRect> {
    let left = i64::from(region.x).max(0);
    let top = i64::from(region.y).max(0);
    let right = (i64::from(region.x) + i64::from(region.width))
        .min(i64::from(FULLSCREEN_CHROME_SURFACE_WIDTH));
    let bottom = (i64::from(region.y) + i64::from(region.height))
        .min(i64::from(FULLSCREEN_CHROME_SURFACE_HEIGHT))
        .min(i64::from(FULLSCREEN_CHROME_HEIGHT));
    (right > left && bottom > top).then(|| RenderRect {
        x: left as i32,
        y: top as i32,
        width: u32::try_from(right - left).unwrap_or(u32::MAX),
        height: u32::try_from(bottom - top).unwrap_or(u32::MAX),
    })
}

fn fullscreen_chrome_benchmark_report(results: &[FullscreenChromeBenchmarkResult]) -> String {
    results
        .iter()
        .map(|result| {
            format!(
                "bench=fullscreen-chrome mode={} surface={}x{} transition_ms={} cpu_prepare={:?} frames={} dirty_pixels={} draw_calls={}\n",
                result.mode.name(),
                FULLSCREEN_CHROME_SURFACE_WIDTH,
                FULLSCREEN_CHROME_SURFACE_HEIGHT,
                FULLSCREEN_CHROME_TRANSITION.as_millis(),
                result.cpu_prepare,
                result.frames,
                result.dirty_pixels,
                result.draw_calls,
            )
        })
        .collect()
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
    let cold_batches = rasterizer.prepare_batches(&scene, &mut fonts)?;
    let cold = cold_batches.instrumentation;
    let mut recycled = Some(cold_batches);
    let started = Instant::now();
    let iterations = 5;
    let mut last = RenderInstrumentation::default();

    for _ in 0..iterations {
        let batches = rasterizer.prepare_batches_reusing(&scene, &mut fonts, recycled.take())?;
        last = batches.instrumentation;
        recycled = Some(batches);
    }

    println!(
        "bench=render-grid-cold cpu_prepare={:?} glyph_misses={} atlas_uploads={}",
        cold.cpu_prepare_time, cold.glyphs.cache_misses, cold.glyphs.atlas_uploads
    );

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
    let cold_batches = rasterizer.prepare_batches(&scene, &mut fonts)?;
    let cold = cold_batches.instrumentation;
    let mut recycled = Some(cold_batches);
    let started = Instant::now();
    let mut last = RenderInstrumentation::default();

    for _ in 0..iterations {
        let batches = rasterizer.prepare_batches_reusing(&scene, &mut fonts, recycled.take())?;
        last = batches.instrumentation;
        recycled = Some(batches);
    }

    println!(
        "bench={name}-cold cpu_prepare={:?} glyph_misses={} atlas_uploads={}",
        cold.cpu_prepare_time, cold.glyphs.cache_misses, cold.glyphs.atlas_uploads
    );

    print_result(BenchmarkResult {
        name,
        iterations,
        bytes: scene.grid.cells.len(),
        elapsed: started.elapsed(),
        instrumentation: last,
    });
    Ok(())
}

fn partial_update_bench() -> Result<(), Box<dyn Error>> {
    let mut scene = coding_agent_scene(DEFAULT_COLS, DEFAULT_ROWS);
    let mut fonts = FontSystem::new(FontConfig::default());
    let metrics = fonts.cell_metrics()?;
    let mut rasterizer = TerminalRasterizer::default();
    let mut recycled = Some(rasterizer.prepare_full_batches(&scene, &mut fonts)?);
    let target_row = 8_u16;
    let target_col = 24_u16;
    let target_index =
        usize::from(target_row) * usize::from(DEFAULT_COLS) + usize::from(target_col);
    let iterations = 500_u64;
    let mut samples = Vec::with_capacity(iterations as usize);
    let mut last = RenderInstrumentation::default();
    let started = Instant::now();

    for iteration in 0..iterations {
        scene.grid.cells[target_index].text = if iteration % 2 == 0 { "x" } else { " " }.to_owned();
        let x0 = (f32::from(target_col) * metrics.cell_width).floor() as i32;
        let x1 = (f32::from(target_col + 1) * metrics.cell_width).ceil() as i32;
        let y0 = (f32::from(target_row) * metrics.cell_height).floor() as i32;
        let y1 = (f32::from(target_row + 1) * metrics.cell_height).ceil() as i32;
        scene.damage_regions = vec![RenderRect {
            x: x0,
            y: y0,
            width: x1.saturating_sub(x0).max(1) as u32,
            height: y1.saturating_sub(y0).max(1) as u32,
        }];
        let batches = rasterizer.prepare_batches_reusing(&scene, &mut fonts, recycled.take())?;
        last = batches.instrumentation;
        samples.push(last.cpu_prepare_time);
        recycled = Some(batches);
    }

    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
    println!(
        "bench=render-partial-update-distribution p50={p50:?} p95={p95:?} damage_regions={} draw_calls={}",
        last.damage_region_count, last.draw_call_count
    );
    print_result(BenchmarkResult {
        name: "render-partial-update",
        iterations,
        bytes: iterations as usize,
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
    if !disabled_report.passed {
        return Err("disabled cursor animation recorded render work".into());
    }

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

        let (frame_count, dimension) = match mode {
            OptionalFeatureCostMode::EnabledDefault => (24, 16),
            OptionalFeatureCostMode::EnabledHeavy => (120, 64),
            OptionalFeatureCostMode::Disabled => unreachable!(),
        };
        let mut image_scene = generated_scene(DEFAULT_COLS, DEFAULT_ROWS, mode);
        let asset = benchmark_cursor_image_asset(frame_count, dimension);
        image_scene.cursor_image = Some(CursorImageVisual {
            asset: Arc::clone(&asset),
            frame_index: u16::try_from(frame_count - 1).unwrap_or(u16::MAX),
            bounds: RenderRect {
                x: 32,
                y: 32,
                width: 16,
                height: 24,
            },
            opacity: 255,
        });
        image_scene.damage_regions = vec![RenderRect {
            x: 32,
            y: 32,
            width: 16,
            height: 24,
        }];
        let batches = rasterizer.prepare_batches(&image_scene, &mut fonts)?;
        let decoded_bytes = asset
            .frames
            .iter()
            .map(|frame| frame.pixels.len())
            .sum::<usize>();
        println!(
            "feature=image-cursor mode={mode:?} frames={frame_count} decoded_bytes={decoded_bytes} quads={} draw_calls={}",
            batches.cursor_image.quad_count(),
            batches.draw_call_count()
        );
    }

    Ok(())
}

fn benchmark_cursor_image_asset(frame_count: usize, dimension: u32) -> Arc<CursorImageAsset> {
    let pixels_per_frame =
        usize::try_from(dimension.saturating_mul(dimension).saturating_mul(4)).unwrap_or(0);
    let frames = (0..frame_count)
        .map(|index| {
            let mut pixels = vec![0; pixels_per_frame];
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.copy_from_slice(&[40, (index % 255) as u8, 220, 220]);
            }
            CursorImageFrame {
                pixels: pixels.into(),
            }
        })
        .collect::<Vec<_>>();
    Arc::new(CursorImageAsset {
        id: u64::try_from(frame_count).unwrap_or(u64::MAX),
        width: dimension,
        height: dimension,
        frames: frames.into(),
    })
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
            border_width_px: 1,
            corner_radius_px: 2,
            z_index: 10,
            label: None,
            label_color: None,
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
            text_color: None,
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
                border_width_px: 0,
                corner_radius_px: 2,
                z_index: 10,
                label: Some("synthetic expensive overlay".to_owned()),
                label_color: None,
            }]
        } else {
            Vec::new()
        },
        animations,
        ..RenderScene::default()
    }
}

fn coding_agent_scene(cols: u16, rows: u16) -> RenderScene {
    let mut scene = generated_scene(cols, rows, OptionalFeatureCostMode::Disabled);
    for cell in &mut scene.grid.cells {
        cell.text = " ".to_owned();
        cell.foreground = RenderColor::rgb(214, 222, 244);
        cell.background = RenderColor::rgb(18, 18, 28);
        cell.style = RenderCellStyle::default();
    }

    let lines = [
        ">_ Panea coding-agent render fixture",
        "model: terminal correctness + stable incremental shaping",
        "directory: ~/panea",
        "",
        "Tip: visit https://chatgpt.com/codex?app=landing-page=true",
        "",
        "- cursor redraw must preserve stable text geometry",
        "- unchanged glyphs remain cached and resident",
        "Ask Codex to do anything",
    ];
    for (row, line) in lines.into_iter().enumerate().take(usize::from(rows)) {
        for (col, ch) in line.chars().enumerate().take(usize::from(cols)) {
            let index = row * usize::from(cols) + col;
            scene.grid.cells[index].text = ch.to_string();
        }
    }
    scene.cursor = Some(CursorVisual {
        position: CellPosition {
            row: i64::from(rows.min(9).saturating_sub(1)),
            col: 26_u16.min(cols.saturating_sub(1)),
        },
        shape: RenderCursorShape::Block,
        color: RenderColor::rgb(235, 235, 245),
        text_color: None,
        visible: true,
        thickness_percent: 15,
        corner_radius_px: 0,
        inactive: false,
    });
    scene
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
    let region = RenderRect {
        x,
        y,
        width: 24,
        height: 24,
    };
    AnimationHandle {
        id,
        kind: AnimationKind::CursorTypingPulse,
        affected_region: region,
        start_region: region,
        end_region: region,
        color: RenderColor::rgb(120, 190, 255),
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
            text_color: None,
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

    #[test]
    fn fullscreen_chrome_benchmark_has_disabled_instant_and_smooth_cases() {
        let results = fullscreen_chrome_benchmark_results().expect("fullscreen chrome benchmark");

        assert_eq!(
            results.iter().map(|result| result.mode).collect::<Vec<_>>(),
            vec![
                FullscreenChromeBenchMode::Disabled,
                FullscreenChromeBenchMode::Instant,
                FullscreenChromeBenchMode::Smooth,
            ]
        );
        let disabled = &results[0];
        assert_eq!(disabled.frames, 0);
        assert_eq!(disabled.dirty_pixels, 0);
        assert_eq!(disabled.draw_calls, 0);

        let smooth = &results[2];
        assert!(smooth.frames <= smooth.frame_budget);
        assert!(smooth.max_damage_height <= FULLSCREEN_CHROME_HEIGHT);
        assert!(smooth.dirty_pixels > 0);
        assert!(smooth.draw_calls > 0);
    }

    #[test]
    fn fullscreen_chrome_benchmark_report_exposes_required_metrics() {
        let results = fullscreen_chrome_benchmark_results().expect("fullscreen chrome benchmark");
        let report = fullscreen_chrome_benchmark_report(&results);

        for mode in ["disabled", "instant", "smooth"] {
            assert!(report.contains(&format!("mode={mode}")));
        }
        for field in ["cpu_prepare=", "frames=", "dirty_pixels=", "draw_calls="] {
            assert!(report.contains(field));
        }
        assert!(report.contains("surface=1920x1080"));
        assert!(report.contains("transition_ms=120"));
    }
}
