// Cursor blink, movement, trail, pulse, and bounded animation state.

/// Cursor moves shorter than this stay snappy (typing keeps the tilt); longer
/// ones -- pane switches, prompt redraws -- glide instead.
const PANEA_CURSOR_JUMP_THRESHOLD_CELLS: u16 = 2;

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
    pub tilt: bool,
    pub smooth_movement: bool,
    /// Cells the cursor must travel on either axis before `smooth_movement`
    /// takes over from `tilt`. Zero animates every move.
    pub jump_threshold_cells: u16,
    pub typing_pulse: bool,
    pub typing_stretch: bool,
    pub trail: bool,
    pub trail_delay: Duration,
    pub trail_start_threshold_cells: u16,
    pub trail_decay_fast: Duration,
    pub trail_decay_slow: Duration,
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
            tilt: false,
            smooth_movement: false,
            jump_threshold_cells: 0,
            typing_pulse: false,
            typing_stretch: false,
            trail: false,
            trail_delay: Duration::from_millis(1),
            trail_start_threshold_cells: 2,
            trail_decay_fast: Duration::from_millis(100),
            trail_decay_slow: Duration::from_millis(400),
            blink_easing: false,
            short_lived_glow: false,
            shadow: false,
            fps: 60,
            max_active_animations: 8,
            max_animated_region_pixels: 250_000,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingCursorTrailTarget {
    region: RenderRect,
    color: RenderColor,
    observed_at: Instant,
}

#[derive(Debug)]
struct PersistentCursorTrail {
    corners: [[f32; 2]; 4],
    target: [[f32; 2]; 4],
    target_region: RenderRect,
    color: RenderColor,
    initialized: bool,
    active: bool,
    pending_target: Option<PendingCursorTrailTarget>,
}

impl Default for PersistentCursorTrail {
    fn default() -> Self {
        Self {
            corners: [[0.0; 2]; 4],
            target: [[0.0; 2]; 4],
            target_region: RenderRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            color: RenderColor::rgb(0, 0, 0),
            initialized: false,
            active: false,
            pending_target: None,
        }
    }
}

impl PersistentCursorTrail {
    fn reset(&mut self) {
        self.initialized = false;
        self.active = false;
        self.pending_target = None;
    }

    fn retarget(
        &mut self,
        target_region: RenderRect,
        color: RenderColor,
        cell_size: [u32; 2],
        settings: CursorAnimationSettings,
    ) {
        let target = rect_quad(target_region);
        self.pending_target = None;
        self.color = color;
        self.target_region = target_region;
        if !self.initialized {
            self.corners = target;
            self.target = target;
            self.initialized = true;
            return;
        }
        if self.target == target {
            return;
        }

        self.target = target;
        if !self.active
            && cursor_trail_move_within_threshold(
                quad_bounds(self.corners),
                target_region,
                cell_size,
                settings.trail_start_threshold_cells,
            )
        {
            self.corners = target;
            return;
        }
        self.active = true;
    }

    fn observe_target(
        &mut self,
        target_region: RenderRect,
        color: RenderColor,
        cell_size: [u32; 2],
        settings: CursorAnimationSettings,
        now: Instant,
    ) {
        if !self.initialized || settings.trail_delay.is_zero() {
            self.retarget(target_region, color, cell_size, settings);
            return;
        }
        if self.target_region == target_region {
            self.color = color;
            self.pending_target = None;
            return;
        }

        let Some(pending) = self.pending_target else {
            self.pending_target = Some(PendingCursorTrailTarget {
                region: target_region,
                color,
                observed_at: now,
            });
            return;
        };
        if pending.region != target_region {
            self.pending_target = Some(PendingCursorTrailTarget {
                region: target_region,
                color,
                observed_at: now,
            });
            return;
        }
        if now.saturating_duration_since(pending.observed_at) >= settings.trail_delay {
            self.retarget(target_region, color, cell_size, settings);
        } else if pending.color != color {
            self.pending_target = Some(PendingCursorTrailTarget { color, ..pending });
        }
    }

