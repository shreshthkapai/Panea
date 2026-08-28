// Damage discovery, retained-scene fingerprints, and region merging.

#[derive(Debug, Default)]
pub struct DamageTracker {
    previous_cells: HashMap<CellPosition, CellFingerprint>,
    previous_cursor: Option<CursorVisual>,
    previous_cursor_image: Option<CursorImageVisual>,
    previous_cursor_vector: Option<CursorVectorVisual>,
    previous_size: Option<(u16, u16)>,
    previous_offset: render_core::RenderOffset,
    previous_content_clip_bounds: Vec<RenderRect>,
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
            || self.content_clip_geometry_changed(scene)
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
            self.remember_content_clip_geometry(scene);
            self.previous_visuals = static_visual_regions(scene, metrics);
            self.remember_static_visuals(scene);
            self.previous_animations = scene.animations.clone();
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

        if self.static_visuals_changed(scene) {
            regions.extend(self.previous_visuals.iter().copied());
            self.previous_visuals = static_visual_regions(scene, metrics);
            regions.extend(self.previous_visuals.iter().copied());
            self.remember_static_visuals(scene);
        }

        if self.previous_animations != scene.animations {
            regions.extend(animation_regions(
                &self.previous_animations,
                scene.content_offset,
            ));
            regions.extend(animation_regions(&scene.animations, scene.content_offset));
            self.previous_animations = scene.animations.clone();
        }

        if self.previous_cursor_image != scene.cursor_image {
            if let Some(previous) = &self.previous_cursor_image {
                regions.push(offset_region(previous.bounds, scene.content_offset));
            }
            if let Some(current) = &scene.cursor_image {
                regions.push(offset_region(current.bounds, scene.content_offset));
            }
            self.previous_cursor_image = scene.cursor_image.clone();
        }

        if self.previous_cursor_vector != scene.cursor_vector {
            if let Some(previous) = &self.previous_cursor_vector {
                regions.push(offset_region(previous.bounds, scene.content_offset));
            }
            if let Some(current) = &scene.cursor_vector {
                regions.push(offset_region(current.bounds, scene.content_offset));
            }
            self.previous_cursor_vector = scene.cursor_vector.clone();
        }

        merge_regions(regions)
    }

    pub fn update_animations_only(
        &mut self,
        scene: &RenderScene,
        metrics: CellMetrics,
    ) -> Vec<DamageRegion> {
        let size = (scene.grid.columns, scene.grid.rows);
        if self.force_full
            || self.previous_size != Some(size)
            || self.previous_offset != scene.content_offset
            || self.content_clip_geometry_changed(scene)
            || self.previous_cursor != scene.cursor
            || self.static_visuals_changed(scene)
            || self.previous_cursor_image != scene.cursor_image
            || self.previous_cursor_vector != scene.cursor_vector
        {
            return self.update(scene, metrics);
        }

        if self.previous_animations == scene.animations {
            return Vec::new();
        }
        let mut regions =
            animation_regions(&self.previous_animations, scene.content_offset).collect::<Vec<_>>();
        regions.extend(animation_regions(&scene.animations, scene.content_offset));
        self.previous_animations = scene.animations.clone();
        merge_regions(regions)
    }

    fn content_clip_geometry_changed(&self, scene: &RenderScene) -> bool {
        self.previous_content_clip_bounds.len() != scene.content_clips.len()
            || self
                .previous_content_clip_bounds
                .iter()
                .zip(&scene.content_clips)
                .any(|(previous, current)| *previous != current.bounds)
    }

    fn remember_content_clip_geometry(&mut self, scene: &RenderScene) {
        self.previous_content_clip_bounds.clear();
        self.previous_content_clip_bounds
            .extend(scene.content_clips.iter().map(|clip| clip.bounds));
    }

    fn static_visuals_changed(&self, scene: &RenderScene) -> bool {
        self.previous_search_highlights != scene.search_highlights
            || self.previous_semantic_overlays != scene.semantic_overlays
            || self.previous_surface_overlays != scene.surface_overlays
            || self.previous_window_chrome != scene.window_chrome
            || self.previous_decorations != scene.decorations
            || self.previous_selections != scene.selections
    }

    fn remember_static_visuals(&mut self, scene: &RenderScene) {
        self.previous_search_highlights = scene.search_highlights.clone();
        self.previous_semantic_overlays = scene.semantic_overlays.clone();
        self.previous_surface_overlays = scene.surface_overlays.clone();
        self.previous_window_chrome = scene.window_chrome.clone();
        self.previous_decorations = scene.decorations.clone();
        self.previous_selections = scene.selections.clone();
    }
}

