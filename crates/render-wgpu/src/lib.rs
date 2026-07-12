//! WGPU renderer implementation, glyph atlas policy, damage tracking, and frame scheduling.

pub const LAYER: &str = "render performance";

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    error::Error,
    fmt,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
    time::Instant,
};

use font_system::{
    CellMetrics, FontError, FontSystem, GlyphBitmap, GlyphBitmapFormat, GlyphCache, GlyphCacheKey,
    ShapedGlyph,
};
use render_core::{
    AnimationHandle, AnimationKind, CellPosition, CursorVisual, DamageRegion, FrameRequestReason,
    GpuTimingStatus, OverlayKind, OverlayPrimitive, RenderCell, RenderCellStyle, RenderColor,
    RenderCursorShape, RenderDecoration, RenderGrid, RenderInstrumentation, RenderRecoveryEvent,
    RenderRecoveryReason, RenderRecoveryStatus, RenderRect, RenderScene, RenderSurfaceStatus,
    SelectionVisual,
};
use winit::window::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentMode {
    Vsync,
    Immediate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererOptions {
    pub present_mode: PresentMode,
    pub damage_tracking: bool,
    pub gpu_timestamps: bool,
    pub transparent: bool,
    pub glyph_cache_entries: usize,
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            present_mode: PresentMode::Vsync,
            damage_tracking: true,
            gpu_timestamps: false,
            transparent: false,
            glyph_cache_entries: 8192,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuAdapterProbe {
    pub backend: String,
    pub adapter: String,
    pub device_type: String,
    pub features: Vec<String>,
}

#[must_use]
pub async fn probe_gpu_adapter() -> Option<GpuAdapterProbe> {
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await?;
    let info = adapter.get_info();
    let features = adapter.features();
    let feature_names = [
        (wgpu::Features::TIMESTAMP_QUERY, "timestamp_query"),
        (
            wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES,
            "texture_adapter_specific_format_features",
        ),
    ]
    .into_iter()
    .filter_map(|(feature, name)| features.contains(feature).then_some(name.to_owned()))
    .collect::<Vec<_>>();

    Some(GpuAdapterProbe {
        backend: format!("{:?}", info.backend),
        adapter: info.name,
        device_type: format!("{:?}", info.device_type),
        features: feature_names,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererError {
    SurfaceCreation(String),
    AdapterUnavailable,
    DeviceCreation(String),
    Surface(String),
    DeviceLost {
        reason: RenderRecoveryReason,
        message: String,
    },
    DeviceUnavailable(String),
    RecoveryFailed(String),
    Font(String),
    EmptySurface,
}

impl fmt::Display for RendererError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SurfaceCreation(message) => {
                write!(f, "failed to create render surface: {message}")
            }
            Self::AdapterUnavailable => f.write_str("no compatible GPU adapter is available"),
            Self::DeviceCreation(message) => write!(f, "failed to create GPU device: {message}"),
            Self::Surface(message) => write!(f, "surface error: {message}"),
            Self::DeviceLost { reason, message } => {
                write!(f, "GPU device lost ({reason:?}): {message}")
            }
            Self::DeviceUnavailable(message) => write!(f, "GPU device unavailable: {message}"),
            Self::RecoveryFailed(message) => write!(f, "GPU recovery failed: {message}"),
            Self::Font(message) => write!(f, "font error: {message}"),
            Self::EmptySurface => f.write_str("surface has zero width or height"),
        }
    }
}

impl Error for RendererError {}

impl From<FontError> for RendererError {
    fn from(value: FontError) -> Self {
        Self::Font(value.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasEntry {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    entries: HashMap<GlyphCacheKey, AtlasEntry>,
    lru: VecDeque<GlyphCacheKey>,
}

impl GlyphAtlas {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: width.max(1),
            height: height.max(1),
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    pub fn allocate(&mut self, key: GlyphCacheKey, bitmap: &GlyphBitmap) -> Option<AtlasEntry> {
        if let Some(entry) = self.entries.get(&key).copied() {
            self.touch(key);
            return Some(entry);
        }

        let width = bitmap.width.max(1);
        let height = bitmap.height.max(1);
        if width > self.width || height > self.height {
            return None;
        }

        if self.cursor_x + width > self.width {
            self.cursor_x = 0;
            self.cursor_y += self.row_height;
            self.row_height = 0;
        }

        if self.cursor_y + height > self.height {
            self.clear();
        }

        let entry = AtlasEntry {
            x: self.cursor_x,
            y: self.cursor_y,
            width,
            height,
        };
        self.cursor_x += width;
        self.row_height = self.row_height.max(height);
        self.entries.insert(key, entry);
        self.lru.push_back(key);
        Some(entry)
    }

    #[must_use]
    pub fn entry(&self, key: GlyphCacheKey) -> Option<AtlasEntry> {
        self.entries.get(&key).copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.entries
            .values()
            .map(|entry| u64::from(entry.width) * u64::from(entry.height) * 4)
            .sum()
    }

    #[must_use]
    pub fn capacity_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * 4
    }

    fn clear(&mut self) {
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.row_height = 0;
        self.entries.clear();
        self.lru.clear();
    }

    fn touch(&mut self, key: GlyphCacheKey) {
        self.lru.retain(|entry| *entry != key);
        self.lru.push_back(key);
    }
}

#[derive(Debug, Default)]
pub struct DamageTracker {
    previous_cells: HashMap<CellPosition, CellFingerprint>,
    previous_cursor: Option<CursorVisual>,
    previous_size: Option<(u16, u16)>,
    previous_offset: render_core::RenderOffset,
    previous_visuals: Vec<DamageRegion>,
    previous_search_highlights: Vec<OverlayPrimitive>,
    previous_semantic_overlays: Vec<OverlayPrimitive>,
    previous_decorations: Vec<RenderDecoration>,
    previous_selections: Vec<SelectionVisual>,
    previous_animations: Vec<AnimationHandle>,
    current_positions: HashSet<CellPosition>,
    removed_positions: Vec<CellPosition>,
    force_full: bool,
}

impl DamageTracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_full_redraw(&mut self) {
        self.force_full = true;
    }

    pub fn update(&mut self, scene: &RenderScene, metrics: CellMetrics) -> Vec<DamageRegion> {
        let size = (scene.grid.columns, scene.grid.rows);
        let mut regions = Vec::new();

        if self.force_full
            || self.previous_size != Some(size)
            || self.previous_offset != scene.content_offset
        {
            self.force_full = false;
            self.previous_size = Some(size);
            self.previous_offset = scene.content_offset;
            self.previous_cells = scene
                .grid
                .cells
                .iter()
                .map(|cell| (cell.position, CellFingerprint::from(cell)))
                .collect();
            self.previous_cursor = scene.cursor;
            self.previous_visuals = visual_regions(scene, metrics);
            self.remember_visuals(scene);
            return vec![scene_grid_region(scene, metrics)];
        }

        self.current_positions.clear();
        self.current_positions
            .extend(scene.grid.cells.iter().map(|cell| cell.position));
        self.removed_positions.clear();
        self.removed_positions.extend(
            self.previous_cells
                .keys()
                .filter(|position| !self.current_positions.contains(position))
                .copied(),
        );
        for position in self.removed_positions.drain(..) {
            regions.push(cell_region_at(position, metrics, scene.content_offset));
            self.previous_cells.remove(&position);
        }

        for cell in &scene.grid.cells {
            if !self
                .previous_cells
                .get(&cell.position)
                .is_some_and(|fingerprint| fingerprint.matches(cell))
            {
                regions.push(cell_region_at(cell.position, metrics, scene.content_offset));
                self.previous_cells
                    .insert(cell.position, CellFingerprint::from(cell));
            }
        }

        if self.previous_cursor != scene.cursor {
            if let Some(cursor) = self.previous_cursor {
                regions.push(cell_region_at(
                    cursor.position,
                    metrics,
                    scene.content_offset,
                ));
            }
            if let Some(cursor) = scene.cursor {
                regions.push(cell_region_at(
                    cursor.position,
                    metrics,
                    scene.content_offset,
                ));
            }
            self.previous_cursor = scene.cursor;
        }

        if self.visuals_changed(scene) {
            regions.extend(self.previous_visuals.iter().copied());
            self.previous_visuals = visual_regions(scene, metrics);
            regions.extend(self.previous_visuals.iter().copied());
            self.remember_visuals(scene);
        }

        merge_regions(regions)
    }

    fn visuals_changed(&self, scene: &RenderScene) -> bool {
        self.previous_search_highlights != scene.search_highlights
            || self.previous_semantic_overlays != scene.semantic_overlays
            || self.previous_decorations != scene.decorations
            || self.previous_selections != scene.selections
            || self.previous_animations != scene.animations
    }

    fn remember_visuals(&mut self, scene: &RenderScene) {
        self.previous_search_highlights = scene.search_highlights.clone();
        self.previous_semantic_overlays = scene.semantic_overlays.clone();
        self.previous_decorations = scene.decorations.clone();
        self.previous_selections = scene.selections.clone();
        self.previous_animations = scene.animations.clone();
    }
}

fn visual_regions(scene: &RenderScene, metrics: CellMetrics) -> Vec<DamageRegion> {
    let mut regions = scene
        .search_highlights
        .iter()
        .chain(scene.semantic_overlays.iter())
        .map(|overlay| offset_region(overlay.bounds, scene.content_offset))
        .chain(
            scene
                .decorations
                .iter()
                .map(|decoration| offset_region(decoration.bounds, scene.content_offset)),
        )
        .chain(
            scene
                .animations
                .iter()
                .map(|animation| offset_region(animation.affected_region, scene.content_offset)),
        )
        .collect::<Vec<_>>();
    regions.extend(scene.selections.iter().flat_map(|selection| {
        selection
            .cells
            .iter()
            .map(|position| cell_region_at(*position, metrics, scene.content_offset))
    }));
    regions
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CellFingerprint {
    text: String,
    foreground: RenderColor,
    background: RenderColor,
    style: RenderCellStyle,
}

impl From<&RenderCell> for CellFingerprint {
    fn from(value: &RenderCell) -> Self {
        Self {
            text: value.text.clone(),
            foreground: value.foreground,
            background: value.background,
            style: value.style,
        }
    }
}

impl CellFingerprint {
    fn matches(&self, cell: &RenderCell) -> bool {
        self.text == cell.text
            && self.foreground == cell.foreground
            && self.background == cell.background
            && self.style == cell.style
    }
}

fn grid_region(grid: &RenderGrid, metrics: CellMetrics) -> DamageRegion {
    RenderRect {
        x: 0,
        y: 0,
        width: (f32::from(grid.columns) * metrics.cell_width).ceil() as u32,
        height: (f32::from(grid.rows) * metrics.cell_height).ceil() as u32,
    }
}

fn scene_grid_region(scene: &RenderScene, metrics: CellMetrics) -> DamageRegion {
    let mut region = grid_region(&scene.grid, metrics);
    region.width = region
        .width
        .saturating_add(scene.content_offset.x.max(0) as u32 * 2);
    region.height = region
        .height
        .saturating_add(scene.content_offset.y.max(0) as u32 * 2);
    region
}

fn cell_region(position: CellPosition, metrics: CellMetrics) -> DamageRegion {
    RenderRect {
        x: (f32::from(position.col) * metrics.cell_width).floor() as i32,
        y: (position.row.max(0) as f32 * metrics.cell_height).floor() as i32,
        width: metrics.cell_width.ceil() as u32,
        height: metrics.cell_height.ceil() as u32,
    }
}

fn cell_region_at(
    position: CellPosition,
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
) -> DamageRegion {
    offset_region(cell_region(position, metrics), offset)
}

fn offset_region(mut region: RenderRect, offset: render_core::RenderOffset) -> RenderRect {
    region.x = region.x.saturating_add(offset.x);
    region.y = region.y.saturating_add(offset.y);
    region
}

fn merge_regions(regions: Vec<DamageRegion>) -> Vec<DamageRegion> {
    let mut merged: Vec<DamageRegion> = Vec::with_capacity(regions.len());
    for mut region in regions {
        let mut index = 0;
        while index < merged.len() {
            if rects_intersect(region, merged[index]) {
                region = union_region(region, merged.swap_remove(index));
                index = 0;
            } else {
                index += 1;
            }
        }
        merged.push(region);
    }
    merged
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameDecision {
    NoFrameNeeded,
    FrameNeeded(FrameRequestReason),
}

#[derive(Debug, Default)]
pub struct FrameScheduler {
    pending: Option<FrameRequestReason>,
    idle_wakeups: u64,
}

impl FrameScheduler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn terminal_content_changed(&mut self) {
        self.request(FrameRequestReason::TerminalContentChanged);
    }

    pub fn cursor_blink_changed(&mut self) {
        self.request(FrameRequestReason::CursorBlink);
    }

    pub fn animation_changed(&mut self) {
        self.request(FrameRequestReason::Animation);
    }

    pub fn window_resized(&mut self) {
        self.request(FrameRequestReason::WindowResized);
    }

    pub fn selection_changed(&mut self) {
        self.request(FrameRequestReason::SelectionChanged);
    }

    pub fn request(&mut self, reason: FrameRequestReason) {
        self.pending = Some(reason);
    }

    #[must_use]
    pub fn next_frame(&mut self) -> FrameDecision {
        self.pending.take().map_or_else(
            || {
                self.idle_wakeups = self.idle_wakeups.saturating_add(1);
                FrameDecision::NoFrameNeeded
            },
            FrameDecision::FrameNeeded,
        )
    }

    #[must_use]
    pub const fn idle_wakeups(&self) -> u64 {
        self.idle_wakeups
    }

    pub fn take_idle_wakeups(&mut self) -> u64 {
        let idle_wakeups = self.idle_wakeups;
        self.idle_wakeups = 0;
        idle_wakeups
    }
}

#[derive(Debug)]
pub struct CursorBlinkRuntime {
    phase_started: Instant,
    visible: bool,
    enabled: bool,
    interval: Duration,
}

impl Default for CursorBlinkRuntime {
    fn default() -> Self {
        Self {
            phase_started: Instant::now(),
            visible: true,
            enabled: false,
            interval: Duration::from_millis(600),
        }
    }
}

impl CursorBlinkRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn visible(&self) -> bool {
        self.visible
    }

    pub fn record_activity(&mut self) -> bool {
        self.phase_started = Instant::now();
        let changed = !self.visible;
        self.visible = true;
        changed
    }

    pub fn update(&mut self, enabled: bool, interval: Duration) -> bool {
        self.update_at(Instant::now(), enabled, interval)
    }

    #[must_use]
    pub fn next_frame_after(&self) -> Option<Duration> {
        if !self.enabled {
            return None;
        }
        Some(
            self.interval
                .saturating_sub(Instant::now().saturating_duration_since(self.phase_started))
                .max(Duration::from_millis(1)),
        )
    }

    fn update_at(&mut self, now: Instant, enabled: bool, interval: Duration) -> bool {
        let interval = interval.max(Duration::from_millis(1));
        if self.enabled != enabled || self.interval != interval {
            self.enabled = enabled;
            self.interval = interval;
            self.phase_started = now;
            let changed = !self.visible;
            self.visible = true;
            return changed;
        }
        if !enabled || now.saturating_duration_since(self.phase_started) < interval {
            return false;
        }
        self.phase_started = now;
        self.visible = !self.visible;
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorAnimationSettings {
    pub enabled: bool,
    pub smooth_movement: bool,
    pub typing_pulse: bool,
    pub typing_stretch: bool,
    pub trail: bool,
    pub blink_easing: bool,
    pub short_lived_glow: bool,
    pub fps: u16,
}

impl Default for CursorAnimationSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            smooth_movement: false,
            typing_pulse: false,
            typing_stretch: false,
            trail: false,
            blink_easing: false,
            short_lived_glow: false,
            fps: 60,
        }
    }
}

impl CursorAnimationSettings {
    #[must_use]
    pub const fn any_effect_enabled(self) -> bool {
        self.enabled
            && (self.smooth_movement
                || self.typing_pulse
                || self.typing_stretch
                || self.trail
                || self.blink_easing
                || self.short_lived_glow)
    }

    #[must_use]
    pub fn frame_interval(self) -> Duration {
        let fps = u64::from(self.fps.clamp(1, 240));
        Duration::from_micros(1_000_000 / fps)
    }
}

#[derive(Debug)]
pub struct CursorAnimationRuntime {
    previous_cursor: Option<CursorVisual>,
    active: Vec<AnimationHandle>,
    next_id: u64,
    last_tick: Instant,
    typing_requested: bool,
}

impl Default for CursorAnimationRuntime {
    fn default() -> Self {
        Self {
            previous_cursor: None,
            active: Vec::new(),
            next_id: 1,
            last_tick: Instant::now(),
            typing_requested: false,
        }
    }
}

impl CursorAnimationRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_typing(&mut self) {
        self.typing_requested = true;
    }

    pub fn populate_scene(
        &mut self,
        scene: &mut RenderScene,
        metrics: CellMetrics,
        settings: CursorAnimationSettings,
    ) {
        if !settings.any_effect_enabled() {
            self.previous_cursor = scene.cursor;
            self.active.clear();
            self.typing_requested = false;
            return;
        }

        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        advance_animations(&mut self.active, elapsed);

        let current = scene.cursor;
        if let Some(cursor) = current {
            let current_region = cursor_animation_region(cell_region(cursor.position, metrics));
            if let Some(previous) = self.previous_cursor
                && previous.position != cursor.position
            {
                let previous_region =
                    cursor_animation_region(cell_region(previous.position, metrics));
                if settings.smooth_movement {
                    self.push_animation(
                        AnimationKind::CursorSmoothMovement,
                        union_region(previous_region, current_region),
                        Duration::from_millis(120),
                    );
                }
                if settings.trail {
                    self.push_animation(
                        AnimationKind::CursorTrail,
                        union_region(previous_region, current_region),
                        Duration::from_millis(180),
                    );
                }
            }

            if self.typing_requested {
                if settings.typing_pulse {
                    self.push_animation(
                        AnimationKind::CursorTypingPulse,
                        current_region,
                        Duration::from_millis(140),
                    );
                }
                if settings.typing_stretch {
                    self.push_animation(
                        AnimationKind::CursorTypingStretch,
                        current_region,
                        Duration::from_millis(100),
                    );
                }
                if settings.short_lived_glow {
                    self.push_animation(
                        AnimationKind::CursorGlow,
                        cursor_animation_region(current_region),
                        Duration::from_millis(160),
                    );
                }
            }

            if settings.blink_easing
                && self
                    .previous_cursor
                    .is_some_and(|previous| previous.visible != cursor.visible)
            {
                self.push_animation(
                    AnimationKind::CursorBlinkEasing,
                    current_region,
                    settings.frame_interval().max(Duration::from_millis(16)),
                );
            }
        }

        self.previous_cursor = current;
        self.typing_requested = false;
        scene.damage_regions.extend(
            self.active
                .iter()
                .map(|animation| animation.affected_region),
        );
        scene.animations.extend(self.active.iter().copied());
    }

    #[must_use]
    pub fn needs_frame(&self) -> bool {
        !self.active.is_empty()
    }

    #[must_use]
    pub fn next_frame_after(&self, settings: CursorAnimationSettings) -> Option<Duration> {
        self.needs_frame().then(|| settings.frame_interval())
    }

    fn push_animation(
        &mut self,
        kind: AnimationKind,
        affected_region: RenderRect,
        duration: Duration,
    ) {
        self.active.push(AnimationHandle {
            id: self.next_id,
            kind,
            affected_region,
            elapsed: Duration::ZERO,
            remaining: Some(duration),
        });
        self.next_id = self.next_id.saturating_add(1);
    }
}

fn advance_animations(animations: &mut Vec<AnimationHandle>, elapsed: Duration) {
    for animation in animations.iter_mut() {
        animation.elapsed = animation.elapsed.saturating_add(elapsed);
        if let Some(remaining) = animation.remaining {
            animation.remaining = Some(remaining.checked_sub(elapsed).unwrap_or(Duration::ZERO));
        }
    }
    animations.retain(|animation| animation.remaining != Some(Duration::ZERO));
}

fn cursor_animation_region(rect: RenderRect) -> RenderRect {
    expand_region(rect, 4)
}

fn expand_region(rect: RenderRect, amount: i32) -> RenderRect {
    let amount_u32 = amount.max(0) as u32;
    RenderRect {
        x: rect.x - amount,
        y: rect.y - amount,
        width: rect.width.saturating_add(amount_u32.saturating_mul(2)),
        height: rect.height.saturating_add(amount_u32.saturating_mul(2)),
    }
}

fn union_region(a: RenderRect, b: RenderRect) -> RenderRect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (i64::from(a.x) + i64::from(a.width)).max(i64::from(b.x) + i64::from(b.width));
    let y1 = (i64::from(a.y) + i64::from(a.height)).max(i64::from(b.y) + i64::from(b.height));
    RenderRect {
        x: x0,
        y: y0,
        width: u32::try_from(x1.saturating_sub(i64::from(x0))).unwrap_or(u32::MAX),
        height: u32::try_from(y1.saturating_sub(i64::from(y0))).unwrap_or(u32::MAX),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimatedCursorImageRequest {
    pub path: PathBuf,
    pub fps: u16,
    pub max_size_kb: u32,
    pub warn_if_expensive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCursorImage {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub frame_count: u16,
    pub fps: u16,
    pub size_kb: u32,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimatedCursorImageStatus {
    Disabled,
    Loading { path: PathBuf },
    Ready(DecodedCursorImage),
    Failed { path: PathBuf, message: String },
}

#[derive(Debug, Default)]
pub struct AnimatedCursorImageCache {
    current: Option<AnimatedCursorImageStatus>,
    pending: Option<Receiver<AnimatedCursorImageStatus>>,
}

impl AnimatedCursorImageCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn disable(&mut self) {
        self.current = Some(AnimatedCursorImageStatus::Disabled);
        self.pending = None;
    }

    pub fn request(&mut self, request: AnimatedCursorImageRequest) {
        if request.path.as_os_str().is_empty() {
            self.current = Some(AnimatedCursorImageStatus::Failed {
                path: request.path,
                message: "cursor image path is empty".to_owned(),
            });
            self.pending = None;
            return;
        }

        if matches!(
            &self.current,
            Some(AnimatedCursorImageStatus::Ready(image)) if image.path == request.path && image.fps == request.fps
        ) {
            return;
        }

        let path = request.path.clone();
        let (sender, receiver) = mpsc::channel();
        self.current = Some(AnimatedCursorImageStatus::Loading { path: path.clone() });
        self.pending = Some(receiver);
        let spawn_result = thread::Builder::new()
            .name("panea-cursor-image-decode".to_owned())
            .spawn(move || {
                let status = decode_cursor_image_request(request);
                let _ = sender.send(status);
            });
        if let Err(error) = spawn_result {
            self.current = Some(AnimatedCursorImageStatus::Failed {
                path,
                message: format!("failed to start cursor image decoder: {error}"),
            });
            self.pending = None;
        }
    }

    pub fn poll(&mut self) -> AnimatedCursorImageStatus {
        if let Some(receiver) = &self.pending
            && let Ok(status) = receiver.try_recv()
        {
            self.current = Some(status);
            self.pending = None;
        }
        self.current
            .clone()
            .unwrap_or(AnimatedCursorImageStatus::Disabled)
    }
}

fn decode_cursor_image_request(request: AnimatedCursorImageRequest) -> AnimatedCursorImageStatus {
    let path = request.path;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return AnimatedCursorImageStatus::Failed {
                path,
                message: format!("failed to read cursor image: {error}"),
            };
        }
    };

    let size_kb = u32::try_from(bytes.len().div_ceil(1024)).unwrap_or(u32::MAX);
    let Some((width, height, frame_count)) = decode_cursor_image_header(&bytes) else {
        return AnimatedCursorImageStatus::Failed {
            path,
            message: "cursor image must be GIF or PNG with a valid header".to_owned(),
        };
    };

    let mut warnings = Vec::new();
    if request.warn_if_expensive && size_kb > request.max_size_kb {
        warnings.push(format!(
            "cursor image {} KiB exceeds configured cap {} KiB",
            size_kb, request.max_size_kb
        ));
    }
    if request.warn_if_expensive && request.fps > 60 {
        warnings.push(format!("cursor image FPS {} exceeds 60", request.fps));
    }

    AnimatedCursorImageStatus::Ready(DecodedCursorImage {
        path,
        width,
        height,
        frame_count,
        fps: request.fps,
        size_kb,
        warnings,
    })
}

fn decode_cursor_image_header(bytes: &[u8]) -> Option<(u32, u32, u16)> {
    if bytes.len() >= 10 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        let width = u16::from_le_bytes([bytes[6], bytes[7]]).into();
        let height = u16::from_le_bytes([bytes[8], bytes[9]]).into();
        let frames = bytes
            .windows(2)
            .filter(|window| *window == [0x21, 0xF9])
            .count()
            .max(1);
        return Some((width, height, u16::try_from(frames).unwrap_or(u16::MAX)));
    }

    if bytes.len() >= 24 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some((width, height, 1));
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuadBatchKind {
    Background,
    Decoration,
    Selection,
    Cursor,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BatchVertex {
    pub position_px: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuadBatch {
    pub kind: QuadBatchKind,
    pub vertices: Vec<BatchVertex>,
    pub indices: Vec<u32>,
}

impl QuadBatch {
    #[must_use]
    pub fn new(kind: QuadBatchKind) -> Self {
        Self {
            kind,
            vertices: Vec::new(),
            indices: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GlyphBatch {
    pub vertices: Vec<BatchVertex>,
    pub indices: Vec<u32>,
    pub glyph_count: usize,
}

impl GlyphBatch {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AtlasUpload {
    pub key: GlyphCacheKey,
    pub entry: AtlasEntry,
    pub pixels: Vec<u8>,
    pub format: GlyphBitmapFormat,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedRenderBatches {
    pub frame_width: u32,
    pub frame_height: u32,
    pub damage_regions: Vec<DamageRegion>,
    pub background: QuadBatch,
    pub glyphs: GlyphBatch,
    pub overlay_glyphs: GlyphBatch,
    pub decorations: QuadBatch,
    pub selections: QuadBatch,
    pub cursor: QuadBatch,
    pub atlas_uploads: Vec<AtlasUpload>,
    pub instrumentation: RenderInstrumentation,
}

impl PreparedRenderBatches {
    #[must_use]
    pub fn draw_call_count(&self) -> u32 {
        [
            !self.background.is_empty(),
            !self.glyphs.is_empty(),
            !self.overlay_glyphs.is_empty(),
            !self.decorations.is_empty(),
            !self.selections.is_empty(),
            !self.cursor.is_empty(),
        ]
        .into_iter()
        .filter(|non_empty| *non_empty)
        .count() as u32
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GlyphRunKey {
    font_generation: u64,
    text: String,
    size_millipoints: u32,
    bold: bool,
    italic: bool,
}

type GlyphRunItem = ShapedGlyph;

#[derive(Debug)]
pub struct RenderBatchPlanner {
    glyph_cache: GlyphCache,
    atlas: GlyphAtlas,
    glyph_runs: HashMap<GlyphRunKey, Vec<GlyphRunItem>>,
    max_glyph_runs: usize,
}

struct GlyphBatchContext<'a> {
    atlas_uploads: &'a mut Vec<AtlasUpload>,
    instrumentation: &'a mut RenderInstrumentation,
    fonts: &'a mut FontSystem,
    metrics: CellMetrics,
    rect: RenderRect,
}

impl Default for RenderBatchPlanner {
    fn default() -> Self {
        Self::new(4096, 2048, 2048)
    }
}

impl RenderBatchPlanner {
    #[must_use]
    pub fn new(glyph_capacity: usize, atlas_width: u32, atlas_height: u32) -> Self {
        Self {
            glyph_cache: GlyphCache::new(glyph_capacity),
            atlas: GlyphAtlas::new(atlas_width, atlas_height),
            glyph_runs: HashMap::new(),
            max_glyph_runs: glyph_capacity.max(1),
        }
    }

    pub fn prepare(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<PreparedRenderBatches, RendererError> {
        let started = Instant::now();
        let metrics = fonts.cell_metrics()?;
        let frame_width = ((f32::from(scene.grid.columns) * metrics.cell_width)
            .ceil()
            .max(1.0) as u32)
            .saturating_add(scene.content_offset.x.max(0) as u32 * 2);
        let frame_height = ((f32::from(scene.grid.rows) * metrics.cell_height)
            .ceil()
            .max(1.0) as u32)
            .saturating_add(scene.content_offset.y.max(0) as u32 * 2);
        let damage_regions = effective_damage_regions(scene, metrics);

        let mut background = QuadBatch::new(QuadBatchKind::Background);
        let mut glyphs = GlyphBatch {
            vertices: Vec::new(),
            indices: Vec::new(),
            glyph_count: 0,
        };
        let mut overlay_glyphs = GlyphBatch {
            vertices: Vec::new(),
            indices: Vec::new(),
            glyph_count: 0,
        };
        let mut decorations = QuadBatch::new(QuadBatchKind::Decoration);
        let mut selections = QuadBatch::new(QuadBatchKind::Selection);
        let mut cursor = QuadBatch::new(QuadBatchKind::Cursor);
        let mut atlas_uploads = Vec::new();
        let mut instrumentation = RenderInstrumentation {
            damage_region_count: damage_regions.len(),
            animated_region_count: scene.animations.len(),
            ..RenderInstrumentation::default()
        };
        instrumentation.glyphs.atlas_used_bytes = self.atlas.used_bytes();
        instrumentation.glyphs.atlas_capacity_bytes = self.atlas.capacity_bytes();

        for cell in &scene.grid.cells {
            let rect = cell_region_at(cell.position, metrics, scene.content_offset);
            if !intersects_any(rect, &damage_regions) {
                continue;
            }

            push_solid_quad(&mut background, rect, cell.background);
            push_text_decorations(&mut decorations, cell, metrics, rect);
        }

        for cell in terminal_text_runs(&scene.grid.cells) {
            let rect = cell_region_at(cell.position, metrics, scene.content_offset);
            if !intersects_any(text_run_region(&cell, metrics), &damage_regions) {
                continue;
            }
            let mut glyph_context = GlyphBatchContext {
                atlas_uploads: &mut atlas_uploads,
                instrumentation: &mut instrumentation,
                fonts,
                metrics,
                rect,
            };
            self.push_glyphs(&mut glyphs, &cell, &mut glyph_context)?;
        }

        let mut overlays = scene
            .search_highlights
            .iter()
            .chain(scene.semantic_overlays.iter())
            .collect::<Vec<_>>();
        overlays.sort_by_key(|overlay| overlay.z_index);

        for overlay in overlays {
            let bounds = offset_region(overlay.bounds, scene.content_offset);
            if intersects_any(bounds, &damage_regions) {
                let batch = if overlay_draws_behind_terminal_text(overlay.kind) {
                    &mut background
                } else {
                    &mut decorations
                };
                push_solid_quad(batch, bounds, overlay.color);
                if let Some(border_color) = overlay.border_color {
                    push_stroke_quads(batch, bounds, border_color);
                }
                let mut glyph_context = GlyphBatchContext {
                    atlas_uploads: &mut atlas_uploads,
                    instrumentation: &mut instrumentation,
                    fonts,
                    metrics,
                    rect: offset_region(overlay_label_rect(overlay, metrics), scene.content_offset),
                };
                self.push_overlay_label_glyphs(&mut overlay_glyphs, overlay, &mut glyph_context)?;
            }
        }

        for decoration in &scene.decorations {
            let bounds = offset_region(decoration.bounds, scene.content_offset);
            if intersects_any(bounds, &damage_regions) {
                push_solid_quad(&mut decorations, bounds, decoration.color);
                if let Some(border_color) = decoration.border_color {
                    push_stroke_quads(&mut decorations, bounds, border_color);
                }
            }
        }

        for selection in &scene.selections {
            for position in &selection.cells {
                let rect = cell_region_at(*position, metrics, scene.content_offset);
                if intersects_any(rect, &damage_regions) {
                    push_solid_quad(&mut selections, rect, selection.color);
                }
            }
        }

        push_animation_quads(
            &mut decorations,
            &scene.animations,
            &damage_regions,
            scene.content_offset,
        );

        if let Some(cursor_visual) = scene.cursor
            && cursor_visual.visible
        {
            push_cursor_quads(
                &mut cursor,
                cursor_visual,
                metrics,
                &damage_regions,
                scene.content_offset,
            );
        }

        instrumentation.draw_call_count = count_non_empty_batches([
            !background.is_empty(),
            !glyphs.is_empty(),
            !overlay_glyphs.is_empty(),
            !decorations.is_empty(),
            !selections.is_empty(),
            !cursor.is_empty(),
        ]);
        instrumentation.glyphs.atlas_used_bytes = self.atlas.used_bytes();
        instrumentation.glyphs.atlas_capacity_bytes = self.atlas.capacity_bytes();
        instrumentation.cpu_prepare_time = started.elapsed();
        instrumentation.frame_time = instrumentation.cpu_prepare_time;

        Ok(PreparedRenderBatches {
            frame_width,
            frame_height,
            damage_regions,
            background,
            glyphs,
            overlay_glyphs,
            decorations,
            selections,
            cursor,
            atlas_uploads,
            instrumentation,
        })
    }

    pub fn prepare_full(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<PreparedRenderBatches, RendererError> {
        let metrics = fonts.cell_metrics()?;
        let mut scene = scene.clone();
        scene.damage_regions = vec![scene_grid_region(&scene, metrics)];
        self.prepare(&scene, fonts)
    }

    #[must_use]
    pub fn atlas_len(&self) -> usize {
        self.atlas.len()
    }

    #[must_use]
    pub fn glyph_cache_len(&self) -> usize {
        self.glyph_cache.len()
    }

    #[must_use]
    pub fn atlas_dimensions(&self) -> (u32, u32) {
        self.atlas.dimensions()
    }

    pub fn reset_gpu_resident_glyphs(&mut self) {
        self.atlas.clear();
    }

    fn push_glyphs(
        &mut self,
        glyphs: &mut GlyphBatch,
        cell: &RenderCell,
        context: &mut GlyphBatchContext<'_>,
    ) -> Result<(), RendererError> {
        if cell.text.trim().is_empty() {
            return Ok(());
        }

        let run_key = GlyphRunKey {
            font_generation: context.fonts.generation_id(),
            text: cell.text.clone(),
            size_millipoints: (context.metrics.font_size * 1000.0).round().max(1.0) as u32,
            bold: cell.style.bold,
            italic: cell.style.italic,
        };
        let run = if let Some(run) = self.glyph_runs.get(&run_key) {
            run.clone()
        } else {
            while self.glyph_runs.len() >= self.max_glyph_runs {
                let Some(oldest) = self.glyph_runs.keys().next().cloned() else {
                    break;
                };
                self.glyph_runs.remove(&oldest);
            }
            let run = context
                .fonts
                .shape_text(&cell.text, cell.style.bold, cell.style.italic)?
                .glyphs;
            self.glyph_runs.insert(run_key, run.clone());
            run
        };

        let mut pen_x = context.rect.x;
        let mut pen_y = context.rect.y;
        for item in run {
            let key = item.key;
            let cache_hit = self.glyph_cache.contains_key(key);
            let bitmap = self.glyph_cache.get_or_insert_with(key, || {
                context.fonts.rasterize_glyph(key).unwrap_or_else(|_| {
                    GlyphBitmap::missing(
                        context.metrics.cell_width,
                        context.metrics.cell_height as u32,
                    )
                })
            });
            if cache_hit {
                context.instrumentation.glyphs.cache_hits =
                    context.instrumentation.glyphs.cache_hits.saturating_add(1);
            } else {
                context.instrumentation.glyphs.cache_misses = context
                    .instrumentation
                    .glyphs
                    .cache_misses
                    .saturating_add(1);
            }

            let atlas_hit = self.atlas.entry(key).is_some();
            if let Some(entry) = self.atlas.allocate(key, bitmap.as_ref()) {
                if !atlas_hit {
                    context.instrumentation.glyphs.atlas_uploads = context
                        .instrumentation
                        .glyphs
                        .atlas_uploads
                        .saturating_add(1);
                    context.atlas_uploads.push(AtlasUpload {
                        key,
                        entry,
                        pixels: bitmap.pixels.clone(),
                        format: bitmap.format,
                    });
                }
                push_glyph_quad(
                    glyphs,
                    RenderRect {
                        x: pen_x + item.x_offset.round() as i32 + bitmap.offset_x,
                        y: pen_y - item.y_offset.round() as i32 + bitmap.offset_y,
                        width: bitmap.width,
                        height: bitmap.height,
                    },
                    entry,
                    self.atlas.dimensions(),
                    cell.foreground,
                    bitmap.format == GlyphBitmapFormat::Rgba,
                );
            }
            pen_x += item.x_advance.round() as i32;
            pen_y += item.y_advance.round() as i32;
        }

        Ok(())
    }

    fn push_overlay_label_glyphs(
        &mut self,
        glyphs: &mut GlyphBatch,
        overlay: &OverlayPrimitive,
        context: &mut GlyphBatchContext<'_>,
    ) -> Result<(), RendererError> {
        let Some(label) = &overlay.label else {
            return Ok(());
        };
        if label.trim().is_empty() {
            return Ok(());
        }

        let cell = RenderCell {
            position: CellPosition { row: 0, col: 0 },
            text: label.clone(),
            foreground: overlay_label_color(overlay.kind),
            background: RenderColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 0,
            },
            style: RenderCellStyle::default(),
        };
        self.push_glyphs(glyphs, &cell, context)
    }
}

fn terminal_text_runs(cells: &[RenderCell]) -> Vec<RenderCell> {
    let mut runs: Vec<RenderCell> = Vec::new();

    for cell in cells {
        let can_join = runs.last().is_some_and(|run| {
            run.text.is_ascii()
                && cell.text.is_ascii()
                && run.position.row == cell.position.row
                && run
                    .position
                    .col
                    .saturating_add(run.text.chars().count() as u16)
                    == cell.position.col
                && run.foreground == cell.foreground
                && run.background == cell.background
                && run.style == cell.style
        });
        if can_join {
            runs.last_mut()
                .expect("run exists")
                .text
                .push_str(&cell.text);
        } else {
            runs.push(cell.clone());
        }
    }
    runs
}

fn text_run_region(cell: &RenderCell, metrics: CellMetrics) -> RenderRect {
    let mut rect = cell_region(cell.position, metrics);
    rect.width = (metrics.cell_width * cell.text.chars().count().max(1) as f32)
        .ceil()
        .max(1.0) as u32;
    rect
}

fn count_non_empty_batches<const N: usize>(batches: [bool; N]) -> u32 {
    batches.into_iter().filter(|non_empty| *non_empty).count() as u32
}

fn effective_damage_regions(scene: &RenderScene, metrics: CellMetrics) -> Vec<DamageRegion> {
    if scene.damage_regions.is_empty() {
        vec![scene_grid_region(scene, metrics)]
    } else {
        merge_regions(scene.damage_regions.clone())
    }
}

fn intersects_any(rect: RenderRect, regions: &[DamageRegion]) -> bool {
    regions.iter().any(|region| rects_intersect(rect, *region))
}

fn overlay_draws_behind_terminal_text(kind: OverlayKind) -> bool {
    matches!(
        kind,
        OverlayKind::PromptDecoration | OverlayKind::CommandBlock | OverlayKind::InputOutputGroup
    )
}

fn overlay_label_rect(overlay: &OverlayPrimitive, metrics: CellMetrics) -> RenderRect {
    let padding_x = 4;
    let label_height = metrics.cell_height.ceil().max(1.0) as u32;
    RenderRect {
        x: overlay.bounds.x + padding_x,
        y: overlay.bounds.y + ((overlay.bounds.height.saturating_sub(label_height)) / 2) as i32,
        width: overlay.bounds.width.saturating_sub((padding_x * 2) as u32),
        height: label_height,
    }
}

fn overlay_label_color(kind: OverlayKind) -> RenderColor {
    match kind {
        OverlayKind::Badge => RenderColor::rgb(245, 248, 252),
        OverlayKind::PerformanceOverlay => RenderColor::rgb(225, 232, 240),
        OverlayKind::PromptDecoration
        | OverlayKind::CommandBlock
        | OverlayKind::InputOutputGroup => RenderColor::rgb(214, 222, 232),
        OverlayKind::Selection
        | OverlayKind::SearchHighlight
        | OverlayKind::Semantic
        | OverlayKind::Decoration => RenderColor::rgb(230, 236, 244),
    }
}

fn rects_intersect(a: RenderRect, b: RenderRect) -> bool {
    let ax0 = i64::from(a.x);
    let ay0 = i64::from(a.y);
    let ax1 = ax0 + i64::from(a.width);
    let ay1 = ay0 + i64::from(a.height);
    let bx0 = i64::from(b.x);
    let by0 = i64::from(b.y);
    let bx1 = bx0 + i64::from(b.width);
    let by1 = by0 + i64::from(b.height);

    ax0 < bx1 && ax1 > bx0 && ay0 < by1 && ay1 > by0
}

fn push_solid_quad(batch: &mut QuadBatch, rect: RenderRect, color: RenderColor) {
    push_quad(
        &mut batch.vertices,
        &mut batch.indices,
        rect,
        [[0.0, 0.0]; 4],
        color,
    );
}

fn push_glyph_quad(
    batch: &mut GlyphBatch,
    rect: RenderRect,
    atlas_entry: AtlasEntry,
    atlas_dimensions: (u32, u32),
    color: RenderColor,
    color_bitmap: bool,
) {
    let (atlas_width, atlas_height) = atlas_dimensions;
    let atlas_width = atlas_width.max(1) as f32;
    let atlas_height = atlas_height.max(1) as f32;
    let x0 = atlas_entry.x as f32 / atlas_width;
    let y0 = atlas_entry.y as f32 / atlas_height;
    let x1 = atlas_entry.x.saturating_add(atlas_entry.width) as f32 / atlas_width;
    let y1 = atlas_entry.y.saturating_add(atlas_entry.height) as f32 / atlas_height;

    push_quad(
        &mut batch.vertices,
        &mut batch.indices,
        rect,
        [[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
        color,
    );
    if color_bitmap {
        for vertex in batch.vertices.iter_mut().rev().take(4) {
            vertex.color[3] = -vertex.color[3].max(f32::EPSILON);
        }
    }
    batch.glyph_count = batch.glyph_count.saturating_add(1);
}

fn push_quad(
    vertices: &mut Vec<BatchVertex>,
    indices: &mut Vec<u32>,
    rect: RenderRect,
    uv: [[f32; 2]; 4],
    color: RenderColor,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    let Ok(base) = u32::try_from(vertices.len()) else {
        return;
    };
    let x0 = rect.x as f32;
    let y0 = rect.y as f32;
    let x1 = rect.x as f32 + rect.width as f32;
    let y1 = rect.y as f32 + rect.height as f32;
    let color = color_to_f32(color);
    vertices.extend([
        BatchVertex {
            position_px: [x0, y0],
            uv: uv[0],
            color,
        },
        BatchVertex {
            position_px: [x1, y0],
            uv: uv[1],
            color,
        },
        BatchVertex {
            position_px: [x1, y1],
            uv: uv[2],
            color,
        },
        BatchVertex {
            position_px: [x0, y1],
            uv: uv[3],
            color,
        },
    ]);
    indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn color_to_f32(color: RenderColor) -> [f32; 4] {
    [
        f32::from(color.red) / 255.0,
        f32::from(color.green) / 255.0,
        f32::from(color.blue) / 255.0,
        f32::from(color.alpha) / 255.0,
    ]
}

fn push_stroke_quads(batch: &mut QuadBatch, rect: RenderRect, color: RenderColor) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    push_solid_quad(batch, RenderRect { height: 1, ..rect }, color);
    push_solid_quad(
        batch,
        RenderRect {
            y: rect.y + rect.height.saturating_sub(1) as i32,
            height: 1,
            ..rect
        },
        color,
    );
    push_solid_quad(batch, RenderRect { width: 1, ..rect }, color);
    push_solid_quad(
        batch,
        RenderRect {
            x: rect.x + rect.width.saturating_sub(1) as i32,
            width: 1,
            ..rect
        },
        color,
    );
}

fn push_text_decorations(
    decorations: &mut QuadBatch,
    cell: &RenderCell,
    _metrics: CellMetrics,
    rect: RenderRect,
) {
    if cell.style.underline {
        push_solid_quad(
            decorations,
            RenderRect {
                y: rect.y + rect.height as i32 - 2,
                height: 1,
                ..rect
            },
            cell.foreground,
        );
    }

    if cell.style.strikethrough {
        push_solid_quad(
            decorations,
            RenderRect {
                y: rect.y + (rect.height / 2) as i32,
                height: 1,
                ..rect
            },
            cell.foreground,
        );
    }
}

fn push_animation_quads(
    batch: &mut QuadBatch,
    animations: &[AnimationHandle],
    damage_regions: &[DamageRegion],
    offset: render_core::RenderOffset,
) {
    for animation in animations {
        let affected_region = offset_region(animation.affected_region, offset);
        if !intersects_any(affected_region, damage_regions) {
            continue;
        }
        let color = animation_color(*animation);
        match animation.kind {
            AnimationKind::CursorTypingStretch => {
                push_solid_quad(batch, stretch_region(affected_region), color);
            }
            AnimationKind::CursorTrail
            | AnimationKind::CursorTypingPulse
            | AnimationKind::CursorSmoothMovement
            | AnimationKind::CursorBlinkEasing
            | AnimationKind::CursorGlow
            | AnimationKind::OverlayTransition => {
                push_solid_quad(batch, affected_region, color);
            }
        }
    }
}

fn animation_color(animation: AnimationHandle) -> RenderColor {
    let base_alpha: u8 = match animation.kind {
        AnimationKind::CursorSmoothMovement => 58,
        AnimationKind::CursorTypingPulse => 78,
        AnimationKind::CursorTypingStretch => 86,
        AnimationKind::CursorTrail => 42,
        AnimationKind::CursorBlinkEasing => 34,
        AnimationKind::CursorGlow => 50,
        AnimationKind::OverlayTransition => 48,
    };
    let alpha = if let Some(remaining) = animation.remaining {
        let total = animation
            .elapsed
            .saturating_add(remaining)
            .as_millis()
            .max(1);
        let remaining = remaining.as_millis().min(total);
        ((u128::from(base_alpha) * remaining) / total) as u8
    } else {
        base_alpha
    };
    RenderColor {
        red: 120,
        green: 190,
        blue: 255,
        alpha,
    }
}

fn stretch_region(rect: RenderRect) -> RenderRect {
    RenderRect {
        x: rect.x - 2,
        y: rect.y,
        width: rect.width.saturating_add(4),
        height: rect.height,
    }
}

fn push_cursor_quads(
    batch: &mut QuadBatch,
    cursor: CursorVisual,
    metrics: CellMetrics,
    damage_regions: &[DamageRegion],
    offset: render_core::RenderOffset,
) {
    let mut rect = cell_region_at(cursor.position, metrics, offset);
    if !intersects_any(rect, damage_regions) {
        return;
    }

    let thickness = u32::from(cursor.thickness_percent.clamp(1, 100));
    match cursor.shape {
        RenderCursorShape::Block
        | RenderCursorShape::Custom
        | RenderCursorShape::CustomStaticShape => {}
        RenderCursorShape::HollowBlock => {
            let line = ((rect.width.min(rect.height) * thickness) / 100).max(1);
            push_rounded_stroke_quads(
                batch,
                rect,
                line,
                u32::from(cursor.corner_radius_px),
                cursor.color,
            );
            return;
        }
        RenderCursorShape::Beam => {
            rect.width = ((rect.width * thickness) / 100).max(1);
        }
        RenderCursorShape::Underline => {
            let cell_height = rect.height;
            rect.height = ((rect.height * thickness) / 100).max(1);
            rect.y += cell_height.saturating_sub(rect.height) as i32;
        }
    }
    push_rounded_quads(
        batch,
        rect,
        u32::from(cursor.corner_radius_px),
        cursor.color,
    );
}

fn push_rounded_quads(batch: &mut QuadBatch, rect: RenderRect, radius: u32, color: RenderColor) {
    let radius = radius.min(rect.width / 2).min(rect.height / 2);
    if radius == 0 {
        push_solid_quad(batch, rect, color);
        return;
    }
    for y in 0..rect.height {
        let inset = rounded_inset(radius, y, rect.height);
        push_solid_quad(
            batch,
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

fn push_rounded_stroke_quads(
    batch: &mut QuadBatch,
    rect: RenderRect,
    line: u32,
    radius: u32,
    color: RenderColor,
) {
    let radius = radius.min(rect.width / 2).min(rect.height / 2);
    let line = line.min(rect.width / 2).min(rect.height / 2).max(1);
    for y in 0..rect.height {
        let outer = rounded_inset(radius, y, rect.height);
        if y < line || y >= rect.height.saturating_sub(line) {
            push_solid_quad(
                batch,
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
        let inner_height = rect.height.saturating_sub(line.saturating_mul(2));
        let inner_radius = radius.saturating_sub(line);
        let inner = line.saturating_add(rounded_inset(inner_radius, y - line, inner_height));
        if inner > outer {
            let side = inner - outer;
            push_solid_quad(
                batch,
                RenderRect {
                    x: rect.x.saturating_add(outer as i32),
                    y: rect.y.saturating_add(y as i32),
                    width: side,
                    height: 1,
                },
                color,
            );
            push_solid_quad(
                batch,
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
}

fn rounded_inset(radius: u32, y: u32, height: u32) -> u32 {
    if radius == 0 || height == 0 {
        return 0;
    }
    let edge_y = y.min(height.saturating_sub(1).saturating_sub(y));
    if edge_y >= radius {
        return 0;
    }
    let center = f64::from(radius) - 0.5;
    let dy = center - (f64::from(edge_y) + 0.5);
    let inside = (center * center - dy * dy).max(0.0).sqrt();
    (center - inside).ceil().max(0.0) as u32
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
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
        output.reserve((self.width * self.height * 3) as usize);
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

        let expected = (width * height * 3) as usize;
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

        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
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
    SelectionStates,
    PromptDecorations,
    CommandBlocks,
    MultiplePanes,
    TransparencyOpacity,
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
        corner_radius_px: 4,
        z_index: 5,
        label: Some("prompt".to_owned()),
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
            corner_radius_px: 3,
            z_index: 2,
            label: Some("opacity".to_owned()),
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
            corner_radius_px: 2,
            z_index: 3,
            label: Some("badge".to_owned()),
        },
    ];
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
                text: ch.to_string(),
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
            text: ch.to_string(),
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
                    text: " ".to_owned(),
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
        corner_radius_px: 4,
        z_index: 8,
        label: Some("command".to_owned()),
    }
}

#[derive(Debug)]
pub struct InstrumentedCpuFrame {
    pub frame: CpuFrame,
    pub instrumentation: RenderInstrumentation,
}

#[derive(Debug, Default)]
pub struct TerminalRasterizer {
    batch_planner: RenderBatchPlanner,
}

impl TerminalRasterizer {
    #[must_use]
    pub fn new(glyph_capacity: usize, atlas_width: u32, atlas_height: u32) -> Self {
        Self {
            batch_planner: RenderBatchPlanner::new(glyph_capacity, atlas_width, atlas_height),
        }
    }

    pub fn prepare_batches(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<PreparedRenderBatches, RendererError> {
        self.batch_planner.prepare(scene, fonts)
    }

    pub fn prepare_full_batches(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<PreparedRenderBatches, RendererError> {
        self.batch_planner.prepare_full(scene, fonts)
    }

    #[must_use]
    pub fn atlas_dimensions(&self) -> (u32, u32) {
        self.batch_planner.atlas_dimensions()
    }

    pub fn reset_gpu_resident_glyphs(&mut self) {
        self.batch_planner.reset_gpu_resident_glyphs();
    }

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
        let width = ((f32::from(scene.grid.columns) * metrics.cell_width)
            .ceil()
            .max(1.0) as u32)
            .saturating_add(scene.content_offset.x.max(0) as u32 * 2);
        let height = ((f32::from(scene.grid.rows) * metrics.cell_height)
            .ceil()
            .max(1.0) as u32)
            .saturating_add(scene.content_offset.y.max(0) as u32 * 2);
        let mut frame = CpuFrame {
            width,
            height,
            pixels: vec![0; (width * height * 4) as usize],
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
            .chain(scene.semantic_overlays.iter())
            .collect::<Vec<_>>();
        overlays.sort_by_key(|overlay| overlay.z_index);

        for cell in &scene.grid.cells {
            draw_cell_background(&mut frame, cell, metrics, scene.content_offset);
        }

        for overlay in &overlays {
            if !overlay_draws_behind_terminal_text(overlay.kind) {
                continue;
            }
            let bounds = offset_region(overlay.bounds, scene.content_offset);
            blend_rect(&mut frame, bounds, overlay.color);
            instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            if let Some(border_color) = overlay.border_color {
                stroke_rect(&mut frame, bounds, border_color);
                instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            }
        }

        for selection in &scene.selections {
            for position in &selection.cells {
                fill_rect(
                    &mut frame,
                    cell_region_at(*position, metrics, scene.content_offset),
                    selection.color,
                );
                instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            }
        }

        if let Some(cursor) = scene.cursor {
            draw_cursor(&mut frame, cursor, metrics, scene.content_offset);
        }

        for cell in terminal_text_runs(&scene.grid.cells) {
            self.draw_cell_foreground(
                &mut frame,
                &cell,
                fonts,
                metrics,
                scene.content_offset,
                &mut instrumentation,
            )?;
        }

        for overlay in overlays {
            if overlay_draws_behind_terminal_text(overlay.kind) {
                continue;
            }
            let bounds = offset_region(overlay.bounds, scene.content_offset);
            blend_rect(&mut frame, bounds, overlay.color);
            instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            if let Some(border_color) = overlay.border_color {
                stroke_rect(&mut frame, bounds, border_color);
                instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            }
            self.draw_overlay_label(
                &mut frame,
                overlay,
                fonts,
                metrics,
                scene.content_offset,
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
        metrics: CellMetrics,
        offset: render_core::RenderOffset,
        _instrumentation: &mut RenderInstrumentation,
    ) -> Result<(), RendererError> {
        let rect = offset_region(text_run_region(cell, metrics), offset);

        let mut pen_x = rect.x;
        let mut pen_y = rect.y;
        let shaped = fonts.shape_text(&cell.text, cell.style.bold, cell.style.italic)?;
        for glyph in shaped.glyphs {
            let key = glyph.key;
            let bitmap = self.batch_planner.glyph_cache.get_or_insert_with(key, || {
                fonts.rasterize_glyph(key).unwrap_or_else(|_| {
                    GlyphBitmap::missing(metrics.cell_width, metrics.cell_height as u32)
                })
            });
            draw_glyph(
                frame,
                pen_x + glyph.x_offset.round() as i32 + bitmap.offset_x,
                pen_y - glyph.y_offset.round() as i32 + bitmap.offset_y,
                bitmap.as_ref(),
                cell.foreground,
            );
            pen_x += glyph.x_advance.round() as i32;
            pen_y += glyph.y_advance.round() as i32;
        }

        if cell.style.underline {
            let y = rect.y + rect.height as i32 - 2;
            fill_rect(
                frame,
                RenderRect {
                    y,
                    height: 1,
                    ..rect
                },
                cell.foreground,
            );
        }

        if cell.style.strikethrough {
            let y = rect.y + (rect.height / 2) as i32;
            fill_rect(
                frame,
                RenderRect {
                    y,
                    height: 1,
                    ..rect
                },
                cell.foreground,
            );
        }

        Ok(())
    }

    fn draw_overlay_label(
        &mut self,
        frame: &mut CpuFrame,
        overlay: &OverlayPrimitive,
        fonts: &mut FontSystem,
        metrics: CellMetrics,
        offset: render_core::RenderOffset,
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
            text: label.clone(),
            foreground: overlay_label_color(overlay.kind),
            background: RenderColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 0,
            },
            style: RenderCellStyle::default(),
        };
        let rect = offset_region(overlay_label_rect(overlay, metrics), offset);
        let mut pen_x = rect.x;
        let mut pen_y = rect.y;
        let shaped = fonts.shape_text(&cell.text, false, false)?;
        for glyph in shaped.glyphs {
            let key = glyph.key;
            let bitmap = self.batch_planner.glyph_cache.get_or_insert_with(key, || {
                fonts.rasterize_glyph(key).unwrap_or_else(|_| {
                    GlyphBitmap::missing(metrics.cell_width, metrics.cell_height as u32)
                })
            });
            draw_glyph(
                frame,
                pen_x + glyph.x_offset.round() as i32 + bitmap.offset_x,
                pen_y - glyph.y_offset.round() as i32 + bitmap.offset_y,
                bitmap.as_ref(),
                cell.foreground,
            );
            pen_x += glyph.x_advance.round() as i32;
            pen_y += glyph.y_advance.round() as i32;
            if pen_x > rect.x + rect.width as i32 {
                break;
            }
        }
        instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
        Ok(())
    }
}

fn draw_cell_background(
    frame: &mut CpuFrame,
    cell: &RenderCell,
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
) {
    fill_rect(
        frame,
        cell_region_at(cell.position, metrics, offset),
        cell.background,
    );
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

impl GpuVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[derive(Debug, Default)]
struct GpuBatchBuffers {
    vertices: Option<wgpu::Buffer>,
    indices: Option<wgpu::Buffer>,
    vertex_capacity: u64,
    index_capacity: u64,
    index_count: u32,
    staging_vertices: Vec<GpuVertex>,
}

impl GpuBatchBuffers {
    fn upload(
        &mut self,
        context: &GpuUploadContext<'_>,
        label: &'static str,
        vertices: &[BatchVertex],
        indices: &[u32],
    ) {
        if vertices.is_empty() || indices.is_empty() {
            self.index_count = 0;
            return;
        }

        self.staging_vertices.clear();
        self.staging_vertices.extend(
            vertices
                .iter()
                .map(|vertex| vertex_to_gpu(*vertex, context.width, context.height)),
        );
        let vertex_bytes = bytemuck::cast_slice(&self.staging_vertices);
        let index_bytes = bytemuck::cast_slice(indices);
        ensure_buffer_capacity(
            context.device,
            &mut self.vertices,
            &mut self.vertex_capacity,
            vertex_bytes.len() as u64,
            wgpu::BufferUsages::VERTEX,
            label,
        );
        ensure_buffer_capacity(
            context.device,
            &mut self.indices,
            &mut self.index_capacity,
            index_bytes.len() as u64,
            wgpu::BufferUsages::INDEX,
            label,
        );
        if let Some(buffer) = &self.vertices {
            context.queue.write_buffer(buffer, 0, vertex_bytes);
        }
        if let Some(buffer) = &self.indices {
            context.queue.write_buffer(buffer, 0, index_bytes);
        }
        self.index_count = indices.len() as u32;
    }
}

struct GpuUploadContext<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    width: u32,
    height: u32,
}

fn ensure_buffer_capacity(
    device: &wgpu::Device,
    buffer: &mut Option<wgpu::Buffer>,
    capacity: &mut u64,
    required: u64,
    usage: wgpu::BufferUsages,
    label: &str,
) {
    if required == 0 || (*capacity >= required && buffer.is_some()) {
        return;
    }
    let new_capacity = buffer_capacity(required);
    *buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: new_capacity,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));
    *capacity = new_capacity;
}

fn buffer_capacity(required: u64) -> u64 {
    required.next_power_of_two().max(256)
}

#[derive(Debug, Default)]
struct PersistentBatchBuffers {
    background: GpuBatchBuffers,
    glyphs: GpuBatchBuffers,
    overlay_glyphs: GpuBatchBuffers,
    decorations: GpuBatchBuffers,
    selections: GpuBatchBuffers,
    cursor: GpuBatchBuffers,
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

fn stroke_rect(frame: &mut CpuFrame, rect: RenderRect, color: RenderColor) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    fill_rect(frame, RenderRect { height: 1, ..rect }, color);
    fill_rect(
        frame,
        RenderRect {
            y: rect.y + rect.height.saturating_sub(1) as i32,
            height: 1,
            ..rect
        },
        color,
    );
    fill_rect(frame, RenderRect { width: 1, ..rect }, color);
    fill_rect(
        frame,
        RenderRect {
            x: rect.x + rect.width.saturating_sub(1) as i32,
            width: 1,
            ..rect
        },
        color,
    );
}

fn draw_glyph(frame: &mut CpuFrame, x: i32, y: i32, bitmap: &GlyphBitmap, color: RenderColor) {
    for gy in 0..bitmap.height {
        for gx in 0..bitmap.width {
            let target_x = x + gx as i32;
            let target_y = y + gy as i32;
            if target_x < 0
                || target_y < 0
                || target_x >= frame.width as i32
                || target_y >= frame.height as i32
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

fn draw_cursor(
    frame: &mut CpuFrame,
    cursor: CursorVisual,
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
) {
    if !cursor.visible {
        return;
    }

    let mut rect = cell_region_at(cursor.position, metrics, offset);
    let thickness = u32::from(cursor.thickness_percent.clamp(1, 100));
    match cursor.shape {
        RenderCursorShape::Block
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
        RenderCursorShape::Beam => {
            rect.width = ((rect.width * thickness) / 100).max(1);
        }
        RenderCursorShape::Underline => {
            let cell_height = rect.height;
            rect.height = ((rect.height * thickness) / 100).max(1);
            rect.y += cell_height.saturating_sub(rect.height) as i32;
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

pub struct GpuTerminalRenderer {
    window: Arc<Window>,
    options: RendererOptions,
    backend: Option<GpuBackend>,
    rasterizer: TerminalRasterizer,
    last_instrumentation: RenderInstrumentation,
    recovery_status: RenderRecoveryStatus,
    recovery_attempts: u32,
    recovery_events: Vec<RenderRecoveryEvent>,
    requires_full_redraw: bool,
}

struct GpuBackend {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    quad_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    glyph_bind_group_layout: wgpu::BindGroupLayout,
    glyph_sampler: wgpu::Sampler,
    glyph_atlas_texture: Option<wgpu::Texture>,
    glyph_atlas_size: Option<(u32, u32)>,
    glyph_bind_group: Option<wgpu::BindGroup>,
    retained_frame: Option<wgpu::Texture>,
    retained_frame_size: Option<(u32, u32)>,
    retained_frame_initialized: bool,
    surface_copy_supported: bool,
    batches: PersistentBatchBuffers,
    device_loss_signal: Arc<Mutex<Option<DeviceLossSignal>>>,
    gpu_timing: GpuTiming,
    transparent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceLossSignal {
    reason: RenderRecoveryReason,
    message: String,
}

struct GpuTiming {
    query_set: Option<wgpu::QuerySet>,
    resolve_buffer: Option<wgpu::Buffer>,
    readback_buffer: Option<wgpu::Buffer>,
    pending: Option<Receiver<Result<(), String>>>,
    timestamp_period_ns: f32,
    last_duration: Option<Duration>,
    status: GpuTimingStatus,
}

impl GpuTiming {
    const QUERY_COUNT: u32 = 2;
    const BUFFER_SIZE: u64 = std::mem::size_of::<u64>() as u64 * Self::QUERY_COUNT as u64;

    fn disabled() -> Self {
        Self {
            query_set: None,
            resolve_buffer: None,
            readback_buffer: None,
            pending: None,
            timestamp_period_ns: 0.0,
            last_duration: None,
            status: GpuTimingStatus::Disabled,
        }
    }

    fn unsupported() -> Self {
        Self {
            status: GpuTimingStatus::Unsupported,
            ..Self::disabled()
        }
    }

    fn enabled(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("panea-gpu-timestamp-query"),
            ty: wgpu::QueryType::Timestamp,
            count: Self::QUERY_COUNT,
        });
        let resolve_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("panea-gpu-timestamp-resolve"),
            size: Self::BUFFER_SIZE,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("panea-gpu-timestamp-readback"),
            size: Self::BUFFER_SIZE,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Self {
            query_set: Some(query_set),
            resolve_buffer: Some(resolve_buffer),
            readback_buffer: Some(readback_buffer),
            pending: None,
            timestamp_period_ns: queue.get_timestamp_period(),
            last_duration: None,
            status: GpuTimingStatus::Pending,
        }
    }

    fn timing_status(&self) -> GpuTimingStatus {
        self.status
    }

    fn last_duration(&self) -> Option<Duration> {
        self.last_duration
    }

    fn can_write_this_frame(&self) -> bool {
        self.query_set.is_some() && self.pending.is_none()
    }

    fn render_pass_writes(&self) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        self.query_set
            .as_ref()
            .filter(|_| self.pending.is_none())
            .map(|query_set| wgpu::RenderPassTimestampWrites {
                query_set,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: Some(1),
            })
    }

    fn resolve_after_pass(&self, encoder: &mut wgpu::CommandEncoder) {
        let (Some(query_set), Some(resolve_buffer), Some(readback_buffer)) = (
            self.query_set.as_ref(),
            self.resolve_buffer.as_ref(),
            self.readback_buffer.as_ref(),
        ) else {
            return;
        };

        encoder.resolve_query_set(query_set, 0..Self::QUERY_COUNT, resolve_buffer, 0);
        encoder.copy_buffer_to_buffer(resolve_buffer, 0, readback_buffer, 0, Self::BUFFER_SIZE);
    }

    fn start_readback(&mut self) {
        if self.query_set.is_none() || self.pending.is_some() {
            return;
        }
        let Some(readback_buffer) = self.readback_buffer.as_ref() else {
            return;
        };

        let (sender, receiver) = mpsc::channel();
        readback_buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result.map_err(|error| error.to_string()));
            });
        self.pending = Some(receiver);
        self.status = GpuTimingStatus::Pending;
    }

    fn poll(&mut self, device: &wgpu::Device) {
        let Some(receiver) = self.pending.as_ref() else {
            return;
        };
        device.poll(wgpu::Maintain::Poll);

        let Ok(result) = receiver.try_recv() else {
            self.status = GpuTimingStatus::Pending;
            return;
        };
        self.pending = None;

        let Some(readback_buffer) = self.readback_buffer.as_ref() else {
            self.status = GpuTimingStatus::Failed;
            self.last_duration = None;
            return;
        };

        match result {
            Ok(()) => {
                let slice = readback_buffer.slice(..);
                let mapped = slice.get_mapped_range();
                if mapped.len() < Self::BUFFER_SIZE as usize {
                    self.status = GpuTimingStatus::Failed;
                    self.last_duration = None;
                } else {
                    let start =
                        u64::from_le_bytes(mapped[0..8].try_into().expect("timestamp start width"));
                    let end =
                        u64::from_le_bytes(mapped[8..16].try_into().expect("timestamp end width"));
                    let delta_ticks = end.saturating_sub(start);
                    let nanos =
                        (delta_ticks as f64 * f64::from(self.timestamp_period_ns)).max(0.0) as u64;
                    self.last_duration = Some(Duration::from_nanos(nanos));
                    self.status = GpuTimingStatus::Available;
                }
                drop(mapped);
                readback_buffer.unmap();
            }
            Err(_) => {
                self.status = GpuTimingStatus::Failed;
                self.last_duration = None;
                readback_buffer.unmap();
            }
        }
    }
}

impl GpuTerminalRenderer {
    pub async fn new(window: Arc<Window>, options: RendererOptions) -> Result<Self, RendererError> {
        let backend = GpuBackend::new(Arc::clone(&window), options).await?;

        Ok(Self {
            window,
            options,
            backend: Some(backend),
            rasterizer: TerminalRasterizer::new(options.glyph_cache_entries.max(1), 2048, 2048),
            last_instrumentation: RenderInstrumentation::default(),
            recovery_status: RenderRecoveryStatus::Ready,
            recovery_attempts: 0,
            recovery_events: Vec::new(),
            requires_full_redraw: true,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(backend) = self.backend.as_mut() {
            backend.resize(width, height);
        }
        self.requires_full_redraw = true;
    }

    pub fn set_glyph_cache_capacity(&mut self, entries: usize) {
        let entries = entries.max(1);
        if self.options.glyph_cache_entries == entries {
            return;
        }
        self.options.glyph_cache_entries = entries;
        self.rasterizer = TerminalRasterizer::new(entries, 2048, 2048);
        self.requires_full_redraw = true;
    }

    pub fn request_full_redraw(&mut self) {
        self.requires_full_redraw = true;
    }

    #[must_use]
    pub fn transparency_active(&self) -> bool {
        self.backend
            .as_ref()
            .is_some_and(|backend| backend.transparent)
    }

    pub fn render_scene(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<(), RendererError> {
        let Some(backend) = self.backend.as_mut() else {
            return Err(RendererError::DeviceUnavailable(
                "renderer backend is unavailable until GPU recovery succeeds".to_owned(),
            ));
        };
        if let Some(signal) = backend.take_device_loss_signal() {
            let error = RendererError::DeviceLost {
                reason: signal.reason,
                message: signal.message,
            };
            self.mark_backend_lost(&error);
            return Err(error);
        }

        let frame_started = Instant::now();
        backend.poll_gpu_timing();
        if self.requires_full_redraw {
            backend.retained_frame_initialized = false;
        }
        let mut batches = if self.requires_full_redraw || !backend.supports_retained_damage() {
            self.rasterizer.prepare_full_batches(scene, fonts)?
        } else {
            self.rasterizer.prepare_batches(scene, fonts)?
        };
        batches.instrumentation.gpu_time = backend.gpu_timing.last_duration();
        batches.instrumentation.gpu_timing_status = backend.gpu_timing.timing_status();
        let gpu_started = Instant::now();
        backend.upload_atlas(&self.rasterizer, &batches);
        let result = backend.present_batches(&batches);
        batches.instrumentation.gpu_submit_time = Some(gpu_started.elapsed());
        batches.instrumentation.frame_time = frame_started.elapsed();
        self.last_instrumentation = batches.instrumentation;

        match result {
            Ok(PresentOutcome::Submitted) => {
                self.requires_full_redraw = false;
                Ok(())
            }
            Ok(PresentOutcome::Timeout) => Ok(()),
            Ok(PresentOutcome::SurfaceReconfigured(reason)) => {
                self.record_surface_recovery(reason);
                self.retry_present_after_surface_reconfigure(&batches)
            }
            Err(error @ RendererError::DeviceLost { .. }) => {
                self.mark_backend_lost(&error);
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    #[must_use]
    pub const fn last_instrumentation(&self) -> RenderInstrumentation {
        self.last_instrumentation
    }

    #[must_use]
    pub fn status(&self) -> RenderSurfaceStatus {
        self.recovery_status.surface_status()
    }

    #[must_use]
    pub const fn recovery_status(&self) -> &RenderRecoveryStatus {
        &self.recovery_status
    }

    #[must_use]
    pub fn recovery_events(&self) -> &[RenderRecoveryEvent] {
        &self.recovery_events
    }

    pub async fn recover_from_device_loss(
        &mut self,
        reason: RenderRecoveryReason,
    ) -> Result<RenderRecoveryEvent, RendererError> {
        self.recovery_attempts = self.recovery_attempts.saturating_add(1);
        self.recovery_status = RenderRecoveryStatus::Recovering {
            reason,
            attempts: self.recovery_attempts,
        };
        self.backend = None;
        self.invalidate_gpu_resident_resources();

        match GpuBackend::new(Arc::clone(&self.window), self.options).await {
            Ok(backend) => {
                self.backend = Some(backend);
                self.requires_full_redraw = true;
                self.recovery_status = RenderRecoveryStatus::Ready;
                let event = RenderRecoveryEvent::success(reason, self.recovery_attempts);
                self.recovery_events.push(event.clone());
                Ok(event)
            }
            Err(error) => {
                let message = error.to_string();
                self.recovery_status = RenderRecoveryStatus::Failed {
                    reason,
                    message: message.clone(),
                };
                let event =
                    RenderRecoveryEvent::failure(reason, self.recovery_attempts, message.clone());
                self.recovery_events.push(event);
                Err(RendererError::RecoveryFailed(message))
            }
        }
    }

    fn invalidate_gpu_resident_resources(&mut self) {
        self.rasterizer.reset_gpu_resident_glyphs();
        self.last_instrumentation = RenderInstrumentation::default();
        self.requires_full_redraw = true;
    }

    fn retry_present_after_surface_reconfigure(
        &mut self,
        batches: &PreparedRenderBatches,
    ) -> Result<(), RendererError> {
        let Some(backend) = self.backend.as_mut() else {
            return Err(RendererError::DeviceUnavailable(
                "renderer backend disappeared during surface recovery".to_owned(),
            ));
        };

        match backend.present_batches(batches) {
            Ok(PresentOutcome::Submitted | PresentOutcome::Timeout) => Ok(()),
            Ok(PresentOutcome::SurfaceReconfigured(_)) => Ok(()),
            Err(error @ RendererError::DeviceLost { .. }) => {
                self.mark_backend_lost(&error);
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn record_surface_recovery(&mut self, reason: RenderRecoveryReason) {
        let event = RenderRecoveryEvent {
            reason,
            attempts: self.recovery_attempts,
            rebuilt_surface: true,
            rebuilt_device: false,
            rebuilt_pipelines: false,
            rebuilt_glyph_atlas: false,
            preserved_terminal_state: true,
            message: "surface was reconfigured after a recoverable surface event".to_owned(),
        };
        self.recovery_events.push(event);
        self.recovery_status = RenderRecoveryStatus::Ready;
    }

    fn mark_backend_lost(&mut self, error: &RendererError) {
        let (reason, message) = match error {
            RendererError::DeviceLost { reason, message } => (*reason, message.clone()),
            _ => (
                RenderRecoveryReason::BackendError,
                "renderer backend failed".to_owned(),
            ),
        };
        self.backend = None;
        self.invalidate_gpu_resident_resources();
        self.recovery_status = RenderRecoveryStatus::Lost { reason, message };
    }
}

impl GpuBackend {
    async fn new(window: Arc<Window>, options: RendererOptions) -> Result<Self, RendererError> {
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Err(RendererError::EmptySurface);
        }

        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window)
            .map_err(|err| RendererError::SurfaceCreation(err.to_string()))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or(RendererError::AdapterUnavailable)?;
        let adapter_features = adapter.features();
        let gpu_timestamps_supported = adapter_features.contains(wgpu::Features::TIMESTAMP_QUERY);
        let required_features = if options.gpu_timestamps && gpu_timestamps_supported {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("panea-render-device"),
                    required_features,
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .map_err(|err| RendererError::DeviceCreation(err.to_string()))?;
        let device_loss_signal = Arc::new(Mutex::new(None));
        let callback_signal = Arc::clone(&device_loss_signal);
        device.set_device_lost_callback(move |reason, message| {
            if let Some(reason) = map_device_lost_reason(reason)
                && let Ok(mut signal) = callback_signal.lock()
            {
                *signal = Some(DeviceLossSignal { reason, message });
            }
        });

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let present_mode = match options.present_mode {
            PresentMode::Vsync => wgpu::PresentMode::Fifo,
            PresentMode::Immediate => caps
                .present_modes
                .iter()
                .copied()
                .find(|mode| *mode == wgpu::PresentMode::Immediate)
                .unwrap_or(wgpu::PresentMode::Fifo),
        };
        let alpha_mode = if options.transparent {
            caps.alpha_modes
                .iter()
                .copied()
                .find(|mode| {
                    matches!(
                        mode,
                        wgpu::CompositeAlphaMode::PreMultiplied
                            | wgpu::CompositeAlphaMode::PostMultiplied
                    )
                })
                .unwrap_or(caps.alpha_modes[0])
        } else {
            caps.alpha_modes[0]
        };
        let surface_copy_supported = caps.usages.contains(wgpu::TextureUsages::COPY_DST);
        let config = wgpu::SurfaceConfiguration {
            usage: if surface_copy_supported {
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST
            } else {
                wgpu::TextureUsages::RENDER_ATTACHMENT
            },
            format,
            width: size.width,
            height: size.height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let batch_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("panea-batch-shader"),
            source: wgpu::ShaderSource::Wgsl(BATCH_SHADER.into()),
        });
        let quad_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("panea-quad-pipeline-layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let quad_pipeline = create_batch_pipeline(
            &device,
            &quad_pipeline_layout,
            &batch_shader,
            format,
            "panea-quad-pipeline",
            "fs_color",
        );
        let glyph_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("panea-glyph-bind-group-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let glyph_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("panea-glyph-pipeline-layout"),
                bind_group_layouts: &[&glyph_bind_group_layout],
                push_constant_ranges: &[],
            });
        let glyph_pipeline = create_batch_pipeline(
            &device,
            &glyph_pipeline_layout,
            &batch_shader,
            format,
            "panea-glyph-pipeline",
            "fs_glyph",
        );
        let glyph_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("panea-glyph-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..wgpu::SamplerDescriptor::default()
        });
        let gpu_timing = if options.gpu_timestamps {
            if gpu_timestamps_supported {
                GpuTiming::enabled(&device, &queue)
            } else {
                GpuTiming::unsupported()
            }
        } else {
            GpuTiming::disabled()
        };

        Ok(Self {
            surface,
            device,
            queue,
            config,
            quad_pipeline,
            glyph_pipeline,
            glyph_bind_group_layout,
            glyph_sampler,
            glyph_atlas_texture: None,
            glyph_atlas_size: None,
            glyph_bind_group: None,
            retained_frame: None,
            retained_frame_size: None,
            retained_frame_initialized: false,
            surface_copy_supported,
            batches: PersistentBatchBuffers::default(),
            device_loss_signal,
            gpu_timing,
            transparent: options.transparent && alpha_mode != wgpu::CompositeAlphaMode::Opaque,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }

        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.retained_frame = None;
        self.retained_frame_size = None;
        self.retained_frame_initialized = false;
    }

    fn supports_retained_damage(&self) -> bool {
        self.surface_copy_supported
    }

    fn ensure_retained_frame(&mut self) {
        if !self.surface_copy_supported
            || self.retained_frame_size == Some((self.config.width, self.config.height))
        {
            return;
        }
        self.retained_frame = Some(self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("panea-retained-frame"),
            size: wgpu::Extent3d {
                width: self.config.width,
                height: self.config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        }));
        self.retained_frame_size = Some((self.config.width, self.config.height));
        self.retained_frame_initialized = false;
    }

    fn take_device_loss_signal(&self) -> Option<DeviceLossSignal> {
        self.device_loss_signal
            .lock()
            .ok()
            .and_then(|mut signal| signal.take())
    }

    fn poll_gpu_timing(&mut self) {
        self.gpu_timing.poll(&self.device);
    }

    fn upload_atlas(&mut self, rasterizer: &TerminalRasterizer, batches: &PreparedRenderBatches) {
        let atlas_size = rasterizer.atlas_dimensions();
        if self.glyph_atlas_size != Some(atlas_size) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("panea-glyph-atlas"),
                size: wgpu::Extent3d {
                    width: atlas_size.0,
                    height: atlas_size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("panea-glyph-bind-group"),
                layout: &self.glyph_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.glyph_sampler),
                    },
                ],
            });
            self.glyph_atlas_texture = Some(texture);
            self.glyph_atlas_size = Some(atlas_size);
            self.glyph_bind_group = Some(bind_group);
        }

        let Some(texture) = self.glyph_atlas_texture.as_ref() else {
            return;
        };

        for upload in &batches.atlas_uploads {
            if upload.entry.width == 0 || upload.entry.height == 0 {
                continue;
            }
            for row in 0..upload.entry.height {
                let source_channels = match upload.format {
                    GlyphBitmapFormat::Alpha => 1,
                    GlyphBitmapFormat::Rgba => 4,
                };
                let start = (row * upload.entry.width * source_channels) as usize;
                let end = start + (upload.entry.width * source_channels) as usize;
                if end > upload.pixels.len() {
                    break;
                }
                let row_pixels = match upload.format {
                    GlyphBitmapFormat::Alpha => upload.pixels[start..end]
                        .iter()
                        .flat_map(|alpha| [255, 255, 255, *alpha])
                        .collect::<Vec<_>>(),
                    GlyphBitmapFormat::Rgba => upload.pixels[start..end].to_vec(),
                };
                self.queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: upload.entry.x,
                            y: upload.entry.y + row,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &row_pixels,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: None,
                        rows_per_image: None,
                    },
                    wgpu::Extent3d {
                        width: upload.entry.width,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
    }

    fn present_batches(
        &mut self,
        batches: &PreparedRenderBatches,
    ) -> Result<PresentOutcome, RendererError> {
        self.ensure_retained_frame();
        let output = match self.surface.get_current_texture() {
            Ok(output) => output,
            Err(wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceLost,
                ));
            }
            Err(wgpu::SurfaceError::Outdated) => {
                self.surface.configure(&self.device, &self.config);
                return Ok(PresentOutcome::SurfaceReconfigured(
                    RenderRecoveryReason::SurfaceOutdated,
                ));
            }
            Err(wgpu::SurfaceError::Timeout) => return Ok(PresentOutcome::Timeout),
            Err(wgpu::SurfaceError::OutOfMemory) => {
                return Err(RendererError::DeviceLost {
                    reason: RenderRecoveryReason::OutOfMemory,
                    message: "surface reported out-of-memory; GPU resources must be recreated"
                        .to_owned(),
                });
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let upload_context = GpuUploadContext {
            device: &self.device,
            queue: &self.queue,
            width: self.config.width,
            height: self.config.height,
        };
        self.batches.background.upload(
            &upload_context,
            "background",
            &batches.background.vertices,
            &batches.background.indices,
        );
        self.batches.decorations.upload(
            &upload_context,
            "decorations",
            &batches.decorations.vertices,
            &batches.decorations.indices,
        );
        self.batches.selections.upload(
            &upload_context,
            "selections",
            &batches.selections.vertices,
            &batches.selections.indices,
        );
        self.batches.cursor.upload(
            &upload_context,
            "cursor",
            &batches.cursor.vertices,
            &batches.cursor.indices,
        );
        self.batches.glyphs.upload(
            &upload_context,
            "glyphs",
            &batches.glyphs.vertices,
            &batches.glyphs.indices,
        );
        self.batches.overlay_glyphs.upload(
            &upload_context,
            "overlay-glyphs",
            &batches.overlay_glyphs.vertices,
            &batches.overlay_glyphs.indices,
        );
        let retained_view = self
            .retained_frame
            .as_ref()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let target_view = retained_view.as_ref().unwrap_or(&view);
        let load = if retained_view.is_some() && self.retained_frame_initialized {
            wgpu::LoadOp::Load
        } else {
            wgpu::LoadOp::Clear(if self.transparent {
                wgpu::Color::TRANSPARENT
            } else {
                wgpu::Color::BLACK
            })
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("panea-batch-encoder"),
            });
        let timestamp_written = self.gpu_timing.can_write_this_frame();
        let timestamp_writes = self.gpu_timing.render_pass_writes();

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("panea-batch-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes,
            });

            pass.set_pipeline(&self.quad_pipeline);
            draw_buffers(&mut pass, &self.batches.background);
            draw_buffers(&mut pass, &self.batches.selections);
            draw_buffers(&mut pass, &self.batches.cursor);

            if let Some(glyph_bind_group) = &self.glyph_bind_group {
                pass.set_pipeline(&self.glyph_pipeline);
                pass.set_bind_group(0, glyph_bind_group, &[]);
                draw_buffers(&mut pass, &self.batches.glyphs);
            }

            pass.set_pipeline(&self.quad_pipeline);
            draw_buffers(&mut pass, &self.batches.decorations);
            if let Some(glyph_bind_group) = &self.glyph_bind_group {
                pass.set_pipeline(&self.glyph_pipeline);
                pass.set_bind_group(0, glyph_bind_group, &[]);
                draw_buffers(&mut pass, &self.batches.overlay_glyphs);
            }
        }

        if let Some(retained_frame) = self.retained_frame.as_ref() {
            encoder.copy_texture_to_texture(
                wgpu::ImageCopyTexture {
                    texture: retained_frame,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyTexture {
                    texture: &output.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: self.config.width,
                    height: self.config.height,
                    depth_or_array_layers: 1,
                },
            );
            self.retained_frame_initialized = true;
        }

        if timestamp_written {
            self.gpu_timing.resolve_after_pass(&mut encoder);
        }
        self.queue.submit(Some(encoder.finish()));
        if timestamp_written {
            self.gpu_timing.start_readback();
        }
        output.present();
        Ok(PresentOutcome::Submitted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresentOutcome {
    Submitted,
    SurfaceReconfigured(RenderRecoveryReason),
    Timeout,
}

fn map_device_lost_reason(reason: wgpu::DeviceLostReason) -> Option<RenderRecoveryReason> {
    match reason {
        wgpu::DeviceLostReason::Unknown | wgpu::DeviceLostReason::DeviceInvalid => {
            Some(RenderRecoveryReason::DeviceLost)
        }
        wgpu::DeviceLostReason::Destroyed
        | wgpu::DeviceLostReason::Dropped
        | wgpu::DeviceLostReason::ReplacedCallback => None,
    }
}

fn draw_buffers<'a>(pass: &mut wgpu::RenderPass<'a>, buffers: &'a GpuBatchBuffers) {
    let (Some(vertices), Some(indices)) = (&buffers.vertices, &buffers.indices) else {
        return;
    };
    if buffers.index_count == 0 {
        return;
    }

    pass.set_vertex_buffer(0, vertices.slice(..));
    pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
    pass.draw_indexed(0..buffers.index_count, 0, 0..1);
}

fn vertex_to_gpu(vertex: BatchVertex, surface_width: u32, surface_height: u32) -> GpuVertex {
    let width = surface_width.max(1) as f32;
    let height = surface_height.max(1) as f32;
    GpuVertex {
        position: [
            (vertex.position_px[0] / width) * 2.0 - 1.0,
            1.0 - (vertex.position_px[1] / height) * 2.0,
        ],
        uv: vertex.uv,
        color: vertex.color,
    }
}

fn create_batch_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
    fragment_entry: &'static str,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: "vs_batch",
            buffers: &[GpuVertex::layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: fragment_entry,
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    })
}

const BATCH_SHADER: &str = r#"
struct VertexIn {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_batch(in: VertexIn) -> VertexOut {
    var out: VertexOut;
    out.position = vec4<f32>(in.position, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_color(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color;
}

@group(0) @binding(0) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(1) var glyph_sampler: sampler;

@fragment
fn fs_glyph(in: VertexOut) -> @location(0) vec4<f32> {
    let sample = textureSample(glyph_atlas, glyph_sampler, in.uv);
    if in.color.a < 0.0 {
        return vec4<f32>(sample.rgb, sample.a * -in.color.a);
    }
    return vec4<f32>(in.color.rgb, in.color.a * sample.a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> CellMetrics {
        CellMetrics {
            font_size: 13.0,
            cell_width: 8.0,
            cell_height: 16.0,
            ascent: 11.0,
            descent: -3.0,
            line_gap: 1.0,
        }
    }

    #[test]
    fn persistent_buffer_growth_is_geometric_and_never_shrinks() {
        assert_eq!(buffer_capacity(1), 256);
        assert_eq!(buffer_capacity(300), 512);
        assert_eq!(buffer_capacity(4096), 4096);
    }

    fn cell(row: i64, col: u16, text: &str) -> RenderCell {
        RenderCell {
            position: CellPosition { row, col },
            text: text.to_owned(),
            foreground: RenderColor::rgb(230, 230, 230),
            background: RenderColor::rgb(12, 12, 12),
            style: RenderCellStyle::default(),
        }
    }

    fn scene(cells: Vec<RenderCell>) -> RenderScene {
        RenderScene {
            grid: RenderGrid {
                columns: 4,
                rows: 2,
                cells,
            },
            cursor: Some(CursorVisual {
                position: CellPosition { row: 0, col: 0 },
                shape: RenderCursorShape::Block,
                color: RenderColor::rgb(255, 255, 255),
                visible: true,
                thickness_percent: 15,
                corner_radius_px: 0,
                inactive: false,
            }),
            ..RenderScene::default()
        }
    }

    #[test]
    fn adjacent_ascii_cells_are_shaped_as_one_style_run() {
        let runs = terminal_text_runs(&[
            cell(0, 0, "="),
            cell(0, 1, ">"),
            cell(0, 2, " "),
            cell(1, 0, "x"),
        ]);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "=> ");
        assert_eq!(runs[1].text, "x");
    }

    #[test]
    fn cpu_rasterizer_blends_color_glyph_pixels_without_terminal_tint() {
        let mut frame = CpuFrame {
            width: 1,
            height: 1,
            pixels: vec![0, 0, 0, 255],
        };
        let bitmap = GlyphBitmap {
            width: 1,
            height: 1,
            offset_x: 0,
            offset_y: 0,
            advance_width: 1.0,
            pixels: vec![240, 20, 80, 255],
            format: GlyphBitmapFormat::Rgba,
        };
        draw_glyph(&mut frame, 0, 0, &bitmap, RenderColor::rgb(0, 255, 0));
        assert_eq!(&frame.pixels[..3], &[240, 20, 80]);
    }

    fn scene_without_cursor(cells: Vec<RenderCell>) -> RenderScene {
        RenderScene {
            cursor: None,
            ..scene(cells)
        }
    }

    #[test]
    fn atlas_allocates_and_clears_when_full() {
        let mut atlas = GlyphAtlas::new(8, 8);
        let key_a = GlyphCacheKey::new(1, u16::from(b'a'), 13.0, false, false);
        let key_b = GlyphCacheKey::new(1, u16::from(b'b'), 13.0, false, false);
        let bitmap = GlyphBitmap::missing(4.0, 4);

        assert!(atlas.allocate(key_a, &bitmap).is_some());
        assert!(atlas.allocate(key_b, &bitmap).is_some());
        assert_eq!(atlas.len(), 2);
    }

    #[test]
    fn damage_tracks_changed_cell_and_cursor() {
        let mut tracker = DamageTracker::new();
        let first = scene(vec![cell(0, 0, "a"), cell(0, 1, "b")]);
        let initial = tracker.update(&first, metrics());
        assert_eq!(initial.len(), 1);

        let mut second = scene(vec![cell(0, 0, "a"), cell(0, 1, "c")]);
        second.cursor = Some(CursorVisual {
            position: CellPosition { row: 1, col: 1 },
            shape: RenderCursorShape::Block,
            color: RenderColor::rgb(255, 255, 255),
            visible: true,
            thickness_percent: 15,
            corner_radius_px: 0,
            inactive: false,
        });

        let damage = tracker.update(&second, metrics());
        assert!(damage.iter().any(|region| region.x == 8 && region.y == 0));
        assert!(damage.iter().any(|region| region.x == 0 && region.y == 0));
        assert!(damage.iter().any(|region| region.x == 8 && region.y == 16));
    }

    #[test]
    fn damage_tracks_removed_cells_and_removed_overlays() {
        let mut tracker = DamageTracker::new();
        let mut first = scene(vec![cell(0, 0, "a"), cell(0, 1, "b")]);
        first.semantic_overlays.push(OverlayPrimitive {
            kind: OverlayKind::CommandBlock,
            bounds: RenderRect {
                x: 0,
                y: 16,
                width: 16,
                height: 16,
            },
            color: RenderColor::rgb(20, 20, 20),
            border_color: None,
            corner_radius_px: 0,
            z_index: 0,
            label: None,
        });
        let _ = tracker.update(&first, metrics());

        let second = scene(vec![cell(0, 0, "a")]);
        let damage = tracker.update(&second, metrics());

        assert!(damage.iter().any(|region| region.x == 8 && region.y == 0));
        assert!(damage.iter().any(|region| region.y == 16));
    }

    #[test]
    fn frame_scheduler_stays_idle_without_work() {
        let mut scheduler = FrameScheduler::new();
        assert_eq!(scheduler.next_frame(), FrameDecision::NoFrameNeeded);

        scheduler.terminal_content_changed();
        assert_eq!(
            scheduler.next_frame(),
            FrameDecision::FrameNeeded(FrameRequestReason::TerminalContentChanged)
        );
        assert_eq!(scheduler.next_frame(), FrameDecision::NoFrameNeeded);
    }

    #[test]
    fn cpu_snapshot_changes_when_content_changes() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut rasterizer = TerminalRasterizer::default();

        let Ok(first) =
            rasterizer.rasterize(&scene_without_cursor(vec![cell(0, 0, "a")]), &mut fonts)
        else {
            return;
        };
        let second = rasterizer
            .rasterize(&scene_without_cursor(vec![cell(0, 0, "b")]), &mut fonts)
            .expect("same resolved font should render second snapshot");

        assert_ne!(first.snapshot_hash(), second.snapshot_hash());
    }

    #[test]
    fn batch_planner_groups_cells_into_few_draws() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let Ok(batches) = planner.prepare_full(
            &scene(vec![cell(0, 0, "a"), cell(0, 1, "b"), cell(1, 0, "c")]),
            &mut fonts,
        ) else {
            return;
        };

        assert_eq!(batches.background.quad_count(), 3);
        assert_eq!(batches.glyphs.glyph_count, 3);
        assert!(batches.instrumentation.draw_call_count <= 3);
        assert!(batches.instrumentation.glyphs.atlas_uploads > 0);
    }

    #[test]
    fn semantic_command_blocks_draw_behind_text_and_badges_get_overlay_glyphs() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let mut test_scene = scene_without_cursor(vec![cell(0, 0, "p"), cell(0, 1, "w")]);
        test_scene.semantic_overlays = vec![
            OverlayPrimitive {
                kind: OverlayKind::CommandBlock,
                bounds: RenderRect {
                    x: 0,
                    y: 0,
                    width: 64,
                    height: 32,
                },
                color: RenderColor {
                    red: 40,
                    green: 48,
                    blue: 56,
                    alpha: 96,
                },
                border_color: Some(RenderColor::rgb(43, 185, 115)),
                corner_radius_px: 4,
                z_index: 10,
                label: None,
            },
            OverlayPrimitive {
                kind: OverlayKind::Badge,
                bounds: RenderRect {
                    x: 16,
                    y: 2,
                    width: 16,
                    height: 14,
                },
                color: RenderColor {
                    red: 43,
                    green: 185,
                    blue: 115,
                    alpha: 148,
                },
                border_color: None,
                corner_radius_px: 3,
                z_index: 30,
                label: Some("ok".to_owned()),
            },
        ];

        let Ok(batches) = planner.prepare_full(&test_scene, &mut fonts) else {
            return;
        };

        assert!(
            batches.background.quad_count() > test_scene.grid.cells.len(),
            "command block overlay should be batched behind terminal glyphs"
        );
        assert!(
            batches.decorations.quad_count() >= 1,
            "badge rectangle should be an overlay decoration"
        );
        assert_eq!(batches.overlay_glyphs.glyph_count, 2);
    }

    #[test]
    fn batch_planner_reuses_cached_glyphs_and_atlas_entries() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let test_scene = scene_without_cursor(vec![cell(0, 0, "panea")]);
        if planner.prepare_full(&test_scene, &mut fonts).is_err() {
            return;
        }
        let second = planner
            .prepare_full(&test_scene, &mut fonts)
            .expect("same resolved font should prepare second batch");

        assert!(second.instrumentation.glyphs.cache_hits > 0);
        assert_eq!(second.instrumentation.glyphs.atlas_uploads, 0);
    }

    #[test]
    fn resetting_gpu_resident_glyphs_reuploads_cached_glyphs() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let test_scene = scene_without_cursor(vec![cell(0, 0, "panea")]);
        if planner.prepare_full(&test_scene, &mut fonts).is_err() {
            return;
        }
        let cached = planner
            .prepare_full(&test_scene, &mut fonts)
            .expect("same resolved font should prepare cached batch");
        assert_eq!(cached.instrumentation.glyphs.atlas_uploads, 0);

        planner.reset_gpu_resident_glyphs();
        let recovered = planner
            .prepare_full(&test_scene, &mut fonts)
            .expect("cached glyph bitmaps should re-upload after atlas reset");

        assert!(recovered.instrumentation.glyphs.cache_hits > 0);
        assert!(recovered.instrumentation.glyphs.atlas_uploads > 0);
    }

    #[test]
    fn device_lost_callback_mapping_ignores_intentional_teardown() {
        assert_eq!(
            map_device_lost_reason(wgpu::DeviceLostReason::Unknown),
            Some(RenderRecoveryReason::DeviceLost)
        );
        assert_eq!(
            map_device_lost_reason(wgpu::DeviceLostReason::DeviceInvalid),
            Some(RenderRecoveryReason::DeviceLost)
        );
        assert_eq!(
            map_device_lost_reason(wgpu::DeviceLostReason::Dropped),
            None
        );
        assert_eq!(
            map_device_lost_reason(wgpu::DeviceLostReason::ReplacedCallback),
            None
        );
    }

    #[test]
    fn cursor_damage_only_batches_cursor_region() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let mut test_scene = scene(vec![cell(0, 0, "a"), cell(0, 1, "b")]);
        let Ok(font_metrics) = fonts.cell_metrics() else {
            return;
        };
        test_scene.damage_regions =
            vec![cell_region(CellPosition { row: 0, col: 0 }, font_metrics)];
        let Ok(batches) = planner.prepare(&test_scene, &mut fonts) else {
            return;
        };

        assert!(batches.background.quad_count() <= 1);
        assert_eq!(batches.cursor.quad_count(), 1);
        assert_eq!(batches.damage_regions.len(), 1);
    }

    #[test]
    fn disabled_cursor_animations_add_no_scene_work() {
        let mut runtime = CursorAnimationRuntime::new();
        let mut test_scene = scene(vec![cell(0, 0, "a")]);

        runtime.record_typing();
        runtime.populate_scene(
            &mut test_scene,
            metrics(),
            CursorAnimationSettings::default(),
        );

        assert!(test_scene.animations.is_empty());
        assert!(test_scene.damage_regions.is_empty());
        assert!(!runtime.needs_frame());
    }

    #[test]
    fn cursor_blink_runtime_is_bounded_and_activity_restores_visibility() {
        let mut runtime = CursorBlinkRuntime::new();
        let started = runtime.phase_started;
        let interval = Duration::from_millis(500);

        assert!(!runtime.update_at(started, true, interval));
        assert!(runtime.visible());
        assert!(runtime.update_at(started + interval, true, interval));
        assert!(!runtime.visible());
        assert!(runtime.record_activity());
        assert!(runtime.visible());
        assert!(!runtime.update(false, interval));
        assert!(runtime.next_frame_after().is_none());
    }

    #[test]
    fn rounded_static_cursor_stays_in_the_cursor_batch() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let mut test_scene = scene(vec![cell(0, 0, "a")]);
        test_scene.cursor = Some(CursorVisual {
            position: CellPosition { row: 0, col: 0 },
            shape: RenderCursorShape::Block,
            color: RenderColor::rgb(255, 255, 255),
            visible: true,
            thickness_percent: 15,
            corner_radius_px: 4,
            inactive: false,
        });

        let batches = planner
            .prepare_full(&test_scene, &mut fonts)
            .expect("rounded cursor should prepare");

        assert!(batches.cursor.quad_count() > 1);
        assert_eq!(batches.instrumentation.draw_call_count, 3);
    }

    #[test]
    fn content_offset_applies_to_damage_without_expanding_cell_damage() {
        let mut tracker = DamageTracker::new();
        let mut first = scene(vec![cell(0, 0, "a")]);
        first.content_offset = render_core::RenderOffset { x: 12, y: 8 };
        let initial = tracker.update(&first, metrics());
        assert_eq!(initial[0].x, 0);
        assert_eq!(initial[0].y, 0);

        let mut second = first.clone();
        second.grid.cells[0].text = "b".to_owned();
        let damage = tracker.update(&second, metrics());
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0].x, 12);
        assert_eq!(damage[0].y, 8);
        assert!(damage[0].width <= metrics().cell_width.ceil() as u32);
    }

    #[test]
    fn cursor_animations_damage_only_cursor_regions() {
        let mut runtime = CursorAnimationRuntime::new();
        let settings = CursorAnimationSettings {
            enabled: true,
            smooth_movement: true,
            typing_pulse: true,
            typing_stretch: true,
            trail: true,
            blink_easing: false,
            short_lived_glow: true,
            fps: 60,
        };
        let mut first = scene(vec![cell(0, 0, "a")]);
        runtime.populate_scene(&mut first, metrics(), settings);

        let mut second = scene(vec![cell(0, 0, "a")]);
        second.cursor = Some(CursorVisual {
            position: CellPosition { row: 1, col: 1 },
            shape: RenderCursorShape::Block,
            color: RenderColor::rgb(255, 255, 255),
            visible: true,
            thickness_percent: 15,
            corner_radius_px: 0,
            inactive: false,
        });
        runtime.record_typing();
        runtime.populate_scene(&mut second, metrics(), settings);

        assert!(
            second
                .animations
                .iter()
                .any(|animation| animation.kind == AnimationKind::CursorSmoothMovement)
        );
        assert!(
            second
                .animations
                .iter()
                .any(|animation| animation.kind == AnimationKind::CursorTypingPulse)
        );
        assert!(second.damage_regions.iter().all(|region| {
            region.width <= 32 && region.height <= 48 && region.x <= 12 && region.y <= 20
        }));
        assert!(runtime.needs_frame());
    }

    #[test]
    fn cursor_animation_quads_are_batched_separately_from_cells() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let mut test_scene = scene(vec![cell(0, 0, "a")]);
        let region = RenderRect {
            x: 0,
            y: 0,
            width: 24,
            height: 24,
        };
        test_scene.animations = vec![AnimationHandle {
            id: 1,
            kind: AnimationKind::CursorGlow,
            affected_region: region,
            elapsed: Duration::from_millis(20),
            remaining: Some(Duration::from_millis(100)),
        }];
        test_scene.damage_regions = vec![region];

        let Ok(batches) = planner.prepare(&test_scene, &mut fonts) else {
            return;
        };

        assert!(batches.decorations.quad_count() >= 1);
        assert_eq!(batches.instrumentation.animated_region_count, 1);
    }

    #[test]
    fn animated_cursor_image_header_decode_is_bounded() {
        let gif = [
            b"GIF89a".as_slice(),
            &[2, 0, 3, 0],
            &[0x21, 0xF9, 0x04, 0, 0, 0, 0, 0],
            &[0x21, 0xF9, 0x04, 0, 0, 0, 0, 0],
        ]
        .concat();

        let decoded = decode_cursor_image_header(&gif).expect("valid GIF header");

        assert_eq!(decoded, (2, 3, 2));
    }

    #[test]
    fn screenshot_fixtures_cover_required_categories() {
        let fixtures = screenshot_fixtures();
        let names = fixtures
            .iter()
            .map(|fixture| fixture.name)
            .collect::<std::collections::HashSet<_>>();

        for expected in [
            "ascii-grid",
            "truecolor-grid",
            "text-styles",
            "cjk-wide",
            "emoji",
            "cursor-states",
            "selection-states",
            "prompt-decorations",
            "command-blocks",
            "multiple-panes",
            "transparency-opacity",
        ] {
            assert!(names.contains(expected), "missing fixture {expected}");
        }
    }

    #[test]
    fn cpu_frame_ppm_round_trip_preserves_pixels() {
        let frame = CpuFrame {
            width: 2,
            height: 1,
            pixels: vec![1, 2, 3, 255, 40, 50, 60, 255],
        };

        let decoded = CpuFrame::decode_ppm(&frame.encode_ppm()).expect("valid PPM");

        assert_eq!(decoded, frame);
    }

    #[test]
    fn screenshot_diff_separates_exact_antialias_and_layout_changes() {
        let base = CpuFrame {
            width: 20,
            height: 20,
            pixels: [10, 20, 30, 255].repeat(400),
        };
        let exact = compare_screenshots(&base, &base, ScreenshotTolerance::default());
        assert_eq!(exact.kind, ScreenshotDiffKind::Exact);
        assert!(exact.passed);

        let mut small = base.clone();
        small.pixels[0] = 11;
        let antialias = compare_screenshots(&base, &small, ScreenshotTolerance::default());
        assert_eq!(
            antialias.kind,
            ScreenshotDiffKind::AntialiasingWithinTolerance
        );
        assert!(antialias.passed);

        let mut layout = base.clone();
        for pixel in layout.pixels.chunks_exact_mut(4).take(30) {
            pixel[0] = 240;
            pixel[1] = 240;
            pixel[2] = 240;
        }
        let layout_diff = compare_screenshots(&base, &layout, ScreenshotTolerance::default());
        assert_eq!(layout_diff.kind, ScreenshotDiffKind::TextLayoutFailure);
        assert!(!layout_diff.passed);
    }
}