    fn pending_delay(&self, now: Instant, settings: CursorAnimationSettings) -> Option<Duration> {
        self.pending_target.map(|pending| {
            settings
                .trail_delay
                .saturating_sub(now.saturating_duration_since(pending.observed_at))
        })
    }

    fn advance(&mut self, elapsed: Duration, settings: CursorAnimationSettings) {
        if !self.active || elapsed.is_zero() {
            return;
        }

        let target_center = [
            (self.target[0][0] + self.target[2][0]) * 0.5,
            (self.target[0][1] + self.target[2][1]) * 0.5,
        ];
        let half_diagonal = (self.target[2][0] - self.target[0][0])
            .hypot(self.target[2][1] - self.target[0][1])
            .mul_add(0.5, 0.0)
            .max(f32::EPSILON);
        let mut deltas = [[0.0; 2]; 4];
        let mut projections = [0.0; 4];
        for index in 0..4 {
            let delta = [
                self.target[index][0] - self.corners[index][0],
                self.target[index][1] - self.corners[index][1],
            ];
            deltas[index] = delta;
            let length = delta[0].hypot(delta[1]);
            if length > f32::EPSILON {
                projections[index] = (delta[0] * (self.target[index][0] - target_center[0])
                    + delta[1] * (self.target[index][1] - target_center[1]))
                    / (half_diagonal * length);
            }
        }
        let minimum = projections.iter().copied().fold(f32::INFINITY, f32::min);
        let maximum = projections
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let span = maximum - minimum;
        let fast = settings.trail_decay_fast.as_secs_f32().max(0.001);
        let slow = settings
            .trail_decay_slow
            .max(settings.trail_decay_fast)
            .as_secs_f32()
            .max(fast);
        let elapsed = elapsed.as_secs_f32();

        for index in 0..4 {
            let blend = if span <= f32::EPSILON {
                0.0
            } else {
                ((projections[index] - minimum) / span).clamp(0.0, 1.0)
            };
            let decay = slow + (fast - slow) * blend;
            let step = 1.0 - (-10.0 * elapsed / decay).exp2();
            self.corners[index][0] += deltas[index][0] * step;
            self.corners[index][1] += deltas[index][1] * step;
        }

        if self
            .corners
            .iter()
            .zip(self.target)
            .all(|(corner, target)| {
                (corner[0] - target[0]).abs() < 0.5 && (corner[1] - target[1]).abs() < 0.5
            })
        {
            self.corners = self.target;
            self.active = false;
        }
        let region =
            cursor_animation_region(union_region(quad_bounds(self.corners), self.target_region));
        if region.width.saturating_mul(region.height) > settings.max_animated_region_pixels {
            self.corners = self.target;
            self.active = false;
        }
    }

    const fn needs_frame(&self) -> bool {
        self.active
    }

    fn visual(&self, settings: CursorAnimationSettings) -> Option<AnimationHandle> {
        self.active.then(|| {
            let current_bounds = quad_bounds(self.corners);
            let region = cursor_animation_region(union_region(current_bounds, self.target_region));
            debug_assert!(
                region.width.saturating_mul(region.height) <= settings.max_animated_region_pixels
            );
            AnimationHandle {
                id: u64::MAX,
                kind: AnimationKind::CursorTrail,
                affected_region: region,
                start_region: current_bounds,
                end_region: self.target_region,
                color: self.color,
                quad: Some(animation_quad(self.corners)),
                elapsed: Duration::ZERO,
                remaining: None,
            }
        })
    }
}

impl CursorAnimationSettings {
    #[must_use]
    pub const fn panea(
        fps: u16,
        max_active_animations: u16,
        max_animated_region_pixels: u32,
    ) -> Self {
        Self {
            enabled: true,
            tilt: true,
            smooth_movement: true,
            jump_threshold_cells: PANEA_CURSOR_JUMP_THRESHOLD_CELLS,
            typing_pulse: false,
            typing_stretch: false,
            trail: false,
            trail_delay: Duration::ZERO,
            trail_start_threshold_cells: 0,
            trail_decay_fast: Duration::from_millis(45),
            trail_decay_slow: Duration::from_millis(140),
            blink_easing: false,
            short_lived_glow: false,
            shadow: false,
            fps,
            max_active_animations,
            max_animated_region_pixels,
        }
    }

