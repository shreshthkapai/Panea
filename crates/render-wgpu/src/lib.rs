//! WGPU renderer implementation, glyph atlas policy, damage tracking, and frame scheduling.

pub const LAYER: &str = "render performance";

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    error::Error,
    fmt,
    hash::{Hash, Hasher},
    io::Cursor,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::Duration,
    time::Instant,
};

use font_system::{
    CellMetrics, FontError, FontSystem, GlyphBitmap, GlyphBitmapFormat, GlyphCache, GlyphCacheKey,
    ShapedGlyph,
};
use image::{AnimationDecoder, ImageDecoder};
use render_core::{
    AnimationHandle, AnimationKind, CellPosition, CursorImageAsset, CursorImageFrame,
    CursorImageVisual, CursorVectorAsset, CursorVectorPrimitive, CursorVectorVisual, CursorVisual,
    DamageRegion, FrameRequestReason, GpuTimingStatus, OverlayKind, OverlayPrimitive, RenderCell,
    RenderCellStyle, RenderColor, RenderCursorShape, RenderDecoration, RenderGrid,
    RenderInstrumentation, RenderRecoveryEvent, RenderRecoveryReason, RenderRecoveryStatus,
    RenderRect, RenderScene, RenderSurfaceStatus, SelectionVisual, WindowChromeControlKind,
    WindowChromeControlVisual, WindowChromeVisual,
};
use serde::Deserialize;
use winit::window::Window;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentMode {
    Vsync,
    Immediate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetainedDamageStatus {
    Enabled,
    DisabledByConfig,
    Unsupported { reason: String },
    Unverified { reason: String },
}

impl fmt::Display for RetainedDamageStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Enabled => formatter.write_str("enabled"),
            Self::DisabledByConfig => formatter.write_str("disabled by configuration"),
            Self::Unsupported { reason } => write!(formatter, "unsupported: {reason}"),
            Self::Unverified { reason } => write!(formatter, "unverified: {reason}"),
        }
    }
}

