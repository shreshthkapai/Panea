// Renderer-independent scene assembly for tabs, panes, selections, and chrome.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SceneCacheUpdate {
    full_rebuild: bool,
    layout_builds: u64,
    layout_hits: u64,
    rows_rebuilt: u64,
    rows_reused: u64,
}

#[derive(Debug, Clone, PartialEq)]
struct LayoutRevision {
    surface_cols: u16,
    surface_rows: u16,
    tab_bar_rows: u16,
    active_tab: TabId,
    mux_layout: u64,
    metrics: Option<CellMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TabBarRevision {
    workspace: String,
    active_tab: TabId,
    tabs: Vec<(TabId, String)>,
}

impl TabBarRevision {
    fn capture(runtime: &MuxRuntime) -> Self {
        let workspace = runtime.model.active_workspace();
        let window = workspace.active_window();
        Self {
            workspace: workspace.name.clone(),
            active_tab: window.active_tab,
            tabs: window
                .tabs
                .iter()
                .map(|tab| (tab.id, tab.name.clone()))
                .collect(),
        }
    }

    fn matches(&self, runtime: &MuxRuntime) -> bool {
        let workspace = runtime.model.active_workspace();
        let window = workspace.active_window();
        self.workspace == workspace.name
            && self.active_tab == window.active_tab
            && self.tabs.len() == window.tabs.len()
            && self
                .tabs
                .iter()
                .zip(&window.tabs)
                .all(|((id, name), tab)| *id == tab.id && *name == tab.name)
    }
}

impl LayoutRevision {
    fn capture(runtime: &MuxRuntime, metrics: Option<CellMetrics>, config: &AppConfig) -> Self {
        let tab = runtime.model.active_tab();
        Self {
            surface_cols: runtime.surface_cols,
            surface_rows: runtime.surface_rows,
            tab_bar_rows: tab_bar_rows(&runtime.model, config),
            active_tab: tab.id,
            mux_layout: runtime.model.layout_revision(),
            metrics,
        }
    }