fn static_visual_regions(scene: &RenderScene, metrics: CellMetrics) -> Vec<DamageRegion> {
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
        .collect::<Vec<_>>();
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

fn animation_regions(
    animations: &[AnimationHandle],
    offset: render_core::RenderOffset,
) -> impl Iterator<Item = DamageRegion> + '_ {
    animations
        .iter()
        .map(move |animation| offset_region(animation.affected_region, offset))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CellFingerprint {
    text: RenderText,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct DamageRows {
    first_row: i64,
    spans: Vec<Option<(u16, u16)>>,
}

impl DamageRows {
    fn new(
        regions: &[DamageRegion],
        metrics: CellMetrics,
        offset: render_core::RenderOffset,
        first_row: i64,
        row_count: usize,
    ) -> Self {
        let mut spans = vec![None; row_count];
        if row_count == 0 || metrics.cell_width <= 0.0 || metrics.cell_height <= 0.0 {
            return Self { first_row, spans };
        }
        let last_row = first_row.saturating_add(row_count.saturating_sub(1) as i64);
        for region in regions
            .iter()
            .copied()
            .filter(|region| region.width > 0 && region.height > 0)
        {
            let x0 = pixel_cell_index(i64::from(region.x), i64::from(offset.x), metrics.cell_width);
            let x1 = pixel_cell_index(
                i64::from(region.x)
                    .saturating_add(i64::from(region.width))
                    .saturating_sub(1),
                i64::from(offset.x),
                metrics.cell_width,
            );
            if x1 < 0 {
                continue;
            }
            let col0 = u16::try_from(x0.max(0)).unwrap_or(u16::MAX);
            let col1 = u16::try_from(x1.max(0)).unwrap_or(u16::MAX);
            let row0 = pixel_cell_index(
                i64::from(region.y),
                i64::from(offset.y),
                metrics.cell_height,
            )
            .max(first_row);
            let row1 = pixel_cell_index(
                i64::from(region.y)
                    .saturating_add(i64::from(region.height))
                    .saturating_sub(1),
                i64::from(offset.y),
                metrics.cell_height,
            )
            .min(last_row);
            if row0 > row1 {
                continue;
            }
            for row in row0..=row1 {
                let index = usize::try_from(row.saturating_sub(first_row)).unwrap_or(usize::MAX);
                let Some(span) = spans.get_mut(index) else {
                    continue;
                };
                *span = Some(span.map_or((col0, col1), |(old0, old1)| {
                    (old0.min(col0), old1.max(col1))
                }));
            }
        }
        Self { first_row, spans }
    }

    fn span(&self, row: i64) -> Option<(u16, u16)> {
        let index = usize::try_from(row.checked_sub(self.first_row)?).ok()?;
        self.spans.get(index).copied().flatten()
    }

    fn intersects_cell(&self, position: CellPosition) -> bool {
        self.span(position.row)
            .is_some_and(|(start, end)| position.col >= start && position.col <= end)
    }

    fn intersects_columns(&self, row: i64, start: u16, end: u16) -> bool {
        self.span(row)
            .is_some_and(|(damage_start, damage_end)| start <= damage_end && end >= damage_start)
    }
}

fn pixel_cell_index(pixel: i64, origin: i64, advance: f32) -> i64 {
    (((pixel.saturating_sub(origin)) as f64) / f64::from(advance)).floor() as i64
}

pub(crate) fn merge_regions(mut regions: Vec<DamageRegion>) -> Vec<DamageRegion> {
    regions.retain(|region| region.width > 0 && region.height > 0);
    regions.sort_unstable_by_key(|region| (region.y, region.height, region.x));

    let mut horizontal: Vec<DamageRegion> = Vec::with_capacity(regions.len());
    for region in regions {
        if let Some(previous) = horizontal.last_mut()
            && previous.y == region.y
            && previous.height == region.height
            && i64::from(region.x)
                <= i64::from(previous.x).saturating_add(i64::from(previous.width))
        {
            *previous = union_region(*previous, region);
        } else {
            horizontal.push(region);
        }
    }

    horizontal.sort_unstable_by_key(|region| (region.x, region.width, region.y));
    let mut merged: Vec<DamageRegion> = Vec::with_capacity(horizontal.len());
    for region in horizontal {
        if let Some(previous) = merged.last_mut()
            && previous.x == region.x
            && previous.width == region.width
            && i64::from(region.y)
                <= i64::from(previous.y).saturating_add(i64::from(previous.height))
        {
            *previous = union_region(*previous, region);
        } else {
            merged.push(region);
        }
    }
    merged.sort_unstable_by_key(|region| (region.y, region.x, region.height, region.width));
    merged
}