fn retained_damage_status(requested: bool, surface_copy_supported: bool) -> RetainedDamageStatus {
    if !requested {
        RetainedDamageStatus::DisabledByConfig
    } else if surface_copy_supported {
        RetainedDamageStatus::Enabled
    } else {
        RetainedDamageStatus::Unsupported {
            reason: "the active WGPU surface cannot receive the retained frame texture".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererOptions {
    pub present_mode: PresentMode,
    pub damage_tracking: bool,
    pub gpu_timestamps: bool,
    pub transparent: bool,
    pub glyph_cache_entries: usize,
    pub background: RenderColor,
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            present_mode: PresentMode::Vsync,
            damage_tracking: false,
            gpu_timestamps: false,
            transparent: false,
            glyph_cache_entries: 8192,
            background: RenderColor::rgb(12, 12, 12),
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
    Asset(String),
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
            Self::Asset(message) => write!(f, "renderer asset error: {message}"),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AtlasCacheKey {
    Glyph(GlyphCacheKey),
    PaneaLogo,
}

impl From<GlyphCacheKey> for AtlasCacheKey {
    fn from(value: GlyphCacheKey) -> Self {
        Self::Glyph(value)
    }
}

#[derive(Debug)]
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    entries: HashMap<AtlasCacheKey, AtlasEntry>,
    lru: VecDeque<AtlasCacheKey>,
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

    pub fn allocate(
        &mut self,
        key: impl Into<AtlasCacheKey>,
        bitmap: &GlyphBitmap,
    ) -> Option<AtlasEntry> {
        let key = key.into();
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
    pub fn entry(&self, key: impl Into<AtlasCacheKey>) -> Option<AtlasEntry> {
        self.entries.get(&key.into()).copied()
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

    fn touch(&mut self, key: AtlasCacheKey) {
        self.lru.retain(|entry| *entry != key);
        self.lru.push_back(key);
    }
}

#[derive(Debug, Default)]
pub struct DamageTracker {
    previous_cells: HashMap<CellPosition, CellFingerprint>,
    previous_cursor: Option<CursorVisual>,
    previous_cursor_image: Option<CursorImageVisual>,
    previous_cursor_vector: Option<CursorVectorVisual>,
    previous_size: Option<(u16, u16)>,
    previous_offset: render_core::RenderOffset,
    previous_visuals: Vec<DamageRegion>,
    previous_search_highlights: Vec<OverlayPrimitive>,
    previous_semantic_overlays: Vec<OverlayPrimitive>,
    previous_surface_overlays: Vec<OverlayPrimitive>,
    previous_window_chrome: Option<WindowChromeVisual>,
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
            self.previous_cursor_image = scene.cursor_image.clone();
            self.previous_cursor_vector = scene.cursor_vector.clone();
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
            push_text_damage_context(
                &mut regions,
                position,
                scene.grid.columns,
                metrics,
                scene.content_offset,
            );
            self.previous_cells.remove(&position);
        }

        for cell in &scene.grid.cells {
            if !self
                .previous_cells
                .get(&cell.position)
                .is_some_and(|fingerprint| fingerprint.matches(cell))
            {
                push_text_damage_context(
                    &mut regions,
                    cell.position,
                    scene.grid.columns,
                    metrics,
                    scene.content_offset,
                );
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
            || self.previous_surface_overlays != scene.surface_overlays
            || self.previous_window_chrome != scene.window_chrome
            || self.previous_decorations != scene.decorations
            || self.previous_selections != scene.selections
            || self.previous_animations != scene.animations
            || self.previous_cursor_image != scene.cursor_image
            || self.previous_cursor_vector != scene.cursor_vector
    }

    fn remember_visuals(&mut self, scene: &RenderScene) {
        self.previous_search_highlights = scene.search_highlights.clone();
        self.previous_semantic_overlays = scene.semantic_overlays.clone();
        self.previous_surface_overlays = scene.surface_overlays.clone();
        self.previous_window_chrome = scene.window_chrome.clone();
        self.previous_decorations = scene.decorations.clone();
        self.previous_selections = scene.selections.clone();
        self.previous_animations = scene.animations.clone();
        self.previous_cursor_image = scene.cursor_image.clone();
        self.previous_cursor_vector = scene.cursor_vector.clone();
    }
}

fn visual_regions(scene: &RenderScene, metrics: CellMetrics) -> Vec<DamageRegion> {
    let mut regions = scene
        .search_highlights
        .iter()
        .chain(scene.semantic_overlays.iter())
        .map(|overlay| offset_region(overlay.bounds, scene.content_offset))
        .chain(scene.surface_overlays.iter().map(|overlay| overlay.bounds))
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
    if let Some(cursor_image) = &scene.cursor_image {
        regions.push(offset_region(cursor_image.bounds, scene.content_offset));
    }
    if let Some(cursor_vector) = &scene.cursor_vector {
        regions.push(offset_region(cursor_vector.bounds, scene.content_offset));
    }
    if let Some(window_chrome) = &scene.window_chrome {
        regions.push(window_chrome.bounds);
    }
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
    for overlay in &scene.surface_overlays {
        region.width = region.width.max(
            overlay
                .bounds
                .x
                .max(0)
                .unsigned_abs()
                .saturating_add(overlay.bounds.width),
        );
        region.height = region.height.max(
            overlay
                .bounds
                .y
                .max(0)
                .unsigned_abs()
                .saturating_add(overlay.bounds.height),
        );
    }
    if let Some(window_chrome) = &scene.window_chrome {
        region.width = region.width.max(
            window_chrome
                .bounds
                .x
                .max(0)
                .unsigned_abs()
                .saturating_add(window_chrome.bounds.width),
        );
        region.height = region.height.max(
            window_chrome
                .bounds
                .y
                .max(0)
                .unsigned_abs()
                .saturating_add(window_chrome.bounds.height),
        );
    }
    region
}

fn cell_region(position: CellPosition, metrics: CellMetrics) -> DamageRegion {
    let x = cell_axis_bounds(u32::from(position.col), metrics.cell_width);
    let y = cell_axis_bounds(position.row.max(0) as u32, metrics.cell_height);
    RenderRect {
        x: x.0,
        y: y.0,
        width: x.1,
        height: y.1,
    }
}

fn cell_axis_bounds(index: u32, advance: f32) -> (i32, u32) {
    let start = (index as f32 * advance).floor() as i32;
    let end = (index.saturating_add(1) as f32 * advance).floor() as i32;
    (start, end.saturating_sub(start).max(1) as u32)
}

fn cell_region_at(
    position: CellPosition,
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
) -> DamageRegion {
    offset_region(cell_region(position, metrics), offset)
}

fn push_text_damage_context(
    regions: &mut Vec<DamageRegion>,
    position: CellPosition,
    columns: u16,
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
) {
    const LIGATURE_CONTEXT_CELLS: u16 = 2;

    if columns == 0 {
        return;
    }
    let start = position.col.saturating_sub(LIGATURE_CONTEXT_CELLS);
    let end = position
        .col
        .saturating_add(LIGATURE_CONTEXT_CELLS)
        .min(columns - 1);
    regions.extend((start..=end).map(|col| {
        cell_region_at(
            CellPosition {
                row: position.row,
                col,
            },
            metrics,
            offset,
        )
    }));
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
    pub shadow: bool,
    pub fps: u16,
    pub max_active_animations: u16,
    pub max_animated_region_pixels: u32,
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
            shadow: false,
            fps: 60,
            max_active_animations: 8,
            max_animated_region_pixels: 250_000,
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
                || self.short_lived_glow
                || self.shadow)
            && self.max_active_animations > 0
            && self.max_animated_region_pixels > 0
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

#[derive(Debug, Clone, Copy)]
struct CursorAnimationSpec {
    kind: AnimationKind,
    affected_region: RenderRect,
    start_region: RenderRect,
    end_region: RenderRect,
    color: RenderColor,
    duration: Duration,
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
            let current_cell = cell_region(cursor.position, metrics);
            let current_region = cursor_animation_region(current_cell);
            if let Some(previous) = self.previous_cursor
                && previous.position != cursor.position
            {
                let previous_cell = cell_region(previous.position, metrics);
                if settings.smooth_movement {
                    self.push_animation(
                        settings,
                        CursorAnimationSpec {
                            kind: AnimationKind::CursorSmoothMovement,
                            affected_region: cursor_animation_region(previous_cell),
                            start_region: previous_cell,
                            end_region: current_cell,
                            color: cursor.color,
                            duration: Duration::from_millis(120),
                        },
                    );
                }
                if settings.trail {
                    self.push_animation(
                        settings,
                        CursorAnimationSpec {
                            kind: AnimationKind::CursorTrail,
                            affected_region: cursor_animation_region(previous_cell),
                            start_region: previous_cell,
                            end_region: current_cell,
                            color: cursor.color,
                            duration: Duration::from_millis(180),
                        },
                    );
                }
            }

            if self.typing_requested {
                if settings.typing_pulse {
                    self.push_animation(
                        settings,
                        CursorAnimationSpec {
                            kind: AnimationKind::CursorTypingPulse,
                            affected_region: current_region,
                            start_region: current_cell,
                            end_region: current_cell,
                            color: cursor.color,
                            duration: Duration::from_millis(140),
                        },
                    );
                }
                if settings.typing_stretch {
                    self.push_animation(
                        settings,
                        CursorAnimationSpec {
                            kind: AnimationKind::CursorTypingStretch,
                            affected_region: current_region,
                            start_region: current_cell,
                            end_region: current_cell,
                            color: cursor.color,
                            duration: Duration::from_millis(100),
                        },
                    );
                }
                if settings.short_lived_glow {
                    self.push_animation(
                        settings,
                        CursorAnimationSpec {
                            kind: AnimationKind::CursorGlow,
                            affected_region: cursor_animation_region(current_region),
                            start_region: current_cell,
                            end_region: current_cell,
                            color: cursor.color,
                            duration: Duration::from_millis(160),
                        },
                    );
                }
                if settings.shadow {
                    self.push_animation(
                        settings,
                        CursorAnimationSpec {
                            kind: AnimationKind::CursorShadow,
                            affected_region: cursor_animation_region(current_region),
                            start_region: current_cell,
                            end_region: current_cell,
                            color: cursor.color,
                            duration: Duration::from_millis(180),
                        },
                    );
                }
            }

            if settings.blink_easing
                && self
                    .previous_cursor
                    .is_some_and(|previous| previous.visible != cursor.visible)
            {
                self.push_animation(
                    settings,
                    CursorAnimationSpec {
                        kind: AnimationKind::CursorBlinkEasing,
                        affected_region: current_region,
                        start_region: current_cell,
                        end_region: current_cell,
                        color: cursor.color,
                        duration: Duration::from_millis(120),
                    },
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

    fn push_animation(&mut self, settings: CursorAnimationSettings, spec: CursorAnimationSpec) {
        let pixels = spec
            .affected_region
            .width
            .saturating_mul(spec.affected_region.height);
        if pixels > settings.max_animated_region_pixels {
            return;
        }
        let animation = AnimationHandle {
            id: self.next_id,
            kind: spec.kind,
            affected_region: spec.affected_region,
            start_region: spec.start_region,
            end_region: spec.end_region,
            color: spec.color,
            elapsed: Duration::ZERO,
            remaining: Some(spec.duration),
        };
        if let Some(existing) = self
            .active
            .iter_mut()
            .find(|active| active.kind == spec.kind)
        {
            *existing = animation;
        } else if self.active.len() < usize::from(settings.max_active_animations) {
            self.active.push(animation);
        } else {
            return;
        }
        self.next_id = self.next_id.saturating_add(1);
    }
}

fn advance_animations(animations: &mut Vec<AnimationHandle>, elapsed: Duration) {
    for animation in animations.iter_mut() {
        animation.elapsed = animation.elapsed.saturating_add(elapsed);
        if let Some(remaining) = animation.remaining {
            animation.remaining = Some(remaining.checked_sub(elapsed).unwrap_or(Duration::ZERO));
        }
        if matches!(
            animation.kind,
            AnimationKind::CursorSmoothMovement | AnimationKind::CursorTrail
        ) {
            let progress = animation_progress(*animation);
            animation.affected_region = cursor_animation_region(interpolate_region(
                animation.start_region,
                animation.end_region,
                ease_out_cubic(progress),
            ));
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
    pub asset: Arc<CursorImageAsset>,
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

#[derive(Debug)]
pub struct AnimatedCursorImageRuntime {
    image: Option<DecodedCursorImage>,
    started_at: Instant,
    visible_last_frame: bool,
}

impl Default for AnimatedCursorImageRuntime {
    fn default() -> Self {
        Self {
            image: None,
            started_at: Instant::now(),
            visible_last_frame: false,
        }
    }
}

impl AnimatedCursorImageRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_image(&mut self, image: &DecodedCursorImage) -> bool {
        if self
            .image
            .as_ref()
            .is_some_and(|current| current.asset.id == image.asset.id && current.fps == image.fps)
        {
            return false;
        }
        self.image = Some(image.clone());
        self.started_at = Instant::now();
        true
    }

    pub fn clear(&mut self) -> bool {
        let changed = self.image.take().is_some();
        self.visible_last_frame = false;
        changed
    }

    pub fn populate_scene(&mut self, scene: &mut RenderScene, metrics: CellMetrics) {
        let (Some(image), Some(cursor)) = (&self.image, scene.cursor) else {
            self.visible_last_frame = false;
            scene.cursor_image = None;
            return;
        };
        let blink = scene
            .animations
            .iter()
            .find(|animation| animation.kind == AnimationKind::CursorBlinkEasing);
        if !cursor.visible && blink.is_none() {
            self.visible_last_frame = false;
            scene.cursor_image = None;
            return;
        }

        let elapsed = self.started_at.elapsed();
        let frame_count = image.asset.frames.len().max(1);
        let frame_index = if frame_count == 1 {
            0
        } else {
            let frame_micros = 1_000_000u128 / u128::from(image.fps.max(1));
            usize::try_from(elapsed.as_micros() / frame_micros).unwrap_or(usize::MAX) % frame_count
        };
        let cursor_region = scene
            .animations
            .iter()
            .find(|animation| animation.kind == AnimationKind::CursorSmoothMovement)
            .map_or_else(
                || cell_region(cursor.position, metrics),
                |animation| {
                    interpolate_region(
                        animation.start_region,
                        animation.end_region,
                        ease_out_cubic(animation_progress(*animation)),
                    )
                },
            );
        let opacity = blink.map_or(u8::MAX, |animation| {
            let progress = animation_progress(*animation);
            let alpha = if cursor.visible {
                progress
            } else {
                1.0 - progress
            };
            (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
        });
        scene.cursor_image = Some(CursorImageVisual {
            asset: Arc::clone(&image.asset),
            frame_index: u16::try_from(frame_index).unwrap_or(u16::MAX),
            bounds: fit_cursor_image_bounds(cursor_region, image.width, image.height),
            opacity,
        });
        self.visible_last_frame = true;
    }

    #[must_use]
    pub fn next_frame_after(&self) -> Option<Duration> {
        self.image.as_ref().and_then(|image| {
            (self.visible_last_frame && image.asset.frames.len() > 1)
                .then(|| Duration::from_micros(1_000_000 / u64::from(image.fps.max(1))))
        })
    }
}

fn fit_cursor_image_bounds(cell: RenderRect, image_width: u32, image_height: u32) -> RenderRect {
    let scale = (cell.width as f32 / image_width.max(1) as f32)
        .min(cell.height as f32 / image_height.max(1) as f32);
    let width = (image_width as f32 * scale).round().max(1.0) as u32;
    let height = (image_height as f32 * scale).round().max(1.0) as u32;
    RenderRect {
        x: cell.x + i32::try_from(cell.width.saturating_sub(width) / 2).unwrap_or(0),
        y: cell.y + i32::try_from(cell.height.saturating_sub(height) / 2).unwrap_or(0),
        width,
        height,
    }
}

fn decode_cursor_image_request(request: AnimatedCursorImageRequest) -> AnimatedCursorImageStatus {
    const MAX_DIMENSION: u32 = 512;
    const MAX_FRAMES: usize = 256;

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
    if size_kb > request.max_size_kb {
        return AnimatedCursorImageStatus::Failed {
            path,
            message: format!(
                "cursor image is {size_kb} KiB, above the configured {} KiB limit",
                request.max_size_kb
            ),
        };
    }

    let decoded_limit = usize::try_from(request.max_size_kb)
        .unwrap_or(usize::MAX)
        .saturating_mul(1024)
        .saturating_mul(16)
        .clamp(4 * 1024 * 1024, 64 * 1024 * 1024);
    let decoded = match decode_cursor_image_frames(&bytes, MAX_DIMENSION, MAX_FRAMES, decoded_limit)
    {
        Ok(decoded) => decoded,
        Err(message) => return AnimatedCursorImageStatus::Failed { path, message },
    };

    let mut warnings = Vec::new();
    if request.warn_if_expensive
        && size_kb > request.max_size_kb.saturating_mul(3).saturating_div(4)
    {
        warnings.push(format!(
            "cursor image {} KiB is close to the configured {} KiB limit",
            size_kb, request.max_size_kb
        ));
    }
    if request.warn_if_expensive && request.fps > 30 {
        warnings.push(format!(
            "cursor image FPS {} exceeds the low-cost 30 FPS range",
            request.fps
        ));
    }

    AnimatedCursorImageStatus::Ready(DecodedCursorImage {
        path,
        width: decoded.width,
        height: decoded.height,
        frame_count: u16::try_from(decoded.frames.len()).unwrap_or(u16::MAX),
        fps: request.fps,
        size_kb,
        warnings,
        asset: Arc::new(CursorImageAsset {
            id: cursor_image_asset_id(&bytes),
            width: decoded.width,
            height: decoded.height,
            frames: decoded.frames.into(),
        }),
    })
}

struct DecodedCursorFrames {
    width: u32,
    height: u32,
    frames: Vec<CursorImageFrame>,
}

fn decode_cursor_image_frames(
    bytes: &[u8],
    max_dimension: u32,
    max_frames: usize,
    max_decoded_bytes: usize,
) -> Result<DecodedCursorFrames, String> {
    let format = image::guess_format(bytes)
        .map_err(|_| "cursor image must be a valid GIF or PNG".to_owned())?;
    match format {
        image::ImageFormat::Gif => {
            let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
                .map_err(|error| format!("failed to decode GIF cursor: {error}"))?;
            let (width, height) = decoder.dimensions();
            validate_cursor_image_dimensions(width, height, max_dimension)?;
            let mut frames = Vec::new();
            let mut decoded_bytes = 0usize;
            for frame in decoder.into_frames().take(max_frames.saturating_add(1)) {
                if frames.len() == max_frames {
                    return Err(format!("cursor GIF exceeds the {max_frames}-frame limit"));
                }
                let frame =
                    frame.map_err(|error| format!("failed to decode GIF cursor frame: {error}"))?;
                let buffer = frame.into_buffer();
                if buffer.width() != width || buffer.height() != height {
                    return Err("cursor GIF frames must use one canvas size".to_owned());
                }
                let pixels = buffer.into_raw();
                decoded_bytes = decoded_bytes.saturating_add(pixels.len());
                if decoded_bytes > max_decoded_bytes {
                    return Err(format!(
                        "decoded cursor frames exceed the {} KiB memory budget",
                        max_decoded_bytes.div_ceil(1024)
                    ));
                }
                frames.push(CursorImageFrame {
                    pixels: pixels.into(),
                });
            }
            if frames.is_empty() {
                return Err("cursor GIF contains no frames".to_owned());
            }
            Ok(DecodedCursorFrames {
                width,
                height,
                frames,
            })
        }
        image::ImageFormat::Png => {
            let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
                .map_err(|error| format!("failed to decode PNG cursor: {error}"))?
                .to_rgba8();
            let (width, height) = image.dimensions();
            validate_cursor_image_dimensions(width, height, max_dimension)?;
            let pixels = image.into_raw();
            if pixels.len() > max_decoded_bytes {
                return Err(format!(
                    "decoded cursor image exceeds the {} KiB memory budget",
                    max_decoded_bytes.div_ceil(1024)
                ));
            }
            Ok(DecodedCursorFrames {
                width,
                height,
                frames: vec![CursorImageFrame {
                    pixels: pixels.into(),
                }],
            })
        }
        _ => Err("cursor image format is unsupported; use GIF or PNG".to_owned()),
    }
}

fn validate_cursor_image_dimensions(
    width: u32,
    height: u32,
    max_dimension: u32,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("cursor image dimensions must be non-zero".to_owned());
    }
    if width > max_dimension || height > max_dimension {
        return Err(format!(
            "cursor image dimensions {width}x{height} exceed the {max_dimension}px limit"
        ));
    }
    Ok(())
}

fn cursor_image_asset_id(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

pub const CURSOR_VECTOR_FORMAT_VERSION: u16 = 1;
pub const CURSOR_VECTOR_CANVAS_UNITS: u16 = 1000;
pub const CURSOR_VECTOR_MAX_PRIMITIVES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorVectorRequest {
    pub path: PathBuf,
    pub max_size_kb: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCursorVector {
    pub path: PathBuf,
    pub size_kb: u32,
    pub asset: Arc<CursorVectorAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorVectorStatus {
    Disabled,
    Loading { path: PathBuf },
    Ready(DecodedCursorVector),
    Failed { path: PathBuf, message: String },
}

#[derive(Debug, Default)]
pub struct CursorVectorCache {
    current: Option<CursorVectorStatus>,
    pending: Option<Receiver<CursorVectorStatus>>,
}

impl CursorVectorCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn disable(&mut self) {
        self.current = Some(CursorVectorStatus::Disabled);
        self.pending = None;
    }

    pub fn request(&mut self, request: CursorVectorRequest) {
        if request.path.as_os_str().is_empty() {
            self.current = Some(CursorVectorStatus::Failed {
                path: request.path,
                message: "cursor vector path is empty".to_owned(),
            });
            self.pending = None;
            return;
        }
        if matches!(
            &self.current,
            Some(CursorVectorStatus::Ready(vector)) if vector.path == request.path
        ) {
            return;
        }

        let path = request.path.clone();
        let (sender, receiver) = mpsc::channel();
        self.current = Some(CursorVectorStatus::Loading { path: path.clone() });
        self.pending = Some(receiver);
        let spawn_result = thread::Builder::new()
            .name("panea-cursor-vector-decode".to_owned())
            .spawn(move || {
                let status = decode_cursor_vector_request(request);
                let _ = sender.send(status);
            });
        if let Err(error) = spawn_result {
            self.current = Some(CursorVectorStatus::Failed {
                path,
                message: format!("failed to start cursor vector decoder: {error}"),
            });
            self.pending = None;
        }
    }

    pub fn poll(&mut self) -> CursorVectorStatus {
        if let Some(receiver) = &self.pending
            && let Ok(status) = receiver.try_recv()
        {
            self.current = Some(status);
            self.pending = None;
        }
        self.current.clone().unwrap_or(CursorVectorStatus::Disabled)
    }
}

#[derive(Debug, Default)]
pub struct CursorVectorRuntime {
    vector: Option<DecodedCursorVector>,
}

impl CursorVectorRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_vector(&mut self, vector: &DecodedCursorVector) -> bool {
        if self
            .vector
            .as_ref()
            .is_some_and(|current| current.asset.id == vector.asset.id)
        {
            return false;
        }
        self.vector = Some(vector.clone());
        true
    }

    pub fn clear(&mut self) -> bool {
        self.vector.take().is_some()
    }

    pub fn populate_scene(&self, scene: &mut RenderScene, metrics: CellMetrics) {
        let (Some(vector), Some(cursor)) = (&self.vector, scene.cursor) else {
            scene.cursor_vector = None;
            return;
        };
        let blink = scene
            .animations
            .iter()
            .find(|animation| animation.kind == AnimationKind::CursorBlinkEasing);
        if !cursor.visible && blink.is_none() {
            scene.cursor_vector = None;
            return;
        }
        let bounds = scene
            .animations
            .iter()
            .find(|animation| animation.kind == AnimationKind::CursorSmoothMovement)
            .map_or_else(
                || cell_region(cursor.position, metrics),
                |animation| {
                    interpolate_region(
                        animation.start_region,
                        animation.end_region,
                        ease_out_cubic(animation_progress(*animation)),
                    )
                },
            );
        scene.cursor_vector = Some(CursorVectorVisual {
            asset: Arc::clone(&vector.asset),
            bounds,
            color: cursor.color,
            opacity: blink.map_or(u8::MAX, |animation| {
                let progress = animation_progress(*animation);
                let alpha = if cursor.visible {
                    progress
                } else {
                    1.0 - progress
                };
                (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
            }),
        });
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorVectorDocument {
    version: u16,
    primitives: Vec<CursorVectorDocumentPrimitive>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorVectorDocumentPrimitive {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    #[serde(default)]
    corner_radius: u16,
    color: Option<[u8; 4]>,
}

fn decode_cursor_vector_request(request: CursorVectorRequest) -> CursorVectorStatus {
    let path = request.path;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return CursorVectorStatus::Failed {
                path,
                message: format!("failed to read cursor vector: {error}"),
            };
        }
    };
    let size_kb = u32::try_from(bytes.len().div_ceil(1024)).unwrap_or(u32::MAX);
    if size_kb > request.max_size_kb {
        return CursorVectorStatus::Failed {
            path,
            message: format!(
                "cursor vector is {size_kb} KiB, above the configured {} KiB limit",
                request.max_size_kb
            ),
        };
    }
    match decode_cursor_vector(&bytes) {
        Ok(primitives) => CursorVectorStatus::Ready(DecodedCursorVector {
            path,
            size_kb,
            asset: Arc::new(CursorVectorAsset {
                id: cursor_image_asset_id(&bytes),
                primitives: primitives.into(),
            }),
        }),
        Err(message) => CursorVectorStatus::Failed { path, message },
    }
}

fn decode_cursor_vector(bytes: &[u8]) -> Result<Vec<CursorVectorPrimitive>, String> {
    let document: CursorVectorDocument = serde_json::from_slice(bytes)
        .map_err(|error| format!("cursor vector must be valid Panea JSON: {error}"))?;
    if document.version != CURSOR_VECTOR_FORMAT_VERSION {
        return Err(format!(
            "unsupported cursor vector version {}; expected {}",
            document.version, CURSOR_VECTOR_FORMAT_VERSION
        ));
    }
    if document.primitives.is_empty() {
        return Err("cursor vector must contain at least one primitive".to_owned());
    }
    if document.primitives.len() > CURSOR_VECTOR_MAX_PRIMITIVES {
        return Err(format!(
            "cursor vector exceeds the {CURSOR_VECTOR_MAX_PRIMITIVES}-primitive limit"
        ));
    }

    document
        .primitives
        .into_iter()
        .enumerate()
        .map(|(index, primitive)| {
            let right = primitive.x.saturating_add(primitive.width);
            let bottom = primitive.y.saturating_add(primitive.height);
            if primitive.width == 0
                || primitive.height == 0
                || right > CURSOR_VECTOR_CANVAS_UNITS
                || bottom > CURSOR_VECTOR_CANVAS_UNITS
                || primitive.corner_radius > CURSOR_VECTOR_CANVAS_UNITS / 2
            {
                return Err(format!(
                    "cursor vector primitive {index} is outside the 1000x1000 canvas or has invalid geometry"
                ));
            }
            Ok(CursorVectorPrimitive {
                x: primitive.x,
                y: primitive.y,
                width: primitive.width,
                height: primitive.height,
                corner_radius: primitive.corner_radius,
                color: primitive.color.map(|color| RenderColor {
                    red: color[0],
                    green: color[1],
                    blue: color[2],
                    alpha: color[3],
                }),
            })
        })
        .collect()
}

#[cfg(test)]
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
    pub key: AtlasCacheKey,
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
    pub window_chrome: QuadBatch,
    pub selections: QuadBatch,
    pub cursor: QuadBatch,
    pub cursor_image: QuadBatch,
    pub cursor_image_asset: Option<Arc<CursorImageAsset>>,
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
            !self.window_chrome.is_empty(),
            !self.selections.is_empty(),
            !self.cursor.is_empty(),
            !self.cursor_image.is_empty(),
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
        let mut frame_width = ((f32::from(scene.grid.columns) * metrics.cell_width)
            .ceil()
            .max(1.0) as u32)
            .saturating_add(scene.content_offset.x.max(0) as u32 * 2);
        let mut frame_height = ((f32::from(scene.grid.rows) * metrics.cell_height)
            .ceil()
            .max(1.0) as u32)
            .saturating_add(scene.content_offset.y.max(0) as u32 * 2);
        if let Some(window_chrome) = &scene.window_chrome {
            frame_width = frame_width.max(
                window_chrome
                    .bounds
                    .x
                    .max(0)
                    .unsigned_abs()
                    .saturating_add(window_chrome.bounds.width),
            );
            frame_height = frame_height.max(
                window_chrome
                    .bounds
                    .y
                    .max(0)
                    .unsigned_abs()
                    .saturating_add(window_chrome.bounds.height),
            );
        }
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
        let mut window_chrome = QuadBatch::new(QuadBatchKind::Decoration);
        let mut selections = QuadBatch::new(QuadBatchKind::Selection);
        let mut cursor = QuadBatch::new(QuadBatchKind::Cursor);
        let mut cursor_image = QuadBatch::new(QuadBatchKind::Cursor);
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

        for cell in damaged_terminal_text_runs(
            &scene.grid.cells,
            &damage_regions,
            metrics,
            scene.content_offset,
        ) {
            let rect = cell_region_at(cell.position, metrics, scene.content_offset);
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
            .map(|overlay| (overlay, scene.content_offset))
            .chain(
                scene
                    .surface_overlays
                    .iter()
                    .map(|overlay| (overlay, render_core::RenderOffset::default())),
            )
            .collect::<Vec<_>>();
        overlays.sort_by_key(|(overlay, _)| overlay.z_index);

        for (overlay, offset) in overlays {
            let bounds = offset_region(overlay.bounds, offset);
            if intersects_any(bounds, &damage_regions) {
                let batch = if overlay_draws_behind_terminal_text(overlay.kind) {
                    &mut background
                } else {
                    &mut decorations
                };
                push_rounded_quads(
                    batch,
                    bounds,
                    u32::from(overlay.corner_radius_px),
                    overlay.color,
                );
                if let Some(border_color) = overlay.border_color {
                    push_rounded_stroke_quads(
                        batch,
                        bounds,
                        u32::from(overlay.border_width_px.max(1)),
                        u32::from(overlay.corner_radius_px),
                        border_color,
                    );
                }
                let mut glyph_context = GlyphBatchContext {
                    atlas_uploads: &mut atlas_uploads,
                    instrumentation: &mut instrumentation,
                    fonts,
                    metrics,
                    rect: offset_region(overlay_label_rect(overlay, metrics), offset),
                };
                self.push_overlay_label_glyphs(&mut overlay_glyphs, overlay, &mut glyph_context)?;
            }
        }

        for decoration in &scene.decorations {
            let bounds = offset_region(decoration.bounds, scene.content_offset);
            if intersects_any(bounds, &damage_regions) {
                push_solid_quad(&mut decorations, bounds, decoration.color);
                if let Some(border_color) = decoration.border_color {
                    push_stroke_quads(&mut decorations, bounds, 1, border_color);
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
            scene.cursor_image.is_some() || scene.cursor_vector.is_some(),
        );

        if let Some(cursor_visual) = scene.cursor
            && cursor_visual.visible
            && scene.cursor_image.is_none()
            && scene.cursor_vector.is_none()
            && !scene
                .animations
                .iter()
                .any(|animation| animation.kind == AnimationKind::CursorSmoothMovement)
        {
            push_cursor_quads(
                &mut cursor,
                cursor_visual,
                metrics,
                &damage_regions,
                scene.content_offset,
            );
        }

        let cursor_image_asset = scene.cursor_image.as_ref().and_then(|visual| {
            let bounds = offset_region(visual.bounds, scene.content_offset);
            if intersects_any(bounds, &damage_regions)
                && usize::from(visual.frame_index) < visual.asset.frames.len()
            {
                push_cursor_image_quad(
                    &mut cursor_image,
                    bounds,
                    visual.frame_index,
                    visual.opacity,
                );
                Some(Arc::clone(&visual.asset))
            } else {
                None
            }
        });

        if let Some(vector) = &scene.cursor_vector {
            push_cursor_vector_quads(&mut cursor, vector, &damage_regions, scene.content_offset);
        }

        if let Some(visual) = &scene.window_chrome
            && intersects_any(visual.bounds, &damage_regions)
        {
            self.push_window_chrome(
                &mut window_chrome,
                &mut overlay_glyphs,
                visual,
                &mut GlyphBatchContext {
                    atlas_uploads: &mut atlas_uploads,
                    instrumentation: &mut instrumentation,
                    fonts,
                    metrics,
                    rect: visual.bounds,
                },
            )?;
        }

        instrumentation.draw_call_count = count_non_empty_batches([
            !background.is_empty(),
            !glyphs.is_empty(),
            !overlay_glyphs.is_empty(),
            !decorations.is_empty(),
            !window_chrome.is_empty(),
            !selections.is_empty(),
            !cursor.is_empty(),
            !cursor_image.is_empty(),
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
            window_chrome,
            selections,
            cursor,
            cursor_image,
            cursor_image_asset,
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

        let mut pen_x = context.rect.x as f32;
        let mut pen_y = context.rect.y as f32;
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
                        key: key.into(),
                        entry,
                        pixels: bitmap.pixels.clone(),
                        format: bitmap.format,
                    });
                }
                push_glyph_quad(
                    glyphs,
                    RenderRect {
                        x: (pen_x + item.x_offset).round() as i32 + bitmap.offset_x,
                        y: (pen_y - item.y_offset).round() as i32 + bitmap.offset_y,
                        width: bitmap.width,
                        height: bitmap.height,
                    },
                    entry,
                    self.atlas.dimensions(),
                    cell.foreground,
                    bitmap.format == GlyphBitmapFormat::Rgba,
                );
            }
            pen_x += item.x_advance;
            pen_y += item.y_advance;
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
        self.push_glyphs(glyphs, &cell, context)
    }

    fn push_window_chrome(
        &mut self,
        geometry: &mut QuadBatch,
        glyphs: &mut GlyphBatch,
        visual: &WindowChromeVisual,
        context: &mut GlyphBatchContext<'_>,
    ) -> Result<(), RendererError> {
        if visual.opacity == 0 || visual.bounds.width == 0 || visual.bounds.height == 0 {
            return Ok(());
        }

        push_solid_quad(
            geometry,
            visual.bounds,
            with_fixed_opacity(RenderColor::rgb(18, 18, 18), visual.opacity),
        );
        for control in &visual.controls {
            push_window_chrome_control(geometry, control, visual.opacity);
        }

        let mut title_x = visual.bounds.x.saturating_add(8);
        if visual.show_logo {
            let bitmap = panea_logo_bitmap()?;
            let key = AtlasCacheKey::PaneaLogo;
            let atlas_hit = self.atlas.entry(key).is_some();
            if let Some(entry) = self.atlas.allocate(key, bitmap) {
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
                        format: GlyphBitmapFormat::Rgba,
                    });
                }
                let logo_bounds = window_chrome_logo_bounds(visual, title_x);
                push_glyph_quad(
                    glyphs,
                    logo_bounds,
                    entry,
                    self.atlas.dimensions(),
                    with_fixed_opacity(RenderColor::rgb(255, 255, 255), visual.opacity),
                    true,
                );
                title_x = title_x.saturating_add(logo_bounds.width as i32 + 8);
            }
        }

        if let Some(overlay) = window_chrome_title_overlay(visual, title_x) {
            context.rect = overlay_label_rect(&overlay, context.metrics);
            self.push_overlay_label_glyphs(glyphs, &overlay, context)?;
        }

        Ok(())
    }
}

static PANEA_LOGO_BITMAP: OnceLock<Result<GlyphBitmap, String>> = OnceLock::new();

fn panea_logo_bitmap() -> Result<&'static GlyphBitmap, RendererError> {
    PANEA_LOGO_BITMAP
        .get_or_init(|| {
            let image = image::load_from_memory_with_format(
                assets::PANEA_ICON_PNG_32,
                image::ImageFormat::Png,
            )
            .map_err(|error| format!("failed to decode built-in Panea logo: {error}"))?
            .to_rgba8();
            let dimensions = (image.width(), image.height());
            if dimensions != assets::PANEA_ICON_PNG_32_DIMENSIONS {
                return Err(format!(
                    "built-in Panea logo is {dimensions:?}; expected {:?}",
                    assets::PANEA_ICON_PNG_32_DIMENSIONS
                ));
            }
            let expected_bytes = usize::try_from(image.width())
                .unwrap_or(usize::MAX)
                .saturating_mul(usize::try_from(image.height()).unwrap_or(usize::MAX))
                .saturating_mul(4);
            if expected_bytes > assets::MAX_RENDERER_BRANDING_BYTES {
                return Err(format!(
                    "decoded Panea logo requires {expected_bytes} bytes; limit is {}",
                    assets::MAX_RENDERER_BRANDING_BYTES
                ));
            }
            Ok(GlyphBitmap {
                width: image.width(),
                height: image.height(),
                offset_x: 0,
                offset_y: 0,
                advance_width: image.width() as f32,
                pixels: image.into_raw(),
                format: GlyphBitmapFormat::Rgba,
            })
        })
        .as_ref()
        .map_err(|message| RendererError::Asset(message.clone()))
}

fn window_chrome_logo_bounds(visual: &WindowChromeVisual, x: i32) -> RenderRect {
    let size = visual.bounds.height.saturating_sub(12).clamp(1, 24);
    RenderRect {
        x,
        y: visual.bounds.y + visual.bounds.height.saturating_sub(size) as i32 / 2,
        width: size,
        height: size,
    }
}

fn window_chrome_title_overlay(
    visual: &WindowChromeVisual,
    title_x: i32,
) -> Option<OverlayPrimitive> {
    if visual.title.trim().is_empty() {
        return None;
    }
    let controls_x = visual
        .controls
        .iter()
        .map(|control| control.bounds.x)
        .min()
        .unwrap_or_else(|| visual.bounds.x.saturating_add(visual.bounds.width as i32));
    let title_width = controls_x.saturating_sub(title_x).saturating_sub(8).max(0) as u32;
    (title_width > 0).then(|| OverlayPrimitive {
        kind: OverlayKind::WindowChrome,
        bounds: RenderRect {
            x: title_x.saturating_sub(4),
            y: visual.bounds.y,
            width: title_width.saturating_add(8),
            height: visual.bounds.height,
        },
        color: RenderColor {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0,
        },
        border_color: None,
        border_width_px: 0,
        corner_radius_px: 0,
        z_index: i16::MAX,
        label: Some(visual.title.clone()),
        label_color: Some(with_fixed_opacity(
            RenderColor::rgb(232, 232, 232),
            visual.opacity,
        )),
    })
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

fn damaged_terminal_text_runs(
    cells: &[RenderCell],
    damage_regions: &[DamageRegion],
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
) -> Vec<RenderCell> {
    let damaged = cells
        .iter()
        .filter(|cell| {
            intersects_any(
                cell_region_at(cell.position, metrics, offset),
                damage_regions,
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    terminal_text_runs(&damaged)
}

fn text_run_region(cell: &RenderCell, metrics: CellMetrics) -> RenderRect {
    let mut rect = cell_region(cell.position, metrics);
    let cells = cell.text.chars().count().max(1) as u32;
    let end = cell_axis_bounds(
        u32::from(cell.position.col).saturating_add(cells),
        metrics.cell_width,
    )
    .0;
    rect.width = end.saturating_sub(rect.x).max(1) as u32;
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
        OverlayKind::Badge | OverlayKind::ContentMask => RenderColor::rgb(245, 248, 252),
        OverlayKind::PerformanceOverlay
        | OverlayKind::WindowChrome
        | OverlayKind::DragTarget
        | OverlayKind::SessionStatus
        | OverlayKind::ImePreedit => RenderColor::rgb(225, 232, 240),
        OverlayKind::SecurityPrompt => RenderColor::rgb(248, 242, 224),
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

fn with_fixed_opacity(mut color: RenderColor, opacity: u16) -> RenderColor {
    color.alpha = ((u32::from(color.alpha) * u32::from(opacity)) / u32::from(u16::MAX)) as u8;
    color
}

fn push_window_chrome_control(
    batch: &mut QuadBatch,
    control: &WindowChromeControlVisual,
    opacity: u16,
) {
    if control.bounds.width == 0 || control.bounds.height == 0 {
        return;
    }

    let background = match (control.kind, control.hovered, control.pressed) {
        (WindowChromeControlKind::Close, _, true) => Some(RenderColor::rgb(176, 20, 30)),
        (WindowChromeControlKind::Close, true, false) => Some(RenderColor::rgb(220, 38, 48)),
        (_, _, true) => Some(RenderColor::rgb(62, 62, 62)),
        (_, true, false) => Some(RenderColor::rgb(46, 46, 46)),
        (_, false, false) => None,
    };
    if let Some(background) = background {
        push_solid_quad(
            batch,
            control.bounds,
            with_fixed_opacity(background, opacity),
        );
    }

    let symbol = with_fixed_opacity(RenderColor::rgb(232, 232, 232), opacity);
    let center_x = control.bounds.x + (control.bounds.width / 2) as i32;
    let center_y = control.bounds.y + (control.bounds.height / 2) as i32;
    match control.kind {
        WindowChromeControlKind::Minimize => push_solid_quad(
            batch,
            RenderRect {
                x: center_x - 5,
                y: center_y + 3,
                width: 10,
                height: 1,
            },
            symbol,
        ),
        WindowChromeControlKind::LeaveFullscreen => push_stroke_quads(
            batch,
            RenderRect {
                x: center_x - 5,
                y: center_y - 4,
                width: 10,
                height: 8,
            },
            1,
            symbol,
        ),
        WindowChromeControlKind::Close => {
            for offset in -4_i32..=4 {
                push_solid_quad(
                    batch,
                    RenderRect {
                        x: center_x + offset,
                        y: center_y + offset,
                        width: 1,
                        height: 1,
                    },
                    symbol,
                );
                push_solid_quad(
                    batch,
                    RenderRect {
                        x: center_x + offset,
                        y: center_y - offset,
                        width: 1,
                        height: 1,
                    },
                    symbol,
                );
            }
        }
    }
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

fn push_stroke_quads(batch: &mut QuadBatch, rect: RenderRect, width: u32, color: RenderColor) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    let width = width.max(1).min(rect.width).min(rect.height);
    push_solid_quad(
        batch,
        RenderRect {
            height: width,
            ..rect
        },
        color,
    );
    push_solid_quad(
        batch,
        RenderRect {
            y: rect.y + rect.height.saturating_sub(width) as i32,
            height: width,
            ..rect
        },
        color,
    );
    push_solid_quad(batch, RenderRect { width, ..rect }, color);
    push_solid_quad(
        batch,
        RenderRect {
            x: rect.x + rect.width.saturating_sub(width) as i32,
            width,
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
    image_cursor_active: bool,
) {
    for animation in animations {
        let affected_region = offset_region(animation.affected_region, offset);
        if !intersects_any(affected_region, damage_regions) {
            continue;
        }
        let start_region = offset_region(animation.start_region, offset);
        let end_region = offset_region(animation.end_region, offset);
        let progress = animation_progress(*animation);
        let color = animation_color(*animation);
        match animation.kind {
            AnimationKind::CursorTypingStretch => {
                push_rounded_quads(batch, stretch_region(end_region, progress), 2, color);
            }
            AnimationKind::CursorSmoothMovement => {
                if !image_cursor_active {
                    push_rounded_quads(
                        batch,
                        interpolate_region(start_region, end_region, ease_out_cubic(progress)),
                        2,
                        color,
                    );
                }
            }
            AnimationKind::CursorTrail => {
                let trail = interpolate_region(start_region, end_region, progress * 0.75);
                push_rounded_quads(batch, trail, 2, color);
            }
            AnimationKind::CursorTypingPulse => {
                let expansion = ((1.0 - progress) * 4.0).round() as i32;
                push_rounded_stroke_quads(batch, expand_region(end_region, expansion), 1, 3, color);
            }
            AnimationKind::CursorBlinkEasing => {
                push_rounded_quads(batch, end_region, 2, color);
            }
            AnimationKind::CursorGlow => {
                for expansion in [2, 4, 6] {
                    let mut layer = color;
                    layer.alpha /= expansion as u8;
                    push_rounded_quads(batch, expand_region(end_region, expansion), 4, layer);
                }
            }
            AnimationKind::CursorShadow => {
                let mut shadow = end_region;
                shadow.x = shadow.x.saturating_add(2);
                shadow.y = shadow.y.saturating_add(2);
                push_rounded_quads(batch, shadow, 2, color);
            }
            AnimationKind::OverlayTransition => {
                push_solid_quad(batch, affected_region, color);
            }
        }
    }
}

fn push_cursor_image_quad(batch: &mut QuadBatch, rect: RenderRect, frame_index: u16, opacity: u8) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let Ok(base) = u32::try_from(batch.vertices.len()) else {
        return;
    };
    let x0 = rect.x as f32;
    let y0 = rect.y as f32;
    let x1 = rect.x as f32 + rect.width as f32;
    let y1 = rect.y as f32 + rect.height as f32;
    let metadata = [f32::from(frame_index), f32::from(opacity) / 255.0, 0.0, 0.0];
    batch.vertices.extend([
        BatchVertex {
            position_px: [x0, y0],
            uv: [0.0, 0.0],
            color: metadata,
        },
        BatchVertex {
            position_px: [x1, y0],
            uv: [1.0, 0.0],
            color: metadata,
        },
        BatchVertex {
            position_px: [x1, y1],
            uv: [1.0, 1.0],
            color: metadata,
        },
        BatchVertex {
            position_px: [x0, y1],
            uv: [0.0, 1.0],
            color: metadata,
        },
    ]);
    batch
        .indices
        .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn animation_color(animation: AnimationHandle) -> RenderColor {
    let base_alpha: u8 = match animation.kind {
        AnimationKind::CursorSmoothMovement => 230,
        AnimationKind::CursorTypingPulse => 120,
        AnimationKind::CursorTypingStretch => 180,
        AnimationKind::CursorTrail => 80,
        AnimationKind::CursorBlinkEasing => 200,
        AnimationKind::CursorGlow => 96,
        AnimationKind::CursorShadow => 80,
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
    let mut color = animation.color;
    if animation.kind == AnimationKind::CursorShadow {
        color.red = 0;
        color.green = 0;
        color.blue = 0;
    }
    color.alpha = ((u16::from(color.alpha) * u16::from(alpha)) / 255) as u8;
    color
}

fn animation_progress(animation: AnimationHandle) -> f32 {
    let Some(remaining) = animation.remaining else {
        return 1.0;
    };
    let total = animation.elapsed.saturating_add(remaining).as_secs_f32();
    if total <= f32::EPSILON {
        1.0
    } else {
        (animation.elapsed.as_secs_f32() / total).clamp(0.0, 1.0)
    }
}

fn ease_out_cubic(progress: f32) -> f32 {
    1.0 - (1.0 - progress).powi(3)
}

fn interpolate_region(start: RenderRect, end: RenderRect, progress: f32) -> RenderRect {
    let lerp = |a: f32, b: f32| a + ((b - a) * progress.clamp(0.0, 1.0));
    RenderRect {
        x: lerp(start.x as f32, end.x as f32).round() as i32,
        y: lerp(start.y as f32, end.y as f32).round() as i32,
        width: lerp(start.width as f32, end.width as f32).round().max(1.0) as u32,
        height: lerp(start.height as f32, end.height as f32)
            .round()
            .max(1.0) as u32,
    }
}

fn stretch_region(rect: RenderRect, progress: f32) -> RenderRect {
    let expansion = ((1.0 - progress) * 4.0).round() as i32;
    RenderRect {
        x: rect.x - expansion,
        y: rect.y,
        width: rect
            .width
            .saturating_add(u32::try_from(expansion.max(0) * 2).unwrap_or(0)),
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

fn push_cursor_vector_quads(
    batch: &mut QuadBatch,
    vector: &CursorVectorVisual,
    damage_regions: &[DamageRegion],
    offset: render_core::RenderOffset,
) {
    let bounds = offset_region(vector.bounds, offset);
    if !intersects_any(bounds, damage_regions) {
        return;
    }
    for primitive in vector.asset.primitives.iter() {
        let scale = |value: u16, extent: u32| {
            (u64::from(value) * u64::from(extent) / u64::from(CURSOR_VECTOR_CANVAS_UNITS)) as u32
        };
        let rect = RenderRect {
            x: bounds.x.saturating_add(
                i32::try_from(scale(primitive.x, bounds.width)).unwrap_or(i32::MAX),
            ),
            y: bounds.y.saturating_add(
                i32::try_from(scale(primitive.y, bounds.height)).unwrap_or(i32::MAX),
            ),
            width: scale(primitive.width, bounds.width).max(1),
            height: scale(primitive.height, bounds.height).max(1),
        };
        let mut color = primitive.color.unwrap_or(vector.color);
        color.alpha = ((u16::from(color.alpha) * u16::from(vector.opacity)) / 255) as u8;
        let radius = scale(
            primitive.corner_radius,
            rect.width.min(rect.height).saturating_mul(2),
        );
        push_rounded_quads(batch, rect, radius, color);
    }
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
            .map(|overlay| (overlay, scene.content_offset))
            .chain(
                scene
                    .surface_overlays
                    .iter()
                    .map(|overlay| (overlay, render_core::RenderOffset::default())),
            )
            .collect::<Vec<_>>();
        overlays.sort_by_key(|(overlay, _)| overlay.z_index);

        for cell in &scene.grid.cells {
            draw_cell_background(&mut frame, cell, metrics, scene.content_offset);
        }

        for (overlay, offset) in &overlays {
            if !overlay_draws_behind_terminal_text(overlay.kind) {
                continue;
            }
            let bounds = offset_region(overlay.bounds, *offset);
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

        if scene.cursor_image.is_none()
            && let Some(cursor) = scene.cursor
        {
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

        for (overlay, offset) in overlays {
            if overlay_draws_behind_terminal_text(overlay.kind) {
                continue;
            }
            let bounds = offset_region(overlay.bounds, offset);
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
                metrics,
                offset,
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
        metrics: CellMetrics,
        offset: render_core::RenderOffset,
        _instrumentation: &mut RenderInstrumentation,
    ) -> Result<(), RendererError> {
        let rect = offset_region(text_run_region(cell, metrics), offset);

        let mut pen_x = rect.x as f32;
        let mut pen_y = rect.y as f32;
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
                (pen_x + glyph.x_offset).round() as i32 + bitmap.offset_x,
                (pen_y - glyph.y_offset).round() as i32 + bitmap.offset_y,
                bitmap.as_ref(),
                cell.foreground,
            );
            pen_x += glyph.x_advance;
            pen_y += glyph.y_advance;
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
        let rect = offset_region(overlay_label_rect(overlay, metrics), offset);
        let mut pen_x = rect.x as f32;
        let mut pen_y = rect.y as f32;
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
                (pen_x + glyph.x_offset).round() as i32 + bitmap.offset_x,
                (pen_y - glyph.y_offset).round() as i32 + bitmap.offset_y,
                bitmap.as_ref(),
                cell.foreground,
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
                metrics,
                render_core::RenderOffset::default(),
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
    damage_clear: GpuBatchBuffers,
    background: GpuBatchBuffers,
    glyphs: GpuBatchBuffers,
    overlay_glyphs: GpuBatchBuffers,
    decorations: GpuBatchBuffers,
    window_chrome: GpuBatchBuffers,
    selections: GpuBatchBuffers,
    cursor: GpuBatchBuffers,
    cursor_image: GpuBatchBuffers,
}

fn prepare_damage_clear_batch(damage_regions: &[DamageRegion], color: RenderColor) -> QuadBatch {
    let mut batch = QuadBatch::new(QuadBatchKind::Background);
    for region in damage_regions {
        push_solid_quad(&mut batch, *region, color);
    }
    batch
}

fn prepare_frame_clear_batch(
    load_previous: bool,
    damage_regions: &[DamageRegion],
    width: u32,
    height: u32,
    color: RenderColor,
) -> QuadBatch {
    if load_previous {
        return prepare_damage_clear_batch(damage_regions, color);
    }

    prepare_damage_clear_batch(
        &[DamageRegion {
            x: 0,
            y: 0,
            width,
            height,
        }],
        color,
    )
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
        blend_rect(
            frame,
            RenderRect {
                x: x0,
                y: y0,
                width: x1.saturating_sub(x0) as u32,
                height: y1.saturating_sub(y0) as u32,
            },
            RenderColor {
                red: (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                green: (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                blue: (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
                alpha: (color[3].clamp(0.0, 1.0) * 255.0).round() as u8,
            },
        );
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
    clear_pipeline: wgpu::RenderPipeline,
    quad_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,
    glyph_bind_group_layout: wgpu::BindGroupLayout,
    glyph_sampler: wgpu::Sampler,
    glyph_atlas_texture: Option<wgpu::Texture>,
    glyph_atlas_size: Option<(u32, u32)>,
    glyph_bind_group: Option<wgpu::BindGroup>,
    cursor_image_resources: Option<CursorImageGpuResources>,
    cursor_image_texture: Option<wgpu::Texture>,
    cursor_image_asset_id: Option<u64>,
    cursor_image_bind_group: Option<wgpu::BindGroup>,
    retained_frame: RetainedFrameState,
    surface_copy_supported: bool,
    batches: PersistentBatchBuffers,
    device_loss_signal: Arc<Mutex<Option<DeviceLossSignal>>>,
    gpu_timing: GpuTiming,
    transparent: bool,
    background: RenderColor,
}

#[derive(Default)]
struct RetainedFrameState {
    texture: Option<wgpu::Texture>,
    size: Option<(u32, u32)>,
    initialized: bool,
}

impl RetainedFrameState {
    fn invalidate(&mut self) {
        self.texture = None;
        self.size = None;
        self.initialized = false;
    }
}

struct CursorImageGpuResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
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

    pub fn set_background(&mut self, background: RenderColor) {
        if self.options.background == background {
            return;
        }
        self.options.background = background;
        if let Some(backend) = self.backend.as_mut() {
            backend.background = background;
        }
        self.requires_full_redraw = true;
    }

    /// Presents the configured background before the native window becomes
    /// visible, avoiding an OS-default flash while GPU resources initialize.
    pub fn present_startup_background(&mut self) -> Result<(), RendererError> {
        let outcome = self
            .backend
            .as_mut()
            .ok_or_else(|| {
                RendererError::DeviceUnavailable(
                    "renderer backend is unavailable until GPU recovery succeeds".to_owned(),
                )
            })?
            .present_background()?;
        match outcome {
            PresentOutcome::Submitted => Ok(()),
            PresentOutcome::SurfaceReconfigured(reason) => {
                self.record_surface_recovery(reason);
                let retry = self
                    .backend
                    .as_mut()
                    .ok_or_else(|| {
                        RendererError::DeviceUnavailable(
                            "renderer backend became unavailable during startup".to_owned(),
                        )
                    })?
                    .present_background()?;
                match retry {
                    PresentOutcome::Submitted => Ok(()),
                    PresentOutcome::SurfaceReconfigured(_) => Err(RendererError::Surface(
                        "startup surface remained unavailable after reconfiguration".to_owned(),
                    )),
                    PresentOutcome::Timeout => Err(RendererError::Surface(
                        "startup surface presentation timed out".to_owned(),
                    )),
                }
            }
            PresentOutcome::Timeout => Err(RendererError::Surface(
                "startup surface presentation timed out".to_owned(),
            )),
        }
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

    #[must_use]
    pub fn damage_tracking_active(&self) -> bool {
        self.retained_damage_status() == RetainedDamageStatus::Enabled
    }

    #[must_use]
    pub fn retained_damage_status(&self) -> RetainedDamageStatus {
        if !self.options.damage_tracking {
            return RetainedDamageStatus::DisabledByConfig;
        }
        self.backend.as_ref().map_or_else(
            || RetainedDamageStatus::Unverified {
                reason: "the GPU backend is unavailable while renderer recovery is pending"
                    .to_owned(),
            },
            |backend| {
                retained_damage_status(
                    self.options.damage_tracking,
                    backend.supports_retained_damage(),
                )
            },
        )
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
        let retained_damage_enabled =
            self.options.damage_tracking && backend.supports_retained_damage();
        let prepare_full_frame = should_prepare_full_frame(
            self.requires_full_redraw,
            self.options.damage_tracking,
            backend.supports_retained_damage(),
        );
        if prepare_full_frame {
            backend.retained_frame.initialized = false;
        }
        let mut batches = if prepare_full_frame {
            self.rasterizer.prepare_full_batches(scene, fonts)?
        } else {
            self.rasterizer.prepare_batches(scene, fonts)?
        };
        batches.instrumentation.gpu_time = backend.gpu_timing.last_duration();
        batches.instrumentation.gpu_timing_status = backend.gpu_timing.timing_status();
        let gpu_started = Instant::now();
        backend.upload_atlas(&self.rasterizer, &batches);
        if let Some(asset) = batches.cursor_image_asset.as_deref() {
            backend.upload_cursor_image(asset);
        }
        let load_retained_frame =
            should_load_retained_frame(retained_damage_enabled, prepare_full_frame);
        batches.instrumentation.draw_call_count = batches
            .instrumentation
            .draw_call_count
            .saturating_add(frame_clear_extra_draw_calls(
                load_retained_frame,
                batches.damage_regions.len(),
            ));
        let result =
            backend.present_batches(&batches, retained_damage_enabled, load_retained_frame);
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
                self.retry_present_after_surface_reconfigure(
                    &batches,
                    retained_damage_enabled,
                    load_retained_frame,
                )
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
        retained_damage_enabled: bool,
        load_retained_frame: bool,
    ) -> Result<(), RendererError> {
        let Some(backend) = self.backend.as_mut() else {
            return Err(RendererError::DeviceUnavailable(
                "renderer backend disappeared during surface recovery".to_owned(),
            ));
        };

        match backend.present_batches(batches, retained_damage_enabled, load_retained_frame) {
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

fn should_prepare_full_frame(
    requires_full_redraw: bool,
    damage_tracking_enabled: bool,
    retained_damage_supported: bool,
) -> bool {
    requires_full_redraw || !damage_tracking_enabled || !retained_damage_supported
}

fn should_load_retained_frame(retained_damage_enabled: bool, prepare_full_frame: bool) -> bool {
    retained_damage_enabled && !prepare_full_frame
}

fn frame_clear_extra_draw_calls(load_previous: bool, damage_region_count: usize) -> u32 {
    u32::from(!load_previous || damage_region_count > 0)
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
            if format.is_srgb() {
                "fs_color_srgb_target"
            } else {
                "fs_color_unorm_target"
            },
        );
        let clear_pipeline = create_replacement_pipeline(
            &device,
            &quad_pipeline_layout,
            &batch_shader,
            format,
            "panea-damage-clear-pipeline",
            if format.is_srgb() {
                "fs_color_srgb_target"
            } else {
                "fs_color_unorm_target"
            },
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
            if format.is_srgb() {
                "fs_glyph_srgb_target"
            } else {
                "fs_glyph_unorm_target"
            },
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
            clear_pipeline,
            quad_pipeline,
            glyph_pipeline,
            glyph_bind_group_layout,
            glyph_sampler,
            glyph_atlas_texture: None,
            glyph_atlas_size: None,
            glyph_bind_group: None,
            cursor_image_resources: None,
            cursor_image_texture: None,
            cursor_image_asset_id: None,
            cursor_image_bind_group: None,
            retained_frame: RetainedFrameState::default(),
            surface_copy_supported,
            batches: PersistentBatchBuffers::default(),
            device_loss_signal,
            gpu_timing,
            transparent: options.transparent && alpha_mode != wgpu::CompositeAlphaMode::Opaque,
            background: options.background,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }

        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.retained_frame.invalidate();
    }

    fn supports_retained_damage(&self) -> bool {
        self.surface_copy_supported
    }

    fn ensure_retained_frame(&mut self) {
        if !self.surface_copy_supported
            || self.retained_frame.size == Some((self.config.width, self.config.height))
        {
            return;
        }
        self.retained_frame.texture = Some(self.device.create_texture(&wgpu::TextureDescriptor {
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
        self.retained_frame.size = Some((self.config.width, self.config.height));
        self.retained_frame.initialized = false;
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

    fn upload_cursor_image(&mut self, asset: &CursorImageAsset) {
        if self.cursor_image_asset_id == Some(asset.id) {
            return;
        }
        let Ok(layer_count) = u32::try_from(asset.frames.len()) else {
            return;
        };
        if asset.width == 0 || asset.height == 0 || layer_count == 0 {
            return;
        }
        self.ensure_cursor_image_resources();
        let Some(resources) = self.cursor_image_resources.as_ref() else {
            return;
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("panea-cursor-image-array"),
            size: wgpu::Extent3d {
                width: asset.width,
                height: asset.height,
                depth_or_array_layers: layer_count,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let unpadded_row_bytes = asset.width.saturating_mul(4);
        let padded_row_bytes = unpadded_row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        for (layer, frame) in asset.frames.iter().enumerate() {
            let expected = usize::try_from(unpadded_row_bytes.saturating_mul(asset.height))
                .unwrap_or(usize::MAX);
            if frame.pixels.len() != expected {
                return;
            }
            let mut upload = vec![
                0;
                usize::try_from(padded_row_bytes.saturating_mul(asset.height))
                    .unwrap_or(0)
            ];
            for row in 0..asset.height {
                let source_start =
                    usize::try_from(row.saturating_mul(unpadded_row_bytes)).unwrap_or(usize::MAX);
                let target_start =
                    usize::try_from(row.saturating_mul(padded_row_bytes)).unwrap_or(usize::MAX);
                let row_len = usize::try_from(unpadded_row_bytes).unwrap_or(0);
                let (Some(source), Some(target)) = (
                    frame
                        .pixels
                        .get(source_start..source_start.saturating_add(row_len)),
                    upload.get_mut(target_start..target_start.saturating_add(row_len)),
                ) else {
                    return;
                };
                target.copy_from_slice(source);
            }
            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: u32::try_from(layer).unwrap_or(u32::MAX),
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &upload,
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(asset.height),
                },
                wgpu::Extent3d {
                    width: asset.width,
                    height: asset.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("panea-cursor-image-array-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            base_array_layer: 0,
            array_layer_count: Some(layer_count),
            ..wgpu::TextureViewDescriptor::default()
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("panea-cursor-image-bind-group"),
            layout: &resources.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&resources.sampler),
                },
            ],
        });
        self.cursor_image_texture = Some(texture);
        self.cursor_image_asset_id = Some(asset.id);
        self.cursor_image_bind_group = Some(bind_group);
    }

    fn ensure_cursor_image_resources(&mut self) {
        if self.cursor_image_resources.is_some() {
            return;
        }

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("panea-cursor-image-shader"),
                source: wgpu::ShaderSource::Wgsl(CURSOR_IMAGE_SHADER.into()),
            });
        let bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("panea-cursor-image-bind-group-layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2Array,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("panea-cursor-image-pipeline-layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });
        let pipeline = create_batch_pipeline(
            &self.device,
            &pipeline_layout,
            &shader,
            self.config.format,
            "panea-cursor-image-pipeline",
            if self.config.format.is_srgb() {
                "fs_cursor_image_srgb_target"
            } else {
                "fs_cursor_image_unorm_target"
            },
        );
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("panea-cursor-image-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..wgpu::SamplerDescriptor::default()
        });
        self.cursor_image_resources = Some(CursorImageGpuResources {
            pipeline,
            bind_group_layout,
            sampler,
        });
    }

    fn present_batches(
        &mut self,
        batches: &PreparedRenderBatches,
        retained_damage_enabled: bool,
        load_retained_frame: bool,
    ) -> Result<PresentOutcome, RendererError> {
        if retained_damage_enabled {
            self.ensure_retained_frame();
        } else {
            self.retained_frame.invalidate();
        }
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
        let load_previous = load_retained_frame
            && self.retained_frame.texture.is_some()
            && self.retained_frame.initialized;
        let damage_clear = prepare_frame_clear_batch(
            load_previous,
            &batches.damage_regions,
            self.config.width,
            self.config.height,
            surface_background_color(self.transparent, self.background),
        );
        self.batches.damage_clear.upload(
            &upload_context,
            "damage-clear",
            &damage_clear.vertices,
            &damage_clear.indices,
        );
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
        self.batches.window_chrome.upload(
            &upload_context,
            "window-chrome",
            &batches.window_chrome.vertices,
            &batches.window_chrome.indices,
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
        self.batches.cursor_image.upload(
            &upload_context,
            "cursor-image",
            &batches.cursor_image.vertices,
            &batches.cursor_image.indices,
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
            .texture
            .as_ref()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let target_view = retained_view.as_ref().unwrap_or(&view);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("panea-batch-encoder"),
            });
        let timestamp_written = self.gpu_timing.can_write_this_frame();
        let timestamp_writes = self.gpu_timing.render_pass_writes();
        encode_retained_frame(
            &mut encoder,
            target_view,
            load_previous,
            surface_clear_color(
                surface_background_color(self.transparent, self.background),
                self.config.format,
            ),
            GpuFrameDraw {
                clear_pipeline: &self.clear_pipeline,
                quad_pipeline: &self.quad_pipeline,
                glyph_pipeline: &self.glyph_pipeline,
                glyph_bind_group: self.glyph_bind_group.as_ref(),
                cursor_image_pipeline: self
                    .cursor_image_resources
                    .as_ref()
                    .map(|resources| &resources.pipeline),
                cursor_image_bind_group: self.cursor_image_bind_group.as_ref(),
                batches: &self.batches,
                cursor_image_active: self.cursor_image_asset_id
                    == batches.cursor_image_asset.as_ref().map(|asset| asset.id),
            },
            timestamp_writes,
        );

        if let Some(retained_frame) = self.retained_frame.texture.as_ref() {
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
            self.retained_frame.initialized = true;
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

    fn present_background(&mut self) -> Result<PresentOutcome, RendererError> {
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
                    message: "surface reported out-of-memory while presenting startup background"
                        .to_owned(),
                });
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("panea-startup-background-encoder"),
            });
        let background = surface_background_color(self.transparent, self.background);
        let clear = prepare_frame_clear_batch(
            false,
            &[],
            self.config.width,
            self.config.height,
            background,
        );
        self.batches.damage_clear.upload(
            &GpuUploadContext {
                device: &self.device,
                queue: &self.queue,
                width: self.config.width,
                height: self.config.height,
            },
            "startup-background",
            &clear.vertices,
            &clear.indices,
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("panea-startup-background-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(surface_clear_color(
                            background,
                            self.config.format,
                        )),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.clear_pipeline);
            draw_buffers(&mut pass, &self.batches.damage_clear);
        }
        self.queue.submit(Some(encoder.finish()));
        output.present();
        Ok(PresentOutcome::Submitted)
    }
}

fn surface_background_color(_transparent: bool, background: RenderColor) -> RenderColor {
    // Transparency selects the compositor alpha mode; it must not create
    // alpha-zero holes outside the terminal cell grid.
    background
}

fn surface_clear_color(color: RenderColor, format: wgpu::TextureFormat) -> wgpu::Color {
    let convert = |channel: u8| {
        let encoded = f64::from(channel) / 255.0;
        if format.is_srgb() {
            if encoded <= 0.04045 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            }
        } else {
            encoded
        }
    };
    wgpu::Color {
        r: convert(color.red),
        g: convert(color.green),
        b: convert(color.blue),
        a: f64::from(color.alpha) / 255.0,
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

struct GpuFrameDraw<'a> {
    clear_pipeline: &'a wgpu::RenderPipeline,
    quad_pipeline: &'a wgpu::RenderPipeline,
    glyph_pipeline: &'a wgpu::RenderPipeline,
    glyph_bind_group: Option<&'a wgpu::BindGroup>,
    cursor_image_pipeline: Option<&'a wgpu::RenderPipeline>,
    cursor_image_bind_group: Option<&'a wgpu::BindGroup>,
    batches: &'a PersistentBatchBuffers,
    cursor_image_active: bool,
}

fn encode_retained_frame<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    retained: &'a wgpu::TextureView,
    load_previous: bool,
    clear_color: wgpu::Color,
    draw: GpuFrameDraw<'a>,
    timestamp_writes: Option<wgpu::RenderPassTimestampWrites<'a>>,
) -> bool {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("panea-batch-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: retained,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if load_previous {
                    wgpu::LoadOp::Load
                } else {
                    wgpu::LoadOp::Clear(clear_color)
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        occlusion_query_set: None,
        timestamp_writes,
    });

    pass.set_pipeline(draw.clear_pipeline);
    draw_buffers(&mut pass, &draw.batches.damage_clear);

    // Cell backgrounds replace the surface clear so their configured alpha is
    // not compounded with the translucent window background beneath them.
    pass.set_pipeline(draw.clear_pipeline);
    draw_buffers(&mut pass, &draw.batches.background);

    pass.set_pipeline(draw.quad_pipeline);
    draw_buffers(&mut pass, &draw.batches.selections);
    draw_buffers(&mut pass, &draw.batches.cursor);

    if let Some(glyph_bind_group) = draw.glyph_bind_group {
        pass.set_pipeline(draw.glyph_pipeline);
        pass.set_bind_group(0, glyph_bind_group, &[]);
        draw_buffers(&mut pass, &draw.batches.glyphs);
    }

    pass.set_pipeline(draw.quad_pipeline);
    draw_buffers(&mut pass, &draw.batches.decorations);
    draw_buffers(&mut pass, &draw.batches.window_chrome);
    if let Some(glyph_bind_group) = draw.glyph_bind_group {
        pass.set_pipeline(draw.glyph_pipeline);
        pass.set_bind_group(0, glyph_bind_group, &[]);
        draw_buffers(&mut pass, &draw.batches.overlay_glyphs);
    }
    if draw.cursor_image_active
        && let Some(cursor_image_pipeline) = draw.cursor_image_pipeline
        && let Some(cursor_image_bind_group) = draw.cursor_image_bind_group
    {
        pass.set_pipeline(cursor_image_pipeline);
        pass.set_bind_group(0, cursor_image_bind_group, &[]);
        draw_buffers(&mut pass, &draw.batches.cursor_image);
    }

    load_previous
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
    create_batch_pipeline_with_blend(
        device,
        layout,
        shader,
        format,
        label,
        fragment_entry,
        Some(wgpu::BlendState::ALPHA_BLENDING),
    )
}

fn create_replacement_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
    fragment_entry: &'static str,
) -> wgpu::RenderPipeline {
    create_batch_pipeline_with_blend(device, layout, shader, format, label, fragment_entry, None)
}

fn create_batch_pipeline_with_blend(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    label: &'static str,
    fragment_entry: &'static str,
    blend: Option<wgpu::BlendState>,
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
                blend,
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

fn srgb_to_linear(color: vec3<f32>) -> vec3<f32> {
    let low = color / 12.92;
    let high = pow((color + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, color <= vec3<f32>(0.04045));
}

fn linear_to_srgb(color: vec3<f32>) -> vec3<f32> {
    let low = color * 12.92;
    let high = 1.055 * pow(color, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(high, low, color <= vec3<f32>(0.0031308));
}

@fragment
fn fs_color_srgb_target(in: VertexOut) -> @location(0) vec4<f32> {
    return vec4<f32>(srgb_to_linear(in.color.rgb), in.color.a);
}

@fragment
fn fs_color_unorm_target(in: VertexOut) -> @location(0) vec4<f32> {
    return in.color;
}

@group(0) @binding(0) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(1) var glyph_sampler: sampler;

@fragment
fn fs_glyph_srgb_target(in: VertexOut) -> @location(0) vec4<f32> {
    let sample = textureSample(glyph_atlas, glyph_sampler, in.uv);
    if in.color.a < 0.0 {
        return vec4<f32>(sample.rgb, sample.a * -in.color.a);
    }
    return vec4<f32>(srgb_to_linear(in.color.rgb), in.color.a * sample.a);
}

@fragment
fn fs_glyph_unorm_target(in: VertexOut) -> @location(0) vec4<f32> {
    let sample = textureSample(glyph_atlas, glyph_sampler, in.uv);
    if in.color.a < 0.0 {
        return vec4<f32>(linear_to_srgb(sample.rgb), sample.a * -in.color.a);
    }
    return vec4<f32>(in.color.rgb, in.color.a * sample.a);
}

"#;

const CURSOR_IMAGE_SHADER: &str = r#"
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

fn linear_to_srgb(color: vec3<f32>) -> vec3<f32> {
    let low = color * 12.92;
    let high = 1.055 * pow(color, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(high, low, color <= vec3<f32>(0.0031308));
}

@group(0) @binding(2) var cursor_images: texture_2d_array<f32>;
@group(0) @binding(3) var cursor_image_sampler: sampler;

@fragment
fn fs_cursor_image_srgb_target(in: VertexOut) -> @location(0) vec4<f32> {
    let frame = i32(round(in.color.r));
    let sample = textureSample(cursor_images, cursor_image_sampler, in.uv, frame);
    return vec4<f32>(sample.rgb, sample.a * in.color.g);
}

@fragment
fn fs_cursor_image_unorm_target(in: VertexOut) -> @location(0) vec4<f32> {
    let frame = i32(round(in.color.r));
    let sample = textureSample(cursor_images, cursor_image_sampler, in.uv, frame);
    return vec4<f32>(linear_to_srgb(sample.rgb), sample.a * in.color.g);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    const RED: RenderColor = RenderColor::rgb(255, 0, 0);
    const GREEN: RenderColor = RenderColor::rgb(0, 255, 0);
    const BLUE: RenderColor = RenderColor::rgb(0, 0, 255);
    const WHITE: RenderColor = RenderColor::rgb(255, 255, 255);
    const YELLOW: RenderColor = RenderColor::rgb(255, 255, 0);

    struct TestFrame {
        clears: Vec<(RenderRect, RenderColor)>,
        quads: Vec<(RenderRect, RenderColor)>,
    }

    impl TestFrame {
        fn quadrants(colors: [RenderColor; 4]) -> Self {
            Self {
                clears: Vec::new(),
                quads: vec![
                    (
                        RenderRect {
                            x: 0,
                            y: 0,
                            width: 8,
                            height: 8,
                        },
                        colors[0],
                    ),
                    (
                        RenderRect {
                            x: 8,
                            y: 0,
                            width: 8,
                            height: 8,
                        },
                        colors[1],
                    ),
                    (
                        RenderRect {
                            x: 0,
                            y: 8,
                            width: 8,
                            height: 8,
                        },
                        colors[2],
                    ),
                    (
                        RenderRect {
                            x: 8,
                            y: 8,
                            width: 8,
                            height: 8,
                        },
                        colors[3],
                    ),
                ],
            }
        }

        fn damage(bounds: RenderRect, color: RenderColor) -> Self {
            Self {
                clears: vec![(bounds, color)],
                quads: Vec::new(),
            }
        }
    }

    struct TestPixels {
        width: u32,
        pixels: Vec<u8>,
    }

    impl TestPixels {
        fn at(&self, x: u32, y: u32) -> [u8; 4] {
            let index = usize::try_from((y * self.width + x) * 4).expect("pixel index");
            self.pixels[index..index + 4]
                .try_into()
                .expect("RGBA pixel")
        }
    }

    fn render_retained_sequence(
        first: TestFrame,
        second: TestFrame,
        clear_color: wgpu::Color,
    ) -> Result<Option<TestPixels>, String> {
        const WIDTH: u32 = 16;
        const HEIGHT: u32 = 16;

        let instance = wgpu::Instance::default();
        let Some(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))
        else {
            eprintln!("retained-frame test skipped: no WGPU adapter is available");
            return Ok(None);
        };
        let adapter_info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("panea-retained-frame-test-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        ))
        .map_err(|error| {
            format!(
                "failed to create retained-frame test device for {} ({:?}): {error}",
                adapter_info.name, adapter_info.backend
            )
        })?;
        eprintln!(
            "retained-frame test adapter={} backend={:?}",
            adapter_info.name, adapter_info.backend
        );

        let format = wgpu::TextureFormat::Rgba8Unorm;
        let retained = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("panea-retained-frame-test-target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let retained_view = retained.create_view(&wgpu::TextureViewDescriptor::default());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("panea-retained-frame-test-shader"),
            source: wgpu::ShaderSource::Wgsl(BATCH_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("panea-retained-frame-test-layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });
        let pipeline = create_batch_pipeline(
            &device,
            &layout,
            &shader,
            format,
            "panea-retained-frame-test-pipeline",
            "fs_color_unorm_target",
        );
        let clear_pipeline = create_replacement_pipeline(
            &device,
            &layout,
            &shader,
            format,
            "panea-retained-frame-test-clear-pipeline",
            "fs_color_unorm_target",
        );
        let upload_context = GpuUploadContext {
            device: &device,
            queue: &queue,
            width: WIDTH,
            height: HEIGHT,
        };
        let mut gpu_batches = PersistentBatchBuffers::default();

        let encode_test_frame = |frame: TestFrame,
                                 load_previous: bool,
                                 gpu_batches: &mut PersistentBatchBuffers|
         -> wgpu::CommandBuffer {
            let mut batch = QuadBatch::new(QuadBatchKind::Background);
            for (bounds, color) in frame.quads {
                push_solid_quad(&mut batch, bounds, color);
            }
            let mut clears = QuadBatch::new(QuadBatchKind::Background);
            for (bounds, color) in frame.clears {
                push_solid_quad(&mut clears, bounds, color);
            }
            gpu_batches.damage_clear.upload(
                &upload_context,
                "retained-frame-test-clear",
                &clears.vertices,
                &clears.indices,
            );
            gpu_batches.background.upload(
                &upload_context,
                "retained-frame-test-background",
                &batch.vertices,
                &batch.indices,
            );
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("panea-retained-frame-test-encoder"),
            });
            encode_retained_frame(
                &mut encoder,
                &retained_view,
                load_previous,
                clear_color,
                GpuFrameDraw {
                    clear_pipeline: &clear_pipeline,
                    quad_pipeline: &pipeline,
                    glyph_pipeline: &pipeline,
                    glyph_bind_group: None,
                    cursor_image_pipeline: None,
                    cursor_image_bind_group: None,
                    batches: gpu_batches,
                    cursor_image_active: false,
                },
                None,
            );
            encoder.finish()
        };

        queue.submit(Some(encode_test_frame(first, false, &mut gpu_batches)));
        queue.submit(Some(encode_test_frame(second, true, &mut gpu_batches)));

        let unpadded_row_bytes = WIDTH * 4;
        let padded_row_bytes = unpadded_row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("panea-retained-frame-test-readback"),
            size: u64::from(padded_row_bytes * HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("panea-retained-frame-test-readback-encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &retained,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &readback,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let (sender, receiver) = mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result.map_err(|error| error.to_string()));
            });
        device.poll(wgpu::Maintain::Wait);
        receiver.recv().map_err(|error| error.to_string())??;

        let mapped = readback.slice(..).get_mapped_range();
        let mut pixels = Vec::with_capacity(usize::try_from(WIDTH * HEIGHT * 4).unwrap_or(0));
        for row in mapped.chunks_exact(usize::try_from(padded_row_bytes).unwrap_or(0)) {
            pixels.extend_from_slice(&row[..usize::try_from(unpadded_row_bytes).unwrap_or(0)]);
        }
        drop(mapped);
        readback.unmap();

        Ok(Some(TestPixels {
            width: WIDTH,
            pixels,
        }))
    }

    #[test]
    fn retained_frame_preserves_unchanged_pixels_and_replaces_damage() {
        let first = TestFrame::quadrants([RED, GREEN, BLUE, WHITE]);
        let second = TestFrame::damage(
            RenderRect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
            YELLOW,
        );
        let Some(pixels) = render_retained_sequence(first, second, wgpu::Color::TRANSPARENT)
            .expect("GPU sequence")
        else {
            return;
        };

        assert_eq!(pixels.at(2, 2), [255, 255, 0, 255]);
        assert_eq!(pixels.at(12, 2), [0, 255, 0, 255]);
        assert_eq!(pixels.at(2, 12), [0, 0, 255, 255]);
        assert_eq!(pixels.at(12, 12), [255, 255, 255, 255]);
    }

    #[test]
    fn translucent_background_replaces_clear_without_alpha_compounding() {
        let background = RenderColor {
            red: 30,
            green: 30,
            blue: 46,
            alpha: 235,
        };
        let first = TestFrame {
            clears: Vec::new(),
            quads: vec![(
                RenderRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 8,
                },
                background,
            )],
        };
        let second = TestFrame {
            clears: Vec::new(),
            quads: Vec::new(),
        };
        let Some(pixels) = render_retained_sequence(
            first,
            second,
            surface_clear_color(background, wgpu::TextureFormat::Rgba8Unorm),
        )
        .expect("GPU sequence") else {
            return;
        };

        assert_eq!(pixels.at(2, 2), [30, 30, 46, 235]);
        assert_eq!(pixels.at(12, 12), [30, 30, 46, 235]);
    }

    #[test]
    fn retained_damage_status_is_explicit() {
        assert_eq!(
            retained_damage_status(false, true),
            RetainedDamageStatus::DisabledByConfig
        );
        assert!(matches!(
            retained_damage_status(true, false),
            RetainedDamageStatus::Unsupported { .. }
        ));
        assert_eq!(
            retained_damage_status(true, true),
            RetainedDamageStatus::Enabled
        );
        assert!(
            retained_damage_status(true, false)
                .to_string()
                .contains("cannot receive")
        );
    }

    #[test]
    fn retained_frame_invalidation_forces_fresh_full_frame() {
        let mut retained = RetainedFrameState {
            texture: None,
            size: Some((80, 24)),
            initialized: true,
        };

        retained.invalidate();

        assert_eq!(retained.size, None);
        assert!(!retained.initialized);
        assert!(!should_load_retained_frame(true, true));
    }

    #[test]
    fn retained_damage_clear_covers_removed_content_regions() {
        let regions = vec![
            RenderRect {
                x: 8,
                y: 16,
                width: 8,
                height: 16,
            },
            RenderRect {
                x: 32,
                y: 48,
                width: 24,
                height: 16,
            },
        ];

        let batch = prepare_damage_clear_batch(&regions, RenderColor::rgb(12, 12, 12));

        assert_eq!(batch.quad_count(), 2);
        assert_eq!(batch.vertices[0].position_px, [8.0, 16.0]);
        assert_eq!(batch.vertices[4].position_px, [32.0, 48.0]);
    }

    #[test]
    fn full_frame_clear_covers_the_entire_surface() {
        let background = RenderColor {
            red: 30,
            green: 30,
            blue: 46,
            alpha: 235,
        };

        let batch = prepare_frame_clear_batch(false, &[], 1920, 1080, background);

        assert_eq!(batch.quad_count(), 1);
        assert_eq!(batch.vertices[0].position_px, [0.0, 0.0]);
        assert_eq!(batch.vertices[2].position_px, [1920.0, 1080.0]);
        assert_eq!(batch.vertices[0].color[3], 235.0 / 255.0);
    }

    #[test]
    fn frame_clear_draw_call_is_instrumented_only_when_used() {
        assert_eq!(frame_clear_extra_draw_calls(false, 0), 1);
        assert_eq!(frame_clear_extra_draw_calls(true, 0), 0);
        assert_eq!(frame_clear_extra_draw_calls(true, 2), 1);
    }

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

    #[test]
    fn cursor_image_shader_and_array_bindings_validate_on_available_adapter() {
        let instance = wgpu::Instance::default();
        let Some(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))
        else {
            return;
        };
        let Ok((device, _queue)) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("panea-cursor-image-test-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
            },
            None,
        )) else {
            return;
        };
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("panea-cursor-image-test-shader"),
            source: wgpu::ShaderSource::Wgsl(CURSOR_IMAGE_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("panea-cursor-image-test-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("panea-cursor-image-test-pipeline-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let _pipeline = create_batch_pipeline(
            &device,
            &layout,
            &shader,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            "panea-cursor-image-test-pipeline",
            "fs_cursor_image_srgb_target",
        );
        device.poll(wgpu::Maintain::Wait);
        let error = pollster::block_on(device.pop_error_scope());
        assert!(
            error.is_none(),
            "cursor image pipeline validation failed: {error:?}"
        );
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

    #[test]
    fn full_frame_fallback_honors_config_and_surface_capability() {
        assert!(!RendererOptions::default().damage_tracking);
        assert!(should_prepare_full_frame(true, true, true));
        assert!(should_prepare_full_frame(false, false, true));
        assert!(should_prepare_full_frame(false, true, false));
        assert!(!should_prepare_full_frame(false, true, true));
    }

    #[test]
    fn surface_clear_color_matches_scene_color_space() {
        let color = RenderColor {
            red: 12,
            green: 64,
            blue: 255,
            alpha: 128,
        };
        let unorm = surface_clear_color(color, wgpu::TextureFormat::Bgra8Unorm);
        let srgb = surface_clear_color(color, wgpu::TextureFormat::Bgra8UnormSrgb);

        assert!((unorm.r - 12.0 / 255.0).abs() < f64::EPSILON);
        assert!(srgb.r < unorm.r);
        assert!(srgb.g < unorm.g);
        assert!((srgb.b - 1.0).abs() < f64::EPSILON);
        assert!((srgb.a - 128.0 / 255.0).abs() < f64::EPSILON);
    }

    #[test]
    fn transparent_surface_clear_uses_configured_background_alpha() {
        let background = RenderColor {
            red: 30,
            green: 30,
            blue: 46,
            alpha: 235,
        };

        assert_eq!(surface_background_color(true, background), background);
        assert_eq!(surface_background_color(false, background), background);
    }

    #[test]
    fn full_frame_rendering_never_loads_stale_retained_pixels() {
        for (damage_tracking_enabled, retained_damage_supported) in
            [(false, false), (false, true), (true, false)]
        {
            let full_frame = should_prepare_full_frame(
                false,
                damage_tracking_enabled,
                retained_damage_supported,
            );
            let retained_damage_enabled = damage_tracking_enabled && retained_damage_supported;
            assert!(full_frame);
            assert!(!should_load_retained_frame(
                retained_damage_enabled,
                full_frame
            ));
        }

        let retained_damage_enabled = true;
        let full_frame = should_prepare_full_frame(false, true, true);
        assert!(!full_frame);
        assert!(should_load_retained_frame(
            retained_damage_enabled,
            full_frame
        ));
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
    fn content_damage_includes_local_ligature_context() {
        let mut tracker = DamageTracker::new();
        let first = scene_without_cursor(vec![
            cell(0, 0, "a"),
            cell(0, 1, "b"),
            cell(0, 2, "c"),
            cell(0, 3, "d"),
        ]);
        let _ = tracker.update(&first, metrics());
        let second = scene_without_cursor(vec![
            cell(0, 0, "a"),
            cell(0, 1, "b"),
            cell(0, 2, "x"),
            cell(0, 3, "d"),
        ]);

        let damage = tracker.update(&second, metrics());

        for col in 0..4 {
            assert!(
                damage.iter().any(|region| region.x == col * 8),
                "column {col} should be repainted for shaping context"
            );
        }
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
            border_width_px: 0,
            corner_radius_px: 0,
            z_index: 0,
            label: None,
            label_color: None,
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
                border_width_px: 1,
                corner_radius_px: 4,
                z_index: 10,
                label: None,
                label_color: None,
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
                border_width_px: 0,
                corner_radius_px: 3,
                z_index: 30,
                label: Some("ok".to_owned()),
                label_color: None,
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
    fn collapsed_content_masks_render_above_terminal_glyphs() {
        assert!(!overlay_draws_behind_terminal_text(
            OverlayKind::ContentMask
        ));

        let mut batch = QuadBatch::new(QuadBatchKind::Decoration);
        push_rounded_stroke_quads(
            &mut batch,
            RenderRect {
                x: 0,
                y: 0,
                width: 80,
                height: 32,
            },
            3,
            6,
            RenderColor::rgb(255, 255, 255),
        );
        assert!(batch.quad_count() > 4);
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
    fn shaped_terminal_run_stays_aligned_with_cursor_cell() {
        const TEXT: &str = "panea-grid-cursor-check";
        let mut fonts = FontSystem::new_with_scale_factor(font_system::FontConfig::default(), 1.25);
        let metrics = fonts.cell_metrics().expect("cell metrics");
        let cells = TEXT
            .chars()
            .enumerate()
            .map(|(col, ch)| cell(0, col as u16, &ch.to_string()))
            .collect::<Vec<_>>();
        let mut test_scene = scene(cells);
        test_scene.cursor = Some(CursorVisual {
            position: CellPosition {
                row: 0,
                col: TEXT.len() as u16,
            },
            shape: RenderCursorShape::Beam,
            color: RenderColor::rgb(255, 255, 255),
            visible: true,
            thickness_percent: 15,
            corner_radius_px: 0,
            inactive: false,
        });
        let mut planner = RenderBatchPlanner::default();
        let batches = planner
            .prepare_full(&test_scene, &mut fonts)
            .expect("prepare terminal run");

        let text_right = batches
            .glyphs
            .vertices
            .iter()
            .map(|vertex| vertex.position_px[0])
            .fold(f32::NEG_INFINITY, f32::max);
        let cursor_left = batches
            .cursor
            .vertices
            .iter()
            .map(|vertex| vertex.position_px[0])
            .fold(f32::INFINITY, f32::min);
        assert!(
            text_right <= cursor_left + metrics.cell_width,
            "text geometry escaped its terminal cells: text_right={text_right}, cursor_left={cursor_left}, cell_width={}",
            metrics.cell_width
        );
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
        assert_eq!(batches.glyphs.glyph_count, 1);
        assert_eq!(batches.cursor.quad_count(), 1);
        assert_eq!(batches.damage_regions.len(), 1);
    }

    #[test]
    fn incremental_batch_does_not_repaint_an_entire_intersecting_text_run() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let mut test_scene = scene_without_cursor(vec![
            cell(0, 0, "a"),
            cell(0, 1, "b"),
            cell(0, 2, "c"),
            cell(0, 3, "d"),
        ]);
        let Ok(font_metrics) = fonts.cell_metrics() else {
            return;
        };
        test_scene.damage_regions =
            vec![cell_region(CellPosition { row: 0, col: 3 }, font_metrics)];
        let Ok(batches) = planner.prepare(&test_scene, &mut fonts) else {
            return;
        };

        assert_eq!(batches.background.quad_count(), 1);
        assert_eq!(batches.glyphs.glyph_count, 1);
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
    fn content_offset_applies_to_bounded_text_context_damage() {
        let mut tracker = DamageTracker::new();
        let mut first = scene(vec![cell(0, 0, "a")]);
        first.content_offset = render_core::RenderOffset { x: 12, y: 8 };
        let initial = tracker.update(&first, metrics());
        assert_eq!(initial[0].x, 0);
        assert_eq!(initial[0].y, 0);

        let mut second = first.clone();
        second.grid.cells[0].text = "b".to_owned();
        let damage = tracker.update(&second, metrics());
        assert_eq!(damage.len(), 3);
        for (index, region) in damage.iter().enumerate() {
            assert_eq!(region.x, 12 + i32::try_from(index).unwrap() * 8);
            assert_eq!(region.y, 8);
            assert!(region.width <= metrics().cell_width.ceil() as u32);
        }
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
            shadow: true,
            fps: 60,
            max_active_animations: 8,
            max_animated_region_pixels: 250_000,
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
            start_region: region,
            end_region: region,
            color: RenderColor::rgb(120, 190, 255),
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
    fn panea_vector_cursor_format_is_bounded_and_batches_primitives() {
        let bytes = br#"{
            "version": 1,
            "primitives": [
                {"x": 0, "y": 0, "width": 250, "height": 1000, "corner_radius": 0},
                {"x": 250, "y": 400, "width": 750, "height": 200, "corner_radius": 0,
                 "color": [10, 20, 30, 255]}
            ]
        }"#;
        let primitives = decode_cursor_vector(bytes).expect("valid vector cursor");
        assert_eq!(primitives.len(), 2);
        let visual = CursorVectorVisual {
            asset: Arc::new(CursorVectorAsset {
                id: 1,
                primitives: primitives.into(),
            }),
            bounds: RenderRect {
                x: 10,
                y: 20,
                width: 20,
                height: 40,
            },
            color: RenderColor::rgb(255, 255, 255),
            opacity: 255,
        };
        let mut batch = QuadBatch::new(QuadBatchKind::Cursor);
        push_cursor_vector_quads(
            &mut batch,
            &visual,
            &[RenderRect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            }],
            render_core::RenderOffset::default(),
        );
        assert_eq!(batch.quad_count(), 2);
    }

    #[test]
    fn panea_vector_cursor_rejects_unknown_or_out_of_bounds_data() {
        let unknown = br#"{"version":1,"unknown":true,"primitives":[]}"#;
        assert!(decode_cursor_vector(unknown).is_err());
        let outside = br#"{
            "version": 1,
            "primitives": [{"x": 900, "y": 0, "width": 200, "height": 10}]
        }"#;
        assert!(decode_cursor_vector(outside).is_err());
    }

    #[test]
    fn animated_gif_frames_decode_to_cached_rgba_with_limits() {
        let mut encoded = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut encoded);
            for color in [[255, 0, 0, 255], [0, 255, 0, 180]] {
                let image = image::RgbaImage::from_pixel(2, 3, image::Rgba(color));
                encoder
                    .encode_frame(image::Frame::new(image))
                    .expect("test GIF frame should encode");
            }
        }

        let decoded =
            decode_cursor_image_frames(&encoded, 32, 8, 4096).expect("test GIF should decode");
        assert_eq!((decoded.width, decoded.height), (2, 3));
        assert_eq!(decoded.frames.len(), 2);
        assert_eq!(decoded.frames[0].pixels.len(), 24);
        assert!(decode_cursor_image_frames(&encoded, 32, 1, 4096).is_err());
        assert!(decode_cursor_image_frames(&encoded, 1, 8, 4096).is_err());
    }

    #[test]
    fn static_png_cursor_decodes_as_one_frame() {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            3,
            2,
            image::Rgba([12, 34, 56, 200]),
        ));
        let mut encoded = Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("test PNG should encode");

        let decoded = decode_cursor_image_frames(encoded.get_ref(), 32, 8, 4096)
            .expect("test PNG should decode");
        assert_eq!((decoded.width, decoded.height), (3, 2));
        assert_eq!(decoded.frames.len(), 1);
        assert_eq!(decoded.frames[0].pixels[3], 200);
    }

    #[test]
    fn image_cursor_is_one_batched_quad_and_suppresses_static_cursor() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let mut test_scene = scene(vec![cell(0, 0, "a")]);
        let asset = test_cursor_image_asset(2);
        test_scene.cursor_image = Some(CursorImageVisual {
            asset: Arc::clone(&asset),
            frame_index: 1,
            bounds: RenderRect {
                x: 0,
                y: 0,
                width: 8,
                height: 16,
            },
            opacity: 220,
        });

        let batches = planner
            .prepare_full(&test_scene, &mut fonts)
            .expect("cursor image scene should prepare");
        assert_eq!(batches.cursor.quad_count(), 0);
        assert_eq!(batches.cursor_image.quad_count(), 1);
        assert_eq!(
            batches.cursor_image_asset.as_ref().map(|asset| asset.id),
            Some(asset.id)
        );
    }

    #[test]
    fn image_cursor_frame_changes_damage_only_its_bounds() {
        let asset = test_cursor_image_asset(2);
        let bounds = RenderRect {
            x: 8,
            y: 16,
            width: 8,
            height: 16,
        };
        let mut first = scene(vec![cell(0, 0, "a")]);
        first.cursor_image = Some(CursorImageVisual {
            asset: Arc::clone(&asset),
            frame_index: 0,
            bounds,
            opacity: 255,
        });
        let mut tracker = DamageTracker::new();
        let _ = tracker.update(&first, metrics());

        let mut second = first.clone();
        second
            .cursor_image
            .as_mut()
            .expect("cursor image")
            .frame_index = 1;
        let damage = tracker.update(&second, metrics());
        assert_eq!(damage, vec![bounds]);
    }

    #[test]
    fn vector_cursor_is_batched_and_damages_only_old_and_new_bounds() {
        let asset = Arc::new(CursorVectorAsset {
            id: 9,
            primitives: vec![CursorVectorPrimitive {
                x: 0,
                y: 0,
                width: 1000,
                height: 1000,
                corner_radius: 0,
                color: None,
            }]
            .into(),
        });
        let first_bounds = RenderRect {
            x: 8,
            y: 16,
            width: 8,
            height: 16,
        };
        let mut first = scene(vec![cell(0, 0, "a")]);
        first.cursor_vector = Some(CursorVectorVisual {
            asset: Arc::clone(&asset),
            bounds: first_bounds,
            color: RenderColor::rgb(40, 80, 120),
            opacity: 255,
        });
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let batches = RenderBatchPlanner::default()
            .prepare_full(&first, &mut fonts)
            .expect("vector cursor scene should prepare");
        assert!(batches.cursor.quad_count() >= 1);
        assert!(batches.cursor_image.is_empty());

        let mut tracker = DamageTracker::new();
        let _ = tracker.update(&first, metrics());
        let mut second = first.clone();
        let second_bounds = RenderRect {
            x: 12,
            ..first_bounds
        };
        second.cursor_vector.as_mut().expect("cursor vector").bounds = second_bounds;
        let damage = tracker.update(&second, metrics());
        assert_eq!(
            damage,
            vec![RenderRect {
                x: 8,
                y: 16,
                width: 12,
                height: 16,
            }]
        );
    }

    #[test]
    fn image_cursor_runtime_schedules_only_visible_multiframe_assets() {
        let asset = test_cursor_image_asset(2);
        let image = DecodedCursorImage {
            path: PathBuf::from("cursor.gif"),
            width: 2,
            height: 2,
            frame_count: 2,
            fps: 24,
            size_kb: 1,
            warnings: Vec::new(),
            asset,
        };
        let mut runtime = AnimatedCursorImageRuntime::new();
        runtime.set_image(&image);
        let mut test_scene = scene(vec![cell(0, 0, "a")]);
        runtime.populate_scene(&mut test_scene, metrics());
        assert!(test_scene.cursor_image.is_some());
        assert_eq!(
            runtime.next_frame_after(),
            Some(Duration::from_micros(1_000_000 / 24))
        );

        runtime.clear();
        assert!(runtime.next_frame_after().is_none());
    }

    #[test]
    fn cursor_animation_runtime_enforces_active_and_pixel_budgets() {
        let mut runtime = CursorAnimationRuntime::new();
        let settings = CursorAnimationSettings {
            enabled: true,
            smooth_movement: true,
            typing_pulse: true,
            typing_stretch: true,
            max_active_animations: 1,
            max_animated_region_pixels: 1024,
            ..CursorAnimationSettings::default()
        };
        let mut first = scene(vec![cell(0, 0, "a")]);
        runtime.populate_scene(&mut first, metrics(), settings);
        let mut second = first.clone();
        second.cursor.as_mut().expect("cursor").position = CellPosition { row: 1, col: 1 };
        runtime.record_typing();
        runtime.populate_scene(&mut second, metrics(), settings);
        assert_eq!(second.animations.len(), 1);

        let mut blocked = CursorAnimationRuntime::new();
        let mut blocked_scene = scene(vec![cell(0, 0, "a")]);
        blocked.record_typing();
        blocked.populate_scene(
            &mut blocked_scene,
            metrics(),
            CursorAnimationSettings {
                max_animated_region_pixels: 1,
                ..settings
            },
        );
        assert!(blocked_scene.animations.is_empty());
        assert!(blocked.next_frame_after(settings).is_none());
    }

    fn test_cursor_image_asset(frame_count: usize) -> Arc<CursorImageAsset> {
        let frames = (0..frame_count)
            .map(|index| CursorImageFrame {
                pixels: [index as u8, 40, 80, 255].repeat(4).into(),
            })
            .collect::<Vec<_>>();
        Arc::new(CursorImageAsset {
            id: 42,
            width: 2,
            height: 2,
            frames: frames.into(),
        })
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
            "cursor-image",
            "selection-states",
            "prompt-decorations",
            "command-blocks",
            "multiple-panes",
            "transparency-opacity",
            "fullscreen-chrome-hidden",
            "fullscreen-chrome-half",
            "fullscreen-chrome-visible",
            "fullscreen-chrome-close-hover",
            "fullscreen-chrome-no-controls",
        ] {
            assert!(names.contains(expected), "missing fixture {expected}");
        }
    }

    #[test]
    fn fullscreen_chrome_fixtures_preserve_terminal_layout_below_chrome() {
        let fixtures = screenshot_fixtures();
        let hidden = fixtures
            .iter()
            .find(|fixture| fixture.name == "fullscreen-chrome-hidden")
            .expect("hidden fullscreen chrome fixture");
        let visible = fixtures
            .iter()
            .find(|fixture| fixture.name == "fullscreen-chrome-visible")
            .expect("visible fullscreen chrome fixture");

        assert_eq!(hidden.scene.grid, visible.scene.grid);
        assert_eq!(hidden.scene.content_offset, visible.scene.content_offset);

        let hidden = capture_screenshot_fixture(hidden.name).expect("hidden fixture capture");
        let visible = capture_screenshot_fixture(visible.name).expect("visible fixture capture");
        assert_eq!(hidden.frame.width, visible.frame.width);
        assert_eq!(hidden.frame.height, visible.frame.height);

        let chrome_height = 36_u32.min(hidden.frame.height);
        let first_unchanged_byte = (chrome_height * hidden.frame.width * u32::from(4_u8)) as usize;
        assert_eq!(
            &hidden.frame.pixels[first_unchanged_byte..],
            &visible.frame.pixels[first_unchanged_byte..],
            "fullscreen chrome must not reflow or alter terminal pixels outside its overlay bounds"
        );
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

    #[test]
    fn surface_overlay_damage_uses_surface_coordinates() {
        let mut tracker = DamageTracker::new();
        let mut first = scene(vec![cell(0, 0, "a")]);
        first.content_offset = render_core::RenderOffset { x: 30, y: 20 };
        first.surface_overlays.push(OverlayPrimitive {
            kind: OverlayKind::WindowChrome,
            bounds: RenderRect {
                x: 0,
                y: 0,
                width: 800,
                height: 36,
            },
            color: RenderColor::rgb(20, 20, 20),
            border_color: None,
            border_width_px: 0,
            corner_radius_px: 0,
            z_index: 100,
            label: None,
            label_color: None,
        });
        let initial = tracker.update(&first, metrics());
        assert!(initial.iter().any(|region| region.width >= 800));

        let mut hidden = first.clone();
        hidden.surface_overlays.clear();
        let damage = tracker.update(&hidden, metrics());
        assert!(damage.iter().any(|region| {
            region.x == 0 && region.y == 0 && region.width >= 800 && region.height >= 36
        }));
    }

    fn window_chrome_visual() -> render_core::WindowChromeVisual {
        use render_core::{WindowChromeControlKind, WindowChromeControlVisual};

        render_core::WindowChromeVisual {
            bounds: RenderRect {
                x: 0,
                y: 0,
                width: 800,
                height: 36,
            },
            opacity: u16::MAX,
            title: "Panea".to_owned(),
            show_logo: true,
            controls: vec![
                WindowChromeControlVisual {
                    kind: WindowChromeControlKind::Minimize,
                    bounds: RenderRect {
                        x: 656,
                        y: 0,
                        width: 48,
                        height: 36,
                    },
                    hovered: false,
                    pressed: false,
                },
                WindowChromeControlVisual {
                    kind: WindowChromeControlKind::LeaveFullscreen,
                    bounds: RenderRect {
                        x: 704,
                        y: 0,
                        width: 48,
                        height: 36,
                    },
                    hovered: false,
                    pressed: false,
                },
                WindowChromeControlVisual {
                    kind: WindowChromeControlKind::Close,
                    bounds: RenderRect {
                        x: 752,
                        y: 0,
                        width: 48,
                        height: 36,
                    },
                    hovered: true,
                    pressed: false,
                },
            ],
        }
    }

    fn has_vertex_strictly_inside(batch: &QuadBatch, bounds: RenderRect) -> bool {
        let x0 = bounds.x as f32;
        let y0 = bounds.y as f32;
        let x1 = x0 + bounds.width as f32;
        let y1 = y0 + bounds.height as f32;
        batch.vertices.iter().any(|vertex| {
            vertex.position_px[0] > x0
                && vertex.position_px[0] < x1
                && vertex.position_px[1] > y0
                && vertex.position_px[1] < y1
        })
    }

    #[test]
    fn window_chrome_is_one_batched_overlay_with_logo_title_and_controls() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let visual = window_chrome_visual();
        let mut test_scene = scene_without_cursor(Vec::new());
        test_scene.window_chrome = Some(visual.clone());
        test_scene.damage_regions = vec![visual.bounds];

        let first = planner
            .prepare(&test_scene, &mut fonts)
            .expect("window chrome should prepare");
        let second = planner
            .prepare(&test_scene, &mut fonts)
            .expect("unchanged window chrome should prepare");

        assert_eq!(first.window_chrome.kind, QuadBatchKind::Decoration);
        assert!(!first.window_chrome.is_empty());
        assert!(first.overlay_glyphs.glyph_count > 0, "title must be shaped");
        assert_eq!(
            first
                .atlas_uploads
                .iter()
                .filter(|upload| upload.key == AtlasCacheKey::PaneaLogo)
                .count(),
            1,
            "the built-in logo must be uploaded once"
        );
        assert_eq!(
            second
                .atlas_uploads
                .iter()
                .filter(|upload| upload.key == AtlasCacheKey::PaneaLogo)
                .count(),
            0,
            "the cached logo must not be uploaded again"
        );
        assert_eq!(first.overlay_glyphs, second.overlay_glyphs);
        for control in &visual.controls {
            assert!(
                has_vertex_strictly_inside(&first.window_chrome, control.bounds),
                "{:?} control must contribute batched geometry",
                control.kind
            );
        }
        assert!(first.window_chrome.vertices.iter().any(|vertex| {
            vertex.color[0] > 0.7 && vertex.color[1] < 0.3 && vertex.color[2] < 0.3
        }));
    }

    #[test]
    fn absent_window_chrome_has_zero_batch_and_upload_cost() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut planner = RenderBatchPlanner::default();
        let test_scene = scene_without_cursor(Vec::new());

        let batches = planner
            .prepare(&test_scene, &mut fonts)
            .expect("empty scene should prepare");

        assert!(batches.window_chrome.is_empty());
        assert!(
            !batches
                .atlas_uploads
                .iter()
                .any(|upload| upload.key == AtlasCacheKey::PaneaLogo)
        );
    }

    #[test]
    fn window_chrome_is_present_in_renderer_independent_cpu_snapshots() {
        let mut fonts = FontSystem::new(font_system::FontConfig::default());
        let mut rasterizer = TerminalRasterizer::default();
        let mut test_scene = scene_without_cursor(Vec::new());
        let visual = window_chrome_visual();
        test_scene.window_chrome = Some(visual.clone());

        let frame = rasterizer
            .rasterize(&test_scene, &mut fonts)
            .expect("window chrome snapshot should render");

        assert!(frame.width >= visual.bounds.width);
        assert!(frame.height >= visual.bounds.height);
        let index = ((2 * frame.width + 2) * 4) as usize;
        assert_ne!(&frame.pixels[index..index + 4], &[12, 12, 12, 255]);
    }

    #[test]
    fn window_chrome_changes_damage_old_and_new_surface_bounds() {
        let mut tracker = DamageTracker::new();
        let mut first = scene_without_cursor(Vec::new());
        first.window_chrome = Some(window_chrome_visual());
        let _ = tracker.update(&first, metrics());

        let mut second = first.clone();
        let chrome = second.window_chrome.as_mut().expect("chrome visual");
        chrome.bounds.y = 12;
        chrome.opacity = u16::MAX / 2;
        let damage = tracker.update(&second, metrics());

        assert!(
            damage
                .iter()
                .any(|region| region.y == 0 && region.height >= 36)
        );
        assert!(
            damage
                .iter()
                .any(|region| region.y <= 12 && region.height >= 36)
        );
    }
}
