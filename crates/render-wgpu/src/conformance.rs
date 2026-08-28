// CPU rasterization and deterministic screenshot fixtures for explicit conformance builds.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

fn rgb_buffer_len(width: u32, height: u32) -> Option<usize> {
    pixel_buffer_len(width, height, 3)
}

fn rgba_buffer_len(width: u32, height: u32) -> Option<usize> {
    pixel_buffer_len(width, height, 4)
}

impl CpuFrame {
    #[must_use]
    pub fn snapshot_hash(&self) -> u64 {
        let mut hash = 14_695_981_039_346_656_037_u64;
        for byte in &self.pixels {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
        hash
    }

    #[must_use]
    pub fn encode_ppm(&self) -> Vec<u8> {
        let mut output = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        output.reserve(rgb_buffer_len(self.width, self.height).unwrap_or(0));
        for pixel in self.pixels.chunks_exact(4) {
            output.extend_from_slice(&pixel[..3]);
        }
        output
    }

    pub fn decode_ppm(bytes: &[u8]) -> Result<Self, ScreenshotError> {
        let mut index = 0;
        let magic = next_ppm_token(bytes, &mut index)?;
        if magic != b"P6" {
            return Err(ScreenshotError::InvalidImage(
                "expected binary PPM magic P6".to_owned(),
            ));
        }

        let width = parse_ppm_u32(next_ppm_token(bytes, &mut index)?, "width")?;
        let height = parse_ppm_u32(next_ppm_token(bytes, &mut index)?, "height")?;
        let max = parse_ppm_u32(next_ppm_token(bytes, &mut index)?, "max channel")?;
        if max != 255 {
            return Err(ScreenshotError::InvalidImage(
                "expected max channel value 255".to_owned(),
            ));
        }

        if index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }

        let expected = rgb_buffer_len(width, height).ok_or_else(|| {
            ScreenshotError::InvalidImage("PPM dimensions overflow addressable memory".to_owned())
        })?;
        let Some(rgb) = bytes.get(index..index.saturating_add(expected)) else {
            return Err(ScreenshotError::InvalidImage(
                "PPM image payload is truncated".to_owned(),
            ));
        };
        if rgb.len() != expected {
            return Err(ScreenshotError::InvalidImage(
                "PPM image payload has unexpected length".to_owned(),
            ));
        }

        let mut pixels = Vec::with_capacity(rgba_buffer_len(width, height).ok_or_else(|| {
            ScreenshotError::InvalidImage("RGBA dimensions overflow addressable memory".to_owned())
        })?);
        for pixel in rgb.chunks_exact(3) {
            pixels.extend_from_slice(pixel);
            pixels.push(u8::MAX);
        }

        Ok(Self {
            width,
            height,
            pixels,
        })
    }
}

fn next_ppm_token<'a>(bytes: &'a [u8], index: &mut usize) -> Result<&'a [u8], ScreenshotError> {
    loop {
        while *index < bytes.len() && bytes[*index].is_ascii_whitespace() {
            *index += 1;
        }
        if *index < bytes.len() && bytes[*index] == b'#' {
            while *index < bytes.len() && bytes[*index] != b'\n' {
                *index += 1;
            }
            continue;
        }
        break;
    }

    let start = *index;
    while *index < bytes.len() && !bytes[*index].is_ascii_whitespace() {
        *index += 1;
    }
    if start == *index {
        Err(ScreenshotError::InvalidImage(
            "PPM image header is incomplete".to_owned(),
        ))
    } else {
        Ok(&bytes[start..*index])
    }
}

fn parse_ppm_u32(token: &[u8], name: &str) -> Result<u32, ScreenshotError> {
    let text = std::str::from_utf8(token).map_err(|_| {
        ScreenshotError::InvalidImage(format!("PPM {name} token is not valid UTF-8"))
    })?;
    text.parse::<u32>()
        .map_err(|error| ScreenshotError::InvalidImage(format!("invalid PPM {name}: {error}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenshotError {
    UnknownFixture(String),
    InvalidImage(String),
    Render(String),
}

impl fmt::Display for ScreenshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFixture(name) => write!(f, "unknown screenshot fixture: {name}"),
            Self::InvalidImage(message) => write!(f, "invalid screenshot image: {message}"),
            Self::Render(message) => write!(f, "failed to render screenshot fixture: {message}"),
        }
    }
}

impl Error for ScreenshotError {}

