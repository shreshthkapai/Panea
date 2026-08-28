// Prompt, command-block, input/output, and semantic badge overlays.

#[derive(Debug, Clone, Copy)]
struct SemanticOverlayViewport {
    origin_row: i64,
    rows: u16,
    cols: u16,
    metrics: CellMetrics,
}

fn semantic_visual_overlays(
    semantic_timeline: &SemanticTimelineStore,
    command_output_collapsed: &HashMap<u64, bool>,
    alternate_screen_active: bool,
    viewport: SemanticOverlayViewport,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    let mut overlays = Vec::new();

    if config.prompt_decorations.enabled
        && (!alternate_screen_active || config.prompt_decorations.allow_in_alternate_screen)
    {
        overlays.extend(prompt_decoration_overlays(
            semantic_timeline,
            viewport,
            config,
        ));
    }
    if config.command_blocks.enabled
        && (!alternate_screen_active || config.command_blocks.allow_in_alternate_screen)
    {
        overlays.extend(command_block_overlays(
            semantic_timeline,
            command_output_collapsed,
            viewport,
            config,
        ));
    }

    overlays
}

fn prompt_decoration_overlays(
    semantic_timeline: &SemanticTimelineStore,
    viewport: SemanticOverlayViewport,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    semantic_timeline
        .regions()
        .filter(|region| region.kind == SemanticRegionKind::Prompt)
        .filter_map(|region| {
            let span = region.span()?;
            let raw_bounds = row_overlay_bounds(span.start.row, span.end.row, viewport)?;
            let previous_status = semantic_timeline
                .previous_command(span.start)
                .map(|block| &block.status);
            let status_color = previous_status
                .map(|status| command_status_color(status, config))
                .unwrap_or_else(|| render_color(config.visual_theme.borders.color));
            let bounds = prompt_decoration_bounds(raw_bounds, viewport.metrics, config);
            let color = if config.prompt_decorations.style
                == PromptDecorationStyle::MinimalSeparator
                && config.prompt_decorations.show_previous_status_accent
                && previous_status.is_some()
            {
                status_color
            } else {
                prompt_decoration_color(config)
            };
            Some(OverlayPrimitive {
                kind: OverlayKind::PromptDecoration,
                bounds,
                color,
                border_color: match config.prompt_decorations.style {
                    PromptDecorationStyle::MinimalSeparator => None,
                    PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => {
                        Some(if config.prompt_decorations.show_previous_status_accent {
                            status_color
                        } else {
                            render_color(config.visual_theme.borders.color)
                        })
                    }
                },
                border_width_px: config.visual_theme.borders.width_px,
                corner_radius_px: match config.prompt_decorations.style {
                    PromptDecorationStyle::MinimalSeparator => 0,
                    PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => {
                        config.visual_theme.borders.radius_px
                    }
                },
                z_index: 20,
                label: prompt_badge_label(&region.metadata, previous_status, config),
                label_color: Some(render_color(config.visual_theme.badge_foreground)),
            })
        })
        .collect()
}

fn prompt_decoration_bounds(
    bounds: RenderRect,
    metrics: CellMetrics,
    config: &AppConfig,
) -> RenderRect {
    let mut bounds = inset_overlay_bounds(bounds, prompt_overlay_padding(config));
    match config.prompt_decorations.style {
        PromptDecorationStyle::MinimalSeparator => {
            let height = u32::from(config.visual_theme.borders.width_px.max(1));
            bounds.y = bounds
                .y
                .saturating_add(bounds.height.saturating_sub(height) as i32);
            bounds.height = height;
        }
        PromptDecorationStyle::RoundedBox => {}
        PromptDecorationStyle::PillHeader => {
            bounds.height = (metrics.cell_height * 0.86).ceil().max(12.0) as u32;
        }
    }
    bounds
}

