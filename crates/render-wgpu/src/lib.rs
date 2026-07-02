//! WGPU renderer implementation, glyph atlas policy, damage tracking, and frame scheduling.

pub const LAYER: &str = "render performance";

use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    fmt,
    sync::{Arc, Mutex},
    time::Instant,
};

use font_system::{CellMetrics, FontError, FontSystem, GlyphBitmap, GlyphCache, GlyphCacheKey};
use render_core::{
    CellPosition, CursorVisual, DamageRegion, FrameRequestReason, RenderCell, RenderCellStyle,
    RenderColor, RenderCursorShape, RenderGrid, RenderInstrumentation, RenderRecoveryEvent,
    RenderRecoveryReason, RenderRecoveryStatus, RenderRect, RenderScene, RenderSurfaceStatus,
};
use wgpu::util::DeviceExt;
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
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            present_mode: PresentMode::Vsync,
            damage_tracking: true,
        }
    }
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

        if self.force_full || self.previous_size != Some(size) {
            self.force_full = false;
            self.previous_size = Some(size);
            self.previous_cells = scene
                .grid
                .cells
                .iter()
                .map(|cell| (cell.position, CellFingerprint::from(cell)))
                .collect();
            self.previous_cursor = scene.cursor;
            return vec![grid_region(&scene.grid, metrics)];
        }

        for cell in &scene.grid.cells {
            let fingerprint = CellFingerprint::from(cell);
            if self.previous_cells.get(&cell.position) != Some(&fingerprint) {
                regions.push(cell_region(cell.position, metrics));
                self.previous_cells.insert(cell.position, fingerprint);
            }
        }

        if self.previous_cursor != scene.cursor {
            if let Some(cursor) = self.previous_cursor {
                regions.push(cell_region(cursor.position, metrics));
            }
            if let Some(cursor) = scene.cursor {
                regions.push(cell_region(cursor.position, metrics));
            }
            self.previous_cursor = scene.cursor;
        }

        merge_regions(regions)
    }
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

fn grid_region(grid: &RenderGrid, metrics: CellMetrics) -> DamageRegion {
    RenderRect {
        x: 0,
        y: 0,
        width: (f32::from(grid.columns) * metrics.cell_width).ceil() as u32,
        height: (f32::from(grid.rows) * metrics.cell_height).ceil() as u32,
    }
}

fn cell_region(position: CellPosition, metrics: CellMetrics) -> DamageRegion {
    RenderRect {
        x: (f32::from(position.col) * metrics.cell_width).floor() as i32,
        y: (position.row.max(0) as f32 * metrics.cell_height).floor() as i32,
        width: metrics.cell_width.ceil() as u32,
        height: metrics.cell_height.ceil() as u32,
    }
}

