// Renderer-independent batching, glyph planning, and quad generation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuadBatchKind {
    Background,
    Decoration,
    CursorTrail,
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

    fn clear_for_reuse(&mut self, kind: QuadBatchKind) {
        self.kind = kind;
        self.vertices.clear();
        self.indices.clear();
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

    fn clear_for_reuse(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.glyph_count = 0;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AtlasUpload {
    pub key: AtlasCacheKey,
    pub entry: AtlasEntry,
    pub pixels: Vec<u8>,
    pub format: GlyphBitmapFormat,
}

struct PaddedAtlasUpload {
    pixels: Vec<u8>,
    bytes_per_row: u32,
    rows_per_image: u32,
}

fn padded_atlas_upload(upload: &AtlasUpload) -> Option<PaddedAtlasUpload> {
    if upload.entry.width == 0 || upload.entry.height == 0 {
        return None;
    }
    let channels = match upload.format {
        GlyphBitmapFormat::Alpha => 1,
        GlyphBitmapFormat::Rgba => 4,
    };
    let padded_width = upload.entry.width.checked_add(GLYPH_ATLAS_PADDING * 2)?;
    let padded_height = upload.entry.height.checked_add(GLYPH_ATLAS_PADDING * 2)?;
    let len = pixel_buffer_len(padded_width, padded_height, channels)?;
    let mut pixels = vec![0_u8; len];
    for (target_index, target) in pixels.chunks_exact_mut(channels as usize).enumerate() {
        let target_index = u32::try_from(target_index).ok()?;
        let y = target_index / padded_width;
        let x = target_index % padded_width;
        let source_y = y
            .saturating_sub(GLYPH_ATLAS_PADDING)
            .min(upload.entry.height.saturating_sub(1));
        let source_x = x
            .saturating_sub(GLYPH_ATLAS_PADDING)
            .min(upload.entry.width.saturating_sub(1));
        let source_pixel = source_y
            .checked_mul(upload.entry.width)?
            .checked_add(source_x)?;
        match upload.format {
            GlyphBitmapFormat::Alpha => {
                target[0] = upload
                    .pixels
                    .get(usize::try_from(source_pixel).ok()?)
                    .copied()
                    .unwrap_or(0);
            }
            GlyphBitmapFormat::Rgba => {
                let source = usize::try_from(source_pixel.checked_mul(4)?).ok()?;
                target.copy_from_slice(upload.pixels.get(source..source.checked_add(4)?)?);
            }
        }
    }
    Some(PaddedAtlasUpload {
        pixels,
        bytes_per_row: padded_width.checked_mul(channels)?,
        rows_per_image: padded_height,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedRenderBatches {
    pub frame_width: u32,
    pub frame_height: u32,
    pub damage_regions: Vec<DamageRegion>,
    pub background: QuadBatch,
    pub glyphs: GlyphBatch,
    pub logo_glyphs: GlyphBatch,
    pub overlay_glyphs: GlyphBatch,
    pub decorations: QuadBatch,
    pub cursor_effects: QuadBatch,
    pub cursor_trail: QuadBatch,
    pub window_chrome: QuadBatch,
    pub selections: QuadBatch,
    pub cursor: QuadBatch,
    pub cursor_image: QuadBatch,
    pub cursor_image_asset: Option<Arc<CursorImageAsset>>,
    pub atlas_uploads: Vec<AtlasUpload>,
    pub instrumentation: RenderInstrumentation,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CursorOverlayFrame {
    pub cursor: Option<CursorVisual>,
    pub animations: Vec<AnimationHandle>,
    pub content_offset: render_core::RenderOffset,
}

impl CursorOverlayFrame {
    #[must_use]
    pub fn from_scene(scene: &RenderScene) -> Self {
        Self {
            cursor: scene.cursor,
            animations: scene
                .animations
                .iter()
                .filter(|animation| cursor_runtime_owns(animation.kind))
                .copied()
                .collect(),
            content_offset: scene.content_offset,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCursorOverlay {
    pub effects: QuadBatch,
    pub cursor_trail: QuadBatch,
    pub cursor: QuadBatch,
    pub animated_pixels: u32,
    damage_regions: Vec<DamageRegion>,
    merged_damage_regions: Vec<DamageRegion>,
}

impl PreparedCursorOverlay {
    #[must_use]
    pub fn draw_call_count(&self) -> u32 {
        [
            !self.effects.is_empty(),
            !self.cursor_trail.is_empty(),
            !self.cursor.is_empty(),
        ]
        .into_iter()
        .filter(|non_empty| *non_empty)
        .count() as u32
    }
}

#[must_use]
pub fn prepare_cursor_overlay(
    frame: &CursorOverlayFrame,
    metrics: CellMetrics,
) -> PreparedCursorOverlay {
    prepare_cursor_overlay_reusing(frame, metrics, None)
}

#[must_use]
pub fn prepare_cursor_overlay_reusing(
    frame: &CursorOverlayFrame,
    metrics: CellMetrics,
    recycled: Option<PreparedCursorOverlay>,
) -> PreparedCursorOverlay {
    let mut prepared = recycled.unwrap_or_else(|| PreparedCursorOverlay {
        effects: QuadBatch::new(QuadBatchKind::Decoration),
        cursor_trail: QuadBatch::new(QuadBatchKind::CursorTrail),
        cursor: QuadBatch::new(QuadBatchKind::Cursor),
        animated_pixels: 0,
        damage_regions: Vec::new(),
        merged_damage_regions: Vec::new(),
    });
    prepared.effects.clear_for_reuse(QuadBatchKind::Decoration);
    prepared
        .cursor_trail
        .clear_for_reuse(QuadBatchKind::CursorTrail);
    prepared.cursor.clear_for_reuse(QuadBatchKind::Cursor);
    prepared.damage_regions.clear();
    prepared.damage_regions.extend(
        frame
            .animations
            .iter()
            .map(|animation| offset_region(animation.affected_region, frame.content_offset))
            .chain(frame.cursor.map(|cursor| {
                offset_region(cursor_visual_region(cursor, metrics), frame.content_offset)
            })),
    );

    push_animation_quads(
        &mut prepared.effects,
        &mut prepared.cursor_trail,
        &frame.animations,
        &prepared.damage_regions,
        frame.content_offset,
        false,
    );
    if let Some(cursor_visual) = frame.cursor
        && cursor_visual.visible
        && !frame
            .animations
            .iter()
            .any(|animation| animation.kind == AnimationKind::CursorSmoothMovement)
    {
        push_cursor_quads(
            &mut prepared.cursor,
            cursor_visual,
            metrics,
            &prepared.damage_regions,
            frame.content_offset,
        );
    }

    // Sort-and-sweep. Merging pairwise with a restart after every union was
    // quadratic in the region count, which a busy TUI drives into the hundreds.
    prepared.merged_damage_regions = merge_regions(prepared.damage_regions.clone());
    prepared.animated_pixels = prepared
        .merged_damage_regions
        .iter()
        .fold(0u32, |pixels, region| {
            pixels.saturating_add(region.width.saturating_mul(region.height))
        });
    prepared
}

fn can_present_cursor_overlay(
    retained_damage_enabled: bool,
    retained_frame_initialized: bool,
    retained_cursor: Option<CursorVisual>,
    frame: &CursorOverlayFrame,
) -> bool {
    retained_damage_enabled
        && retained_frame_initialized
        && retained_cursor == frame.cursor
        && frame.cursor.is_some_and(|cursor| cursor.visible)
}

impl PreparedRenderBatches {
    fn empty() -> Self {
        Self {
            frame_width: 1,
            frame_height: 1,
            damage_regions: Vec::new(),
            background: QuadBatch::new(QuadBatchKind::Background),
            glyphs: GlyphBatch {
                vertices: Vec::new(),
                indices: Vec::new(),
                glyph_count: 0,
            },
            logo_glyphs: GlyphBatch {
                vertices: Vec::new(),
                indices: Vec::new(),
                glyph_count: 0,
            },
            overlay_glyphs: GlyphBatch {
                vertices: Vec::new(),
                indices: Vec::new(),
                glyph_count: 0,
            },
            decorations: QuadBatch::new(QuadBatchKind::Decoration),
            cursor_effects: QuadBatch::new(QuadBatchKind::Decoration),
            cursor_trail: QuadBatch::new(QuadBatchKind::CursorTrail),
            window_chrome: QuadBatch::new(QuadBatchKind::Decoration),
            selections: QuadBatch::new(QuadBatchKind::Selection),
            cursor: QuadBatch::new(QuadBatchKind::Cursor),
            cursor_image: QuadBatch::new(QuadBatchKind::Cursor),
            cursor_image_asset: None,
            atlas_uploads: Vec::new(),
            instrumentation: RenderInstrumentation::default(),
        }
    }

    #[must_use]
    pub fn draw_call_count(&self) -> u32 {
        [
            !self.background.is_empty(),
            !self.glyphs.is_empty(),
            !self.logo_glyphs.is_empty(),
            !self.overlay_glyphs.is_empty(),
            !self.decorations.is_empty(),
            !self.cursor_effects.is_empty(),
            !self.cursor_trail.is_empty(),
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
    text: RenderText,
    size_millipoints: u32,
    bold: bool,
    italic: bool,
}

#[derive(Hash)]
struct GlyphRunQuery<'a> {
    font_generation: u64,
    text: &'a str,
    size_millipoints: u32,
    bold: bool,
    italic: bool,
}

impl Equivalent<GlyphRunKey> for GlyphRunQuery<'_> {
    fn equivalent(&self, key: &GlyphRunKey) -> bool {
        self.font_generation == key.font_generation
            && self.text == key.text.as_str()
            && self.size_millipoints == key.size_millipoints
            && self.bold == key.bold
            && self.italic == key.italic
    }
}

type GlyphRunItem = ShapedGlyph;

#[derive(Debug)]
pub struct RenderBatchPlanner {
    glyph_cache: GlyphCache,
    atlas: GlyphAtlas,
    atlas_exhausted: bool,
    atlas_font_generation: Option<u64>,
    glyph_runs: HbHashMap<GlyphRunKey, CachedGlyphRun>,
    glyph_run_clock: u64,
    max_glyph_runs: usize,
}

#[derive(Debug)]
struct CachedGlyphRun {
    run: Arc<[GlyphRunItem]>,
    last_used: u64,
}

struct GlyphBatchContext<'a> {
    atlas_uploads: &'a mut Vec<AtlasUpload>,
    instrumentation: &'a mut RenderInstrumentation,
    fonts: &'a mut FontSystem,
    metrics: CellMetrics,
    rect: RenderRect,
    clip_regions: Option<&'a [DamageRegion]>,
    content_clip: Option<RenderRect>,
    cursor_text: Option<CursorTextOverride>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorTextOverride {
    bounds: RenderRect,
    color: RenderColor,
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
            atlas_exhausted: false,
            atlas_font_generation: None,
            glyph_runs: HbHashMap::new(),
            glyph_run_clock: 0,
            max_glyph_runs: glyph_capacity.max(1),
        }
    }

    pub fn prepare(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<PreparedRenderBatches, RendererError> {
        self.prepare_reusing(scene, fonts, None)
    }

    pub fn prepare_reusing(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
        recycled: Option<PreparedRenderBatches>,
    ) -> Result<PreparedRenderBatches, RendererError> {
        self.prepare_reusing_with_damage(scene, fonts, recycled, None)
    }

    fn prepare_reusing_with_damage(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
        recycled: Option<PreparedRenderBatches>,
        damage_override: Option<&[DamageRegion]>,
    ) -> Result<PreparedRenderBatches, RendererError> {
        let font_generation = fonts.generation_id();
        if self.atlas_font_generation != Some(font_generation) {
            self.atlas.clear();
            self.atlas_font_generation = Some(font_generation);
        }

        self.atlas_exhausted = false;
        let batches = self.prepare_once_reusing(scene, fonts, recycled, damage_override)?;
        if !self.atlas_exhausted {
            return Ok(batches);
        }

        self.atlas.clear();
        self.atlas_exhausted = false;
        let batches = self.prepare_once_reusing(scene, fonts, Some(batches), damage_override)?;
        if self.atlas_exhausted {
            return Err(RendererError::Asset(
                "visible glyphs do not fit in the empty glyph atlas".to_owned(),
            ));
        }
        Ok(batches)
    }

    fn prepare_once_reusing(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
        recycled: Option<PreparedRenderBatches>,
        damage_override: Option<&[DamageRegion]>,
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
        let mut recycled = recycled.unwrap_or_else(PreparedRenderBatches::empty);
        recycled.damage_regions.clear();
        recycled.damage_regions.extend(match damage_override {
            Some(regions) => merge_regions(regions.to_vec()),
            None => effective_damage_regions(scene, metrics),
        });
        let damage_regions = recycled.damage_regions;
        let (first_cell_row, cell_row_count) = cell_row_bounds(&scene.grid.cells);
        let damage_rows = DamageRows::new(
            &damage_regions,
            metrics,
            scene.content_offset,
            first_cell_row,
            cell_row_count,
        );
        let mut background = recycled.background;
        background.clear_for_reuse(QuadBatchKind::Background);
        let mut glyphs = recycled.glyphs;
        glyphs.clear_for_reuse();
        let mut logo_glyphs = recycled.logo_glyphs;
        logo_glyphs.clear_for_reuse();
        let mut overlay_glyphs = recycled.overlay_glyphs;
        overlay_glyphs.clear_for_reuse();
        let mut decorations = recycled.decorations;
        decorations.clear_for_reuse(QuadBatchKind::Decoration);
        let mut cursor_effects = recycled.cursor_effects;
        cursor_effects.clear_for_reuse(QuadBatchKind::Decoration);
        let mut cursor_trail = recycled.cursor_trail;
        cursor_trail.clear_for_reuse(QuadBatchKind::CursorTrail);
        let mut window_chrome = recycled.window_chrome;
        window_chrome.clear_for_reuse(QuadBatchKind::Decoration);
        let mut selections = recycled.selections;
        selections.clear_for_reuse(QuadBatchKind::Selection);
        let mut cursor = recycled.cursor;
        cursor.clear_for_reuse(QuadBatchKind::Cursor);
        let mut cursor_image = recycled.cursor_image;
        cursor_image.clear_for_reuse(QuadBatchKind::Cursor);
        let mut atlas_uploads = recycled.atlas_uploads;
        atlas_uploads.clear();
        let mut instrumentation = RenderInstrumentation {
            damage_region_count: damage_regions.len(),
            animated_region_count: scene.animations.len(),
            ..RenderInstrumentation::default()
        };
        let cursor_text = cursor_text_override(scene, metrics);
        instrumentation.glyphs.atlas_used_bytes = self.atlas.used_bytes();
        instrumentation.glyphs.atlas_capacity_bytes = self.atlas.capacity_bytes();

        for (index, cell) in scene.grid.cells.iter().enumerate() {
            let rect = cell_region_at(cell.position, metrics, scene.content_offset);
            if !damage_rows.intersects_cell(cell.position) {
                continue;
            }

            let content_clip = content_clip_for_cell(scene, index, scene.content_offset);
            let Some(clipped_rect) = clip_optional_rect(rect, content_clip) else {
                continue;
            };

            push_solid_quad(&mut background, clipped_rect, cell.background);
            push_text_decorations(&mut decorations, cell, metrics, rect, content_clip);
        }

        for clipped_cell in damaged_terminal_text_runs(
            &scene.grid.cells,
            &damage_regions,
            metrics,
            scene.content_offset,
            &scene.content_clips,
        ) {
            let cell = clipped_cell.cell;
            let rect = cell_region_at(cell.position, metrics, scene.content_offset);
            let mut glyph_context = GlyphBatchContext {
                atlas_uploads: &mut atlas_uploads,
                instrumentation: &mut instrumentation,
                fonts,
                metrics,
                rect,
                clip_regions: Some(&damage_regions),
                content_clip: clipped_cell
                    .clip
                    .map(|clip| offset_region(clip, scene.content_offset)),
                cursor_text,
            };
            self.push_glyphs(&mut glyphs, &cell, &mut glyph_context)?;
        }

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

        for (overlay, offset, content_clip) in overlays {
            let Some(bounds) =
                clip_optional_rect(offset_region(overlay.bounds, offset), content_clip)
            else {
                continue;
            };
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
                    clip_regions: None,
                    content_clip,
                    cursor_text: None,
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

        for (index, selection) in scene.selections.iter().enumerate() {
            let content_clip = content_clip_for_selection(scene, index, scene.content_offset);
            for position in &selection.cells {
                let rect = cell_region_at(*position, metrics, scene.content_offset);
                let Some(rect) = clip_optional_rect(rect, content_clip) else {
                    continue;
                };
                if damage_rows.intersects_cell(*position) {
                    push_solid_quad(&mut selections, rect, selection.color);
                }
            }
        }

        push_animation_quads(
            &mut cursor_effects,
            &mut cursor_trail,
            &scene.animations,
            &damage_regions,
            scene.content_offset,
            scene.cursor_image.is_some() || scene.cursor_vector.is_some(),
        );

        if let Some(cursor_visual) = scene.cursor
            && cursor_visual.visible
            && scene.cursor_image.is_none()
            && scene.cursor_vector.is_none()
            && !scene.animations.iter().any(|animation| {
                matches!(
                    animation.kind,
                    AnimationKind::CursorSmoothMovement | AnimationKind::CursorTilt
                )
            })
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
                &mut logo_glyphs,
                visual,
                &mut GlyphBatchContext {
                    atlas_uploads: &mut atlas_uploads,
                    instrumentation: &mut instrumentation,
                    fonts,
                    metrics,
                    rect: visual.bounds,
                    clip_regions: None,
                    content_clip: None,
                    cursor_text: None,
                },
            )?;
        }

        instrumentation.draw_call_count = count_non_empty_batches([
            !background.is_empty(),
            !glyphs.is_empty(),
            !logo_glyphs.is_empty(),
            !overlay_glyphs.is_empty(),
            !decorations.is_empty(),
            !cursor_effects.is_empty(),
            !cursor_trail.is_empty(),
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
            logo_glyphs,
            overlay_glyphs,
            decorations,
            cursor_effects,
            cursor_trail,
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
        self.prepare_full_reusing(scene, fonts, None)
    }

    pub fn prepare_full_reusing(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
        recycled: Option<PreparedRenderBatches>,
    ) -> Result<PreparedRenderBatches, RendererError> {
        let metrics = fonts.cell_metrics()?;
        let full_damage = [scene_grid_region(scene, metrics)];
        self.prepare_reusing_with_damage(scene, fonts, recycled, Some(&full_damage))
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
        if cell.style.hidden {
            return Ok(());
        }

        if let Some(powerline) = SolidPowerlineGlyph::from_text(&cell.text) {
            let key = AtlasCacheKey::PowerlineCap {
                codepoint: powerline.codepoint(),
                width: context.rect.width,
                height: context.rect.height,
            };
            let atlas_hit = self.atlas.entry(key);
            let entry = if let Some(entry) = atlas_hit {
                context.instrumentation.glyphs.cache_hits =
                    context.instrumentation.glyphs.cache_hits.saturating_add(1);
                Some(entry)
            } else {
                context.instrumentation.glyphs.cache_misses = context
                    .instrumentation
                    .glyphs
                    .cache_misses
                    .saturating_add(1);
                let bitmap = rasterize_solid_powerline_glyph(
                    powerline,
                    context.rect.width,
                    context.rect.height,
                );
                match self.atlas.allocate(key, &bitmap) {
                    Some(entry) => {
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
                        Some(entry)
                    }
                    None => {
                        self.atlas_exhausted = true;
                        None
                    }
                }
            };
            if let Some(entry) = entry {
                push_scissor_culled_glyph_quad(
                    glyphs,
                    context.rect,
                    entry,
                    self.atlas.dimensions(),
                    glyph_color(context.cursor_text, context.rect, cell.foreground),
                    false,
                    GlyphQuadClip {
                        content: context.content_clip,
                        damage_regions: context.clip_regions,
                    },
                );
            }
            return Ok(());
        }

        if cell.text.trim().is_empty() {
            return Ok(());
        }

        let text: &str = cell.text.as_ref();
        let query = GlyphRunQuery {
            font_generation: context.fonts.generation_id(),
            text,
            size_millipoints: (context.metrics.font_size * 1000.0).round().max(1.0) as u32,
            bold: cell.style.bold,
            italic: cell.style.italic,
        };
        self.glyph_run_clock = self.glyph_run_clock.wrapping_add(1);
        let access = self.glyph_run_clock;
        let run = if let Some(cached) = self.glyph_runs.get_mut(&query) {
            cached.last_used = access;
            Arc::clone(&cached.run)
        } else {
            while self.glyph_runs.len() >= self.max_glyph_runs {
                let Some(oldest) = self
                    .glyph_runs
                    .iter()
                    .min_by_key(|(_, cached)| cached.last_used)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                self.glyph_runs.remove(&oldest);
            }
            let run: Arc<[GlyphRunItem]> = context
                .fonts
                .shape_text(&cell.text, cell.style.bold, cell.style.italic)?
                .glyphs
                .into();
            let run_key = GlyphRunKey {
                font_generation: query.font_generation,
                text: cell.text.clone(),
                size_millipoints: query.size_millipoints,
                bold: query.bold,
                italic: query.italic,
            };
            self.glyph_runs.insert(
                run_key,
                CachedGlyphRun {
                    run: Arc::clone(&run),
                    last_used: access,
                },
            );
            run
        };

        let mut pen_x = context.rect.x as f32;
        let mut pen_y = glyph_baseline_y(context.rect, context.metrics);
        for item in run.iter().copied() {
            let coverage = RenderRect {
                x: (pen_x + item.x_offset).floor() as i32,
                y: context.rect.y,
                width: item
                    .x_advance
                    .abs()
                    .ceil()
                    .max(context.metrics.cell_width)
                    .max(1.0) as u32,
                height: context.rect.height.max(1),
            };
            if context
                .clip_regions
                .is_some_and(|regions| !intersects_any(coverage, regions))
            {
                pen_x += item.x_advance;
                pen_y += item.y_advance;
                continue;
            }
            let key = item.key;
            let (bitmap, cache_hit) = self.glyph_cache.get_or_insert_with_status(key, || {
                context
                    .fonts
                    .rasterize_glyph(key)
                    .unwrap_or_else(|_| missing_glyph_bitmap(context.metrics))
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

            if let Some((entry, atlas_hit)) = self.atlas.allocate_with_status(key, bitmap.as_ref())
            {
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
                let glyph_rect = RenderRect {
                    x: (pen_x + item.x_offset).round() as i32 + bitmap.offset_x,
                    y: (pen_y - item.y_offset).round() as i32 + bitmap.offset_y,
                    width: bitmap.width,
                    height: bitmap.height,
                };
                push_scissor_culled_glyph_quad(
                    glyphs,
                    glyph_rect,
                    entry,
                    self.atlas.dimensions(),
                    glyph_color(context.cursor_text, glyph_rect, cell.foreground),
                    bitmap.format == GlyphBitmapFormat::Rgba,
                    GlyphQuadClip {
                        content: context.content_clip,
                        damage_regions: context.clip_regions,
                    },
                );
            } else {
                self.atlas_exhausted = true;
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
        self.push_glyphs(glyphs, &cell, context)
    }

    fn push_window_chrome(
        &mut self,
        geometry: &mut QuadBatch,
        glyphs: &mut GlyphBatch,
        logo_glyphs: &mut GlyphBatch,
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
                    logo_glyphs,
                    logo_bounds,
                    entry,
                    self.atlas.dimensions(),
                    with_fixed_opacity(RenderColor::rgb(255, 255, 255), visual.opacity),
                    true,
                );
                title_x = title_x.saturating_add(logo_bounds.width as i32 + 8);
            } else {
                self.atlas_exhausted = true;
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

#[cfg(test)]
fn terminal_text_runs(cells: &[RenderCell]) -> Vec<RenderCell> {
    terminal_text_runs_from_iter(cells.iter())
}

#[cfg(test)]
fn terminal_text_runs_from_iter<'a>(
    cells: impl IntoIterator<Item = &'a RenderCell>,
) -> Vec<RenderCell> {
    let mut runs: Vec<RenderCell> = Vec::new();

    for cell in cells {
        let can_join = runs.last().is_some_and(|run| {
            run.text.is_ascii()
                && cell.text.is_ascii()
                && run.position.row == cell.position.row
                && run
                    .position
                    .col
                    .saturating_add(run_column_span(run.text.as_ref()))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SolidPowerlineGlyph {
    RoundedRight,
    RoundedLeft,
    TriangleRight,
    TriangleLeft,
}

impl SolidPowerlineGlyph {
    fn from_text(text: &str) -> Option<Self> {
        match text {
            "\u{e0b4}" => Some(Self::RoundedRight),
            "\u{e0b6}" => Some(Self::RoundedLeft),
            "\u{e0b0}" => Some(Self::TriangleRight),
            "\u{e0b2}" => Some(Self::TriangleLeft),
            _ => None,
        }
    }

    const fn codepoint(self) -> char {
        match self {
            Self::RoundedRight => '\u{e0b4}',
            Self::RoundedLeft => '\u{e0b6}',
            Self::TriangleRight => '\u{e0b0}',
            Self::TriangleLeft => '\u{e0b2}',
        }
    }
}

fn rasterize_solid_powerline_glyph(
    powerline: SolidPowerlineGlyph,
    width: u32,
    height: u32,
) -> GlyphBitmap {
    const SAMPLE_GRID: u32 = 4;

    let width = width.max(1);
    let height = height.max(1);
    let mut pixels = vec![0_u8; (width * height) as usize];
    let samples_per_pixel = SAMPLE_GRID * SAMPLE_GRID;
    for y in 0..height {
        for x in 0..width {
            let mut covered = 0_u32;
            for sample_y in 0..SAMPLE_GRID {
                let normalized_y =
                    (y as f32 + (sample_y as f32 + 0.5) / SAMPLE_GRID as f32) / height as f32;
                let centered_y = normalized_y.mul_add(2.0, -1.0);
                for sample_x in 0..SAMPLE_GRID {
                    let normalized_x =
                        (x as f32 + (sample_x as f32 + 0.5) / SAMPLE_GRID as f32) / width as f32;
                    let inside = match powerline {
                        SolidPowerlineGlyph::RoundedRight => {
                            normalized_x <= (1.0 - centered_y * centered_y).max(0.0).sqrt()
                        }
                        SolidPowerlineGlyph::RoundedLeft => {
                            normalized_x >= 1.0 - (1.0 - centered_y * centered_y).max(0.0).sqrt()
                        }
                        SolidPowerlineGlyph::TriangleRight => {
                            normalized_x <= 1.0 - centered_y.abs()
                        }
                        SolidPowerlineGlyph::TriangleLeft => normalized_x >= centered_y.abs(),
                    };
                    covered += u32::from(inside);
                }
            }
            pixels[(y * width + x) as usize] =
                ((covered * u32::from(u8::MAX)) / samples_per_pixel) as u8;
        }
    }

    GlyphBitmap {
        width,
        height,
        offset_x: 0,
        offset_y: 0,
        advance_width: width as f32,
        pixels,
        format: GlyphBitmapFormat::Alpha,
    }
}

fn damaged_terminal_text_runs(
    cells: &[RenderCell],
    damage_regions: &[DamageRegion],
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
    content_clips: &[render_core::RenderContentClip],
) -> Vec<ClippedRenderCell> {
    damaged_terminal_text_runs_with_stats(cells, damage_regions, metrics, offset, content_clips)
        .runs
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClippedRenderCell {
    cell: RenderCell,
    clip: Option<RenderRect>,
}

struct DamagedTerminalTextRuns {
    runs: Vec<ClippedRenderCell>,
    #[cfg_attr(not(test), allow(dead_code))]
    source_cells: usize,
}

fn damaged_terminal_text_runs_with_stats(
    cells: &[RenderCell],
    damage_regions: &[DamageRegion],
    metrics: CellMetrics,
    offset: render_core::RenderOffset,
    content_clips: &[render_core::RenderContentClip],
) -> DamagedTerminalTextRuns {
    let (first_row, row_count) = cell_row_bounds(cells);
    let damage_rows = DamageRows::new(damage_regions, metrics, offset, first_row, row_count);
    let mut source_cells = 0;
    let mut runs: Vec<ClippedRenderCell> = Vec::new();
    for (index, cell) in cells.iter().enumerate().filter(|(_, cell)| {
        let selected = damage_rows.span(cell.position.row).is_some();
        source_cells += usize::from(selected);
        selected
    }) {
        let clip = content_clip_for_cell_ranges(content_clips, index);
        let can_join = runs.last().is_some_and(|run| {
            run.clip == clip
                && run.cell.text.is_ascii()
                && cell.text.is_ascii()
                && run.cell.position.row == cell.position.row
                && run
                    .cell
                    .position
                    .col
                    .saturating_add(run_column_span(run.cell.text.as_ref()))
                    == cell.position.col
                && run.cell.foreground == cell.foreground
                && run.cell.background == cell.background
                && run.cell.style == cell.style
        });
        if can_join {
            runs.last_mut()
                .expect("run exists")
                .cell
                .text
                .push_str(&cell.text);
        } else {
            runs.push(ClippedRenderCell {
                cell: cell.clone(),
                clip,
            });
        }
    }
    let runs = runs
        .into_iter()
        .filter(|run| {
            let text: &str = run.cell.text.as_ref();
            let width = UnicodeWidthStr::width(text).max(1);
            let end = run
                .cell
                .position
                .col
                .saturating_add(u16::try_from(width.saturating_sub(1)).unwrap_or(u16::MAX));
            damage_rows.intersects_columns(run.cell.position.row, run.cell.position.col, end)
        })
        .collect();
    DamagedTerminalTextRuns { runs, source_cells }
}

fn cell_row_bounds(cells: &[RenderCell]) -> (i64, usize) {
    let Some(first) = cells.iter().map(|cell| cell.position.row).min() else {
        return (0, 0);
    };
    let last = cells
        .iter()
        .map(|cell| cell.position.row)
        .max()
        .unwrap_or(first);
    (
        first,
        usize::try_from(last.saturating_sub(first).saturating_add(1)).unwrap_or(usize::MAX),
    )
}

#[cfg(any(test, feature = "conformance"))]
fn text_run_region(cell: &RenderCell, metrics: CellMetrics) -> RenderRect {
    let mut rect = cell_region(cell.position, metrics);
    let text: &str = cell.text.as_ref();
    let cells = UnicodeWidthStr::width(text).max(1) as u32;
    let end = cell_axis_bounds(
        u32::from(cell.position.col).saturating_add(cells),
        metrics.cell_width,
    )
    .0;
    rect.width = end.saturating_sub(rect.x).max(1) as u32;
    rect
}

/// Columns a joined text run occupies.
///
/// Runs only ever join ASCII cells, so the byte length is also the column count
/// and this stays O(1) — counting characters per cell made run building
/// quadratic in the row width. The assertion keeps that equivalence from being
/// broken silently if the join conditions are ever relaxed to non-ASCII text.
fn run_column_span(text: &str) -> u16 {
    debug_assert!(
        text.is_ascii(),
        "run column spans assume ASCII-only joins, got {text:?}"
    );
    u16::try_from(text.len()).unwrap_or(u16::MAX)
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

fn content_clip_for_cell_ranges(
    clips: &[render_core::RenderContentClip],
    index: usize,
) -> Option<RenderRect> {
    clips
        .iter()
        .find(|clip| clip.cells.contains(index))
        .map(|clip| clip.bounds)
}

fn content_clip_for_cell(
    scene: &RenderScene,
    index: usize,
    offset: render_core::RenderOffset,
) -> Option<RenderRect> {
    content_clip_for_cell_ranges(&scene.content_clips, index)
        .map(|clip| offset_region(clip, offset))
}

fn content_clip_for_search(
    scene: &RenderScene,
    index: usize,
    offset: render_core::RenderOffset,
) -> Option<RenderRect> {
    scene
        .content_clips
        .iter()
        .find(|clip| clip.search_highlights.contains(index))
        .map(|clip| offset_region(clip.bounds, offset))
}

fn content_clip_for_semantic(
    scene: &RenderScene,
    index: usize,
    offset: render_core::RenderOffset,
) -> Option<RenderRect> {
    scene
        .content_clips
        .iter()
        .find(|clip| clip.semantic_overlays.contains(index))
        .map(|clip| offset_region(clip.bounds, offset))
}

fn content_clip_for_selection(
    scene: &RenderScene,
    index: usize,
    offset: render_core::RenderOffset,
) -> Option<RenderRect> {
    scene
        .content_clips
        .iter()
        .find(|clip| clip.selections.contains(index))
        .map(|clip| offset_region(clip.bounds, offset))
}

fn glyph_color(
    cursor_text: Option<CursorTextOverride>,
    glyph_bounds: RenderRect,
    fallback: RenderColor,
) -> RenderColor {
    cursor_text
        .filter(|override_| rects_intersect(glyph_bounds, override_.bounds))
        .map_or(fallback, |override_| override_.color)
}

fn cursor_text_override(scene: &RenderScene, metrics: CellMetrics) -> Option<CursorTextOverride> {
    let cursor = scene.cursor?;
    if !cursor.visible
        || !matches!(
            cursor.shape,
            RenderCursorShape::Block
                | RenderCursorShape::Custom
                | RenderCursorShape::CustomStaticShape
        )
    {
        return None;
    }
    Some(CursorTextOverride {
        bounds: cell_region_at(cursor.position, metrics, scene.content_offset),
        color: cursor.text_color?,
    })
}

fn overlay_draws_behind_terminal_text(kind: OverlayKind) -> bool {
    matches!(
        kind,
        OverlayKind::Decoration
            | OverlayKind::PromptDecoration
            | OverlayKind::CommandBlock
            | OverlayKind::InputOutputGroup
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

#[cfg(test)]
fn rect_contains(outer: RenderRect, inner: RenderRect) -> bool {
    let outer_x1 = i64::from(outer.x) + i64::from(outer.width);
    let outer_y1 = i64::from(outer.y) + i64::from(outer.height);
    let inner_x1 = i64::from(inner.x) + i64::from(inner.width);
    let inner_y1 = i64::from(inner.y) + i64::from(inner.height);

    outer.x <= inner.x && outer.y <= inner.y && outer_x1 >= inner_x1 && outer_y1 >= inner_y1
}

fn rect_intersection(a: RenderRect, b: RenderRect) -> Option<RenderRect> {
    let x0 = i64::from(a.x).max(i64::from(b.x));
    let y0 = i64::from(a.y).max(i64::from(b.y));
    let x1 = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width));
    let y1 = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height));
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    Some(RenderRect {
        x: i32::try_from(x0).ok()?,
        y: i32::try_from(y0).ok()?,
        width: u32::try_from(x1 - x0).ok()?,
        height: u32::try_from(y1 - y0).ok()?,
    })
}

fn clip_optional_rect(rect: RenderRect, clip: Option<RenderRect>) -> Option<RenderRect> {
    clip.map_or(Some(rect), |clip| rect_intersection(rect, clip))
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
    let uv = atlas_uv_bounds(atlas_entry, atlas_dimensions);

    push_quad(
        &mut batch.vertices,
        &mut batch.indices,
        rect,
        [[uv.0, uv.1], [uv.2, uv.1], [uv.2, uv.3], [uv.0, uv.3]],
        color,
    );
    if color_bitmap {
        for vertex in batch.vertices.iter_mut().rev().take(4) {
            vertex.color[3] = -vertex.color[3].max(f32::EPSILON);
        }
    }
    batch.glyph_count = batch.glyph_count.saturating_add(1);
}

fn push_clipped_glyph_quad(
    batch: &mut GlyphBatch,
    rect: RenderRect,
    atlas_entry: AtlasEntry,
    atlas_dimensions: (u32, u32),
    color: RenderColor,
    color_bitmap: bool,
    clip: Option<RenderRect>,
) {
    let Some(clipped) = clip_optional_rect(rect, clip) else {
        return;
    };
    if clipped == rect {
        push_glyph_quad(
            batch,
            rect,
            atlas_entry,
            atlas_dimensions,
            color,
            color_bitmap,
        );
        return;
    }

    let uv = atlas_uv_bounds(atlas_entry, atlas_dimensions);
    let x0 = (clipped.x - rect.x) as f32 / rect.width.max(1) as f32;
    let y0 = (clipped.y - rect.y) as f32 / rect.height.max(1) as f32;
    let x1 = (clipped.x + clipped.width as i32 - rect.x) as f32 / rect.width.max(1) as f32;
    let y1 = (clipped.y + clipped.height as i32 - rect.y) as f32 / rect.height.max(1) as f32;
    let lerp = |start: f32, end: f32, value: f32| start + (end - start) * value;
    let clipped_uv = [
        [lerp(uv.0, uv.2, x0), lerp(uv.1, uv.3, y0)],
        [lerp(uv.0, uv.2, x1), lerp(uv.1, uv.3, y0)],
        [lerp(uv.0, uv.2, x1), lerp(uv.1, uv.3, y1)],
        [lerp(uv.0, uv.2, x0), lerp(uv.1, uv.3, y1)],
    ];
    push_quad(
        &mut batch.vertices,
        &mut batch.indices,
        clipped,
        clipped_uv,
        color,
    );
    if color_bitmap {
        for vertex in batch.vertices.iter_mut().rev().take(4) {
            vertex.color[3] = -vertex.color[3].max(f32::EPSILON);
        }
    }
    batch.glyph_count = batch.glyph_count.saturating_add(1);
}

#[derive(Clone, Copy)]
struct GlyphQuadClip<'a> {
    content: Option<RenderRect>,
    damage_regions: Option<&'a [DamageRegion]>,
}

fn push_scissor_culled_glyph_quad(
    batch: &mut GlyphBatch,
    rect: RenderRect,
    atlas_entry: AtlasEntry,
    atlas_dimensions: (u32, u32),
    color: RenderColor,
    color_bitmap: bool,
    clip: GlyphQuadClip<'_>,
) {
    let Some(content_bounds) = clip_optional_rect(rect, clip.content) else {
        return;
    };
    if clip
        .damage_regions
        .is_some_and(|regions| !intersects_any(content_bounds, regions))
    {
        return;
    }
    push_clipped_glyph_quad(
        batch,
        rect,
        atlas_entry,
        atlas_dimensions,
        color,
        color_bitmap,
        Some(content_bounds),
    );
}

fn atlas_uv_bounds(entry: AtlasEntry, atlas_dimensions: (u32, u32)) -> (f32, f32, f32, f32) {
    let atlas_width = atlas_dimensions.0.max(1) as f32;
    let atlas_height = atlas_dimensions.1.max(1) as f32;
    let x0 = entry.x as f32 / atlas_width;
    let y0 = entry.y as f32 / atlas_height;
    let x1 = entry.x.saturating_add(entry.width) as f32 / atlas_width;
    let y1 = entry.y.saturating_add(entry.height) as f32 / atlas_height;
    (x0, y0, x1, y1)
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

fn push_positioned_quad(batch: &mut QuadBatch, positions: [[f32; 2]; 4], color: RenderColor) {
    let Ok(base) = u32::try_from(batch.vertices.len()) else {
        return;
    };
    let color = color_to_f32(color);
    batch
        .vertices
        .extend(positions.map(|position_px| BatchVertex {
            position_px,
            uv: [0.0, 0.0],
            color,
        }));
    batch
        .indices
        .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
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
    metrics: CellMetrics,
    rect: RenderRect,
    content_clip: Option<RenderRect>,
) {
    if cell.style.hidden {
        return;
    }

    if cell.style.underline
        && let Some(rect) = clip_optional_rect(
            metric_decoration_rect(rect, metrics, metrics.underline_position),
            content_clip,
        )
    {
        push_solid_quad(decorations, rect, cell.foreground);
    }

    if cell.style.strikethrough
        && let Some(rect) = clip_optional_rect(
            metric_decoration_rect(rect, metrics, metrics.strikethrough_position),
            content_clip,
        )
    {
        push_solid_quad(decorations, rect, cell.foreground);
    }

    if cell.style.overline
        && let Some(rect) = clip_optional_rect(
            RenderRect {
                y: rect.y,
                height: 1,
                ..rect
            },
            content_clip,
        )
    {
        push_solid_quad(decorations, rect, cell.foreground);
    }
}

fn glyph_baseline_y(rect: RenderRect, metrics: CellMetrics) -> f32 {
    rect.y as f32 + vertical_metric_offset(rect, metrics) + metrics.baseline
}

fn vertical_metric_offset(rect: RenderRect, metrics: CellMetrics) -> f32 {
    (rect.height as f32 - metrics.cell_height) * 0.5
}

fn metric_decoration_rect(rect: RenderRect, metrics: CellMetrics, position: f32) -> RenderRect {
    let height = metrics
        .decoration_thickness
        .round()
        .max(1.0)
        .min(rect.height.max(1) as f32) as u32;
    let requested_y =
        (rect.y as f32 + vertical_metric_offset(rect, metrics) + position).round() as i32;
    let maximum_y = rect
        .y
        .saturating_add(rect.height.saturating_sub(height) as i32);
    RenderRect {
        y: requested_y.clamp(rect.y, maximum_y),
        height,
        ..rect
    }
}

fn missing_glyph_bitmap(metrics: CellMetrics) -> GlyphBitmap {
    let mut bitmap = GlyphBitmap::missing(metrics.cell_width, metrics.cell_height.ceil() as u32);
    bitmap.offset_y = -metrics.baseline.round() as i32;
    bitmap
}

fn push_animation_quads(
    effects_batch: &mut QuadBatch,
    trail_batch: &mut QuadBatch,
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
            AnimationKind::CursorTilt => {
                if !image_cursor_active {
                    let target = rect_quad(end_region);
                    let progress = ease_out_cubic(progress);
                    let positions = animation
                        .quad
                        .map(|quad| offset_animation_quad_pixels(quad, offset))
                        .unwrap_or(target);
                    let positions = std::array::from_fn(|index| {
                        [
                            positions[index][0]
                                + (target[index][0] - positions[index][0]) * progress,
                            positions[index][1]
                                + (target[index][1] - positions[index][1]) * progress,
                        ]
                    });
                    push_positioned_quad(effects_batch, positions, color);
                }
            }
            AnimationKind::CursorElasticExtension => {
                if !image_cursor_active {
                    let target = rect_quad(end_region);
                    let progress = ease_out_cubic(progress);
                    let positions = animation
                        .quad
                        .map(|quad| offset_animation_quad_pixels(quad, offset))
                        .unwrap_or(target);
                    let positions = std::array::from_fn(|index| {
                        [
                            positions[index][0]
                                + (target[index][0] - positions[index][0]) * progress,
                            positions[index][1]
                                + (target[index][1] - positions[index][1]) * progress,
                        ]
                    });
                    push_positioned_quad(effects_batch, positions, color);
                }
            }
            AnimationKind::CursorTypingStretch => {
                push_rounded_quads(
                    effects_batch,
                    stretch_region(end_region, progress),
                    2,
                    color,
                );
            }
            AnimationKind::CursorSmoothMovement => {
                if !image_cursor_active {
                    push_rounded_quads(
                        effects_batch,
                        interpolate_region(start_region, end_region, ease_out_cubic(progress)),
                        2,
                        color,
                    );
                }
            }
            AnimationKind::CursorTrail => {
                push_clipped_cursor_trail(
                    trail_batch,
                    animation.quad.map_or_else(
                        || cursor_trail_quad(start_region, end_region, animation.elapsed),
                        |quad| offset_animation_quad_pixels(quad, offset),
                    ),
                    start_region,
                    end_region,
                    color,
                );
            }
            AnimationKind::CursorTypingPulse => {
                let expansion = ((1.0 - progress) * 4.0).round() as i32;
                push_rounded_stroke_quads(
                    effects_batch,
                    expand_region(end_region, expansion),
                    1,
                    3,
                    color,
                );
            }
            AnimationKind::CursorBlinkEasing => {
                push_rounded_quads(effects_batch, end_region, 2, color);
            }
            AnimationKind::CursorGlow => {
                for expansion in [2, 4, 6] {
                    let mut layer = color;
                    layer.alpha /= expansion as u8;
                    push_rounded_quads(
                        effects_batch,
                        expand_region(end_region, expansion),
                        4,
                        layer,
                    );
                }
            }
            AnimationKind::CursorShadow => {
                let mut shadow = end_region;
                shadow.x = shadow.x.saturating_add(2);
                shadow.y = shadow.y.saturating_add(2);
                push_rounded_quads(effects_batch, shadow, 2, color);
            }
            AnimationKind::OverlayTransition => {
                push_solid_quad(effects_batch, affected_region, color);
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum TrailClipBoundary {
    MaxX(f32),
    MinX(f32),
    MaxY(f32),
    MinY(f32),
}

fn push_clipped_cursor_trail(
    batch: &mut QuadBatch,
    mut positions: [[f32; 2]; 4],
    start: RenderRect,
    target: RenderRect,
    color: RenderColor,
) {
    let delta_x = target.x - start.x;
    let delta_y = target.y - start.y;
    let boundary = if delta_x.unsigned_abs() >= delta_y.unsigned_abs() && delta_x != 0 {
        if delta_x > 0 {
            positions[1] = [target.x as f32, target.y as f32];
            positions[2] = [target.x as f32, (target.y + target.height as i32) as f32];
            TrailClipBoundary::MaxX(target.x as f32)
        } else {
            let target_right = (target.x + target.width as i32) as f32;
            positions[0] = [target_right, target.y as f32];
            positions[3] = [target_right, (target.y + target.height as i32) as f32];
            TrailClipBoundary::MinX((target.x + target.width as i32) as f32)
        }
    } else if delta_y != 0 {
        if delta_y > 0 {
            positions[2] = [(target.x + target.width as i32) as f32, target.y as f32];
            positions[3] = [target.x as f32, target.y as f32];
            TrailClipBoundary::MaxY(target.y as f32)
        } else {
            let target_bottom = (target.y + target.height as i32) as f32;
            positions[0] = [target.x as f32, target_bottom];
            positions[1] = [(target.x + target.width as i32) as f32, target_bottom];
            TrailClipBoundary::MinY((target.y + target.height as i32) as f32)
        }
    } else {
        return;
    };

    let (clipped, clipped_len) = clip_trail_polygon(positions, boundary);
    push_solid_polygon(batch, &clipped[..clipped_len], color);
}

fn clip_trail_polygon(
    positions: [[f32; 2]; 4],
    boundary: TrailClipBoundary,
) -> ([[f32; 2]; 6], usize) {
    let mut output = [[0.0; 2]; 6];
    let mut output_len = 0;
    let mut previous = positions[3];
    let mut previous_inside = trail_point_inside(previous, boundary);

    for current in positions {
        let current_inside = trail_point_inside(current, boundary);
        if current_inside != previous_inside {
            output[output_len] = trail_boundary_intersection(previous, current, boundary);
            output_len += 1;
        }
        if current_inside {
            output[output_len] = current;
            output_len += 1;
        }
        previous = current;
        previous_inside = current_inside;
    }

    (output, output_len)
}

fn trail_point_inside(point: [f32; 2], boundary: TrailClipBoundary) -> bool {
    match boundary {
        TrailClipBoundary::MaxX(limit) => point[0] <= limit,
        TrailClipBoundary::MinX(limit) => point[0] >= limit,
        TrailClipBoundary::MaxY(limit) => point[1] <= limit,
        TrailClipBoundary::MinY(limit) => point[1] >= limit,
    }
}

fn trail_boundary_intersection(
    start: [f32; 2],
    end: [f32; 2],
    boundary: TrailClipBoundary,
) -> [f32; 2] {
    let (axis, limit) = match boundary {
        TrailClipBoundary::MaxX(limit) | TrailClipBoundary::MinX(limit) => (0, limit),
        TrailClipBoundary::MaxY(limit) | TrailClipBoundary::MinY(limit) => (1, limit),
    };
    let denominator = end[axis] - start[axis];
    let t = if denominator.abs() <= f32::EPSILON {
        0.0
    } else {
        ((limit - start[axis]) / denominator).clamp(0.0, 1.0)
    };
    [
        start[0] + (end[0] - start[0]) * t,
        start[1] + (end[1] - start[1]) * t,
    ]
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

fn push_solid_polygon(batch: &mut QuadBatch, positions: &[[f32; 2]], color: RenderColor) {
    if positions.len() < 3 {
        return;
    }
    let twice_area = positions
        .iter()
        .zip(positions.iter().cycle().skip(1))
        .take(positions.len())
        .map(|(current, next)| current[0] * next[1] - next[0] * current[1])
        .sum::<f32>();
    if twice_area.abs() <= 0.01 {
        return;
    }
    for index in 1..positions.len() - 1 {
        push_positioned_quad(
            batch,
            [
                positions[0],
                positions[index],
                positions[index + 1],
                positions[index + 1],
            ],
            color,
        );
    }
}

fn animation_color(animation: AnimationHandle) -> RenderColor {
    let base_alpha: u8 = match animation.kind {
        AnimationKind::CursorSmoothMovement => 230,
        AnimationKind::CursorTypingPulse => 120,
        AnimationKind::CursorTypingStretch => 180,
        AnimationKind::CursorTilt => 255,
        AnimationKind::CursorElasticExtension => 210,
        AnimationKind::CursorTrail => 255,
        AnimationKind::CursorBlinkEasing => 200,
        AnimationKind::CursorGlow => 96,
        AnimationKind::CursorShadow => 80,
        AnimationKind::OverlayTransition => 48,
    };
    let alpha = if matches!(
        animation.kind,
        AnimationKind::CursorTilt | AnimationKind::CursorTrail
    ) {
        base_alpha
    } else if let Some(remaining) = animation.remaining {
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

fn rect_quad(rect: RenderRect) -> [[f32; 2]; 4] {
    let left = rect.x as f32;
    let top = rect.y as f32;
    let right = left + rect.width as f32;
    let bottom = top + rect.height as f32;
    [[left, top], [right, top], [right, bottom], [left, bottom]]
}

fn quad_bounds(corners: [[f32; 2]; 4]) -> RenderRect {
    let minimum_x = corners
        .iter()
        .map(|corner| corner[0])
        .fold(f32::INFINITY, f32::min)
        .floor();
    let minimum_y = corners
        .iter()
        .map(|corner| corner[1])
        .fold(f32::INFINITY, f32::min)
        .floor();
    let maximum_x = corners
        .iter()
        .map(|corner| corner[0])
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil();
    let maximum_y = corners
        .iter()
        .map(|corner| corner[1])
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil();
    RenderRect {
        x: minimum_x.clamp(i32::MIN as f32, i32::MAX as f32) as i32,
        y: minimum_y.clamp(i32::MIN as f32, i32::MAX as f32) as i32,
        width: (maximum_x - minimum_x).max(0.0).min(u32::MAX as f32) as u32,
        height: (maximum_y - minimum_y).max(0.0).min(u32::MAX as f32) as u32,
    }
}

fn cursor_trail_move_within_threshold(
    from: RenderRect,
    to: RenderRect,
    cell_size: [u32; 2],
    threshold_cells: u16,
) -> bool {
    let cell_width = cell_size[0].max(1);
    let cell_height = cell_size[1].max(1);
    let horizontal_cells = from.x.abs_diff(to.x) / cell_width;
    let vertical_cells = from.y.abs_diff(to.y) / cell_height;
    horizontal_cells <= u32::from(threshold_cells) && vertical_cells <= u32::from(threshold_cells)
}

fn animation_quad(corners: [[f32; 2]; 4]) -> AnimationQuad {
    const SUBPIXELS_PER_PIXEL: f32 = 256.0;
    AnimationQuad {
        corners_subpixels: corners.map(|corner| {
            [
                (corner[0] * SUBPIXELS_PER_PIXEL)
                    .round()
                    .clamp(i32::MIN as f32, i32::MAX as f32) as i32,
                (corner[1] * SUBPIXELS_PER_PIXEL)
                    .round()
                    .clamp(i32::MIN as f32, i32::MAX as f32) as i32,
            ]
        }),
    }
}

fn animation_quad_pixels(quad: AnimationQuad) -> [[f32; 2]; 4] {
    const PIXELS_PER_SUBPIXEL: f32 = 1.0 / 256.0;
    quad.corners_subpixels.map(|corner| {
        [
            corner[0] as f32 * PIXELS_PER_SUBPIXEL,
            corner[1] as f32 * PIXELS_PER_SUBPIXEL,
        ]
    })
}

fn offset_animation_quad_pixels(
    quad: AnimationQuad,
    offset: render_core::RenderOffset,
) -> [[f32; 2]; 4] {
    animation_quad_pixels(quad)
        .map(|corner| [corner[0] + offset.x as f32, corner[1] + offset.y as f32])
}

fn cursor_trail_quad(start: RenderRect, end: RenderRect, elapsed: Duration) -> [[f32; 2]; 4] {
    const FAST_DECAY_SECONDS: f32 = 0.1;
    const SLOW_DECAY_SECONDS: f32 = 0.4;

    let start = rect_quad(start);
    let target = rect_quad(end);
    let start_center = [
        (start[0][0] + start[2][0]) * 0.5,
        (start[0][1] + start[2][1]) * 0.5,
    ];
    let target_center = [
        (target[0][0] + target[2][0]) * 0.5,
        (target[0][1] + target[2][1]) * 0.5,
    ];
    let movement = [
        target_center[0] - start_center[0],
        target_center[1] - start_center[1],
    ];
    let movement_length = movement[0].hypot(movement[1]);
    if movement_length <= f32::EPSILON {
        return target;
    }
    let direction = [movement[0] / movement_length, movement[1] / movement_length];
    let projections = target.map(|point| {
        (point[0] - target_center[0]) * direction[0] + (point[1] - target_center[1]) * direction[1]
    });
    let projection_min = projections.iter().copied().fold(f32::INFINITY, f32::min);
    let projection_max = projections
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let projection_span = projection_max - projection_min;
    let elapsed = elapsed.as_secs_f32();

    std::array::from_fn(|index| {
        let leading = if projection_span <= f32::EPSILON {
            0.0
        } else {
            ((projections[index] - projection_min) / projection_span).clamp(0.0, 1.0)
        };
        let decay = SLOW_DECAY_SECONDS + (FAST_DECAY_SECONDS - SLOW_DECAY_SECONDS) * leading;
        let remaining = (-10.0 * elapsed / decay).exp2();
        [
            target[index][0] + (start[index][0] - target[index][0]) * remaining,
            target[index][1] + (start[index][1] - target[index][1]) * remaining,
        ]
    })
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
    let rect = offset_region(cursor_visual_region(cursor, metrics), offset);
    if !intersects_any(rect, damage_regions) {
        return;
    }

    let thickness = u32::from(cursor.thickness_percent.clamp(1, 100));
    match cursor.shape {
        RenderCursorShape::Block
        | RenderCursorShape::Beam
        | RenderCursorShape::Underline
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
    push_rounded_sdf_quad(batch, rect, radius, 0, color);
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
    if radius == 0 {
        push_stroke_quads(batch, rect, line, color);
        return;
    }
    push_rounded_sdf_quad(batch, rect, radius, line, color);
}

fn push_rounded_sdf_quad(
    batch: &mut QuadBatch,
    rect: RenderRect,
    radius: u32,
    line: u32,
    color: RenderColor,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let Ok(base) = u32::try_from(batch.vertices.len()) else {
        return;
    };
    let plain = color_to_f32(color);
    let metadata = [
        rect.width as f32 * 2.0 + plain[0],
        rect.height as f32 * 2.0 + plain[1],
        radius as f32 * 2.0 + plain[2],
        -(1.0 + line as f32 * 2.0 + plain[3]),
    ];
    let x0 = rect.x as f32;
    let y0 = rect.y as f32;
    let x1 = x0 + rect.width as f32;
    let y1 = y0 + rect.height as f32;
    let width = rect.width as f32;
    let height = rect.height as f32;
    batch.vertices.extend([
        BatchVertex {
            position_px: [x0, y0],
            uv: [0.0, 0.0],
            color: metadata,
        },
        BatchVertex {
            position_px: [x1, y0],
            uv: [width, 0.0],
            color: metadata,
        },
        BatchVertex {
            position_px: [x1, y1],
            uv: [width, height],
            color: metadata,
        },
        BatchVertex {
            position_px: [x0, y1],
            uv: [0.0, height],
            color: metadata,
        },
    ]);
    batch
        .indices
        .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
}

#[cfg(any(test, feature = "conformance"))]
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

fn pixel_buffer_len(width: u32, height: u32, channels: u32) -> Option<usize> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(u64::from(channels))?;
    usize::try_from(bytes).ok()
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

    pub fn prepare_batches_reusing(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
        recycled: Option<PreparedRenderBatches>,
    ) -> Result<PreparedRenderBatches, RendererError> {
        self.batch_planner.prepare_reusing(scene, fonts, recycled)
    }

    pub fn prepare_full_batches(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
    ) -> Result<PreparedRenderBatches, RendererError> {
        self.batch_planner.prepare_full(scene, fonts)
    }

    pub fn prepare_full_batches_reusing(
        &mut self,
        scene: &RenderScene,
        fonts: &mut FontSystem,
        recycled: Option<PreparedRenderBatches>,
    ) -> Result<PreparedRenderBatches, RendererError> {
        self.batch_planner
            .prepare_full_reusing(scene, fonts, recycled)
    }

    #[must_use]
    pub fn atlas_dimensions(&self) -> (u32, u32) {
        self.batch_planner.atlas_dimensions()
    }

    pub fn reset_gpu_resident_glyphs(&mut self) {
        self.batch_planner.reset_gpu_resident_glyphs();
    }
}