fn command_block_overlays(
    semantic_timeline: &SemanticTimelineStore,
    command_output_collapsed: &HashMap<u64, bool>,
    viewport: SemanticOverlayViewport,
    config: &AppConfig,
) -> Vec<OverlayPrimitive> {
    let mut overlays = Vec::new();

    if config.command_blocks.style == CommandBlockStyle::Traditional {
        return overlays;
    }

    for block in semantic_timeline.command_blocks() {
        let Some(span) = semantic_timeline.command_span(block) else {
            continue;
        };
        let Some(raw_bounds) = row_overlay_bounds(span.start.row, span.end.row, viewport) else {
            continue;
        };

        let bounds = margin_overlay_bounds(raw_bounds, config.visual_theme.spacing.block_margin_px);
        let decoration_bounds = command_block_bounds(bounds, viewport.metrics, config);
        let metadata = semantic_timeline
            .command_metadata(block)
            .unwrap_or_else(|| semantic_timeline.metadata());
        let status_color = command_status_color(&block.status, config);
        overlays.push(OverlayPrimitive {
            kind: OverlayKind::CommandBlock,
            bounds: decoration_bounds,
            color: command_block_fill(config),
            border_color: Some(status_color),
            border_width_px: config.visual_theme.borders.width_px,
            corner_radius_px: command_block_corner_radius(config),
            z_index: 15,
            label: None,
            label_color: None,
        });

        if config.command_blocks.separate_prompt_input_output {
            append_input_output_group_overlays(
                &mut overlays,
                semantic_timeline,
                [
                    (block.input_region_id, "input"),
                    (block.output_region_id, "output"),
                ],
                viewport,
                config,
            );
        }

        append_command_badges(
            &mut overlays,
            bounds,
            block,
            metadata,
            status_color,
            viewport.metrics,
            config,
        );

        append_collapsed_output_overlay(
            &mut overlays,
            semantic_timeline,
            block,
            command_output_collapsed,
            viewport,
            config,
        );
    }

    overlays
}

fn append_collapsed_output_overlay(
    overlays: &mut Vec<OverlayPrimitive>,
    timeline: &SemanticTimelineStore,
    block: &semantics::CommandBlock,
    collapsed_overrides: &HashMap<u64, bool>,
    viewport: SemanticOverlayViewport,
    config: &AppConfig,
) {
    let Some(span) = timeline.output_span_for_command(block) else {
        return;
    };
    let output_rows = semantic_span_rows(span);
    let auto_collapsed = config.command_blocks.collapse_long_output
        && output_rows > u32::from(config.command_blocks.collapse_after_lines);
    if !collapsed_overrides
        .get(&block.region_id)
        .copied()
        .unwrap_or(auto_collapsed)
    {
        return;
    }

    let preview_rows = i64::from(config.command_blocks.collapsed_preview_lines);
    let hidden_start = span.start.row.saturating_add(preview_rows);
    if hidden_start >= span.end.row {
        return;
    }
    let Some(bounds) = row_overlay_bounds(hidden_start, span.end.row, viewport) else {
        return;
    };
    let hidden_rows = span.end.row.saturating_sub(hidden_start);
    overlays.push(OverlayPrimitive {
        kind: OverlayKind::ContentMask,
        bounds: inset_overlay_bounds(bounds, command_block_padding(config)),
        color: RenderColor {
            alpha: u8::MAX,
            ..render_color(config.colors.background)
        },
        border_color: Some(render_color(config.visual_theme.borders.color)),
        border_width_px: config.visual_theme.borders.width_px,
        corner_radius_px: command_block_corner_radius(config),
        z_index: 40,
        label: Some(format!("{hidden_rows} output lines collapsed")),
        label_color: Some(render_color(config.visual_theme.badge_foreground)),
    });
}

fn semantic_span_rows(span: SemanticSpan) -> u32 {
    u32::try_from(span.end.row.saturating_sub(span.start.row).max(0)).unwrap_or(u32::MAX)
}