fn merge_regions(regions: Vec<DamageRegion>) -> Vec<DamageRegion> {
    regions
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedRenderBatches {
    pub frame_width: u32,
    pub frame_height: u32,
    pub damage_regions: Vec<DamageRegion>,
    pub background: QuadBatch,
    pub glyphs: GlyphBatch,
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
    font_id: u64,
    text: String,
    size_millipoints: u32,
    bold: bool,
    italic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GlyphRunItem {
    key: Option<GlyphCacheKey>,
}

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
    font_id: u64,
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
        let frame_width = (f32::from(scene.grid.columns) * metrics.cell_width)
            .ceil()
            .max(1.0) as u32;
        let frame_height = (f32::from(scene.grid.rows) * metrics.cell_height)
            .ceil()
            .max(1.0) as u32;
        let damage_regions = effective_damage_regions(scene, metrics);
        let font_id = fonts.primary_font()?.id();

        let mut background = QuadBatch::new(QuadBatchKind::Background);
        let mut glyphs = GlyphBatch {
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

        for cell in &scene.grid.cells {
            let rect = cell_region(cell.position, metrics);
            if !intersects_any(rect, &damage_regions) {
                continue;
            }

            push_solid_quad(&mut background, rect, cell.background);
            let mut glyph_context = GlyphBatchContext {
                atlas_uploads: &mut atlas_uploads,
                instrumentation: &mut instrumentation,
                fonts,
                font_id,
                metrics,
                rect,
            };
            self.push_glyphs(&mut glyphs, cell, &mut glyph_context)?;
            push_text_decorations(&mut decorations, cell, metrics, rect);
        }

        let mut overlays = scene
            .search_highlights
            .iter()
            .chain(scene.semantic_overlays.iter())
            .collect::<Vec<_>>();
        overlays.sort_by_key(|overlay| overlay.z_index);

        for overlay in overlays {
            if intersects_any(overlay.bounds, &damage_regions) {
                push_solid_quad(&mut decorations, overlay.bounds, overlay.color);
                if let Some(border_color) = overlay.border_color {
                    push_stroke_quads(&mut decorations, overlay.bounds, border_color);
                }
            }
        }

        for decoration in &scene.decorations {
            if intersects_any(decoration.bounds, &damage_regions) {
                push_solid_quad(&mut decorations, decoration.bounds, decoration.color);
                if let Some(border_color) = decoration.border_color {
                    push_stroke_quads(&mut decorations, decoration.bounds, border_color);
                }
            }
        }

        for selection in &scene.selections {
            for position in &selection.cells {
                let rect = cell_region(*position, metrics);
                if intersects_any(rect, &damage_regions) {
                    push_solid_quad(&mut selections, rect, selection.color);
                }
            }
        }

        if let Some(cursor_visual) = scene.cursor
            && cursor_visual.visible
        {
            push_cursor_quads(&mut cursor, cursor_visual, metrics, &damage_regions);
        }

        instrumentation.draw_call_count = count_non_empty_batches([
            !background.is_empty(),
            !glyphs.is_empty(),
            !decorations.is_empty(),
            !selections.is_empty(),
            !cursor.is_empty(),
        ]);
        instrumentation.cpu_prepare_time = started.elapsed();
        instrumentation.frame_time = instrumentation.cpu_prepare_time;

        Ok(PreparedRenderBatches {
            frame_width,
            frame_height,
            damage_regions,
            background,
            glyphs,
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
        scene.damage_regions = vec![grid_region(&scene.grid, metrics)];
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
            font_id: context.font_id,
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
            let run = cell
                .text
                .chars()
                .filter(|ch| !ch.is_control())
                .map(|ch| GlyphRunItem {
                    key: if ch == ' ' {
                        None
                    } else {
                        Some(GlyphCacheKey::new(
                            context.font_id,
                            ch,
                            context.metrics.font_size,
                            cell.style.bold,
                            cell.style.italic,
                        ))
                    },
                })
                .collect::<Vec<_>>();
            self.glyph_runs.insert(run_key, run.clone());
            run
        };

        let mut pen_x = context.rect.x;
        for item in run {
            let Some(key) = item.key else {
                pen_x += context.metrics.cell_width.ceil() as i32;
                continue;
            };
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
                    });
                }
                push_glyph_quad(
                    glyphs,
                    RenderRect {
                        x: pen_x + bitmap.offset_x,
                        y: context.rect.y + bitmap.offset_y,
                        width: bitmap.width,
                        height: bitmap.height,
                    },
                    entry,
                    self.atlas.dimensions(),
                    cell.foreground,
                );
            }
            pen_x += bitmap.advance_width.ceil() as i32;
        }

        Ok(())
    }
}

fn count_non_empty_batches<const N: usize>(batches: [bool; N]) -> u32 {
    batches.into_iter().filter(|non_empty| *non_empty).count() as u32
}

fn effective_damage_regions(scene: &RenderScene, metrics: CellMetrics) -> Vec<DamageRegion> {
    if scene.damage_regions.is_empty() {
        vec![grid_region(&scene.grid, metrics)]
    } else {
        merge_regions(scene.damage_regions.clone())
    }
}