    #[must_use]
    pub const fn any_effect_enabled(self) -> bool {
        self.enabled
            && (self.tilt
                || self.smooth_movement
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
    trail: PersistentCursorTrail,
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
            trail: PersistentCursorTrail::default(),
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

    pub fn refresh_retained_scene(
        &mut self,
        scene: &mut RenderScene,
        metrics: CellMetrics,
        settings: CursorAnimationSettings,
    ) {
        scene
            .animations
            .retain(|animation| !cursor_runtime_owns(animation.kind));
        scene.damage_regions.clear();
        self.populate_scene(scene, metrics, settings);
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
            self.trail.reset();
            self.typing_requested = false;
            return;
        }

        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        advance_animations(&mut self.active, elapsed);
        self.trail.advance(elapsed, settings);

        let current = scene.cursor;
        if let Some(cursor) = current {
            let current_cell = cell_region(cursor.position, metrics);
            let current_region = cursor_animation_region(current_cell);
            if settings.trail && cursor.visible {
                self.trail.observe_target(
                    cursor_visual_region(cursor, metrics),
                    cursor.color,
                    [current_cell.width, current_cell.height],
                    settings,
                    now,
                );
            } else {
                self.trail.reset();
            }
            if let Some(previous) = self.previous_cursor
                && previous.position != cursor.position
            {
                let previous_cell = cell_region(previous.position, metrics);
                // A focus jump -- switching panes, or a prompt redrawing further
                // down -- moves the cursor an arbitrary distance on both axes.
                // Tilt only models same-row shear, so it never fires for stacked
                // panes and stops firing for side-by-side ones the moment the two
                // prompts land on different rows. Glide those instead: smooth
                // movement recomputes its affected region from the interpolated
                // cell every frame, so damage stays cell-sized however far the
                // cursor travelled.
                let glide = settings.smooth_movement
                    && !cursor_trail_move_within_threshold(
                        previous_cell,
                        current_cell,
                        [current_cell.width, current_cell.height],
                        settings.jump_threshold_cells,
                    );
                if settings.tilt
                    && !glide
                    && previous.position.row == cursor.position.row
                    && previous.position.col != cursor.position.col
                {
                    self.push_tilt_animation(
                        settings,
                        previous,
                        cursor,
                        if cursor.position.col > previous.position.col {
                            1.0
                        } else {
                            -1.0
                        },
                        metrics,
                    );
                }
                if glide {
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
        } else {
            self.trail.reset();
        }

        self.previous_cursor = current;
        self.typing_requested = false;
        scene.damage_regions.extend(
            self.active
                .iter()
                .map(|animation| animation.affected_region),
        );
        scene.animations.extend(self.active.iter().copied());
        if let Some(trail) = self.trail.visual(settings) {
            scene.damage_regions.push(trail.affected_region);
            scene.animations.push(trail);
        }
    }

    #[must_use]
    pub fn needs_frame(&self) -> bool {
        !self.active.is_empty() || self.trail.needs_frame()
    }

    #[must_use]
    pub fn next_frame_after(&self, settings: CursorAnimationSettings) -> Option<Duration> {
        let active_delay = self.needs_frame().then(|| settings.frame_interval());
        let pending_delay = self.trail.pending_delay(Instant::now(), settings);
        match (active_delay, pending_delay) {
            (Some(active), Some(pending)) => Some(active.min(pending)),
            (Some(delay), None) | (None, Some(delay)) => Some(delay),
            (None, None) => None,
        }
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
            quad: None,
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

    fn push_tilt_animation(
        &mut self,
        settings: CursorAnimationSettings,
        previous: CursorVisual,
        cursor: CursorVisual,
        direction: f32,
        metrics: CellMetrics,
    ) {
        let region = cursor_visual_region(cursor, metrics);
        let previous_region = cursor_visual_region(previous, metrics);
        let shear = (region.height as f32 * 0.45)
            .min(metrics.cell_width)
            .max(1.0)
            * direction;
        let half_shear = shear * 0.5;
        let mut corners = rect_quad(region);
        corners[0][0] += half_shear;
        corners[1][0] += half_shear;
        corners[2][0] -= half_shear;
        corners[3][0] -= half_shear;
        let affected_region = expand_region(quad_bounds(corners), 2);
        if affected_region.width.saturating_mul(affected_region.height)
            > settings.max_animated_region_pixels
        {
            return;
        }

        let tilt = AnimationHandle {
            id: self.next_id,
            kind: AnimationKind::CursorTilt,
            affected_region,
            start_region: region,
            end_region: region,
            color: cursor.color,
            quad: Some(animation_quad(corners)),
            elapsed: Duration::ZERO,
            remaining: Some(Duration::from_millis(90)),
        };
        if !self.store_animation(settings, tilt) {
            return;
        }

        let mut extension_corners = rect_quad(previous_region);
        if direction > 0.0 {
            extension_corners[1] = corners[0];
            extension_corners[2] = corners[3];
        } else {
            extension_corners[0] = corners[1];
            extension_corners[3] = corners[2];
        }
        let extension_region = cursor_animation_region(quad_bounds(extension_corners));
        let extension = AnimationHandle {
            id: self.next_id,
            kind: AnimationKind::CursorElasticExtension,
            affected_region: extension_region,
            start_region: previous_region,
            end_region: region,
            color: cursor.color,
            quad: Some(animation_quad(extension_corners)),
            elapsed: Duration::ZERO,
            remaining: Some(Duration::from_millis(90)),
        };
        self.store_animation(settings, extension);
    }

    fn store_animation(
        &mut self,
        settings: CursorAnimationSettings,
        animation: AnimationHandle,
    ) -> bool {
        let pixels = animation
            .affected_region
            .width
            .saturating_mul(animation.affected_region.height);
        if pixels > settings.max_animated_region_pixels {
            return false;
        }
        if let Some(existing) = self
            .active
            .iter_mut()
            .find(|active| active.kind == animation.kind)
        {
            *existing = animation;
        } else if self.active.len() < usize::from(settings.max_active_animations) {
            self.active.push(animation);
        } else {
            return false;
        }
        self.next_id = self.next_id.saturating_add(1);
        true
    }
}

const fn cursor_runtime_owns(kind: AnimationKind) -> bool {
    !matches!(kind, AnimationKind::OverlayTransition)
}

fn advance_animations(animations: &mut Vec<AnimationHandle>, elapsed: Duration) {
    for animation in animations.iter_mut() {
        animation.elapsed = animation.elapsed.saturating_add(elapsed);
        if let Some(remaining) = animation.remaining {
            animation.remaining = Some(remaining.checked_sub(elapsed).unwrap_or(Duration::ZERO));
        }
        if animation.kind == AnimationKind::CursorSmoothMovement {
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

fn cursor_visual_region(cursor: CursorVisual, metrics: CellMetrics) -> RenderRect {
    let mut rect = cell_region(cursor.position, metrics);
    let thickness = u32::from(cursor.thickness_percent.clamp(1, 100));
    match cursor.shape {
        RenderCursorShape::Beam => {
            rect.width = ((rect.width * thickness) / 100).max(1);
        }
        RenderCursorShape::Underline => {
            let cell_height = rect.height;
            rect.height = ((rect.height * thickness) / 100).max(1);
            rect.y += cell_height.saturating_sub(rect.height) as i32;
        }
        RenderCursorShape::Block
        | RenderCursorShape::HollowBlock
        | RenderCursorShape::Custom
        | RenderCursorShape::CustomStaticShape => {}
    }
    rect
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