fn append_input_output_group_overlays(
    overlays: &mut Vec<OverlayPrimitive>,
    semantic_timeline: &SemanticTimelineStore,
    regions: [(Option<u64>, &'static str); 2],
    viewport: SemanticOverlayViewport,
    config: &AppConfig,
) {
    if config.visual_theme.grouping_style == InputOutputGroupingStyle::Traditional {
        return;
    }
    for (region_id, label) in regions {
        let Some(region_id) = region_id else {
            continue;
        };
        let Some(span) = semantic_timeline
            .region(region_id)
            .and_then(|region| region.span())
        else {
            continue;
        };
        let Some(bounds) = row_overlay_bounds(span.start.row, span.end.row, viewport) else {
            continue;
        };
        let bounds = input_output_group_bounds(
            inset_overlay_bounds(bounds, input_output_group_padding(config)),
            viewport.metrics,
            config,
        );
        overlays.push(OverlayPrimitive {
            kind: OverlayKind::InputOutputGroup,
            bounds,
            color: input_output_group_color(label, config),
            border_color: None,
            border_width_px: 0,
            corner_radius_px: input_output_group_radius(config),
            z_index: 16,
            label: matches!(
                config.visual_theme.grouping_style,
                InputOutputGroupingStyle::MinimalHeaders
            )
            .then(|| label.to_owned()),
            label_color: None,
        });
    }
}

fn append_command_badges(
    overlays: &mut Vec<OverlayPrimitive>,
    bounds: RenderRect,
    block: &semantics::CommandBlock,
    metadata: &SemanticMetadata,
    status_color: RenderColor,
    metrics: CellMetrics,
    config: &AppConfig,
) {
    let labels = command_badge_labels(block, metadata, config);
    if labels.is_empty() {
        return;
    }

    let gap = i32::from(config.visual_theme.spacing.badge_gap_px.max(2));
    let badge_height = (metrics.cell_height * 0.72).ceil().max(12.0) as u32;
    let padding = i32::from(config.visual_theme.spacing.block_padding_px.max(3));
    let mut right = bounds.x + bounds.width as i32 - padding;
    let y = bounds.y + padding.min(bounds.height.saturating_sub(badge_height) as i32);

    for label in labels.into_iter().rev() {
        let width = badge_width(&label, metrics, config);
        if width == 0 || right - width as i32 <= bounds.x {
            continue;
        }
        right -= width as i32;
        overlays.push(OverlayPrimitive {
            kind: OverlayKind::Badge,
            bounds: RenderRect {
                x: right,
                y,
                width,
                height: badge_height,
            },
            color: badge_color(&label, status_color, config),
            border_color: None,
            border_width_px: 0,
            corner_radius_px: config.visual_theme.borders.radius_px.min(8),
            z_index: 35,
            label: Some(label),
            label_color: Some(render_color(config.visual_theme.badge_foreground)),
        });
        right -= gap;
    }
}

fn row_overlay_bounds(
    start_row: i64,
    end_row: i64,
    viewport: SemanticOverlayViewport,
) -> Option<RenderRect> {
    let start_row = start_row.saturating_sub(viewport.origin_row);
    let end_row = end_row.saturating_sub(viewport.origin_row);
    if end_row < 0 || start_row >= i64::from(viewport.rows) {
        return None;
    }

    let start = start_row.max(0);
    let end = end_row.max(start + 1).min(i64::from(viewport.rows));
    Some(RenderRect {
        x: 0,
        y: (start as f32 * viewport.metrics.cell_height).floor() as i32,
        width: (f32::from(viewport.cols) * viewport.metrics.cell_width).ceil() as u32,
        height: ((end - start) as f32 * viewport.metrics.cell_height).ceil() as u32,
    })
}

fn inset_overlay_bounds(bounds: RenderRect, padding_px: u8) -> RenderRect {
    let padding = u32::from(padding_px);
    if padding == 0 || bounds.width <= padding.saturating_mul(2) {
        return bounds;
    }

    RenderRect {
        x: bounds.x + padding as i32,
        y: bounds.y,
        width: bounds.width.saturating_sub(padding.saturating_mul(2)),
        height: bounds.height,
    }
}

fn margin_overlay_bounds(bounds: RenderRect, margin_px: u8) -> RenderRect {
    let margin = u32::from(margin_px);
    if margin == 0 {
        return bounds;
    }
    let horizontal = margin.saturating_mul(2).min(bounds.width);
    let vertical = margin.saturating_mul(2).min(bounds.height);
    RenderRect {
        x: bounds.x.saturating_add(margin as i32),
        y: bounds.y.saturating_add(margin as i32),
        width: bounds.width.saturating_sub(horizontal),
        height: bounds.height.saturating_sub(vertical),
    }
}

fn command_block_bounds(
    mut bounds: RenderRect,
    metrics: CellMetrics,
    config: &AppConfig,
) -> RenderRect {
    match config.command_blocks.style {
        CommandBlockStyle::Traditional
        | CommandBlockStyle::Card
        | CommandBlockStyle::Split
        | CommandBlockStyle::CustomTheme => bounds,
        CommandBlockStyle::Subtle => {
            let height = u32::from(config.visual_theme.borders.width_px.max(1));
            bounds.y = bounds
                .y
                .saturating_add(bounds.height.saturating_sub(height) as i32);
            bounds.height = height;
            bounds
        }
        CommandBlockStyle::MinimalHeader => {
            bounds.height = metrics.cell_height.ceil().max(1.0) as u32;
            bounds
        }
    }
}

fn input_output_group_bounds(
    mut bounds: RenderRect,
    metrics: CellMetrics,
    config: &AppConfig,
) -> RenderRect {
    match config.visual_theme.grouping_style {
        InputOutputGroupingStyle::Traditional
        | InputOutputGroupingStyle::CommandCards
        | InputOutputGroupingStyle::InputOutputSplit
        | InputOutputGroupingStyle::CustomTheme => bounds,
        InputOutputGroupingStyle::SubtleSeparators => {
            bounds.height = u32::from(config.visual_theme.borders.width_px.max(1));
            bounds
        }
        InputOutputGroupingStyle::MinimalHeaders => {
            bounds.height = metrics.cell_height.ceil().max(1.0) as u32;
            bounds
        }
    }
}

fn prompt_overlay_padding(config: &AppConfig) -> u8 {
    match config.prompt_decorations.style {
        PromptDecorationStyle::MinimalSeparator => 0,
        PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => {
            config.visual_theme.spacing.block_padding_px / 2
        }
    }
}

fn command_block_padding(config: &AppConfig) -> u8 {
    match config.command_blocks.style {
        CommandBlockStyle::Traditional => 0,
        CommandBlockStyle::Subtle => config.visual_theme.spacing.block_padding_px / 2,
        CommandBlockStyle::Card
        | CommandBlockStyle::Split
        | CommandBlockStyle::MinimalHeader
        | CommandBlockStyle::CustomTheme => config.visual_theme.spacing.block_padding_px,
    }
}

fn input_output_group_padding(config: &AppConfig) -> u8 {
    match config.visual_theme.grouping_style {
        InputOutputGroupingStyle::Traditional => 0,
        InputOutputGroupingStyle::SubtleSeparators | InputOutputGroupingStyle::MinimalHeaders => {
            config.visual_theme.spacing.block_padding_px / 2
        }
        InputOutputGroupingStyle::CommandCards
        | InputOutputGroupingStyle::InputOutputSplit
        | InputOutputGroupingStyle::CustomTheme => config.visual_theme.spacing.block_padding_px,
    }
}

fn command_block_corner_radius(config: &AppConfig) -> u8 {
    match config.command_blocks.style {
        CommandBlockStyle::Traditional => 0,
        CommandBlockStyle::Subtle | CommandBlockStyle::MinimalHeader => 2,
        CommandBlockStyle::Card | CommandBlockStyle::Split | CommandBlockStyle::CustomTheme => {
            config.visual_theme.borders.radius_px
        }
    }
}

fn input_output_group_radius(config: &AppConfig) -> u8 {
    match config.visual_theme.grouping_style {
        InputOutputGroupingStyle::Traditional
        | InputOutputGroupingStyle::SubtleSeparators
        | InputOutputGroupingStyle::MinimalHeaders => 0,
        InputOutputGroupingStyle::CommandCards
        | InputOutputGroupingStyle::InputOutputSplit
        | InputOutputGroupingStyle::CustomTheme => config.visual_theme.borders.radius_px / 2,
    }
}

fn prompt_decoration_color(config: &AppConfig) -> RenderColor {
    match config.prompt_decorations.style {
        PromptDecorationStyle::MinimalSeparator => RenderColor {
            alpha: config.visual_theme.borders.color.alpha.max(96),
            ..render_color(config.visual_theme.borders.color)
        },
        PromptDecorationStyle::RoundedBox | PromptDecorationStyle::PillHeader => {
            render_color(config.visual_theme.prompt_background)
        }
    }
}

fn command_block_fill(config: &AppConfig) -> RenderColor {
    match config.command_blocks.style {
        CommandBlockStyle::Traditional => RenderColor {
            alpha: 0,
            ..render_color(config.colors.background)
        },
        CommandBlockStyle::Subtle => RenderColor {
            alpha: 20,
            ..render_color(config.visual_theme.command_background)
        },
        CommandBlockStyle::Card
        | CommandBlockStyle::Split
        | CommandBlockStyle::MinimalHeader
        | CommandBlockStyle::CustomTheme => render_color(config.visual_theme.command_background),
    }
}

fn input_output_group_color(label: &str, config: &AppConfig) -> RenderColor {
    let alpha = match config.visual_theme.grouping_style {
        InputOutputGroupingStyle::Traditional => 0,
        InputOutputGroupingStyle::SubtleSeparators => 18,
        InputOutputGroupingStyle::CommandCards => 34,
        InputOutputGroupingStyle::InputOutputSplit => 28,
        InputOutputGroupingStyle::MinimalHeaders => 22,
        InputOutputGroupingStyle::CustomTheme => 36,
    };
    match label {
        "input" => RenderColor {
            alpha,
            ..render_color(config.visual_theme.input_background)
        },
        _ => RenderColor {
            alpha,
            ..render_color(config.visual_theme.output_background)
        },
    }
}

fn prompt_badge_label(
    metadata: &SemanticMetadata,
    previous_status: Option<&CommandStatus>,
    config: &AppConfig,
) -> Option<String> {
    if config.prompt_decorations.style == PromptDecorationStyle::MinimalSeparator {
        return None;
    }
    let mut badges = Vec::new();
    if (config.prompt_decorations.show_shell_badge || config.visual_theme.badges.shell)
        && let Some(shell) = metadata.shell.shell.as_deref()
    {
        badges.push(truncate_badge_text(shell, 20));
    }
    if (config.prompt_decorations.show_current_directory
        || config.visual_theme.badges.current_directory)
        && let Some(cwd) = metadata
            .remote
            .as_ref()
            .and_then(|remote| remote.remote_current_working_directory.as_deref())
            .or(metadata.shell.current_working_directory.as_deref())
    {
        badges.push(compact_path_label(cwd));
    }
    if (config.prompt_decorations.show_remote_host || config.visual_theme.badges.remote)
        && let Some(remote) = metadata.remote.as_ref()
        && let Some(host) = remote.remote_host.as_deref()
    {
        badges.push(remote.remote_user.as_ref().map_or_else(
            || truncate_badge_text(host, 28),
            |user| truncate_badge_text(&format!("{user}@{host}"), 28),
        ));
    }
    if (config.prompt_decorations.show_admin_badge || config.visual_theme.badges.admin)
        && semantic_attribute_is_true(metadata, "elevated")
    {
        badges.push("admin".to_owned());
    }
    if config.prompt_decorations.show_previous_status_accent
        && let Some(status) = previous_status.and_then(command_status_label)
    {
        badges.push(status);
    }
    (!badges.is_empty()).then(|| truncate_badge_text(&badges.join(" "), 72))
}

fn semantic_attribute_is_true(metadata: &SemanticMetadata, key: &str) -> bool {
    metadata.attributes.iter().any(|(candidate, value)| {
        candidate.eq_ignore_ascii_case(key)
            && matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes")
    })
}

fn command_status_color(status: &CommandStatus, config: &AppConfig) -> RenderColor {
    match status {
        CommandStatus::Code(0) => render_color(config.visual_theme.success_color),
        CommandStatus::Code(_) | CommandStatus::Signal(_) => {
            render_color(config.visual_theme.error_color)
        }
        CommandStatus::Running | CommandStatus::Unknown => {
            render_color(config.visual_theme.borders.color)
        }
    }
}

fn command_badge_labels(
    block: &semantics::CommandBlock,
    metadata: &SemanticMetadata,
    config: &AppConfig,
) -> Vec<String> {
    let mut labels = Vec::new();
    if (config.command_blocks.show_exit_status || config.visual_theme.badges.status)
        && let Some(label) = command_status_label(&block.status)
    {
        labels.push(label);
    }
    if config.command_blocks.show_duration
        && let Some(duration) = block.duration
    {
        labels.push(format_duration_badge(duration));
    }
    if (config.command_blocks.show_current_directory
        || config.visual_theme.badges.current_directory)
        && let Some(cwd) = metadata
            .shell
            .current_working_directory
            .as_ref()
            .or_else(|| {
                metadata
                    .remote
                    .as_ref()
                    .and_then(|remote| remote.remote_current_working_directory.as_ref())
            })
    {
        labels.push(format!("cwd {}", compact_path_label(cwd)));
    }
    if (config.command_blocks.show_shell_host
        || config.visual_theme.badges.shell
        || config.visual_theme.badges.remote)
        && let Some(label) = shell_host_badge_label(metadata)
    {
        labels.push(label);
    }
    if config.visual_theme.badges.admin && semantic_attribute_is_true(metadata, "elevated") {
        labels.push("admin".to_owned());
    }
    labels
}

fn command_status_label(status: &CommandStatus) -> Option<String> {
    match status {
        CommandStatus::Code(0) => Some("ok".to_owned()),
        CommandStatus::Code(status) => Some(format!("exit {status}")),
        CommandStatus::Signal(signal) => Some(format!("signal {signal}")),
        CommandStatus::Running | CommandStatus::Unknown => None,
    }
}

fn format_duration_badge(duration: Duration) -> String {
    if duration.as_millis() < 1_000 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{:.1}s", duration.as_secs_f32())
    }
}

fn compact_path_label(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let Some(last) = trimmed.rsplit(['/', '\\']).find(|part| !part.is_empty()) else {
        return path.to_owned();
    };
    truncate_badge_text(last, 20)
}

fn shell_host_badge_label(metadata: &SemanticMetadata) -> Option<String> {
    if let Some(remote) = &metadata.remote
        && let Some(host) = &remote.remote_host
    {
        if let Some(user) = &remote.remote_user {
            return Some(truncate_badge_text(&format!("{user}@{host}"), 28));
        }
        return Some(truncate_badge_text(host, 28));
    }
    metadata
        .shell
        .shell
        .as_ref()
        .map(|shell| truncate_badge_text(shell, 20))
}

fn truncate_badge_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut out = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        out.push('~');
    }
    out
}