fn intersects_any(rect: RenderRect, regions: &[DamageRegion]) -> bool {
    regions.iter().any(|region| rects_intersect(rect, *region))
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

fn push_cursor_quads(
    batch: &mut QuadBatch,
    cursor: CursorVisual,
    metrics: CellMetrics,
    damage_regions: &[DamageRegion],
) {
    let mut rect = cell_region(cursor.position, metrics);
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
            push_solid_quad(
                batch,
                RenderRect {
                    height: line,
                    ..rect
                },
                cursor.color,
            );
            push_solid_quad(
                batch,
                RenderRect {
                    y: rect.y + rect.height.saturating_sub(line) as i32,
                    height: line,
                    ..rect
                },
                cursor.color,
            );
            push_solid_quad(
                batch,
                RenderRect {
                    width: line,
                    ..rect
                },
                cursor.color,
            );
            push_solid_quad(
                batch,
                RenderRect {
                    x: rect.x + rect.width.saturating_sub(line) as i32,
                    width: line,
                    ..rect
                },
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
    push_solid_quad(batch, rect, cursor.color);
}

#[derive(Debug)]
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
        let width = (f32::from(scene.grid.columns) * metrics.cell_width)
            .ceil()
            .max(1.0) as u32;
        let height = (f32::from(scene.grid.rows) * metrics.cell_height)
            .ceil()
            .max(1.0) as u32;
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

        for cell in &scene.grid.cells {
            self.draw_cell(&mut frame, cell, fonts, metrics, &mut instrumentation)?;
        }

        let mut overlays = scene
            .search_highlights
            .iter()
            .chain(scene.semantic_overlays.iter())
            .collect::<Vec<_>>();
        overlays.sort_by_key(|overlay| overlay.z_index);

        for overlay in overlays {
            blend_rect(&mut frame, overlay.bounds, overlay.color);
            instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            if let Some(border_color) = overlay.border_color {
                stroke_rect(&mut frame, overlay.bounds, border_color);
                instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            }
        }

        for selection in &scene.selections {
            for position in &selection.cells {
                fill_rect(&mut frame, cell_region(*position, metrics), selection.color);
                instrumentation.draw_call_count = instrumentation.draw_call_count.saturating_add(1);
            }
        }

        if let Some(cursor) = scene.cursor {
            draw_cursor(&mut frame, cursor, metrics);
        }

        instrumentation.cpu_prepare_time = started.elapsed();
        instrumentation.frame_time = instrumentation.cpu_prepare_time;

        Ok(InstrumentedCpuFrame {
            frame,
            instrumentation,
        })
    }

    fn draw_cell(
        &mut self,
        frame: &mut CpuFrame,
        cell: &RenderCell,
        fonts: &mut FontSystem,
        metrics: CellMetrics,
        _instrumentation: &mut RenderInstrumentation,
    ) -> Result<(), RendererError> {
        let rect = cell_region(cell.position, metrics);
        fill_rect(frame, rect, cell.background);

        let font_id = fonts.primary_font()?.id();
        let mut pen_x = rect.x;

        for ch in cell.text.chars() {
            if ch == ' ' {
                pen_x += metrics.cell_width.ceil() as i32;
                continue;
            }

            let key = GlyphCacheKey::new(
                font_id,
                ch,
                metrics.font_size,
                cell.style.bold,
                cell.style.italic,
            );
            let bitmap = self.batch_planner.glyph_cache.get_or_insert_with(key, || {
                fonts.rasterize_glyph(key).unwrap_or_else(|_| {
                    GlyphBitmap::missing(metrics.cell_width, metrics.cell_height as u32)
                })
            });
            draw_glyph(
                frame,
                pen_x + bitmap.offset_x,
                rect.y + bitmap.offset_y,
                bitmap.as_ref(),
                cell.foreground,
            );
            pen_x += bitmap.advance_width.ceil() as i32;
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

#[derive(Debug)]
struct GpuBatchBuffers {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
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

            let alpha = bitmap.pixels[(gy * bitmap.width + gx) as usize];
            if alpha == 0 {
                continue;
            }

            let index = (((target_y as u32 * frame.width) + target_x as u32) * 4) as usize;
            blend_pixel(&mut frame.pixels[index..index + 4], color, alpha);
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

fn draw_cursor(frame: &mut CpuFrame, cursor: CursorVisual, metrics: CellMetrics) {
    if !cursor.visible {
        return;
    }

    let mut rect = cell_region(cursor.position, metrics);
    let thickness = u32::from(cursor.thickness_percent.clamp(1, 100));
    match cursor.shape {
        RenderCursorShape::Block
        | RenderCursorShape::Custom
        | RenderCursorShape::CustomStaticShape => {}
        RenderCursorShape::HollowBlock => {
            let line = ((rect.width.min(rect.height) * thickness) / 100).max(1);
            fill_rect(
                frame,
                RenderRect {
                    height: line,
                    ..rect
                },
                cursor.color,
            );
            fill_rect(
                frame,
                RenderRect {
                    y: rect.y + rect.height.saturating_sub(line) as i32,
                    height: line,
                    ..rect
                },
                cursor.color,
            );
            fill_rect(
                frame,
                RenderRect {
                    width: line,
                    ..rect
                },
                cursor.color,
            );
            fill_rect(
                frame,
                RenderRect {
                    x: rect.x + rect.width.saturating_sub(line) as i32,
                    width: line,
                    ..rect
                },
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
    fill_rect(frame, rect, cursor.color);
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
    device_loss_signal: Arc<Mutex<Option<DeviceLossSignal>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceLossSignal {
    reason: RenderRecoveryReason,
    message: String,
}

impl GpuTerminalRenderer {
    pub async fn new(window: Arc<Window>, options: RendererOptions) -> Result<Self, RendererError> {
        let backend = GpuBackend::new(Arc::clone(&window), options).await?;

        Ok(Self {
            window,
            options,
            backend: Some(backend),
            rasterizer: TerminalRasterizer::default(),
            last_instrumentation: RenderInstrumentation::default(),
            recovery_status: RenderRecoveryStatus::Ready,
            recovery_attempts: 0,
            recovery_events: Vec::new(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(backend) = self.backend.as_mut() {
            backend.resize(width, height);
        }
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
        let mut batches = self.rasterizer.prepare_batches(scene, fonts)?;
        let gpu_started = Instant::now();
        backend.upload_atlas(&self.rasterizer, &batches);
        let result = backend.present_batches(&batches);
        batches.instrumentation.gpu_submit_time = Some(gpu_started.elapsed());
        batches.instrumentation.frame_time = frame_started.elapsed();
        self.last_instrumentation = batches.instrumentation;

        match result {
            Ok(PresentOutcome::Submitted | PresentOutcome::Timeout) => Ok(()),
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
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("panea-render-device"),
                    required_features: wgpu::Features::empty(),
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
        let alpha_mode = caps.alpha_modes[0];
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
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
            device_loss_signal,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }

        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn take_device_loss_signal(&self) -> Option<DeviceLossSignal> {
        self.device_loss_signal
            .lock()
            .ok()
            .and_then(|mut signal| signal.take())
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
                format: wgpu::TextureFormat::R8Unorm,
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
                let start = (row * upload.entry.width) as usize;
                let end = start + upload.entry.width as usize;
                if end > upload.pixels.len() {
                    break;
                }
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
                    &upload.pixels[start..end],
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
        let background =
            self.make_buffers(&batches.background.vertices, &batches.background.indices);
        let decorations =
            self.make_buffers(&batches.decorations.vertices, &batches.decorations.indices);
        let selections =
            self.make_buffers(&batches.selections.vertices, &batches.selections.indices);
        let cursor = self.make_buffers(&batches.cursor.vertices, &batches.cursor.indices);
        let glyphs = self.make_buffers(&batches.glyphs.vertices, &batches.glyphs.indices);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("panea-batch-encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("panea-batch-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            pass.set_pipeline(&self.quad_pipeline);
            draw_buffers(&mut pass, background.as_ref());
            draw_buffers(&mut pass, selections.as_ref());

            if let Some(glyph_bind_group) = &self.glyph_bind_group {
                pass.set_pipeline(&self.glyph_pipeline);
                pass.set_bind_group(0, glyph_bind_group, &[]);
                draw_buffers(&mut pass, glyphs.as_ref());
            }

            pass.set_pipeline(&self.quad_pipeline);
            draw_buffers(&mut pass, decorations.as_ref());
            draw_buffers(&mut pass, cursor.as_ref());
        }

        self.queue.submit(Some(encoder.finish()));
        output.present();
        Ok(PresentOutcome::Submitted)
    }

    fn make_buffers(&self, vertices: &[BatchVertex], indices: &[u32]) -> Option<GpuBatchBuffers> {
        if vertices.is_empty() || indices.is_empty() {
            return None;
        }

        let vertices = vertices
            .iter()
            .map(|vertex| vertex_to_gpu(*vertex, self.config.width, self.config.height))
            .collect::<Vec<_>>();
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("panea-batch-vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("panea-batch-indices"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        Some(GpuBatchBuffers {
            vertices: vertex_buffer,
            indices: index_buffer,
            index_count: indices.len() as u32,
        })
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

fn draw_buffers<'a>(pass: &mut wgpu::RenderPass<'a>, buffers: Option<&'a GpuBatchBuffers>) {
    let Some(buffers) = buffers else {
        return;
    };

    pass.set_vertex_buffer(0, buffers.vertices.slice(..));
    pass.set_index_buffer(buffers.indices.slice(..), wgpu::IndexFormat::Uint32);
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
    let coverage = textureSample(glyph_atlas, glyph_sampler, in.uv).r;
    return vec4<f32>(in.color.rgb, in.color.a * coverage);
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

    fn scene_without_cursor(cells: Vec<RenderCell>) -> RenderScene {
        RenderScene {
            cursor: None,
            ..scene(cells)
        }
    }

    #[test]
    fn atlas_allocates_and_clears_when_full() {
        let mut atlas = GlyphAtlas::new(8, 8);
        let key_a = GlyphCacheKey::new(1, 'a', 13.0, false, false);
        let key_b = GlyphCacheKey::new(1, 'b', 13.0, false, false);
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
}
