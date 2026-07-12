//! Renderer-independent drawing contracts.

pub const LAYER: &str = "render performance";

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl RenderColor {
    #[must_use]
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: u8::MAX,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellPosition {
    pub row: i64,
    pub col: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderCell {
    pub position: CellPosition,
    pub text: String,
    pub foreground: RenderColor,
    pub background: RenderColor,
    pub style: RenderCellStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderCellStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderGrid {
    pub columns: u16,
    pub rows: u16,
    pub cells: Vec<RenderCell>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderCursorShape {
    Block,
    Beam,
    Underline,
    HollowBlock,
    Custom,
    CustomStaticShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorVisual {
    pub position: CellPosition,
    pub shape: RenderCursorShape,
    pub color: RenderColor,
    pub visible: bool,
    pub thickness_percent: u8,
    pub corner_radius_px: u8,
    pub inactive: bool,
}

pub type RenderCursor = CursorVisual;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub type DamageRegion = RenderRect;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderOffset {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    Selection,
    SearchHighlight,
    Semantic,
    Decoration,
    PromptDecoration,
    CommandBlock,
    InputOutputGroup,
    Badge,
    PerformanceOverlay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayPrimitive {
    pub kind: OverlayKind,
    pub bounds: RenderRect,
    pub color: RenderColor,
    pub border_color: Option<RenderColor>,
    pub corner_radius_px: u8,
    pub z_index: i16,
    pub label: Option<String>,
}

impl OverlayPrimitive {
    #[must_use]
    pub const fn filled(kind: OverlayKind, bounds: RenderRect, color: RenderColor) -> Self {
        Self {
            kind,
            bounds,
            color,
            border_color: None,
            corner_radius_px: 0,
            z_index: 0,
            label: None,
        }
    }
}

pub type RenderOverlay = OverlayPrimitive;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionVisual {
    pub cells: Vec<CellPosition>,
    pub color: RenderColor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderDecoration {
    pub bounds: RenderRect,
    pub color: RenderColor,
    pub border_color: Option<RenderColor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationKind {
    CursorSmoothMovement,
    CursorTypingPulse,
    CursorTypingStretch,
    CursorTrail,
    CursorBlinkEasing,
    CursorGlow,
    OverlayTransition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationHandle {
    pub id: u64,
    pub kind: AnimationKind,
    pub affected_region: RenderRect,
    pub elapsed: Duration,
    pub remaining: Option<Duration>,
}

pub type RenderAnimation = AnimationHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRequestReason {
    TerminalContentChanged,
    CursorBlink,
    Animation,
    WindowResized,
    SelectionChanged,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRequest {
    pub reason: FrameRequestReason,
    pub damage: Option<DamageRegion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionalFeature {
    CursorAnimation,
    SemanticOverlays,
    CommandBlocks,
    VisualEffects,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionalFeatureCostMode {
    Disabled,
    EnabledDefault,
    EnabledHeavy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlyphInstrumentation {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub atlas_uploads: u64,
    pub atlas_used_bytes: u64,
    pub atlas_capacity_bytes: u64,
}

impl GlyphInstrumentation {
    #[must_use]
    pub const fn total_lookups(self) -> u64 {
        self.cache_hits + self.cache_misses
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuTimingStatus {
    #[default]
    Disabled,
    Unsupported,
    Pending,
    Available,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderInstrumentation {
    pub frame_time: Duration,
    pub cpu_prepare_time: Duration,
    pub gpu_submit_time: Option<Duration>,
    pub gpu_time: Option<Duration>,
    pub gpu_timing_status: GpuTimingStatus,
    pub glyphs: GlyphInstrumentation,
    pub damage_region_count: usize,
    pub draw_call_count: u32,
    pub animated_region_count: usize,
    pub idle_wakeups: u64,
    pub pty_read_bytes_per_second: u64,
    pub parser_bytes_per_second: u64,
    pub memory_usage_bytes: Option<u64>,
    pub scrollback_memory_bytes: u64,
}

impl Default for RenderInstrumentation {
    fn default() -> Self {
        Self {
            frame_time: Duration::ZERO,
            cpu_prepare_time: Duration::ZERO,
            gpu_submit_time: None,
            gpu_time: None,
            gpu_timing_status: GpuTimingStatus::Disabled,
            glyphs: GlyphInstrumentation::default(),
            damage_region_count: 0,
            draw_call_count: 0,
            animated_region_count: 0,
            idle_wakeups: 0,
            pty_read_bytes_per_second: 0,
            parser_bytes_per_second: 0,
            memory_usage_bytes: None,
            scrollback_memory_bytes: 0,
        }
    }
}

impl RenderInstrumentation {
    #[must_use]
    pub fn over_budget(self, max_frame_time: Duration) -> bool {
        self.frame_time > max_frame_time
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureCostSample {
    pub feature: OptionalFeature,
    pub mode: OptionalFeatureCostMode,
    pub instrumentation: RenderInstrumentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderScene {
    pub grid: RenderGrid,
    /// Pixel offset of terminal content inside the surface. Window margins and
    /// padding are resolved before reaching the renderer.
    pub content_offset: RenderOffset,
    pub cursor: Option<CursorVisual>,
    pub selections: Vec<SelectionVisual>,
    pub search_highlights: Vec<OverlayPrimitive>,
    pub semantic_overlays: Vec<OverlayPrimitive>,
    pub decorations: Vec<RenderDecoration>,
    pub animations: Vec<AnimationHandle>,
    pub damage_regions: Vec<DamageRegion>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderSurfaceSize {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

impl RenderSurfaceSize {
    #[must_use]
    pub const fn new(width: u32, height: u32, scale_factor: f64) -> Self {
        Self {
            width,
            height,
            scale_factor,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderSurfaceStatus {
    Ready,
    Recovering,
    Lost,
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderRecoveryReason {
    SurfaceLost,
    SurfaceOutdated,
    DeviceLost,
    OutOfMemory,
    BackendError,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderRecoveryStatus {
    Ready,
    Recovering {
        reason: RenderRecoveryReason,
        attempts: u32,
    },
    Lost {
        reason: RenderRecoveryReason,
        message: String,
    },
    Failed {
        reason: RenderRecoveryReason,
        message: String,
    },
}

impl RenderRecoveryStatus {
    #[must_use]
    pub fn surface_status(&self) -> RenderSurfaceStatus {
        match self {
            Self::Ready => RenderSurfaceStatus::Ready,
            Self::Recovering { .. } => RenderSurfaceStatus::Recovering,
            Self::Lost { .. } => RenderSurfaceStatus::Lost,
            Self::Failed { message, .. } => RenderSurfaceStatus::Unavailable {
                reason: message.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRecoveryEvent {
    pub reason: RenderRecoveryReason,
    pub attempts: u32,
    pub rebuilt_surface: bool,
    pub rebuilt_device: bool,
    pub rebuilt_pipelines: bool,
    pub rebuilt_glyph_atlas: bool,
    pub preserved_terminal_state: bool,
    pub message: String,
}

impl RenderRecoveryEvent {
    #[must_use]
    pub fn success(reason: RenderRecoveryReason, attempts: u32) -> Self {
        Self {
            reason,
            attempts,
            rebuilt_surface: true,
            rebuilt_device: true,
            rebuilt_pipelines: true,
            rebuilt_glyph_atlas: true,
            preserved_terminal_state: true,
            message: "renderer GPU resources were recreated".to_owned(),
        }
    }

    #[must_use]
    pub fn failure(
        reason: RenderRecoveryReason,
        attempts: u32,
        message: impl Into<String>,
    ) -> Self {
        Self {
            reason,
            attempts,
            rebuilt_surface: false,
            rebuilt_device: false,
            rebuilt_pipelines: false,
            rebuilt_glyph_atlas: false,
            preserved_terminal_state: true,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderSurfaceError {
    pub message: String,
}

impl RenderSurfaceError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Renderer surface contract used by app code without exposing a GPU backend.
pub trait RendererSurface {
    fn resize(&mut self, size: RenderSurfaceSize) -> Result<(), RenderSurfaceError>;

    fn render_scene(
        &mut self,
        scene: &RenderScene,
    ) -> Result<RenderInstrumentation, RenderSurfaceError>;

    fn status(&self) -> RenderSurfaceStatus;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_core_has_no_crate_dependencies() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("[dependencies]"),
            "render-core must stay renderer-independent and avoid GPU API dependencies"
        );
    }

    #[derive(Debug, Default)]
    struct FakeRenderer {
        rendered_cells: usize,
        size: Option<RenderSurfaceSize>,
    }

    impl RendererSurface for FakeRenderer {
        fn resize(&mut self, size: RenderSurfaceSize) -> Result<(), RenderSurfaceError> {
            self.size = Some(size);
            Ok(())
        }

        fn render_scene(
            &mut self,
            scene: &RenderScene,
        ) -> Result<RenderInstrumentation, RenderSurfaceError> {
            self.rendered_cells += scene.grid.cells.len();
            Ok(RenderInstrumentation {
                damage_region_count: scene.damage_regions.len(),
                ..RenderInstrumentation::default()
            })
        }

        fn status(&self) -> RenderSurfaceStatus {
            RenderSurfaceStatus::Ready
        }
    }

    #[test]
    fn renderer_contract_can_be_exercised_with_fake_terminal_data() {
        let mut renderer = FakeRenderer::default();
        renderer
            .resize(RenderSurfaceSize::new(800, 600, 1.0))
            .expect("fake resize should work");

        let scene = RenderScene {
            grid: RenderGrid {
                columns: 1,
                rows: 1,
                cells: vec![RenderCell {
                    position: CellPosition { row: 0, col: 0 },
                    text: "P".to_owned(),
                    foreground: RenderColor::rgb(230, 230, 230),
                    background: RenderColor::rgb(10, 10, 10),
                    style: RenderCellStyle::default(),
                }],
            },
            damage_regions: vec![RenderRect {
                x: 0,
                y: 0,
                width: 10,
                height: 20,
            }],
            ..RenderScene::default()
        };

        let instrumentation = renderer
            .render_scene(&scene)
            .expect("fake renderer should render scene");

        assert_eq!(renderer.rendered_cells, 1);
        assert_eq!(renderer.status(), RenderSurfaceStatus::Ready);
        assert_eq!(instrumentation.damage_region_count, 1);
    }

    #[test]
    fn recovery_event_preserves_terminal_state_contract() {
        let event = RenderRecoveryEvent::success(RenderRecoveryReason::DeviceLost, 1);

        assert!(event.rebuilt_surface);
        assert!(event.rebuilt_device);
        assert!(event.rebuilt_pipelines);
        assert!(event.rebuilt_glyph_atlas);
        assert!(event.preserved_terminal_state);
    }

    #[test]
    fn recovery_status_maps_to_surface_status() {
        let recovering = RenderRecoveryStatus::Recovering {
            reason: RenderRecoveryReason::SurfaceLost,
            attempts: 1,
        };
        assert_eq!(recovering.surface_status(), RenderSurfaceStatus::Recovering);

        let failed = RenderRecoveryStatus::Failed {
            reason: RenderRecoveryReason::DeviceLost,
            message: "adapter unavailable".to_owned(),
        };
        assert_eq!(
            failed.surface_status(),
            RenderSurfaceStatus::Unavailable {
                reason: "adapter unavailable".to_owned()
            }
        );
    }
}