fn badge_width(label: &str, metrics: CellMetrics, config: &AppConfig) -> u32 {
    let text_width = label.chars().count() as f32 * metrics.cell_width * 0.62;
    let padding = f32::from(config.visual_theme.spacing.badge_gap_px.max(4)) * 2.0;
    (text_width + padding).ceil().max(16.0) as u32
}

fn badge_color(label: &str, status_color: RenderColor, config: &AppConfig) -> RenderColor {
    if label == "ok" || label.starts_with("exit ") || label.starts_with("signal ") {
        return RenderColor {
            alpha: 148,
            ..status_color
        };
    }
    render_color(config.visual_theme.badge_background)
}

fn style_for_attributes(attributes: CellAttributes) -> RenderCellStyle {
    RenderCellStyle {
        bold: attributes.bold,
        italic: attributes.italic,
        underline: attributes.underline,
        strikethrough: attributes.strikethrough,
        overline: attributes.overline,
        hidden: attributes.hidden,
    }
}

fn colors_for_attributes(
    attributes: CellAttributes,
    config: &AppConfig,
) -> (RenderColor, RenderColor) {
    let mut foreground = color_or_default(
        attributes.foreground,
        render_color(config.colors.foreground),
        config,
    );
    let mut background = color_or_default(
        attributes.background,
        render_color(config.colors.background),
        config,
    );
    if attributes.inverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    background.alpha = ((f64::from(background.alpha) * config.window.opacity)
        .round()
        .clamp(0.0, 255.0)) as u8;

    (foreground, background)
}

fn color_or_default(color: Option<Color>, default: RenderColor, config: &AppConfig) -> RenderColor {
    match color {
        Some(Color::Rgb { red, green, blue }) => RenderColor::rgb(red, green, blue),
        Some(Color::Indexed(index)) => ansi_color(index, config),
        Some(Color::DefaultForeground | Color::DefaultBackground) | None => default,
    }
}

fn render_color(color: config_core::RgbaColor) -> RenderColor {
    RenderColor {
        red: color.red,
        green: color.green,
        blue: color.blue,
        alpha: color.alpha,
    }
}

fn config_rgb(color: config_core::RgbaColor) -> [u8; 3] {
    [color.red, color.green, color.blue]
}