impl From<RendererError> for ScreenshotError {
    fn from(value: RendererError) -> Self {
        Self::Render(value.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotFixtureKind {
    AsciiGrid,
    TruecolorGrid,
    TextStyles,
    CjkWide,
    Emoji,
    CursorStates,
    CursorImage,
    SelectionStates,
    PromptDecorations,
    CommandBlocks,
    MultiplePanes,
    TransparencyOpacity,
    FullscreenChrome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotFixture {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: ScreenshotFixtureKind,
    pub scene: RenderScene,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotCapture {
    pub fixture_name: String,
    pub frame: CpuFrame,
    pub instrumentation: RenderInstrumentation,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenshotTolerance {
    pub max_channel_delta: u8,
    pub max_different_pixel_percent: f32,
    pub max_mean_channel_delta: f32,
    pub layout_failure_pixel_percent: f32,
}

impl Default for ScreenshotTolerance {
    fn default() -> Self {
        Self {
            max_channel_delta: 3,
            max_different_pixel_percent: 0.25,
            max_mean_channel_delta: 0.75,
            layout_failure_pixel_percent: 5.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotDiffKind {
    Exact,
    AntialiasingWithinTolerance,
    MinorPixelDrift,
    TextLayoutFailure,
    DimensionMismatch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenshotDiff {
    pub kind: ScreenshotDiffKind,
    pub passed: bool,
    pub width: u32,
    pub height: u32,
    pub total_pixels: u64,
    pub different_pixels: u64,
    pub different_pixel_percent: f32,
    pub max_channel_delta: u8,
    pub mean_channel_delta: f32,
    pub message: String,
}

impl ScreenshotDiff {
    #[must_use]
    pub fn render_summary(&self, fixture_name: &str) -> String {
        format!(
            "fixture={} result={:?} passed={} size={}x{} diff_pixels={}/{} ({:.3}%) max_delta={} mean_delta={:.3} message={}",
            fixture_name,
            self.kind,
            self.passed,
            self.width,
            self.height,
            self.different_pixels,
            self.total_pixels,
            self.different_pixel_percent,
            self.max_channel_delta,
            self.mean_channel_delta,
            self.message
        )
    }
}

pub fn screenshot_fixture_names() -> Vec<&'static str> {
    screenshot_fixtures()
        .into_iter()
        .map(|fixture| fixture.name)
        .collect()
}

pub fn screenshot_fixtures() -> Vec<ScreenshotFixture> {
    vec![
        ScreenshotFixture {
            name: "ascii-grid",
            description: "plain ASCII grid with cursor",
            kind: ScreenshotFixtureKind::AsciiGrid,
            scene: ascii_grid_scene(),
        },
        ScreenshotFixture {
            name: "truecolor-grid",
            description: "deterministic truecolor foreground/background cells",
            kind: ScreenshotFixtureKind::TruecolorGrid,
            scene: truecolor_grid_scene(),
        },
        ScreenshotFixture {
            name: "text-styles",
            description: "bold, italic, underline, and strikethrough groundwork",
            kind: ScreenshotFixtureKind::TextStyles,
            scene: text_styles_scene(),
        },
        ScreenshotFixture {
            name: "cjk-wide",
            description: "wide CJK and mixed Latin text",
            kind: ScreenshotFixtureKind::CjkWide,
            scene: cjk_scene(),
        },
        ScreenshotFixture {
            name: "emoji",
            description: "emoji, modifiers, variation selectors, and ZWJ samples",
            kind: ScreenshotFixtureKind::Emoji,
            scene: emoji_scene(),
        },
        ScreenshotFixture {
            name: "cursor-states",
            description: "block, beam, underline, hollow, and inactive cursor shapes",
            kind: ScreenshotFixtureKind::CursorStates,
            scene: cursor_states_scene(),
        },
        ScreenshotFixture {
            name: "cursor-image",
            description: "decoded custom image cursor composited as an overlay",
            kind: ScreenshotFixtureKind::CursorImage,
            scene: cursor_image_scene(),
        },
        ScreenshotFixture {
            name: "selection-states",
            description: "selection highlight cells over terminal content",
            kind: ScreenshotFixtureKind::SelectionStates,
            scene: selection_scene(),
        },
        ScreenshotFixture {
            name: "prompt-decorations",
            description: "semantic prompt decoration overlays",
            kind: ScreenshotFixtureKind::PromptDecorations,
            scene: prompt_decoration_scene(),
        },
        ScreenshotFixture {
            name: "command-blocks",
            description: "semantic command block overlays",
            kind: ScreenshotFixtureKind::CommandBlocks,
            scene: command_blocks_scene(),
        },
        ScreenshotFixture {
            name: "multiple-panes",
            description: "split-pane style visual composition",
            kind: ScreenshotFixtureKind::MultiplePanes,
            scene: multiple_panes_scene(),
        },
        ScreenshotFixture {
            name: "transparency-opacity",
            description: "semi-transparent overlays blended over terminal cells",
            kind: ScreenshotFixtureKind::TransparencyOpacity,
            scene: transparency_scene(),
        },
        ScreenshotFixture {
            name: "fullscreen-chrome-hidden",
            description: "fullscreen terminal with client chrome fully hidden",
            kind: ScreenshotFixtureKind::FullscreenChrome,
            scene: fullscreen_chrome_scene(0, false, true),
        },
        ScreenshotFixture {
            name: "fullscreen-chrome-half",
            description: "fullscreen terminal with client chrome half revealed",
            kind: ScreenshotFixtureKind::FullscreenChrome,
            scene: fullscreen_chrome_scene(u16::MAX / 2, false, true),
        },
        ScreenshotFixture {
            name: "fullscreen-chrome-visible",
            description: "fullscreen terminal with client chrome fully revealed",
            kind: ScreenshotFixtureKind::FullscreenChrome,
            scene: fullscreen_chrome_scene(u16::MAX, false, true),
        },
        ScreenshotFixture {
            name: "fullscreen-chrome-close-hover",
            description: "fullscreen terminal with close control hovered",
            kind: ScreenshotFixtureKind::FullscreenChrome,
            scene: fullscreen_chrome_scene(u16::MAX, true, true),
        },
        ScreenshotFixture {
            name: "fullscreen-chrome-no-controls",
            description: "fullscreen terminal chrome without window controls",
            kind: ScreenshotFixtureKind::FullscreenChrome,
            scene: fullscreen_chrome_scene(u16::MAX, false, false),
        },
    ]
}

pub fn capture_screenshot_fixture(name: &str) -> Result<ScreenshotCapture, ScreenshotError> {
    let fixture = screenshot_fixtures()
        .into_iter()
        .find(|fixture| fixture.name == name)
        .ok_or_else(|| ScreenshotError::UnknownFixture(name.to_owned()))?;
    let mut fonts = FontSystem::new(font_system::FontConfig::default());
    let mut rasterizer = TerminalRasterizer::default();
    let rendered = rasterizer.rasterize_instrumented(&fixture.scene, &mut fonts)?;

    Ok(ScreenshotCapture {
        fixture_name: fixture.name.to_owned(),
        frame: rendered.frame,
        instrumentation: rendered.instrumentation,
    })
}

pub fn capture_all_screenshot_fixtures() -> Result<Vec<ScreenshotCapture>, ScreenshotError> {
    screenshot_fixture_names()
        .into_iter()
        .map(capture_screenshot_fixture)
        .collect()
}

#[must_use]
pub fn compare_screenshots(
    expected: &CpuFrame,
    actual: &CpuFrame,
    tolerance: ScreenshotTolerance,
) -> ScreenshotDiff {
    if expected.width != actual.width || expected.height != actual.height {
        return ScreenshotDiff {
            kind: ScreenshotDiffKind::DimensionMismatch,
            passed: false,
            width: actual.width,
            height: actual.height,
            total_pixels: 0,
            different_pixels: 0,
            different_pixel_percent: 100.0,
            max_channel_delta: u8::MAX,
            mean_channel_delta: f32::MAX,
            message: format!(
                "expected {}x{}, got {}x{}",
                expected.width, expected.height, actual.width, actual.height
            ),
        };
    }

    let mut different_pixels = 0_u64;
    let mut total_delta = 0_u64;
    let mut max_channel_delta = 0_u8;
    let total_pixels = u64::from(expected.width) * u64::from(expected.height);

    for (expected_pixel, actual_pixel) in expected
        .pixels
        .chunks_exact(4)
        .zip(actual.pixels.chunks_exact(4))
    {
        let mut pixel_changed = false;
        for channel in 0..3 {
            let delta = expected_pixel[channel].abs_diff(actual_pixel[channel]);
            if delta > 0 {
                pixel_changed = true;
            }
            max_channel_delta = max_channel_delta.max(delta);
            total_delta = total_delta.saturating_add(u64::from(delta));
        }
        if pixel_changed {
            different_pixels = different_pixels.saturating_add(1);
        }
    }

    let different_pixel_percent = if total_pixels == 0 {
        0.0
    } else {
        different_pixels as f32 * 100.0 / total_pixels as f32
    };
    let mean_channel_delta = if total_pixels == 0 {
        0.0
    } else {
        total_delta as f32 / (total_pixels as f32 * 3.0)
    };

    let (kind, passed, message) = if different_pixels == 0 {
        (
            ScreenshotDiffKind::Exact,
            true,
            "pixels match exactly".to_owned(),
        )
    } else if different_pixel_percent >= tolerance.layout_failure_pixel_percent {
        (
            ScreenshotDiffKind::TextLayoutFailure,
            false,
            "large pixel movement suggests font, glyph layout, or overlay geometry drift"
                .to_owned(),
        )
    } else if max_channel_delta <= tolerance.max_channel_delta
        && different_pixel_percent <= tolerance.max_different_pixel_percent
        && mean_channel_delta <= tolerance.max_mean_channel_delta
    {
        (
            ScreenshotDiffKind::AntialiasingWithinTolerance,
            true,
            "only bounded antialiasing-level differences detected".to_owned(),
        )
    } else {
        (
            ScreenshotDiffKind::MinorPixelDrift,
            false,
            "pixel differences exceed tolerance but do not look like broad text reflow".to_owned(),
        )
    };

    ScreenshotDiff {
        kind,
        passed,
        width: actual.width,
        height: actual.height,
        total_pixels,
        different_pixels,
        different_pixel_percent,
        max_channel_delta,
        mean_channel_delta,
        message,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenshotReport {
    pub platform_key: String,
    pub diffs: BTreeMap<String, ScreenshotDiff>,
}

impl ScreenshotReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.diffs.values().all(|diff| diff.passed)
    }

    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut lines = vec![
            format!("# Screenshot Verification Report - {}", self.platform_key),
            String::new(),
            "| Fixture | Result | Pixels Different | Max Delta | Mean Delta | Notes |".to_owned(),
            "| --- | --- | ---: | ---: | ---: | --- |".to_owned(),
        ];

        for (fixture, diff) in &self.diffs {
            lines.push(format!(
                "| {} | {:?} | {}/{} ({:.3}%) | {} | {:.3} | {} |",
                fixture,
                diff.kind,
                diff.different_pixels,
                diff.total_pixels,
                diff.different_pixel_percent,
                diff.max_channel_delta,
                diff.mean_channel_delta,
                diff.message.replace('|', "\\|")
            ));
        }

        lines.push(String::new());
        lines.push(format!(
            "Overall: {}",
            if self.passed() { "pass" } else { "fail" }
        ));
        lines.join("\n")
    }
}

#[must_use]
pub fn detect_screenshot_platform_key() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "linux-wayland"
        } else if std::env::var_os("DISPLAY").is_some() {
            "linux-x11"
        } else {
            "linux"
        }
    } else {
        "unknown"
    }
}

fn ascii_grid_scene() -> RenderScene {
    let mut scene = patterned_scene(24, 8, &["Panea ", "ASCII ", "grid  "]);
    scene.cursor = Some(cursor(2, 8, RenderCursorShape::Block, false));
    scene
}

fn truecolor_grid_scene() -> RenderScene {
    let mut scene = patterned_scene(24, 8, &["red   ", "green ", "blue  ", "cyan  "]);
    for cell in &mut scene.grid.cells {
        let row = cell.position.row as u8;
        let col = cell.position.col as u8;
        cell.foreground = RenderColor::rgb(
            80_u8.saturating_add(row.saturating_mul(16)),
            180_u8.saturating_sub(col.saturating_mul(3)),
            120_u8.saturating_add(col.saturating_mul(4)),
        );
        cell.background = RenderColor::rgb(
            8_u8.saturating_add(col.saturating_mul(4)),
            18_u8.saturating_add(row.saturating_mul(8)),
            42,
        );
    }
    scene.cursor = None;
    scene
}

fn text_styles_scene() -> RenderScene {
    let rows = [
        (
            "bold text sample",
            RenderCellStyle {
                bold: true,
                ..RenderCellStyle::default()
            },
        ),
        (
            "italic text sample",
            RenderCellStyle {
                italic: true,
                ..RenderCellStyle::default()
            },
        ),
        (
            "underline sample",
            RenderCellStyle {
                underline: true,
                ..RenderCellStyle::default()
            },
        ),
        (
            "strike text sample",
            RenderCellStyle {
                strikethrough: true,
                ..RenderCellStyle::default()
            },
        ),
    ];
    let mut cells = Vec::new();
    for (row, (text, style)) in rows.into_iter().enumerate() {
        push_text_row(&mut cells, row as i64, text, style);
    }
    RenderScene {
        grid: RenderGrid {
            columns: 24,
            rows: 8,
            cells: pad_cells(cells, 24, 8),
        },
        cursor: None,
        ..RenderScene::default()
    }
}

fn cjk_scene() -> RenderScene {
    text_rows_scene(&[
        "Panea terminal",
        "wide CJK: 端末 性能",
        "mixed: build 測定 ok",
        "copy keeps cells",
    ])
}

fn emoji_scene() -> RenderScene {
    text_rows_scene(&[
        "emoji: 🚀 ✅ ⚠️",
        "skin tone: 👍🏽",
        "zwj: 👩‍💻",
        "variant: ✨ ✈️",
    ])
}

fn cursor_states_scene() -> RenderScene {
    let mut scene = text_rows_scene(&[
        "block cursor",
        "beam cursor",
        "underline cursor",
        "hollow inactive",
    ]);
    scene.cursor = Some(cursor(0, 6, RenderCursorShape::Block, false));
    scene.decorations = vec![
        cursor_decoration(1, 6, RenderCursorShape::Beam),
        cursor_decoration(2, 10, RenderCursorShape::Underline),
        cursor_decoration(3, 7, RenderCursorShape::HollowBlock),
    ];
    scene
}

fn cursor_image_scene() -> RenderScene {
    let mut scene = text_rows_scene(&[
        "custom image cursor",
        "RGBA overlay asset",
        "raw cells unchanged",
        "bounded GPU upload",
    ]);
    let pixels = [
        255, 80, 90, 255, 255, 210, 80, 255, 70, 210, 255, 255, 255, 255, 255, 0, 255, 210, 80,
        255, 70, 210, 255, 255, 255, 255, 255, 0, 255, 80, 90, 255, 70, 210, 255, 255, 255, 255,
        255, 0, 255, 80, 90, 255, 255, 210, 80, 255, 255, 255, 255, 0, 255, 80, 90, 255, 255, 210,
        80, 255, 70, 210, 255, 255,
    ];
    scene.cursor_image = Some(CursorImageVisual {
        asset: Arc::new(CursorImageAsset {
            id: 0xC0_55_0A,
            width: 4,
            height: 4,
            frames: vec![CursorImageFrame {
                pixels: pixels.to_vec().into(),
            }]
            .into(),
        }),
        frame_index: 0,
        bounds: RenderRect {
            x: 48,
            y: 0,
            width: 12,
            height: 16,
        },
        opacity: 255,
    });
    scene
}

fn selection_scene() -> RenderScene {
    let mut scene = text_rows_scene(&[
        "select these words",
        "selection line two",
        "normal terminal text",
        "copy range stable",
    ]);
    scene.selections = vec![SelectionVisual {
        cells: (0..12)
            .map(|col| CellPosition { row: 1, col })
            .chain((7..12).map(|col| CellPosition { row: 0, col }))
            .collect(),
        color: RenderColor {
            red: 60,
            green: 120,
            blue: 210,
            alpha: 150,
        },
    }];
    scene.cursor = None;
    scene
}

fn prompt_decoration_scene() -> RenderScene {
    let mut scene = text_rows_scene(&[
        "panea ~/work main",
        "$ cargo test -q",
        "running tests...",
        "ok",
    ]);
    scene.semantic_overlays = vec![OverlayPrimitive {
        kind: OverlayKind::PromptDecoration,
        bounds: RenderRect {
            x: 4,
            y: 2,
            width: 150,
            height: 20,
        },
        color: RenderColor {
            red: 46,
            green: 88,
            blue: 98,
            alpha: 72,
        },
        border_color: Some(RenderColor::rgb(92, 170, 180)),
        border_width_px: 1,
        corner_radius_px: 4,
        z_index: 5,
        label: Some("prompt".to_owned()),
        label_color: None,
    }];
    scene
}

fn command_blocks_scene() -> RenderScene {
    let mut scene = text_rows_scene(&[
        "$ rg render",
        "render-core/src/lib.rs",
        "render-wgpu/src/lib.rs",
        "$ cargo test",
    ]);
    scene.semantic_overlays = vec![
        command_block_overlay(0, 0, 190, 62, RenderColor::rgb(72, 112, 76)),
        command_block_overlay(0, 66, 190, 44, RenderColor::rgb(120, 80, 70)),
    ];
    scene
}

fn multiple_panes_scene() -> RenderScene {
    let mut scene = text_rows_scene(&[
        "pane 1 build",
        "pane 1 output",
        "pane 2 shell",
        "pane 2 logs",
    ]);
    scene.decorations = vec![
        RenderDecoration {
            bounds: RenderRect {
                x: 0,
                y: 61,
                width: 200,
                height: 2,
            },
            color: RenderColor::rgb(110, 110, 118),
            border_color: None,
        },
        RenderDecoration {
            bounds: RenderRect {
                x: 100,
                y: 0,
                width: 2,
                height: 128,
            },
            color: RenderColor::rgb(110, 110, 118),
            border_color: None,
        },
    ];
    scene
}

fn transparency_scene() -> RenderScene {
    let mut scene = truecolor_grid_scene();
    scene.semantic_overlays = vec![
        OverlayPrimitive {
            kind: OverlayKind::Decoration,
            bounds: RenderRect {
                x: 18,
                y: 18,
                width: 112,
                height: 42,
            },
            color: RenderColor {
                red: 180,
                green: 80,
                blue: 120,
                alpha: 96,
            },
            border_color: Some(RenderColor {
                red: 255,
                green: 220,
                blue: 180,
                alpha: 180,
            }),
            border_width_px: 1,
            corner_radius_px: 3,
            z_index: 2,
            label: Some("opacity".to_owned()),
            label_color: None,
        },
        OverlayPrimitive {
            kind: OverlayKind::Badge,
            bounds: RenderRect {
                x: 58,
                y: 44,
                width: 116,
                height: 34,
            },
            color: RenderColor {
                red: 40,
                green: 160,
                blue: 180,
                alpha: 88,
            },
            border_color: None,
            border_width_px: 0,
            corner_radius_px: 2,
            z_index: 3,
            label: Some("badge".to_owned()),
            label_color: None,
        },
    ];
    scene
}

fn fullscreen_chrome_scene(
    progress: u16,
    close_hovered: bool,
    show_window_controls: bool,
) -> RenderScene {
    const SURFACE_WIDTH: u32 = 192;
    const CHROME_HEIGHT: u32 = 36;
    const CONTROL_WIDTH: u32 = 48;

    let mut scene = text_rows_scene(&[
        "PS C:\\Users\\panea>",
        "cargo test -q",
        "running 42 tests",
        "test result: ok",
    ]);
    scene.cursor = Some(cursor(1, 13, RenderCursorShape::Beam, false));
    if progress == 0 {
        return scene;
    }

    let visible_height =
        (u64::from(CHROME_HEIGHT) * u64::from(progress)).div_ceil(u64::from(u16::MAX)) as u32;
    let y = visible_height as i32 - CHROME_HEIGHT as i32;
    let controls = if show_window_controls {
        [
            (WindowChromeControlKind::Minimize, 3_u32),
            (WindowChromeControlKind::LeaveFullscreen, 2_u32),
            (WindowChromeControlKind::Close, 1_u32),
        ]
        .into_iter()
        .map(|(kind, slot)| WindowChromeControlVisual {
            kind,
            bounds: RenderRect {
                x: SURFACE_WIDTH.saturating_sub(CONTROL_WIDTH * slot) as i32,
                y,
                width: CONTROL_WIDTH,
                height: CHROME_HEIGHT,
            },
            hovered: close_hovered && kind == WindowChromeControlKind::Close,
            pressed: false,
        })
        .collect()
    } else {
        Vec::new()
    };
    scene.window_chrome = Some(WindowChromeVisual {
        bounds: RenderRect {
            x: 0,
            y,
            width: SURFACE_WIDTH,
            height: CHROME_HEIGHT,
        },
        opacity: progress,
        title: "Panea".to_owned(),
        show_logo: true,
        controls,
    });
    scene
}

fn patterned_scene(cols: u16, rows: u16, samples: &[&str]) -> RenderScene {
    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    for row in 0..rows {
        let sample = samples[usize::from(row) % samples.len()];
        for col in 0..cols {
            let ch = sample
                .chars()
                .nth(usize::from(col) % sample.chars().count())
                .unwrap_or(' ');
            cells.push(RenderCell {
                position: CellPosition {
                    row: i64::from(row),
                    col,
                },
                text: ch.to_string().into(),
                foreground: RenderColor::rgb(226, 226, 220),
                background: if row % 2 == 0 {
                    RenderColor::rgb(12, 14, 16)
                } else {
                    RenderColor::rgb(18, 20, 24)
                },
                style: RenderCellStyle::default(),
            });
        }
    }
    RenderScene {
        grid: RenderGrid {
            columns: cols,
            rows,
            cells,
        },
        ..RenderScene::default()
    }
}

fn text_rows_scene(rows: &[&str]) -> RenderScene {
    let mut cells = Vec::new();
    for (row, text) in rows.iter().enumerate() {
        push_text_row(&mut cells, row as i64, text, RenderCellStyle::default());
    }
    RenderScene {
        grid: RenderGrid {
            columns: 24,
            rows: 8,
            cells: pad_cells(cells, 24, 8),
        },
        cursor: Some(cursor(0, 0, RenderCursorShape::Beam, false)),
        ..RenderScene::default()
    }
}

fn push_text_row(cells: &mut Vec<RenderCell>, row: i64, text: &str, style: RenderCellStyle) {
    for (col, ch) in text.chars().take(24).enumerate() {
        cells.push(RenderCell {
            position: CellPosition {
                row,
                col: col as u16,
            },
            text: ch.to_string().into(),
            foreground: RenderColor::rgb(232, 232, 226),
            background: if row % 2 == 0 {
                RenderColor::rgb(11, 14, 18)
            } else {
                RenderColor::rgb(17, 21, 27)
            },
            style,
        });
    }
}

fn pad_cells(mut cells: Vec<RenderCell>, cols: u16, rows: u16) -> Vec<RenderCell> {
    let mut occupied = cells
        .iter()
        .map(|cell| (cell.position.row, cell.position.col))
        .collect::<std::collections::HashSet<_>>();
    for row in 0..rows {
        for col in 0..cols {
            if occupied.insert((i64::from(row), col)) {
                cells.push(RenderCell {
                    position: CellPosition {
                        row: i64::from(row),
                        col,
                    },
                    text: " ".into(),
                    foreground: RenderColor::rgb(232, 232, 226),
                    background: if row % 2 == 0 {
                        RenderColor::rgb(11, 14, 18)
                    } else {
                        RenderColor::rgb(17, 21, 27)
                    },
                    style: RenderCellStyle::default(),
                });
            }
        }
    }
    cells.sort_by_key(|cell| (cell.position.row, cell.position.col));
    cells
}

fn cursor(row: i64, col: u16, shape: RenderCursorShape, inactive: bool) -> CursorVisual {
    CursorVisual {
        position: CellPosition { row, col },
        shape,
        color: if inactive {
            RenderColor::rgb(120, 120, 128)
        } else {
            RenderColor::rgb(245, 245, 235)
        },
        text_color: None,
        visible: true,
        thickness_percent: 16,
        corner_radius_px: 0,
        inactive,
    }
}

fn cursor_decoration(row: i64, col: u16, shape: RenderCursorShape) -> RenderDecoration {
    let metrics = CellMetrics {
        font_size: 13.0,
        cell_width: 8.0,
        cell_height: 16.0,
        ascent: 11.0,
        descent: -3.0,
        line_gap: 1.0,
        baseline: 12.0,
        underline_position: 14.0,
        strikethrough_position: 7.0,
        decoration_thickness: 1.0,
    };
    let mut batch = QuadBatch::new(QuadBatchKind::Cursor);
    push_cursor_quads(
        &mut batch,
        cursor(row, col, shape, shape == RenderCursorShape::HollowBlock),
        metrics,
        &[RenderRect {
            x: 0,
            y: 0,
            width: 1000,
            height: 1000,
        }],
        render_core::RenderOffset::default(),
    );
    let first = batch
        .vertices
        .first()
        .map_or([0.0, 0.0], |vertex| vertex.position_px);
    let last = batch
        .vertices
        .get(2)
        .map_or(first, |vertex| vertex.position_px);
    RenderDecoration {
        bounds: RenderRect {
            x: first[0].floor() as i32,
            y: first[1].floor() as i32,
            width: (last[0] - first[0]).abs().ceil().max(1.0) as u32,
            height: (last[1] - first[1]).abs().ceil().max(1.0) as u32,
        },
        color: RenderColor::rgb(245, 245, 235),
        border_color: None,
    }
}

fn command_block_overlay(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    color: RenderColor,
) -> OverlayPrimitive {
    OverlayPrimitive {
        kind: OverlayKind::CommandBlock,
        bounds: RenderRect {
            x,
            y,
            width,
            height,
        },
        color: RenderColor { alpha: 62, ..color },
        border_color: Some(RenderColor {
            alpha: 150,
            ..color
        }),
        border_width_px: 1,
        corner_radius_px: 4,
        z_index: 8,
        label: Some("command".to_owned()),
        label_color: None,
    }
}

#[derive(Debug)]
pub struct InstrumentedCpuFrame {
    pub frame: CpuFrame,
    pub instrumentation: RenderInstrumentation,
}

#[derive(Clone, Copy, Debug)]
struct CpuDrawPlacement {
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
    content_clip: Option<RenderRect>,
}

impl TerminalRasterizer {
    pub fn rasterize(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<CpuFrame, RendererError> {
        self.rasterize_instrumented(scene, fonts)
            .map(|frame| frame.frame)
    }

    pub fn rasterize_instrumented(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<InstrumentedCpuFrame, RendererError> {
        let started = Instant::now();
        let batches = self.batch_planner.prepare_full(scene, fonts)?;
        let metrics = fonts.cell_metrics()?;
        let mut width = ((f32::from(scene.grid.columns) * metrics.cell_width)
            .ceil()
            .max(1.0) as u32)
            .saturating_add(scene.content_offset.x.max(0) as u32 * 2);
        let mut height = ((f32::from(scene.grid.rows) * metrics.cell_height)
            .ceil()
            .max(1.0) as u32)
            .saturating_add(scene.content_offset.y.max(0) as u32 * 2);
        for overlay in &scene.surface_overlays {
            width = width.max(
                overlay
                    .bounds
                    .x
                    .max(0)
                    .unsigned_abs()
                    .saturating_add(overlay.bounds.width),
            );
            height = height.max(
                overlay
                    .bounds
                    .y
                    .max(0)
                    .unsigned_abs()
                    .saturating_add(overlay.bounds.height),
            );
        }
        if let Some(window_chrome) = &scene.window_chrome {
            width = width.max(
                window_chrome
                    .bounds
                    .x
                    .max(0)
                    .unsigned_abs()
                    .saturating_add(window_chrome.bounds.width),
            );
            height = height.max(
                window_chrome
                    .bounds
                    .y
                    .max(0)
                    .unsigned_abs()
                    .saturating_add(window_chrome.bounds.height),
            );
        }
        let pixel_len = rgba_buffer_len(width, height).ok_or_else(|| {
            RendererError::Asset("CPU frame dimensions overflow addressable memory".to_owned())
        })?;
        let mut frame = CpuFrame {
            width,
            height,
            pixels: vec![0; pixel_len],
        };

        fill_rect(
            &mut frame,
            RenderRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            RenderColor::rgb(12, 12, 12),
        );
        let mut instrumentation = batches.instrumentation;

        let mut overlays = scene
            .search_highlights
            .iter()
            .enumerate()
            .map(|(index, overlay)| {
                (
                    overlay,
                    scene.content_offset,
                    content_clip_for_search(scene, index, scene.content_offset),
                )
            })
            .chain(
                scene
                    .semantic_overlays
                    .iter()
                    .enumerate()
                    .map(|(index, overlay)| {
                        (
                            overlay,
                            scene.content_offset,
                            content_clip_for_semantic(scene, index, scene.content_offset),
                        )
                    }),
            )
            .chain(
                scene
                    .surface_overlays
                    .iter()
                    .map(|overlay| (overlay, render_core::RenderOffset::default(), None)),
            )
            .collect::<Vec<_>>();
        overlays.sort_by_key(|(overlay, _, _)| overlay.z_index);

        for (index, cell) in scene.grid.cells.iter().enumerate() {
            draw_cell_background(
                &mut frame,
                cell,
                metrics,
                scene.content_offset,
                content_clip_for_cell(scene, index, scene.content_offset),
            );
        }

        for (overlay, offset, content_clip) in &overlays {
            if !overlay_draws_behind_terminal_text(overlay.kind) {
                continue;
            }
            let Some(bounds) =
                clip_optional_rect(offset_region(overlay.bounds, *offset), *content_clip)
            else {
                continue;
            };
            blend_rounded_rect(
                &mut frame,
                bounds,
                u32::from(overlay.corner_radius_px),
                overlay.color,
            );
            instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            if let Some(border_color) = overlay.border_color {
                stroke_rounded_rect(
                    &mut frame,
                    bounds,
                    u32::from(overlay.border_width_px.max(1)),
                    u32::from(overlay.corner_radius_px),
                    border_color,
                );
                instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            }
        }

        for (index, selection) in scene.selections.iter().enumerate() {
            let content_clip = content_clip_for_selection(scene, index, scene.content_offset);
            for position in &selection.cells {
                if let Some(rect) = clip_optional_rect(
                    cell_region_at(*position, metrics, scene.content_offset),
                    content_clip,
                ) {
                    fill_rect(&mut frame, rect, selection.color);
                }
                instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            }
        }

        if scene.cursor_image.is_none()
            && let Some(cursor) = scene.cursor
        {
            draw_cursor(&mut frame, cursor, metrics, scene.content_offset);
        }

        let cursor_text = cursor_text_override(scene, metrics);
        let full_frame_damage = [RenderRect {
            x: 0,
            y: 0,
            width: frame.width,
            height: frame.height,
        }];
        for clipped_cell in damaged_terminal_text_runs(
            &scene.grid.cells,
            &full_frame_damage,
            metrics,
            scene.content_offset,
            &scene.content_clips,
        ) {
            self.draw_cell_foreground(
                &mut frame,
                &clipped_cell.cell,
                fonts,
                CpuDrawPlacement {
                    metrics,
                    offset: scene.content_offset,
                    content_clip: clipped_cell
                        .clip
                        .map(|clip| offset_region(clip, scene.content_offset)),
                },
                cursor_text,
            )?;
        }

        for (overlay, offset, content_clip) in &overlays {
            if !overlay_draws_behind_terminal_text(overlay.kind) {
                continue;
            }
            self.draw_overlay_label(
                &mut frame,
                overlay,
                fonts,
                CpuDrawPlacement {
                    metrics,
                    offset: *offset,
                    content_clip: *content_clip,
                },
                &mut instrumentation,
            )?;
        }

        for (overlay, offset, content_clip) in overlays {
            if overlay_draws_behind_terminal_text(overlay.kind) {
                continue;
            }
            let Some(bounds) =
                clip_optional_rect(offset_region(overlay.bounds, offset), content_clip)
            else {
                continue;
            };
            blend_rounded_rect(
                &mut frame,
                bounds,
                u32::from(overlay.corner_radius_px),
                overlay.color,
            );
            instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            if let Some(border_color) = overlay.border_color {
                stroke_rounded_rect(
                    &mut frame,
                    bounds,
                    u32::from(overlay.border_width_px.max(1)),
                    u32::from(overlay.corner_radius_px),
                    border_color,
                );
                instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            }
            self.draw_overlay_label(
                &mut frame,
                overlay,
                fonts,
                CpuDrawPlacement {
                    metrics,
                    offset,
                    content_clip,
                },
                &mut instrumentation,
            )?;
        }

        if let Some(cursor_image) = &scene.cursor_image {
            draw_cursor_image(&mut frame, cursor_image, scene.content_offset);
            instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
        }

        if let Some(window_chrome) = &scene.window_chrome {
            self.draw_window_chrome(
                &mut frame,
                window_chrome,
                fonts,
                metrics,
                &mut instrumentation,
            )?;
        }

        instrumentation.cpu_prepare_time = started.elapsed();
        instrumentation.frame_time = instrumentation.cpu_prepare_time;

        Ok(InstrumentedCpuFrame {
            frame,
            instrumentation,
        })
    }

    fn draw_cell_foreground(
        &mut self,
        frame: &mut CpuFrame,
        cell: &RenderCell,
        fonts: &mut FontSystem,
        placement: CpuDrawPlacement,
        cursor_text: Option<CursorTextOverride>,
    ) -> Result<(), RendererError> {
        let metrics = placement.metrics;
        let rect = offset_region(text_run_region(cell, metrics), placement.offset);

        if cell.style.hidden {
            return Ok(());
        }

        if let Some(powerline) = SolidPowerlineGlyph::from_text(&cell.text) {
            let bitmap = rasterize_solid_powerline_glyph(powerline, rect.width, rect.height);
            draw_glyph_clipped(
                frame,
                rect.x,
                rect.y,
                &bitmap,
                glyph_color(cursor_text, rect, cell.foreground),
                placement.content_clip,
            );
            return Ok(());
        }

        let mut pen_x = rect.x as f32;
        let mut pen_y = glyph_baseline_y(rect, metrics);
        let shaped = fonts.shape_text(&cell.text, cell.style.bold, cell.style.italic)?;
        for glyph in shaped.glyphs {
            let key = glyph.key;
            let bitmap = self.batch_planner.glyph_cache.get_or_insert_with(key, || {
                fonts
                    .rasterize_glyph(key)
                    .unwrap_or_else(|_| missing_glyph_bitmap(metrics))
            });
            let glyph_rect = RenderRect {
                x: (pen_x + glyph.x_offset).round() as i32 + bitmap.offset_x,
                y: (pen_y - glyph.y_offset).round() as i32 + bitmap.offset_y,
                width: bitmap.width,
                height: bitmap.height,
            };
            draw_glyph_clipped(
                frame,
                glyph_rect.x,
                glyph_rect.y,
                bitmap.as_ref(),
                glyph_color(cursor_text, glyph_rect, cell.foreground),
                placement.content_clip,
            );
            pen_x += glyph.x_advance;
            pen_y += glyph.y_advance;
        }

        if cell.style.underline
            && let Some(rect) = clip_optional_rect(
                metric_decoration_rect(rect, metrics, metrics.underline_position),
                placement.content_clip,
            )
        {
            fill_rect(frame, rect, cell.foreground);
        }

        if cell.style.strikethrough
            && let Some(rect) = clip_optional_rect(
                metric_decoration_rect(rect, metrics, metrics.strikethrough_position),
                placement.content_clip,
            )
        {
            fill_rect(frame, rect, cell.foreground);
        }

        if cell.style.overline
            && let Some(rect) = clip_optional_rect(
                RenderRect {
                    y: rect.y,
                    height: 1,
                    ..rect
                },
                placement.content_clip,
            )
        {
            fill_rect(frame, rect, cell.foreground);
        }

        Ok(())
    }

    fn draw_overlay_label(
        &mut self,
        frame: &mut CpuFrame,
        overlay: &OverlayPrimitive,
        fonts: &mut FontSystem,
        placement: CpuDrawPlacement,
        instrumentation: &mut RenderInstrumentation,
    ) -> Result<(), RendererError> {
        let Some(label) = &overlay.label else {
            return Ok(());
        };
        if label.trim().is_empty() {
            return Ok(());
        }

        let cell = RenderCell {
            position: CellPosition { row: 0, col: 0 },
            text: label.clone().into(),
            foreground: overlay
                .label_color
                .unwrap_or_else(|| overlay_label_color(overlay.kind)),
            background: RenderColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 0,
            },
            style: RenderCellStyle::default(),
        };
        let metrics = placement.metrics;
        let rect = offset_region(overlay_label_rect(overlay, metrics), placement.offset);
        let mut pen_x = rect.x as f32;
        let mut pen_y = glyph_baseline_y(rect, metrics);
        let shaped = fonts.shape_text(&cell.text, false, false)?;
        for glyph in shaped.glyphs {
            let key = glyph.key;
            let bitmap = self.batch_planner.glyph_cache.get_or_insert_with(key, || {
                fonts
                    .rasterize_glyph(key)
                    .unwrap_or_else(|_| missing_glyph_bitmap(metrics))
            });
            draw_glyph_clipped(
                frame,
                (pen_x + glyph.x_offset).round() as i32 + bitmap.offset_x,
                (pen_y - glyph.y_offset).round() as i32 + bitmap.offset_y,
                bitmap.as_ref(),
                cell.foreground,
                placement.content_clip,
            );
            pen_x += glyph.x_advance;
            pen_y += glyph.y_advance;
            if pen_x > (rect.x + rect.width as i32) as f32 {
                break;
            }
        }
        instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
        Ok(())
    }

    fn draw_window_chrome(
        &mut self,
        frame: &mut CpuFrame,
        visual: &WindowChromeVisual,
        fonts: &mut FontSystem,
        metrics: CellMetrics,
        instrumentation: &mut RenderInstrumentation,
    ) -> Result<(), RendererError> {
        if visual.opacity == 0 || visual.bounds.width == 0 || visual.bounds.height == 0 {
            return Ok(());
        }

        let mut geometry = QuadBatch::new(QuadBatchKind::Decoration);
        push_solid_quad(
            &mut geometry,
            visual.bounds,
            with_fixed_opacity(RenderColor::rgb(18, 18, 18), visual.opacity),
        );
        for control in &visual.controls {
            push_window_chrome_control(&mut geometry, control, visual.opacity);
        }
        draw_quad_batch_cpu(frame, &geometry);
        instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);

        let mut title_x = visual.bounds.x.saturating_add(8);
        if visual.show_logo {
            let bitmap = panea_logo_bitmap()?;
            let logo_bounds = window_chrome_logo_bounds(visual, title_x);
            draw_rgba_bitmap(
                frame,
                logo_bounds,
                bitmap.width,
                bitmap.height,
                &bitmap.pixels,
                ((u32::from(visual.opacity) * 255) / u32::from(u16::MAX)) as u8,
            );
            instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            title_x = title_x.saturating_add(logo_bounds.width as i32 + 8);
        }
        if let Some(overlay) = window_chrome_title_overlay(visual, title_x) {
            self.draw_overlay_label(
                frame,
                &overlay,
                fonts,
                CpuDrawPlacement {
                    metrics,
                    offset: render_core::RenderOffset::default(),
                    content_clip: None,
                },
                instrumentation,
            )?;
        }
        Ok(())
    }
}

fn draw_cell_background(
    frame: &mut CpuFrame,
    cell: &RenderCell,
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
    content_clip: Option<RenderRect>,
) {
    if let Some(rect) =
        clip_optional_rect(cell_region_at(cell.position, metrics, offset), content_clip)
    {
        fill_rect(frame, rect, cell.background);
    }
}

fn fill_rect(frame: &mut CpuFrame, rect: RenderRect, color: RenderColor) {
    let x0 = rect.x.max(0) as u32;
    let y0 = rect.y.max(0) as u32;
    let x1 = (rect.x.max(0) as u32 + rect.width).min(frame.width);
    let y1 = (rect.y.max(0) as u32 + rect.height).min(frame.height);

    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * frame.width + x) * 4) as usize;
            frame.pixels[index] = color.red;
            frame.pixels[index + 1] = color.green;
            frame.pixels[index + 2] = color.blue;
            frame.pixels[index + 3] = color.alpha;
        }
    }
}

fn blend_rect(frame: &mut CpuFrame, rect: RenderRect, color: RenderColor) {
    let x0 = rect.x.max(0) as u32;
    let y0 = rect.y.max(0) as u32;
    let x1 = (rect.x.max(0) as u32 + rect.width).min(frame.width);
    let y1 = (rect.y.max(0) as u32 + rect.height).min(frame.height);

    for y in y0..y1 {
        for x in x0..x1 {
            let index = ((y * frame.width + x) * 4) as usize;
            blend_pixel(&mut frame.pixels[index..index + 4], color, color.alpha);
        }
    }
}

fn draw_quad_batch_cpu(frame: &mut CpuFrame, batch: &QuadBatch) {
    for quad in batch.vertices.chunks_exact(4) {
        let x0 = quad
            .iter()
            .map(|vertex| vertex.position_px[0])
            .fold(f32::INFINITY, f32::min)
            .round() as i32;
        let y0 = quad
            .iter()
            .map(|vertex| vertex.position_px[1])
            .fold(f32::INFINITY, f32::min)
            .round() as i32;
        let x1 = quad
            .iter()
            .map(|vertex| vertex.position_px[0])
            .fold(f32::NEG_INFINITY, f32::max)
            .round() as i32;
        let y1 = quad
            .iter()
            .map(|vertex| vertex.position_px[1])
            .fold(f32::NEG_INFINITY, f32::max)
            .round() as i32;
        let color = quad[0].color;
        let rect = RenderRect {
            x: x0,
            y: y0,
            width: x1.saturating_sub(x0) as u32,
            height: y1.saturating_sub(y0) as u32,
        };
        if color[3] < 0.0 {
            let width = (color[0] * 0.5).floor();
            let height = (color[1] * 0.5).floor();
            let radius = (color[2] * 0.5).floor();
            let payload = -color[3] - 1.0;
            let line = (payload * 0.5).floor();
            let decoded = RenderColor {
                red: ((color[0] - width * 2.0).clamp(0.0, 1.0) * 255.0).round() as u8,
                green: ((color[1] - height * 2.0).clamp(0.0, 1.0) * 255.0).round() as u8,
                blue: ((color[2] - radius * 2.0).clamp(0.0, 1.0) * 255.0).round() as u8,
                alpha: ((payload - line * 2.0).clamp(0.0, 1.0) * 255.0).round() as u8,
            };
            if line > 0.0 {
                stroke_rounded_rect(frame, rect, line as u32, radius as u32, decoded);
            } else {
                blend_rounded_rect(frame, rect, radius as u32, decoded);
            }
        } else {
            blend_rect(
                frame,
                rect,
                RenderColor {
                    red: (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                    green: (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                    blue: (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
                    alpha: (color[3].clamp(0.0, 1.0) * 255.0).round() as u8,
                },
            );
        }
    }
}

fn draw_rgba_bitmap(
    frame: &mut CpuFrame,
    bounds: RenderRect,
    source_width: u32,
    source_height: u32,
    pixels: &[u8],
    opacity: u8,
) {
    if bounds.width == 0 || bounds.height == 0 || source_width == 0 || source_height == 0 {
        return;
    }
    for target_y in 0..bounds.height {
        let source_y = target_y.saturating_mul(source_height) / bounds.height;
        for target_x in 0..bounds.width {
            let source_x = target_x.saturating_mul(source_width) / bounds.width;
            let source_index = usize::try_from(
                source_y
                    .saturating_mul(source_width)
                    .saturating_add(source_x)
                    .saturating_mul(4),
            )
            .unwrap_or(usize::MAX);
            let Some(pixel) = pixels.get(source_index..source_index.saturating_add(4)) else {
                continue;
            };
            let x = bounds.x.saturating_add(target_x as i32);
            let y = bounds.y.saturating_add(target_y as i32);
            if x < 0 || y < 0 || x as u32 >= frame.width || y as u32 >= frame.height {
                continue;
            }
            let target_index = ((y as u32 * frame.width + x as u32) * 4) as usize;
            let alpha = ((u16::from(pixel[3]) * u16::from(opacity)) / 255) as u8;
            blend_pixel(
                &mut frame.pixels[target_index..target_index + 4],
                RenderColor {
                    red: pixel[0],
                    green: pixel[1],
                    blue: pixel[2],
                    alpha,
                },
                alpha,
            );
        }
    }
}

fn blend_rounded_rect(frame: &mut CpuFrame, rect: RenderRect, radius: u32, color: RenderColor) {
    let radius = radius.min(rect.width / 2).min(rect.height / 2);
    if radius == 0 {
        blend_rect(frame, rect, color);
        return;
    }
    for y in 0..rect.height {
        let inset = rounded_inset(radius, y, rect.height);
        blend_rect(
            frame,
            RenderRect {
                x: rect.x.saturating_add(inset as i32),
                y: rect.y.saturating_add(y as i32),
                width: rect.width.saturating_sub(inset.saturating_mul(2)),
                height: 1,
            },
            color,
        );
    }
}

fn stroke_rounded_rect(
    frame: &mut CpuFrame,
    rect: RenderRect,
    width: u32,
    radius: u32,
    color: RenderColor,
) {
    let radius = radius.min(rect.width / 2).min(rect.height / 2);
    if radius == 0 {
        stroke_rect(frame, rect, width, color);
        return;
    }
    let width = width.max(1).min(rect.width / 2).min(rect.height / 2);
    for y in 0..rect.height {
        let outer = rounded_inset(radius, y, rect.height);
        if y < width || y >= rect.height.saturating_sub(width) {
            fill_rect(
                frame,
                RenderRect {
                    x: rect.x.saturating_add(outer as i32),
                    y: rect.y.saturating_add(y as i32),
                    width: rect.width.saturating_sub(outer.saturating_mul(2)),
                    height: 1,
                },
                color,
            );
            continue;
        }
        let inner = width.saturating_add(rounded_inset(
            radius.saturating_sub(width),
            y.saturating_sub(width),
            rect.height.saturating_sub(width.saturating_mul(2)),
        ));
        let side = inner.saturating_sub(outer);
        fill_rect(
            frame,
            RenderRect {
                x: rect.x.saturating_add(outer as i32),
                y: rect.y.saturating_add(y as i32),
                width: side,
                height: 1,
            },
            color,
        );
        fill_rect(
            frame,
            RenderRect {
                x: rect
                    .x
                    .saturating_add(rect.width.saturating_sub(inner) as i32),
                y: rect.y.saturating_add(y as i32),
                width: side,
                height: 1,
            },
            color,
        );
    }
}

fn stroke_rect(frame: &mut CpuFrame, rect: RenderRect, width: u32, color: RenderColor) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    let width = width.max(1).min(rect.width).min(rect.height);
    fill_rect(
        frame,
        RenderRect {
            height: width,
            ..rect
        },
        color,
    );
    fill_rect(
        frame,
        RenderRect {
            y: rect.y + rect.height.saturating_sub(width) as i32,
            height: width,
            ..rect
        },
        color,
    );
    fill_rect(frame, RenderRect { width, ..rect }, color);
    fill_rect(
        frame,
        RenderRect {
            x: rect.x + rect.width.saturating_sub(width) as i32,
            width,
            ..rect
        },
        color,
    );
}

#[cfg(test)]
fn draw_glyph(frame: &mut CpuFrame, x: i32, y: i32, bitmap: &GlyphBitmap, color: RenderColor) {
    draw_glyph_clipped(frame, x, y, bitmap, color, None);
}

fn draw_glyph_clipped(
    frame: &mut CpuFrame,
    x: i32,
    y: i32,
    bitmap: &GlyphBitmap,
    color: RenderColor,
    clip: Option<RenderRect>,
) {
    for gy in 0..bitmap.height {
        for gx in 0..bitmap.width {
            let target_x = x + gx as i32;
            let target_y = y + gy as i32;
            if target_x < 0
                || target_y < 0
                || target_x >= frame.width as i32
                || target_y >= frame.height as i32
                || clip.is_some_and(|clip| {
                    target_x < clip.x
                        || target_y < clip.y
                        || target_x >= clip.x.saturating_add(clip.width as i32)
                        || target_y >= clip.y.saturating_add(clip.height as i32)
                })
            {
                continue;
            }

            let index = (((target_y as u32 * frame.width) + target_x as u32) * 4) as usize;
            let source = (gy * bitmap.width + gx) as usize;
            match bitmap.format {
                GlyphBitmapFormat::Alpha => {
                    let alpha = bitmap.pixels[source];
                    if alpha != 0 {
                        blend_pixel(&mut frame.pixels[index..index + 4], color, alpha);
                    }
                }
                GlyphBitmapFormat::Rgba => {
                    let source = source * 4;
                    let Some(rgba) = bitmap.pixels.get(source..source + 4) else {
                        continue;
                    };
                    let emoji = RenderColor {
                        red: rgba[0],
                        green: rgba[1],
                        blue: rgba[2],
                        alpha: rgba[3],
                    };
                    if emoji.alpha != 0 {
                        blend_pixel(&mut frame.pixels[index..index + 4], emoji, emoji.alpha);
                    }
                }
            }
        }
    }
}

fn blend_pixel(pixel: &mut [u8], color: RenderColor, alpha: u8) {
    let alpha = u16::from(alpha);
    let inverse = 255 - alpha;
    pixel[0] = (((u16::from(color.red) * alpha) + (u16::from(pixel[0]) * inverse)) / 255) as u8;
    pixel[1] = (((u16::from(color.green) * alpha) + (u16::from(pixel[1]) * inverse)) / 255) as u8;
    pixel[2] = (((u16::from(color.blue) * alpha) + (u16::from(pixel[2]) * inverse)) / 255) as u8;
    pixel[3] = u8::MAX;
}

fn draw_cursor_image(
    frame: &mut CpuFrame,
    visual: &CursorImageVisual,
    offset: render_core::RenderOffset,
) {
    let Some(source) = visual.asset.frames.get(usize::from(visual.frame_index)) else {
        return;
    };
    let bounds = offset_region(visual.bounds, offset);
    if bounds.width == 0 || bounds.height == 0 {
        return;
    }
    for target_y in 0..bounds.height {
        let source_y = target_y.saturating_mul(visual.asset.height) / bounds.height;
        for target_x in 0..bounds.width {
            let source_x = target_x.saturating_mul(visual.asset.width) / bounds.width;
            let source_index = usize::try_from(
                source_y
                    .saturating_mul(visual.asset.width)
                    .saturating_add(source_x)
                    .saturating_mul(4),
            )
            .unwrap_or(usize::MAX);
            let Some(pixel) = source
                .pixels
                .get(source_index..source_index.saturating_add(4))
            else {
                continue;
            };
            let x = bounds.x.saturating_add(target_x as i32);
            let y = bounds.y.saturating_add(target_y as i32);
            if x < 0 || y < 0 || x as u32 >= frame.width || y as u32 >= frame.height {
                continue;
            }
            let target_index = ((y as u32 * frame.width + x as u32) * 4) as usize;
            let alpha = ((u16::from(pixel[3]) * u16::from(visual.opacity)) / 255) as u8;
            blend_pixel(
                &mut frame.pixels[target_index..target_index + 4],
                RenderColor {
                    red: pixel[0],
                    green: pixel[1],
                    blue: pixel[2],
                    alpha,
                },
                alpha,
            );
        }
    }
}

fn draw_cursor(
    frame: &mut CpuFrame,
    cursor: CursorVisual,
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
) {
    if !cursor.visible {
        return;
    }

    let rect = offset_region(cursor_visual_region(cursor, metrics), offset);
    let thickness = u32::from(cursor.thickness_percent.clamp(1, 100));
    match cursor.shape {
        RenderCursorShape::Block
        | RenderCursorShape::Beam
        | RenderCursorShape::Underline
        | RenderCursorShape::Custom
        | RenderCursorShape::CustomStaticShape => {}
        RenderCursorShape::HollowBlock => {
            let line = ((rect.width.min(rect.height) * thickness) / 100).max(1);
            draw_rounded_stroke(
                frame,
                rect,
                line,
                u32::from(cursor.corner_radius_px),
                cursor.color,
            );
            return;
        }
    }
    draw_rounded_rect(
        frame,
        rect,
        u32::from(cursor.corner_radius_px),
        cursor.color,
    );
}

fn draw_rounded_rect(frame: &mut CpuFrame, rect: RenderRect, radius: u32, color: RenderColor) {
    let radius = radius.min(rect.width / 2).min(rect.height / 2);
    if radius == 0 {
        fill_rect(frame, rect, color);
        return;
    }
    for y in 0..rect.height {
        let inset = rounded_inset(radius, y, rect.height);
        fill_rect(
            frame,
            RenderRect {
                x: rect.x.saturating_add(inset as i32),
                y: rect.y.saturating_add(y as i32),
                width: rect.width.saturating_sub(inset.saturating_mul(2)),
                height: 1,
            },
            color,
        );
    }
}

fn draw_rounded_stroke(
    frame: &mut CpuFrame,
    rect: RenderRect,
    line: u32,
    radius: u32,
    color: RenderColor,
) {
    let mut batch = QuadBatch::new(QuadBatchKind::Cursor);
    push_rounded_stroke_quads(&mut batch, rect, line, radius, color);
    for vertices in batch.vertices.chunks_exact(4) {
        let x0 = vertices[0].position_px[0].floor() as i32;
        let y0 = vertices[0].position_px[1].floor() as i32;
        let x1 = vertices[2].position_px[0].ceil() as i32;
        let y1 = vertices[2].position_px[1].ceil() as i32;
        fill_rect(
            frame,
            RenderRect {
                x: x0,
                y: y0,
                width: x1.saturating_sub(x0) as u32,
                height: y1.saturating_sub(y0) as u32,
            },
            color,
        );
    }
}