    fn matches(&self, runtime: &MuxRuntime, metrics: Option<CellMetrics>, config: &AppConfig) -> bool {
        let tab = runtime.model.active_tab();
        self.surface_cols == runtime.surface_cols
            && self.surface_rows == runtime.surface_rows
            && self.tab_bar_rows == tab_bar_rows(&runtime.model, config)
            && self.active_tab == tab.id
            && self.mux_layout == runtime.model.layout_revision()
            && self.metrics == metrics
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowRevision {
    absolute_row: i64,
    generation: u64,
}

#[derive(Debug)]
struct PaneSceneRevision {
    terminal: u64,
    semantic: u64,
    search: u64,
    command_overlay: u64,
    selection: Option<Selection>,
    alternate_screen: bool,
    viewport_origin: i64,
    cell_range: RenderItemRange,
    search_range: RenderItemRange,
    semantic_range: RenderItemRange,
    selection_range: RenderItemRange,
    visuals_dirty: bool,
    rows: Vec<RowRevision>,
}

#[derive(Debug, Default)]
struct SceneCache {
    scene: RenderScene,
    layout_revision: Option<LayoutRevision>,
    tab_bar_revision: Option<TabBarRevision>,
    layouts: Vec<PaneLayout>,
    panes: HashMap<PaneId, PaneSceneRevision>,
    config_revision: u64,
    pane_semantic_len: usize,
    initialized: bool,
    last_update: SceneCacheUpdate,
}

impl SceneCache {
    #[allow(clippy::too_many_arguments)]
    fn prepare(
        &mut self,
        runtime: &MuxRuntime,
        metrics: Option<CellMetrics>,
        config: &AppConfig,
        config_revision: u64,
        cursor_animator: Option<&mut CursorAnimationRuntime>,
        cursor_image_runtime: Option<&mut AnimatedCursorImageRuntime>,
        cursor_vector_runtime: Option<&mut CursorVectorRuntime>,
        cursor: CursorPresentation,
    ) -> &mut RenderScene {
        self.last_update = SceneCacheUpdate::default();
        let layout_matches = self
            .layout_revision
            .as_ref()
            .is_some_and(|revision| revision.matches(runtime, metrics, config));
        let tab_bar_matches = tab_bar_rows(&runtime.model, config) == 0
            || self
                .tab_bar_revision
                .as_ref()
                .is_some_and(|revision| revision.matches(runtime));
        let must_rebuild = !self.initialized
            || self.config_revision != config_revision
            || !layout_matches
            || !tab_bar_matches
            || self.panes.len() != runtime.model.active_tab().panes.len();

        if must_rebuild {
            if layout_matches {
                self.last_update.layout_hits = 1;
            } else {
                self.layouts = runtime.active_layouts(config);
                self.layout_revision = Some(LayoutRevision::capture(runtime, metrics, config));
                self.last_update.layout_builds = 1;
            }
            self.rebuild(
                runtime,
                metrics,
                config,
                cursor_animator,
                cursor_image_runtime,
                cursor_vector_runtime,
                cursor,
            );
            self.config_revision = config_revision;
            self.tab_bar_revision = Some(TabBarRevision::capture(runtime));
            self.initialized = true;
            self.last_update.full_rebuild = true;
            return &mut self.scene;
        }

        self.last_update.layout_hits = 1;
        if !self.refresh_rows(runtime, config) {
            self.rebuild(
                runtime,
                metrics,
                config,
                cursor_animator,
                cursor_image_runtime,
                cursor_vector_runtime,
                cursor,
            );
            self.last_update.full_rebuild = true;
            return &mut self.scene;
        }
        self.refresh_visuals(
            runtime,
            metrics,
            config,
            cursor_animator,
            cursor_image_runtime,
            cursor_vector_runtime,
            cursor,
        );
        &mut self.scene
    }

    #[must_use]
    const fn last_update(&self) -> SceneCacheUpdate {
        self.last_update
    }

    #[must_use]
    fn has_scene(&self) -> bool {
        self.initialized
    }

    fn scene_mut(&mut self) -> &mut RenderScene {
        &mut self.scene
    }

    #[allow(clippy::too_many_arguments)]
    fn rebuild(
        &mut self,
        runtime: &MuxRuntime,
        metrics: Option<CellMetrics>,
        config: &AppConfig,
        cursor_animator: Option<&mut CursorAnimationRuntime>,
        cursor_image_runtime: Option<&mut AnimatedCursorImageRuntime>,
        cursor_vector_runtime: Option<&mut CursorVectorRuntime>,
        cursor: CursorPresentation,
    ) {
        let recycled = std::mem::take(&mut self.scene);
        self.scene = scene_from_mux_with_layouts(
            Some(recycled),
            runtime,
            &self.layouts,
            metrics,
            config,
            cursor_animator,
            cursor_image_runtime,
            cursor_vector_runtime,
            cursor,
        );
        self.panes.clear();
        for (clip, layout) in self.scene.content_clips.iter().zip(&self.layouts) {
            let Some(pane) = runtime.panes.get(&layout.pane_id) else {
                continue;
            };
            let rows = pane
                .terminal
                .state()
                .visible_rows()
                .map(|row| RowRevision {
                    absolute_row: row.absolute_row,
                    generation: row.generation,
                })
                .collect::<Vec<_>>();
            self.last_update.rows_rebuilt = self
                .last_update
                .rows_rebuilt
                .saturating_add(rows.len() as u64);
            self.panes.insert(
                layout.pane_id,
                PaneSceneRevision {
                    terminal: pane.terminal.state().render_revision(),
                    semantic: pane.semantic_timeline.revision(),
                    search: pane.search.revision,
                    command_overlay: pane.command_overlay_revision,
                    selection: pane.terminal.selection_state(),
                    alternate_screen: pane
                        .terminal
                        .modes_ref()
                        .contains(&TerminalMode::AlternateScreen),
                    viewport_origin: pane.terminal.state().viewport().origin_row,
                    cell_range: clip.cells,
                    search_range: clip.search_highlights,
                    semantic_range: clip.semantic_overlays,
                    selection_range: clip.selections,
                    visuals_dirty: false,
                    rows,
                },
            );
        }
        self.pane_semantic_len = self
            .scene
            .content_clips
            .iter()
            .map(|clip| clip.semantic_overlays.end)
            .max()
            .unwrap_or(0);
    }

    fn refresh_rows(&mut self, runtime: &MuxRuntime, config: &AppConfig) -> bool {
        for layout in &self.layouts {
            let Some(pane) = runtime.panes.get(&layout.pane_id) else {
                return false;
            };
            let Some(cached) = self.panes.get_mut(&layout.pane_id) else {
                return false;
            };
            let terminal_revision = pane.terminal.state().render_revision();
            if cached.terminal == terminal_revision {
                self.last_update.rows_reused = self
                    .last_update
                    .rows_reused
                    .saturating_add(cached.rows.len() as u64);
                continue;
            }

            let selection = pane.terminal.selection_state();
            let alternate_screen = pane
                .terminal
                .modes_ref()
                .contains(&TerminalMode::AlternateScreen);
            let force_all = cached.selection != selection
                || cached.alternate_screen != alternate_screen;
            cached.visuals_dirty |= force_all;
            let viewport = pane.terminal.state().viewport();
            cached.visuals_dirty |= cached.viewport_origin != viewport.origin_row;
            let columns = usize::from(viewport.size.cols);
            let expected_cells = columns.saturating_mul(usize::from(viewport.size.rows));
            if cached.rows.len() != usize::from(viewport.size.rows)
                || cached.cell_range.end.saturating_sub(cached.cell_range.start) != expected_cells
            {
                return false;
            }

            for (visible_row, row) in pane.terminal.state().visible_rows().enumerate() {
                if row.cells.len() != columns {
                    return false;
                }
                let revision = RowRevision {
                    absolute_row: row.absolute_row,
                    generation: row.generation,
                };
                let changed = force_all || cached.rows[visible_row] != revision;
                if changed {
                    let start = cached
                        .cell_range
                        .start
                        .saturating_add(visible_row.saturating_mul(columns));
                    let end = start.saturating_add(columns);
                    let Some(target) = self.scene.grid.cells.get_mut(start..end) else {
                        return false;
                    };
                    write_render_row(
                        target,
                        &row.cells,
                        row.absolute_row,
                        visible_row,
                        layout,
                        selection,
                        config,
                    );
                    cached.rows[visible_row] = revision;
                    self.last_update.rows_rebuilt =
                        self.last_update.rows_rebuilt.saturating_add(1);
                } else {
                    self.last_update.rows_reused =
                        self.last_update.rows_reused.saturating_add(1);
                }
            }
            cached.terminal = terminal_revision;
            cached.selection = selection;
            cached.alternate_screen = alternate_screen;
            cached.viewport_origin = viewport.origin_row;
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn refresh_visuals(
        &mut self,
        runtime: &MuxRuntime,
        metrics: Option<CellMetrics>,
        config: &AppConfig,
        cursor_animator: Option<&mut CursorAnimationRuntime>,
        cursor_image_runtime: Option<&mut AnimatedCursorImageRuntime>,
        cursor_vector_runtime: Option<&mut CursorVectorRuntime>,
        cursor: CursorPresentation,
    ) {
        let semantic_changed = self.layouts.iter().any(|layout| {
            runtime.panes.get(&layout.pane_id).is_some_and(|pane| {
                self.panes.get(&layout.pane_id).is_none_or(|cached| {
                    cached.semantic != pane.semantic_timeline.revision()
                        || cached.command_overlay != pane.command_overlay_revision
                        || cached.visuals_dirty
                })
            })
        });
        let search_changed = self.layouts.iter().any(|layout| {
            runtime.panes.get(&layout.pane_id).is_some_and(|pane| {
                self.panes.get(&layout.pane_id).is_none_or(|cached| {
                    cached.search != pane.search.revision || cached.visuals_dirty
                })
            })
        });
        let selection_changed = self.panes.values().any(|cached| cached.visuals_dirty);

        self.scene.cursor = None;
        self.scene.cursor_image = None;
        self.scene.cursor_vector = None;
        if selection_changed {
            self.scene.selections.clear();
        }
        if search_changed {
            self.scene.search_highlights.clear();
        }
        self.scene
            .semantic_overlays
            .truncate(self.pane_semantic_len);
        if semantic_changed {
            self.scene.semantic_overlays.clear();
        }
        self.scene.content_clips.clear();
        self.scene.surface_overlays.clear();
        self.scene.window_chrome = None;
        self.scene.decorations.clear();
        self.scene.animations.clear();
        self.scene.damage_regions.clear();

        let active_pane = runtime.model.active_tab().active_pane;
        for layout in self.layouts.iter().copied() {
            let Some(pane) = runtime.panes.get(&layout.pane_id) else {
                continue;
            };
            let Some(cached) = self.panes.get_mut(&layout.pane_id) else {
                continue;
            };
            let row_offset = layout.rect.y.floor() as i64;
            let col_offset = layout.rect.x.floor() as u16;
            if semantic_changed {
                let start = self.scene.semantic_overlays.len();
                append_terminal_semantic_overlays(
                    &mut self.scene,
                    &pane.terminal,
                    &pane.semantic_timeline,
                    &pane.command_output_collapsed,
                    metrics,
                    config,
                    row_offset,
                    col_offset,
                );
                cached.semantic_range =
                    RenderItemRange::new(start, self.scene.semantic_overlays.len());
            }
            if search_changed {
                let start = self.scene.search_highlights.len();
                append_terminal_search_overlays(
                    &mut self.scene,
                    &pane.terminal,
                    &pane.search,
                    metrics,
                    config,
                    row_offset,
                    col_offset,
                );
                cached.search_range =
                    RenderItemRange::new(start, self.scene.search_highlights.len());
            }
            if selection_changed {
                let start = self.scene.selections.len();
                append_terminal_selection(
                    &mut self.scene,
                    &pane.terminal,
                    config,
                    row_offset,
                    col_offset,
                );
                cached.selection_range =
                    RenderItemRange::new(start, self.scene.selections.len());
            }
            if layout.pane_id == active_pane {
                self.scene.cursor = Some(terminal_cursor_visual(
                    &pane.terminal,
                    metrics,
                    config,
                    cursor,
                    row_offset,
                    col_offset,
                ));
            }
            cached.semantic = pane.semantic_timeline.revision();
            cached.search = pane.search.revision;
            cached.command_overlay = pane.command_overlay_revision;
            cached.visuals_dirty = false;
            if let Some(metrics) = metrics {
                self.scene.content_clips.push(RenderContentClip {
                    bounds: rect_from_layout(layout.rect, metrics),
                    cells: cached.cell_range,
                    search_highlights: cached.search_range,
                    semantic_overlays: cached.semantic_range,
                    selections: cached.selection_range,
                });
            }
        }

        if semantic_changed {
            self.pane_semantic_len = self.scene.semantic_overlays.len();
        }

        if let Some(metrics) = metrics {
            append_pane_borders(&mut self.scene, runtime, &self.layouts, metrics, config);
            append_mux_drag_overlay(&mut self.scene, runtime, &self.layouts, metrics, config);
            if let Some(cursor_animator) = cursor_animator {
                cursor_animator.populate_scene(
                    &mut self.scene,
                    metrics,
                    cursor_animation_settings(config),
                );
            }
            if let Some(cursor_image_runtime) = cursor_image_runtime {
                cursor_image_runtime.populate_scene(&mut self.scene, metrics);
            }
            if let Some(cursor_vector_runtime) = cursor_vector_runtime {
                cursor_vector_runtime.populate_scene(&mut self.scene, metrics);
            }
            append_active_ime_overlay(&mut self.scene, runtime, metrics);
            append_session_product_overlay(&mut self.scene, runtime, metrics);
        }
    }
}

fn write_render_row(
    target: &mut [RenderCell],
    cells: &[term_core::Cell],
    absolute_row: i64,
    visible_row: usize,
    layout: &PaneLayout,
    selection: Option<Selection>,
    config: &AppConfig,
) {
    let row_offset = layout.rect.y.floor() as i64;
    let col_offset = layout.rect.x.floor() as u16;
    let selected_foreground = config.colors.selection_foreground.map(render_color);
    let selected_span = selection.and_then(|selection| {
        selection.span_for_row(absolute_row, layout.terminal_size.cols)
    });
    for (column, (target, cell)) in target.iter_mut().zip(cells).enumerate() {
        let col = u16::try_from(column).unwrap_or(u16::MAX);
        let (mut foreground, background) = colors_for_attributes(cell.attributes, config);
        if let (Some(selected), Some((start, end))) = (selected_foreground, selected_span)
            && col >= start
            && col <= end
        {
            foreground = selected;
        }
        *target = RenderCell {
            position: CellPosition {
                row: i64::try_from(visible_row)
                    .unwrap_or(i64::MAX)
                    .saturating_add(row_offset),
                col: col.saturating_add(col_offset),
            },
            text: cell.text.clone(),
            foreground,
            background,
            style: style_for_attributes(cell.attributes),
        };
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn scene_from_mux(
    recycled: Option<RenderScene>,
    runtime: &MuxRuntime,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    cursor_animator: Option<&mut CursorAnimationRuntime>,
    cursor_image_runtime: Option<&mut AnimatedCursorImageRuntime>,
    cursor_vector_runtime: Option<&mut CursorVectorRuntime>,
    cursor: CursorPresentation,
) -> RenderScene {
    let layouts = runtime.active_layouts(config);
    scene_from_mux_with_layouts(
        recycled,
        runtime,
        &layouts,
        metrics,
        config,
        cursor_animator,
        cursor_image_runtime,
        cursor_vector_runtime,
        cursor,
    )
}

#[allow(clippy::too_many_arguments)]
fn scene_from_mux_with_layouts(
    recycled: Option<RenderScene>,
    runtime: &MuxRuntime,
    layouts: &[PaneLayout],
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    cursor_animator: Option<&mut CursorAnimationRuntime>,
    cursor_image_runtime: Option<&mut AnimatedCursorImageRuntime>,
    cursor_vector_runtime: Option<&mut CursorVectorRuntime>,
    cursor: CursorPresentation,
) -> RenderScene {
    let mut scene = recycled.unwrap_or_default();
    reset_scene_for_reuse(&mut scene, runtime, config);
    let tab_bar_rows = tab_bar_rows(&runtime.model, config);

    if tab_bar_rows > 0 {
        append_tab_bar_cells(&mut scene, runtime, config);
    }

    let active_pane = runtime.model.active_tab().active_pane;
    for layout in layouts.iter().copied() {
        let Some(pane) = runtime.panes.get(&layout.pane_id) else {
            continue;
        };
        append_pane_scene(
            &mut scene,
            pane,
            layout,
            active_pane,
            metrics,
            config,
            cursor,
        );
    }

    if let Some(metrics) = metrics {
        append_pane_borders(&mut scene, runtime, layouts, metrics, config);
        append_mux_drag_overlay(&mut scene, runtime, layouts, metrics, config);
        if let Some(cursor_animator) = cursor_animator {
            cursor_animator.populate_scene(&mut scene, metrics, cursor_animation_settings(config));
        }
        if let Some(cursor_image_runtime) = cursor_image_runtime {
            cursor_image_runtime.populate_scene(&mut scene, metrics);
        }
        if let Some(cursor_vector_runtime) = cursor_vector_runtime {
            cursor_vector_runtime.populate_scene(&mut scene, metrics);
        }
        append_active_ime_overlay(&mut scene, runtime, metrics);
        append_session_product_overlay(&mut scene, runtime, metrics);
    }

    scene
}

fn reset_scene_for_reuse(scene: &mut RenderScene, runtime: &MuxRuntime, config: &AppConfig) {
    scene.grid.columns = runtime.surface_cols;
    scene.grid.rows = runtime.surface_rows;
    scene.grid.cells.clear();
    scene.content_offset = RenderOffset {
        x: horizontal_content_inset(config).min(i32::MAX as u32) as i32,
        y: vertical_content_inset(config).min(i32::MAX as u32) as i32,
    };
    scene.cursor = None;
    scene.cursor_image = None;
    scene.cursor_vector = None;
    scene.selections.clear();
    scene.search_highlights.clear();
    scene.semantic_overlays.clear();
    scene.content_clips.clear();
    scene.surface_overlays.clear();
    scene.window_chrome = None;
    scene.decorations.clear();
    scene.animations.clear();
    scene.damage_regions.clear();
}

fn append_mux_drag_overlay(
    scene: &mut RenderScene,
    runtime: &MuxRuntime,
    layouts: &[PaneLayout],
    metrics: CellMetrics,
    config: &AppConfig,
) {
    let Some(drag) = runtime.drag else {
        return;
    };
    let bounds = match drag {
        MuxDragState::Pane { target, .. } => layouts
            .iter()
            .copied()
            .find(|layout| layout.pane_id == target)
            .map(|layout| rect_from_layout(layout.rect, metrics)),
        MuxDragState::Tab { target, .. } => {
            let workspace = runtime.model.active_workspace();
            let mut start = 0usize;
            workspace
                .active_window()
                .tabs
                .iter()
                .enumerate()
                .find_map(|(index, tab)| {
                    let width = formatted_tab_width(config, &workspace.name, index, tab);
                    let rect = (tab.id == target).then(|| RenderRect {
                        x: (start as f32 * metrics.cell_width).floor() as i32,
                        y: 0,
                        width: (width as f32 * metrics.cell_width).ceil() as u32,
                        height: metrics.cell_height.ceil() as u32,
                    });
                    start = start.saturating_add(width);
                    rect
                })
        }
    };
    let Some(bounds) = bounds else {
        return;
    };
    scene.semantic_overlays.push(OverlayPrimitive {
        kind: OverlayKind::DragTarget,
        bounds,
        color: RenderColor {
            red: 72,
            green: 142,
            blue: 230,
            alpha: 42,
        },
        border_color: Some(RenderColor {
            red: 112,
            green: 178,
            blue: 255,
            alpha: 245,
        }),
        border_width_px: 2,
        corner_radius_px: 3,
        z_index: 1600,
        label: None,
        label_color: None,
    });
}

fn formatted_tab_width(
    config: &AppConfig,
    workspace_name: &str,
    index: usize,
    tab: &mux::Tab,
) -> usize {
    config
        .mux
        .tab_title_format
        .replace("{index}", &(index + 1).to_string())
        .replace("{title}", &tab.name)
        .replace("{workspace}", workspace_name)
        .chars()
        .count()
        .saturating_add(2)
}

fn append_active_ime_overlay(scene: &mut RenderScene, runtime: &MuxRuntime, metrics: CellMetrics) {
    let Some(pane) = runtime.active_pane() else {
        return;
    };
    if pane.ime_preedit.is_empty() || pane.ssh_prompt.is_some() || pane.osc52_prompt.is_some() {
        return;
    }
    let Some(cursor) = scene.cursor else {
        return;
    };
    let width = ((pane.ime_preedit_cells.max(1) as f32 * metrics.cell_width).ceil() as u32)
        .saturating_add(8);
    scene.semantic_overlays.push(OverlayPrimitive {
        kind: OverlayKind::ImePreedit,
        bounds: RenderRect {
            x: scene.content_offset.x
                + (f32::from(cursor.position.col) * metrics.cell_width).floor() as i32,
            y: scene.content_offset.y
                + ((cursor.position.row + 1) as f32 * metrics.cell_height).floor() as i32,
            width,
            height: metrics.cell_height.ceil() as u32 + 4,
        },
        color: RenderColor {
            red: 24,
            green: 28,
            blue: 36,
            alpha: 245,
        },
        border_color: Some(RenderColor {
            red: 110,
            green: 170,
            blue: 255,
            alpha: 255,
        }),
        border_width_px: 1,
        corner_radius_px: 3,
        z_index: 1800,
        label: Some(pane.ime_preedit.clone()),
        label_color: None,
    });
}

fn append_session_product_overlay(
    scene: &mut RenderScene,
    runtime: &MuxRuntime,
    metrics: CellMetrics,
) {
    let Some(pane) = runtime.active_pane() else {
        return;
    };
    if let Some(prompt) = pane.osc52_prompt.as_ref() {
        append_centered_security_overlay(
            scene,
            osc52_prompt_lines(pane, prompt),
            metrics,
            RenderColor {
                red: 240,
                green: 96,
                blue: 96,
                alpha: 255,
            },
        );
        return;
    }
    if let Some(prompt) = pane.ssh_prompt.as_ref() {
        append_centered_security_overlay(
            scene,
            ssh_prompt_lines(prompt),
            metrics,
            RenderColor {
                red: 245,
                green: 185,
                blue: 72,
                alpha: 255,
            },
        );
        return;
    }

    let label = match &pane.connection_state {
        PaneConnectionState::Connecting => Some(format!(
            "Connecting SSH profile '{}'...",
            pane.session_spec.profile_name
        )),
        PaneConnectionState::Disconnected(message) if pane.remote_session => {
            Some(format!("SSH disconnected: {message}"))
        }
        PaneConnectionState::Disconnected(message) => Some(match pane.exit_code {
            Some(code) => {
                format!("Local session exited with code {code}. Ctrl+Alt+R to restart")
            }
            None => format!("Local session unavailable: {message}. Ctrl+Alt+R to retry"),
        }),
        PaneConnectionState::Connected => None,
    };
    let Some(label) = label else {
        return;
    };
    let width = ((label.chars().count() as f32 * metrics.cell_width).ceil() as u32)
        .saturating_add(16)
        .min((f32::from(scene.grid.columns) * metrics.cell_width).ceil() as u32);
    scene.semantic_overlays.push(OverlayPrimitive {
        kind: OverlayKind::SessionStatus,
        bounds: RenderRect {
            x: 8,
            y: ((f32::from(scene.grid.rows) * metrics.cell_height).ceil() as i32)
                .saturating_sub(metrics.cell_height.ceil() as i32)
                .saturating_sub(12),
            width,
            height: metrics.cell_height.ceil() as u32 + 6,
        },
        color: RenderColor {
            red: 22,
            green: 28,
            blue: 37,
            alpha: 240,
        },
        border_color: Some(RenderColor {
            red: 115,
            green: 134,
            blue: 160,
            alpha: 255,
        }),
        border_width_px: 1,
        corner_radius_px: 4,
        z_index: 1700,
        label: Some(label),
        label_color: None,
    });
}

fn append_centered_security_overlay(
    scene: &mut RenderScene,
    lines: Vec<String>,
    metrics: CellMetrics,
    accent: RenderColor,
) {
    let max_chars = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(20);
    let width = ((max_chars as f32 * metrics.cell_width).ceil() as u32)
        .saturating_add(24)
        .min((f32::from(scene.grid.columns) * metrics.cell_width).ceil() as u32);
    let line_height = metrics.cell_height.ceil() as u32 + 5;
    let total_height = line_height.saturating_mul(lines.len() as u32);
    let surface_width = (f32::from(scene.grid.columns) * metrics.cell_width).ceil() as i32;
    let surface_height = (f32::from(scene.grid.rows) * metrics.cell_height).ceil() as i32;
    let x = ((surface_width - width as i32) / 2).max(0);
    let mut y = ((surface_height - total_height as i32) / 2).max(0);
    for (index, line) in lines.into_iter().enumerate() {
        scene.semantic_overlays.push(OverlayPrimitive {
            kind: OverlayKind::SecurityPrompt,
            bounds: RenderRect {
                x,
                y,
                width,
                height: line_height,
            },
            color: RenderColor {
                red: 18,
                green: 22,
                blue: 29,
                alpha: 252,
            },
            border_color: (index == 0).then_some(accent),
            border_width_px: u8::from(index == 0),
            corner_radius_px: u8::from(index == 0) * 4,
            z_index: 2000,
            label: Some(line),
            label_color: None,
        });
        y = y.saturating_add(line_height as i32);
    }
}

fn osc52_prompt_lines(pane: &PaneRuntime, prompt: &Osc52PromptState) -> Vec<String> {
    let target = match prompt.request.target {
        Osc52ClipboardTarget::Clipboard | Osc52ClipboardTarget::Select => "system clipboard",
        Osc52ClipboardTarget::PrimarySelection => "primary selection",
        Osc52ClipboardTarget::Unknown(_) => "unknown target",
    };
    vec![
        "Remote clipboard write requested".to_owned(),
        format!("Session: {}", pane.session_spec.profile_name),
        format!("Target: {target}"),
        format!("Payload size: {} bytes", prompt.bytes),
        prompt.reason.clone(),
        "Y allow once   N/Esc deny".to_owned(),
    ]
}

fn ssh_prompt_lines(prompt: &SshPromptState) -> Vec<String> {
    match prompt {
        SshPromptState::HostTrust { request, .. } => {
            let mut lines = vec![
                match request.reason {
                    HostKeyTrustReason::UnknownHost => "Unknown SSH host".to_owned(),
                    HostKeyTrustReason::ChangedHostKey => {
                        "WARNING: SSH host key changed".to_owned()
                    }
                    HostKeyTrustReason::PinnedFingerprintMismatch => {
                        "BLOCKED: pinned SSH fingerprint mismatch".to_owned()
                    }
                },
                format!("Host: {}:{}", request.key.host, request.key.port),
                format!("Key: {}", request.key.algorithm),
                format!("Fingerprint: {}", request.key.sha256_fingerprint),
            ];
            if let Some(expected) = request.expected_fingerprint.as_deref() {
                lines.push(format!("Expected: {expected}"));
            }
            lines.push(match request.reason {
                HostKeyTrustReason::UnknownHost => {
                    "O trust once   S trust and store   Esc reject".to_owned()
                }
                HostKeyTrustReason::ChangedHostKey => {
                    "R replace stored key   Esc reject".to_owned()
                }
                HostKeyTrustReason::PinnedFingerprintMismatch => {
                    "Esc reject; update the pinned fingerprint in config to continue".to_owned()
                }
            });
            lines
        }
        SshPromptState::Secret {
            request,
            input,
            keychain,
            save_to_keychain,
            ..
        } => {
            let storage = if keychain.available {
                format!(
                    "Tab save to OS keychain: {}   Enter continue   Esc cancel",
                    if *save_to_keychain { "yes" } else { "no" }
                )
            } else {
                format!(
                    "OS keychain unavailable; secret stays transient ({})",
                    keychain.message
                )
            };
            vec![
                request.prompt_label(),
                format!("Secret: {}", "*".repeat(input.graphemes(true).count())),
                storage,
            ]
        }
    }
}

fn append_pane_scene(
    target: &mut RenderScene,
    pane: &PaneRuntime,
    layout: PaneLayout,
    active_pane: PaneId,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    cursor: CursorPresentation,
) {
    let cell_start = target.grid.cells.len();
    let search_start = target.search_highlights.len();
    let semantic_start = target.semantic_overlays.len();
    let selection_start = target.selections.len();
    let row_offset = layout.rect.y.floor() as i64;
    let col_offset = layout.rect.x.floor() as u16;
    append_terminal_scene(
        target,
        &pane.terminal,
        &pane.semantic_timeline,
        &pane.search,
        &pane.command_output_collapsed,
        metrics,
        config,
        cursor,
        row_offset,
        col_offset,
        layout.pane_id == active_pane,
    );

    if let Some(metrics) = metrics {
        target.content_clips.push(RenderContentClip {
            bounds: rect_from_layout(layout.rect, metrics),
            cells: RenderItemRange::new(cell_start, target.grid.cells.len()),
            search_highlights: RenderItemRange::new(search_start, target.search_highlights.len()),
            semantic_overlays: RenderItemRange::new(semantic_start, target.semantic_overlays.len()),
            selections: RenderItemRange::new(selection_start, target.selections.len()),
        });
    }
}

fn append_tab_bar_cells(scene: &mut RenderScene, runtime: &MuxRuntime, config: &AppConfig) {
    let window = runtime.model.active_workspace().active_window();
    let mut col = 0u16;
    for (index, tab) in window.tabs.iter().enumerate() {
        let active = tab.id == window.active_tab;
        let formatted = config
            .mux
            .tab_title_format
            .replace("{index}", &(index + 1).to_string())
            .replace("{title}", &tab.name)
            .replace("{workspace}", &runtime.model.active_workspace().name);
        let label = format!(" {formatted} ");
        for ch in label.chars() {
            if col >= runtime.surface_cols {
                return;
            }
            scene.grid.cells.push(RenderCell {
                position: CellPosition { row: 0, col },
                text: ch.to_string().into(),
                foreground: render_color(if active {
                    config.mux.appearance.active_tab_foreground
                } else {
                    config.mux.appearance.inactive_tab_foreground
                }),
                background: if active {
                    render_color(config.mux.appearance.active_tab_background)
                } else {
                    render_color(config.mux.appearance.inactive_tab_background)
                },
                style: RenderCellStyle {
                    bold: active,
                    ..RenderCellStyle::default()
                },
            });
            col = col.saturating_add(1);
        }
    }
    while col < runtime.surface_cols {
        scene.grid.cells.push(RenderCell {
            position: CellPosition { row: 0, col },
            text: " ".into(),
            foreground: render_color(config.mux.appearance.inactive_tab_foreground),
            background: render_color(config.mux.appearance.tab_bar_background),
            style: RenderCellStyle::default(),
        });
        col = col.saturating_add(1);
    }
}

fn append_pane_borders(
    scene: &mut RenderScene,
    runtime: &MuxRuntime,
    pane_layouts: &[PaneLayout],
    metrics: CellMetrics,
    config: &AppConfig,
) {
    let width = u32::from(config.mux.appearance.pane_border_width);
    if width == 0 {
        return;
    }

    let active = runtime.model.active_tab().active_pane;
    let layouts = pane_layouts
        .iter()
        .copied()
        .map(|layout| (layout.pane_id, rect_from_layout(layout.rect, metrics)))
        .collect::<Vec<_>>();

    for left_index in 0..layouts.len() {
        for right_index in left_index + 1..layouts.len() {
            let (left_id, left) = layouts[left_index];
            let (right_id, right) = layouts[right_index];
            let Some(bounds) = shared_pane_separator(left, right, width) else {
                continue;
            };
            let color = if left_id == active || right_id == active {
                render_color(config.mux.appearance.active_pane_border)
            } else {
                render_color(config.mux.appearance.inactive_pane_border)
            };
            scene.decorations.push(RenderDecoration {
                bounds,
                color,
                border_color: None,
            });
        }
    }
}

fn shared_pane_separator(left: RenderRect, right: RenderRect, width: u32) -> Option<RenderRect> {
    let left_x0 = i64::from(left.x);
    let left_y0 = i64::from(left.y);
    let left_x1 = left_x0 + i64::from(left.width);
    let left_y1 = left_y0 + i64::from(left.height);
    let right_x0 = i64::from(right.x);
    let right_y0 = i64::from(right.y);
    let right_x1 = right_x0 + i64::from(right.width);
    let right_y1 = right_y0 + i64::from(right.height);
    let half_width = i64::from(width / 2);

    let vertical_edge = if (left_x1 - right_x0).abs() <= 1 {
        Some((left_x1 + right_x0) / 2)
    } else if (right_x1 - left_x0).abs() <= 1 {
        Some((right_x1 + left_x0) / 2)
    } else {
        None
    };
    let overlap_y0 = left_y0.max(right_y0);
    let overlap_y1 = left_y1.min(right_y1);
    if let Some(edge) = vertical_edge
        && overlap_y1 > overlap_y0
    {
        return Some(RenderRect {
            x: i32::try_from(edge - half_width).ok()?,
            y: i32::try_from(overlap_y0).ok()?,
            width,
            height: u32::try_from(overlap_y1 - overlap_y0).ok()?,
        });
    }

    let horizontal_edge = if (left_y1 - right_y0).abs() <= 1 {
        Some((left_y1 + right_y0) / 2)
    } else if (right_y1 - left_y0).abs() <= 1 {
        Some((right_y1 + left_y0) / 2)
    } else {
        None
    };
    let overlap_x0 = left_x0.max(right_x0);
    let overlap_x1 = left_x1.min(right_x1);
    if let Some(edge) = horizontal_edge
        && overlap_x1 > overlap_x0
    {
        return Some(RenderRect {
            x: i32::try_from(overlap_x0).ok()?,
            y: i32::try_from(edge - half_width).ok()?,
            width: u32::try_from(overlap_x1 - overlap_x0).ok()?,
            height: width,
        });
    }

    None
}

fn append_performance_overlay(
    scene: &mut RenderScene,
    overlay: &PerformanceOverlay,
    ui: &PerformanceOverlayUiState,
    budget: PerformanceBudget,
    metrics: CellMetrics,
) {
    let Some((lines, metric_lines)) = performance_overlay_lines(overlay, ui, budget) else {
        return;
    };
    let layout = performance_overlay_layout(
        &lines,
        scene.grid.columns,
        scene.grid.rows,
        metrics,
        ui.position,
    );
    for (index, (line, bounds)) in lines.into_iter().zip(layout.rows).enumerate() {
        let max_chars =
            ((bounds.width.saturating_sub(14) as f32 / metrics.cell_width).floor() as usize).max(1);
        scene.semantic_overlays.push(OverlayPrimitive {
            kind: OverlayKind::PerformanceOverlay,
            bounds,
            color: if index >= metric_lines {
                RenderColor {
                    red: 24,
                    green: 31,
                    blue: 42,
                    alpha: 242,
                }
            } else {
                RenderColor {
                    red: 10,
                    green: 14,
                    blue: 20,
                    alpha: 224,
                }
            },
            border_color: Some(RenderColor {
                red: if index == 0 { 96 } else { 70 },
                green: if index == 0 { 172 } else { 82 },
                blue: if index == 0 { 238 } else { 98 },
                alpha: 210,
            }),
            border_width_px: 1,
            corner_radius_px: 4,
            z_index: 1000,
            label: Some(truncate_overlay_label(&line, max_chars)),
            label_color: None,
        });
    }
}

#[derive(Debug, Clone)]
struct PerformanceOverlayLayout {
    rows: Vec<RenderRect>,
}

fn performance_overlay_lines(
    overlay: &PerformanceOverlay,
    ui: &PerformanceOverlayUiState,
    budget: PerformanceBudget,
) -> Option<(Vec<String>, usize)> {
    let mut lines = overlay.render_lines(budget)?;
    let metric_lines = match ui.detail {
        PerformanceOverlayDetail::Compact => 2,
        PerformanceOverlayDetail::Detailed => 4,
    };
    lines.truncate(metric_lines);
    let metric_lines = lines.len();
    if ui.menu_open {
        lines.extend([
            format!("View  {}", performance_overlay_detail_name(ui.detail)),
            format!(
                "Position  {}",
                performance_overlay_position_name(ui.position)
            ),
            "Hide".to_owned(),
        ]);
    }
    Some((lines, metric_lines))
}

fn performance_overlay_layout(
    lines: &[String],
    cols: u16,
    rows: u16,
    metrics: CellMetrics,
    position: PerformanceOverlayPosition,
) -> PerformanceOverlayLayout {
    let surface_width = (f32::from(cols.max(1)) * metrics.cell_width).ceil() as u32;
    let surface_height = (f32::from(rows.max(1)) * metrics.cell_height).ceil() as u32;
    let max_chars = usize::from(cols.saturating_sub(4).clamp(12, 72));
    let content_chars = lines
        .iter()
        .map(|line| line.chars().count().min(max_chars))
        .max()
        .unwrap_or(12)
        .max(12);
    let padding = 7u32;
    let width = ((content_chars as f32 * metrics.cell_width).ceil() as u32)
        .saturating_add(padding * 2)
        .min(surface_width.saturating_sub(16).max(1));
    let row_height = metrics.cell_height.ceil().max(14.0) as u32 + 5;
    let gap = 3u32;
    let total_height = row_height
        .saturating_mul(lines.len() as u32)
        .saturating_add(gap.saturating_mul(lines.len().saturating_sub(1) as u32));
    let left = matches!(
        position,
        PerformanceOverlayPosition::TopLeft | PerformanceOverlayPosition::BottomLeft
    );
    let top = matches!(
        position,
        PerformanceOverlayPosition::TopLeft | PerformanceOverlayPosition::TopRight
    );
    let x = if left {
        8
    } else {
        surface_width.saturating_sub(width).saturating_sub(8) as i32
    };
    let start_y = if top {
        8
    } else {
        surface_height
            .saturating_sub(total_height)
            .saturating_sub(8) as i32
    };
    let rows = lines
        .iter()
        .enumerate()
        .map(|(index, _)| RenderRect {
            x,
            y: start_y.saturating_add((index as u32 * (row_height + gap)) as i32),
            width,
            height: row_height,
        })
        .collect();
    PerformanceOverlayLayout { rows }
}

#[allow(clippy::too_many_arguments)]
fn handle_performance_overlay_mouse(
    mouse: MouseEvent,
    overlay: &PerformanceOverlay,
    ui: &mut PerformanceOverlayUiState,
    budget: PerformanceBudget,
    metrics: CellMetrics,
    cols: u16,
    rows: u16,
    config: &AppConfig,
) -> bool {
    if !ui.enabled || !matches!(mouse.kind, MouseEventKind::Pressed(MouseButton::Left)) {
        return false;
    }
    let Some((lines, metric_lines)) = performance_overlay_lines(overlay, ui, budget) else {
        return false;
    };
    let layout = performance_overlay_layout(&lines, cols, rows, metrics, ui.position);
    let x = mouse.x - f64::from(horizontal_content_inset(config));
    let y = mouse.y - f64::from(vertical_content_inset(config));
    let Some(index) = layout
        .rows
        .iter()
        .position(|rect| point_in_rect(x, y, *rect))
    else {
        return false;
    };
    if index < metric_lines {
        ui.menu_open = !ui.menu_open;
    } else {
        match index - metric_lines {
            0 => ui.cycle_detail(),
            1 => ui.cycle_position(),
            2 => ui.hide(),
            _ => return false,
        }
    }
    true
}

fn point_in_rect(x: f64, y: f64, rect: RenderRect) -> bool {
    x >= f64::from(rect.x)
        && y >= f64::from(rect.y)
        && x < f64::from(rect.x) + f64::from(rect.width)
        && y < f64::from(rect.y) + f64::from(rect.height)
}

fn truncate_overlay_label(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }

    let keep = max_chars.saturating_sub(1);
    let mut output = text.chars().take(keep).collect::<String>();
    output.push('~');
    output
}

fn rect_from_layout(rect: LogicalRect, metrics: CellMetrics) -> RenderRect {
    RenderRect {
        x: (rect.x * metrics.cell_width).floor() as i32,
        y: (rect.y * metrics.cell_height).floor() as i32,
        width: (rect.width * metrics.cell_width).ceil() as u32,
        height: (rect.height * metrics.cell_height).ceil() as u32,
    }
}

fn offset_rect(
    rect: &mut RenderRect,
    row_offset: i64,
    col_offset: u16,
    metrics: Option<CellMetrics>,
) {
    if let Some(metrics) = metrics {
        rect.x += (f32::from(col_offset) * metrics.cell_width).floor() as i32;
        rect.y += (row_offset as f32 * metrics.cell_height).floor() as i32;
    }
}

#[allow(clippy::too_many_arguments)]
fn append_terminal_scene(
    target: &mut RenderScene,
    terminal: &TerminalEmulator,
    semantic_timeline: &SemanticTimelineStore,
    search: &PaneSearch,
    command_output_collapsed: &HashMap<u64, bool>,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    presentation: CursorPresentation,
    row_offset: i64,
    col_offset: u16,
    include_cursor: bool,
) {
    let viewport = terminal.state().viewport();
    let selection = terminal.selection_state();
    let selected_foreground = config.colors.selection_foreground.map(render_color);
    target
        .grid
        .cells
        .reserve(usize::from(viewport.size.cols).saturating_mul(usize::from(viewport.size.rows)));

    for (visible_row, row) in terminal.state().visible_rows().enumerate() {
        let local_row = i64::try_from(visible_row).unwrap_or(i64::MAX);
        let selected_span = selection
            .and_then(|selection| selection.span_for_row(row.absolute_row, viewport.size.cols));
        for (column, cell) in row.cells.iter().enumerate() {
            let col = u16::try_from(column).unwrap_or(u16::MAX);
            let (mut foreground, background) = colors_for_attributes(cell.attributes, config);
            if let (Some(selected), Some((start, end))) = (selected_foreground, selected_span)
                && col >= start
                && col <= end
            {
                foreground = selected;
            }
            target.grid.cells.push(RenderCell {
                position: CellPosition {
                    row: local_row.saturating_add(row_offset),
                    col: col.saturating_add(col_offset),
                },
                text: cell.text.clone(),
                foreground,
                background,
                style: style_for_attributes(cell.attributes),
            });
        }
    }

    append_terminal_visuals(
        target,
        terminal,
        semantic_timeline,
        search,
        command_output_collapsed,
        metrics,
        config,
        presentation,
        row_offset,
        col_offset,
        include_cursor,
    );
}

#[allow(clippy::too_many_arguments)]
fn append_terminal_visuals(
    target: &mut RenderScene,
    terminal: &TerminalEmulator,
    semantic_timeline: &SemanticTimelineStore,
    search: &PaneSearch,
    command_output_collapsed: &HashMap<u64, bool>,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    presentation: CursorPresentation,
    row_offset: i64,
    col_offset: u16,
    include_cursor: bool,
) {
    append_terminal_semantic_overlays(
        target,
        terminal,
        semantic_timeline,
        command_output_collapsed,
        metrics,
        config,
        row_offset,
        col_offset,
    );
    append_terminal_search_overlays(
        target,
        terminal,
        search,
        metrics,
        config,
        row_offset,
        col_offset,
    );
    append_terminal_selection(target, terminal, config, row_offset, col_offset);
    if include_cursor {
        target.cursor = Some(terminal_cursor_visual(
            terminal,
            metrics,
            config,
            presentation,
            row_offset,
            col_offset,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn append_terminal_semantic_overlays(
    target: &mut RenderScene,
    terminal: &TerminalEmulator,
    semantic_timeline: &SemanticTimelineStore,
    command_output_collapsed: &HashMap<u64, bool>,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    row_offset: i64,
    col_offset: u16,
) {
    let viewport = terminal.state().viewport();
    let modes = terminal.modes_ref();
    if let Some(metrics) = metrics {
        let mut semantic = semantic_visual_overlays(
            semantic_timeline,
            command_output_collapsed,
            modes.contains(&TerminalMode::AlternateScreen),
            SemanticOverlayViewport {
                origin_row: viewport.origin_row,
                rows: viewport.size.rows,
                cols: viewport.size.cols,
                metrics,
            },
            config,
        );
        for overlay in &mut semantic {
            offset_rect(&mut overlay.bounds, row_offset, col_offset, Some(metrics));
        }
        target.semantic_overlays.extend(semantic);
    }
}

#[allow(clippy::too_many_arguments)]
fn append_terminal_search_overlays(
    target: &mut RenderScene,
    terminal: &TerminalEmulator,
    search: &PaneSearch,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    row_offset: i64,
    col_offset: u16,
) {
    let viewport = terminal.state().viewport();
    if let Some(metrics) = metrics {
        let mut highlights = search_overlays(search, viewport, metrics, config);
        for overlay in &mut highlights {
            offset_rect(&mut overlay.bounds, row_offset, col_offset, Some(metrics));
        }
        target.search_highlights.extend(highlights);
    }
}

fn append_terminal_selection(
    target: &mut RenderScene,
    terminal: &TerminalEmulator,
    config: &AppConfig,
    row_offset: i64,
    col_offset: u16,
) {
    let viewport = terminal.state().viewport();
    if let Some(mut visual) = selection_visual(terminal, viewport, config) {
        for position in &mut visual.cells {
            position.row = position.row.saturating_add(row_offset);
            position.col = position.col.saturating_add(col_offset);
        }
        target.selections.push(visual);
    }
}

fn terminal_cursor_visual(
    terminal: &TerminalEmulator,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    presentation: CursorPresentation,
    row_offset: i64,
    col_offset: u16,
) -> CursorVisual {
    let cursor = terminal.cursor_state();
    let modes = terminal.modes_ref();
    let configured_cursor_shape =
        resolved_cursor_shape(config, cursor.shape, modes, presentation.window_focused);
    CursorVisual {
        position: CellPosition {
            row: cursor.position.row.saturating_add(row_offset),
            col: cursor.position.col.saturating_add(col_offset),
        },
        shape: configured_cursor_shape,
        color: render_color(if presentation.window_focused {
            config.cursor.color.unwrap_or(config.colors.cursor)
        } else {
            config
                .cursor
                .inactive_color
                .or(config.cursor.color)
                .unwrap_or(config.colors.cursor)
        }),
        text_color: config.colors.cursor_text.map(render_color),
        visible: cursor.visible
            && terminal.state().viewport_offset() == 0
            && (!presentation.window_focused || presentation.blink_visible),
        thickness_percent: (config.cursor.thickness.clamp(0.05, 1.0) * 100.0).round() as u8,
        corner_radius_px: cursor_radius_px(config, metrics),
        inactive: !presentation.window_focused,
    }
}

#[cfg(test)]
fn scene_from_terminal(
    terminal: &TerminalEmulator,
    semantic_timeline: &SemanticTimelineStore,
    search: &PaneSearch,
    command_output_collapsed: &HashMap<u64, bool>,
    metrics: Option<CellMetrics>,
    config: &AppConfig,
    presentation: CursorPresentation,
) -> RenderScene {
    let viewport = terminal.state().viewport();
    let mut scene = RenderScene {
        grid: RenderGrid {
            columns: viewport.size.cols,
            rows: viewport.size.rows,
            cells: Vec::new(),
        },
        ..RenderScene::default()
    };
    append_terminal_scene(
        &mut scene,
        terminal,
        semantic_timeline,
        search,
        command_output_collapsed,
        metrics,
        config,
        presentation,
        0,
        0,
        true,
    );
    scene
}

fn resolved_cursor_shape(
    config: &AppConfig,
    terminal_shape: CursorShape,
    modes: &BTreeSet<TerminalMode>,
    focused: bool,
) -> RenderCursorShape {
    if !focused {
        return render_cursor_shape(config.cursor.inactive_shape);
    }
    let mode = if modes.contains(&TerminalMode::AlternateScreen) {
        Some("alternate_screen")
    } else if modes.contains(&TerminalMode::Insert) {
        Some("insert")
    } else if modes.contains(&TerminalMode::ApplicationCursorKeys) {
        Some("application_cursor")
    } else if modes.contains(&TerminalMode::ApplicationKeypad) {
        Some("application_keypad")
    } else {
        Some("normal")
    };
    if let Some(shape) = mode.and_then(|mode| {
        config
            .cursor
            .mode_specific_styles
            .iter()
            .find(|(configured, _)| configured.eq_ignore_ascii_case(mode))
            .map(|(_, shape)| shape)
    }) {
        return render_cursor_shape(*shape);
    }
    match terminal_shape {
        CursorShape::Beam => RenderCursorShape::Beam,
        CursorShape::Underline => RenderCursorShape::Underline,
        CursorShape::Block => render_cursor_shape(config.cursor.shape),
    }
}

fn search_overlays(
    search: &PaneSearch,
    viewport: term_core::Viewport,
    metrics: CellMetrics,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    let mut overlays = Vec::new();

    for visible_row in 0..viewport.size.rows {
        let row = viewport.origin_row + i64::from(visible_row);
        if let Some(spans) = search.rows.get(&row) {
            for span in spans {
                overlays.push(OverlayPrimitive {
                    kind: OverlayKind::SearchHighlight,
                    bounds: RenderRect {
                        x: (f32::from(span.start_col) * metrics.cell_width).floor() as i32,
                        y: (f32::from(visible_row) * metrics.cell_height).floor() as i32,
                        width: (f32::from(
                            span.end_col
                                .saturating_sub(span.start_col)
                                .saturating_add(1),
                        ) * metrics.cell_width)
                            .ceil() as u32,
                        height: metrics.cell_height.ceil() as u32,
                    },
                    color: if span.match_index == search.active_match {
                        render_color(config.colors.selection_background)
                    } else {
                        RenderColor {
                            red: 240,
                            green: 190,
                            blue: 50,
                            alpha: 90,
                        }
                    },
                    border_color: None,
                    border_width_px: 0,
                    corner_radius_px: 1,
                    z_index: 12,
                    label: None,
                    label_color: None,
                });
            }
        }
    }

    if search.input_active {
        let status = if search.matches.is_empty() {
            "0/0".to_owned()
        } else {
            format!("{}/{}", search.active_match + 1, search.matches.len())
        };
        let panel_cols = usize::from(viewport.size.cols).clamp(1, 42);
        let label = truncate_overlay_label(
            &format!("Find: {}  {status}", search.query),
            panel_cols.saturating_sub(2).max(1),
        );
        overlays.push(OverlayPrimitive {
            kind: OverlayKind::SearchHighlight,
            bounds: RenderRect {
                x: 6,
                y: 6,
                width: (metrics.cell_width * panel_cols as f32).ceil() as u32,
                height: (metrics.cell_height + 8.0).ceil() as u32,
            },
            color: RenderColor {
                red: 20,
                green: 24,
                blue: 30,
                alpha: 235,
            },
            border_color: Some(render_color(config.colors.selection_background)),
            border_width_px: 1,
            corner_radius_px: 4,
            z_index: 100,
            label: Some(label),
            label_color: None,
        });
    }
    overlays
}

fn selection_visual(
    terminal: &TerminalEmulator,
    viewport: term_core::Viewport,
    config: &AppConfig,
) -> Option<SelectionVisual> {
    let selection = terminal.selection_state()?;
    let (start, end) = if selection.start <= selection.end {
        (selection.start, selection.end)
    } else {
        (selection.end, selection.start)
    };
    let mut cells = Vec::new();
    for visible_row in 0..viewport.size.rows {
        let absolute_row = viewport.origin_row + i64::from(visible_row);
        if absolute_row < start.row || absolute_row > end.row {
            continue;
        }
        for col in 0..viewport.size.cols {
            let selected = match selection.kind {
                SelectionKind::Rectangular => {
                    col >= start.col.min(end.col) && col <= start.col.max(end.col)
                }
                SelectionKind::Normal if start.row == end.row => col >= start.col && col <= end.col,
                SelectionKind::Normal if absolute_row == start.row => col >= start.col,
                SelectionKind::Normal if absolute_row == end.row => col <= end.col,
                SelectionKind::Normal => true,
            };
            if selected {
                cells.push(CellPosition {
                    row: i64::from(visible_row),
                    col,
                });
            }
        }
    }

    (!cells.is_empty()).then_some(SelectionVisual {
        cells,
        color: render_color(config.colors.selection_background),
    })
}

fn render_cursor_shape(shape: config_core::CursorShape) -> RenderCursorShape {
    match shape {
        config_core::CursorShape::Block => RenderCursorShape::Block,
        config_core::CursorShape::Beam => RenderCursorShape::Beam,
        config_core::CursorShape::Underline => RenderCursorShape::Underline,
        config_core::CursorShape::HollowBlock => RenderCursorShape::HollowBlock,
        config_core::CursorShape::Custom => RenderCursorShape::Custom,
        config_core::CursorShape::CustomStaticShape => RenderCursorShape::CustomStaticShape,
    }
}

fn cursor_radius_px(config: &AppConfig, metrics: Option<CellMetrics>) -> u8 {
    let cell_edge = metrics.map_or(16.0, |metrics| metrics.cell_width.min(metrics.cell_height));
    let radius = config.cursor.corner_radius.clamp(0.0, 0.5) * f64::from(cell_edge);
    radius.round() as u8
}

fn visible_url_hints(terminal: &TerminalEmulator, row: u16) -> Vec<semantics::DetectedHint> {
    let Some(line) = terminal.state().visible_line(row) else {
        return Vec::new();
    };
    let text = line.raw_text();
    let mut hints = detect_url_hints([(i64::from(row), text.as_str())]);
    for hint in &mut hints {
        hint.start.col = line_column_for_char_offset(&line, usize::from(hint.start.col));
        hint.end.col = line_column_for_char_offset(&line, usize::from(hint.end.col));
    }
    hints
}

fn line_column_for_char_offset(line: &term_core::Line, offset: usize) -> u16 {
    let mut chars = 0usize;
    let mut col = 0u16;
    for cell in &line.cells {
        if cell.wide_continuation {
            continue;
        }
        if offset <= chars {
            return col;
        }
        let next_chars = chars.saturating_add(cell.text.chars().count());
        if offset < next_chars {
            return col;
        }
        chars = next_chars;
        col = col.saturating_add(u16::from(cell.width.max(1)));
    }
    col
}
