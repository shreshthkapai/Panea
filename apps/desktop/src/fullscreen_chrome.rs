use std::time::{Duration, Instant};

const MAX_PROGRESS: u16 = u16::MAX;
const DOUBLE_CLICK_DISTANCE: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromePhase {
    Hidden,
    Revealing,
    Visible,
    Hiding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeMotion {
    Instant,
    Smooth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeControl {
    Minimize,
    LeaveFullscreen,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeIntent {
    BeginDrag,
    Minimize,
    LeaveFullscreen,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromePresentation {
    pub progress: u16,
    pub hovered_control: Option<ChromeControl>,
    pub pressed_control: Option<ChromeControl>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromeUpdate {
    pub consumed: bool,
    pub redraw: bool,
    pub intent: Option<ChromeIntent>,
}

impl ChromeUpdate {
    #[must_use]
    pub const fn idle() -> Self {
        Self {
            consumed: false,
            redraw: false,
            intent: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromePoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromePointerButton {
    pub point: ChromePoint,
    pub pressed: bool,
    pub now: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromeSettings {
    pub enabled: bool,
    pub surface_width: u32,
    pub chrome_height: u32,
    pub reveal_height: u32,
    pub control_width: u32,
    pub motion: ChromeMotion,
    pub transition_duration: Duration,
    pub hide_delay: Duration,
    pub double_click_interval: Duration,
}

#[derive(Debug, Clone, Copy)]
struct Transition {
    started_at: Instant,
    from: u16,
    to: u16,
    next_frame: Instant,
}

#[derive(Debug, Clone, Copy)]
struct Click {
    point: ChromePoint,
    at: Instant,
}

#[derive(Debug)]
pub struct FullscreenChromeController {
    settings: ChromeSettings,
    active: bool,
    phase: ChromePhase,
    progress: u16,
    transition: Option<Transition>,
    pointer_inside: bool,
    hovered_control: Option<ChromeControl>,
    captured_control: Option<ChromeControl>,
    hide_deadline: Option<Instant>,
    last_click: Option<Click>,
}

impl FullscreenChromeController {
    const FRAME_INTERVAL: Duration = Duration::from_millis(8);

    #[must_use]
    pub const fn new(settings: ChromeSettings) -> Self {
        Self {
            settings,
            active: true,
            phase: ChromePhase::Hidden,
            progress: 0,
            transition: None,
            pointer_inside: false,
            hovered_control: None,
            captured_control: None,
            hide_deadline: None,
            last_click: None,
        }
    }

    #[must_use]
    pub const fn phase(&self) -> ChromePhase {
        self.phase
    }

    #[must_use]
    pub const fn presentation(&self) -> Option<ChromePresentation> {
        if !self.operational() || matches!(self.phase, ChromePhase::Hidden) {
            return None;
        }
        Some(ChromePresentation {
            progress: self.progress,
            hovered_control: self.hovered_control,
            pressed_control: self.captured_control,
        })
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        match (self.transition, self.hide_deadline) {
            (Some(transition), Some(hide)) => Some(transition.next_frame.min(hide)),
            (Some(transition), None) => Some(transition.next_frame),
            (None, Some(hide)) => Some(hide),
            (None, None) => None,
        }
    }

    pub fn set_active(&mut self, active: bool) -> ChromeUpdate {
        if self.active == active {
            return ChromeUpdate::idle();
        }
        self.active = active;
        if active {
            ChromeUpdate::idle()
        } else {
            self.reset()
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) -> ChromeUpdate {
        if self.settings.enabled == enabled {
            return ChromeUpdate::idle();
        }
        self.settings.enabled = enabled;
        if enabled {
            ChromeUpdate::idle()
        } else {
            self.reset()
        }
    }

    pub fn pointer_moved(&mut self, point: ChromePoint, now: Instant) -> ChromeUpdate {
        if !self.operational() {
            return ChromeUpdate::idle();
        }

        let was_hovered = self.hovered_control;
        let inside_chrome = self.contains_chrome(point);
        self.pointer_inside = inside_chrome;
        self.hovered_control = inside_chrome.then(|| self.hit_control(point)).flatten();

        if matches!(self.phase, ChromePhase::Hidden) {
            if self.contains_reveal_strip(point) {
                self.begin_reveal(now);
                return ChromeUpdate {
                    consumed: true,
                    redraw: true,
                    intent: None,
                };
            }
            self.hovered_control = None;
            return ChromeUpdate::idle();
        }

        let mut redraw = was_hovered != self.hovered_control;
        if inside_chrome {
            self.hide_deadline = None;
            if matches!(self.phase, ChromePhase::Hiding) {
                self.begin_reveal(now);
                redraw = true;
            }
        } else if self.captured_control.is_none() && self.hide_deadline.is_none() {
            self.hide_deadline = Some(now + self.settings.hide_delay);
        }

        ChromeUpdate {
            consumed: inside_chrome || self.captured_control.is_some(),
            redraw,
            intent: None,
        }
    }

    pub fn pointer_button(&mut self, input: ChromePointerButton) -> ChromeUpdate {
        if !self.operational() || matches!(self.phase, ChromePhase::Hidden) {
            return ChromeUpdate::idle();
        }

        let hit = self
            .contains_chrome(input.point)
            .then(|| self.hit_control(input.point))
            .flatten();

        if input.pressed {
            if let Some(control) = hit {
                self.captured_control = Some(control);
                self.hide_deadline = None;
                return ChromeUpdate {
                    consumed: true,
                    redraw: true,
                    intent: None,
                };
            }
            if self.contains_chrome(input.point) {
                let is_double_click = self.last_click.is_some_and(|last| {
                    input.now.saturating_duration_since(last.at)
                        <= self.settings.double_click_interval
                        && points_are_near(last.point, input.point)
                });
                self.last_click = if is_double_click {
                    None
                } else {
                    Some(Click {
                        point: input.point,
                        at: input.now,
                    })
                };
                return ChromeUpdate {
                    consumed: true,
                    redraw: false,
                    intent: Some(if is_double_click {
                        ChromeIntent::LeaveFullscreen
                    } else {
                        ChromeIntent::BeginDrag
                    }),
                };
            }
            return ChromeUpdate::idle();
        }

        if let Some(captured) = self.captured_control.take() {
            return ChromeUpdate {
                consumed: true,
                redraw: true,
                intent: (hit == Some(captured)).then(|| intent_for_control(captured)),
            };
        }

        ChromeUpdate {
            consumed: self.contains_chrome(input.point),
            redraw: false,
            intent: None,
        }
    }

    pub fn focus_changed(&mut self, focused: bool, _now: Instant) -> ChromeUpdate {
        if focused {
            ChromeUpdate::idle()
        } else {
            self.reset()
        }
    }

    pub fn tick(&mut self, now: Instant) -> ChromeUpdate {
        if !self.operational() {
            return ChromeUpdate::idle();
        }

        if self.hide_deadline.is_some_and(|deadline| now >= deadline) {
            self.hide_deadline = None;
            self.begin_hide(now);
            return ChromeUpdate {
                consumed: false,
                redraw: true,
                intent: None,
            };
        }

        let Some(mut transition) = self.transition else {
            return ChromeUpdate::idle();
        };
        if now < transition.next_frame {
            return ChromeUpdate::idle();
        }

        let duration_nanos = self.settings.transition_duration.as_nanos();
        let elapsed_nanos = now
            .saturating_duration_since(transition.started_at)
            .as_nanos();
        if duration_nanos == 0 || elapsed_nanos >= duration_nanos {
            self.progress = transition.to;
            self.transition = None;
            self.phase = if transition.to == MAX_PROGRESS {
                ChromePhase::Visible
            } else {
                ChromePhase::Hidden
            };
            if matches!(self.phase, ChromePhase::Hidden) {
                self.clear_interaction();
            }
            return ChromeUpdate {
                consumed: false,
                redraw: true,
                intent: None,
            };
        }

        let linear = ((elapsed_nanos * u128::from(MAX_PROGRESS)) / duration_nanos) as u16;
        let eased = if transition.to > transition.from {
            ease_out(linear)
        } else {
            ease_in(linear)
        };
        let next_progress = interpolate(transition.from, transition.to, eased);
        let redraw = next_progress != self.progress;
        self.progress = next_progress;
        transition.next_frame = now + Self::FRAME_INTERVAL;
        self.transition = Some(transition);

        ChromeUpdate {
            consumed: false,
            redraw,
            intent: None,
        }
    }

    pub fn reset(&mut self) -> ChromeUpdate {
        let redraw = !matches!(self.phase, ChromePhase::Hidden)
            || self.progress != 0
            || self.hovered_control.is_some()
            || self.captured_control.is_some();
        self.phase = ChromePhase::Hidden;
        self.progress = 0;
        self.transition = None;
        self.hide_deadline = None;
        self.last_click = None;
        self.clear_interaction();
        ChromeUpdate {
            consumed: false,
            redraw,
            intent: None,
        }
    }

    const fn operational(&self) -> bool {
        self.active
            && self.settings.enabled
            && self.settings.surface_width > 0
            && self.settings.chrome_height > 0
            && self.settings.reveal_height > 0
    }

    fn begin_reveal(&mut self, now: Instant) {
        self.hide_deadline = None;
        if matches!(self.settings.motion, ChromeMotion::Instant)
            || self.settings.transition_duration.is_zero()
        {
            self.phase = ChromePhase::Visible;
            self.progress = MAX_PROGRESS;
            self.transition = None;
            return;
        }
        self.phase = ChromePhase::Revealing;
        self.progress = self.progress.max(1);
        self.transition = Some(Transition {
            started_at: now,
            from: self.progress,
            to: MAX_PROGRESS,
            next_frame: now,
        });
    }

    fn begin_hide(&mut self, now: Instant) {
        if matches!(self.settings.motion, ChromeMotion::Instant)
            || self.settings.transition_duration.is_zero()
        {
            self.phase = ChromePhase::Hidden;
            self.progress = 0;
            self.transition = None;
            self.clear_interaction();
            return;
        }
        self.phase = ChromePhase::Hiding;
        self.transition = Some(Transition {
            started_at: now,
            from: self.progress,
            to: 0,
            next_frame: now,
        });
    }

    fn contains_reveal_strip(&self, point: ChromePoint) -> bool {
        self.contains_x(point.x)
            && point.y >= 0.0
            && point.y < f64::from(self.settings.reveal_height)
    }

    fn contains_chrome(&self, point: ChromePoint) -> bool {
        self.contains_x(point.x)
            && point.y >= 0.0
            && point.y < f64::from(self.settings.chrome_height)
    }

    fn contains_x(&self, x: f64) -> bool {
        x >= 0.0 && x < f64::from(self.settings.surface_width)
    }

    fn hit_control(&self, point: ChromePoint) -> Option<ChromeControl> {
        if self.settings.control_width == 0 || !self.contains_chrome(point) {
            return None;
        }
        let distance_from_right = f64::from(self.settings.surface_width) - point.x;
        let slot = (distance_from_right / f64::from(self.settings.control_width)).floor() as u32;
        match slot {
            0 => Some(ChromeControl::Close),
            1 => Some(ChromeControl::LeaveFullscreen),
            2 => Some(ChromeControl::Minimize),
            _ => None,
        }
    }

    fn clear_interaction(&mut self) {
        self.pointer_inside = false;
        self.hovered_control = None;
        self.captured_control = None;
    }
}

fn points_are_near(left: ChromePoint, right: ChromePoint) -> bool {
    (left.x - right.x).abs() <= DOUBLE_CLICK_DISTANCE
        && (left.y - right.y).abs() <= DOUBLE_CLICK_DISTANCE
}

const fn intent_for_control(control: ChromeControl) -> ChromeIntent {
    match control {
        ChromeControl::Minimize => ChromeIntent::Minimize,
        ChromeControl::LeaveFullscreen => ChromeIntent::LeaveFullscreen,
        ChromeControl::Close => ChromeIntent::Close,
    }
}

fn ease_out(value: u16) -> u16 {
    let inverse = u128::from(MAX_PROGRESS - value);
    (u128::from(MAX_PROGRESS) - inverse * inverse / u128::from(MAX_PROGRESS)) as u16
}

fn ease_in(value: u16) -> u16 {
    let value = u128::from(value);
    (value * value / u128::from(MAX_PROGRESS)) as u16
}

fn interpolate(from: u16, to: u16, progress: u16) -> u16 {
    let from = i128::from(from);
    let distance = i128::from(to) - from;
    (from + distance * i128::from(progress) / i128::from(MAX_PROGRESS)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const REVEAL_DURATION: Duration = Duration::from_millis(120);
    const HIDE_DELAY: Duration = Duration::from_millis(180);

    fn controller(motion: ChromeMotion) -> FullscreenChromeController {
        FullscreenChromeController::new(ChromeSettings {
            enabled: true,
            surface_width: 1_000,
            chrome_height: 48,
            reveal_height: 3,
            control_width: 48,
            motion,
            transition_duration: REVEAL_DURATION,
            hide_delay: HIDE_DELAY,
            double_click_interval: Duration::from_millis(400),
        })
    }

    fn point(x: f64, y: f64) -> ChromePoint {
        ChromePoint { x, y }
    }

    fn reveal(chrome: &mut FullscreenChromeController, now: Instant) {
        assert!(chrome.pointer_moved(point(20.0, 1.0), now).redraw);
        chrome.tick(now + REVEAL_DURATION);
        assert_eq!(chrome.phase(), ChromePhase::Visible);
    }

    #[test]
    fn fullscreen_chrome_reveals_from_hidden_top_edge() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Smooth);

        assert_eq!(chrome.phase(), ChromePhase::Hidden);
        assert_eq!(chrome.presentation(), None);

        let update = chrome.pointer_moved(point(20.0, 1.0), now);

        assert!(update.consumed);
        assert!(update.redraw);
        assert_eq!(update.intent, None);
        assert_eq!(chrome.phase(), ChromePhase::Revealing);
        assert_eq!(chrome.next_deadline(), Some(now));
        assert_eq!(chrome.presentation().map(|value| value.progress), Some(1));
    }

    #[test]
    fn fullscreen_chrome_leaving_starts_delayed_hide() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Smooth);
        reveal(&mut chrome, now);

        let left_at = now + REVEAL_DURATION + Duration::from_millis(1);
        let update = chrome.pointer_moved(point(20.0, 80.0), left_at);

        assert!(!update.consumed);
        assert!(!update.redraw);
        assert_eq!(chrome.phase(), ChromePhase::Visible);
        assert_eq!(chrome.next_deadline(), Some(left_at + HIDE_DELAY));
        assert_eq!(chrome.tick(left_at + HIDE_DELAY / 2), ChromeUpdate::idle());
        assert_eq!(chrome.phase(), ChromePhase::Visible);
        assert!(chrome.tick(left_at + HIDE_DELAY).redraw);
        assert_eq!(chrome.phase(), ChromePhase::Hiding);
    }

    #[test]
    fn fullscreen_chrome_reentry_cancels_pending_hide() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Smooth);
        reveal(&mut chrome, now);
        let left_at = now + REVEAL_DURATION + Duration::from_millis(1);

        chrome.pointer_moved(point(20.0, 80.0), left_at);
        let update = chrome.pointer_moved(point(20.0, 10.0), left_at + HIDE_DELAY / 2);

        assert!(update.consumed);
        assert!(!update.redraw);
        assert_eq!(chrome.phase(), ChromePhase::Visible);
        assert_eq!(chrome.next_deadline(), None);
    }

    #[test]
    fn fullscreen_chrome_reverses_hiding_on_reentry() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Smooth);
        reveal(&mut chrome, now);
        let left_at = now + REVEAL_DURATION;
        chrome.pointer_moved(point(20.0, 80.0), left_at);
        chrome.tick(left_at + HIDE_DELAY);
        chrome.tick(left_at + HIDE_DELAY + REVEAL_DURATION / 2);
        let before = chrome.presentation().expect("hiding chrome").progress;

        let update = chrome.pointer_moved(
            point(20.0, 10.0),
            left_at + HIDE_DELAY + REVEAL_DURATION / 2,
        );

        assert!(update.redraw);
        assert_eq!(chrome.phase(), ChromePhase::Revealing);
        assert_eq!(
            chrome.presentation().expect("reversing chrome").progress,
            before
        );
    }

    #[test]
    fn fullscreen_chrome_instant_motion_has_no_animation_deadline() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Instant);

        let reveal_update = chrome.pointer_moved(point(20.0, 1.0), now);
        assert!(reveal_update.redraw);
        assert_eq!(chrome.phase(), ChromePhase::Visible);
        assert_eq!(chrome.next_deadline(), None);
        assert_eq!(
            chrome.presentation().expect("visible chrome").progress,
            u16::MAX
        );

        chrome.pointer_moved(point(20.0, 80.0), now);
        let hide_update = chrome.tick(now + HIDE_DELAY);
        assert!(hide_update.redraw);
        assert_eq!(chrome.phase(), ChromePhase::Hidden);
        assert_eq!(chrome.presentation(), None);
    }

    #[test]
    fn fullscreen_chrome_focus_loss_cancels_interaction_and_hides() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Smooth);
        reveal(&mut chrome, now);
        chrome.pointer_button(ChromePointerButton {
            point: point(980.0, 20.0),
            pressed: true,
            now,
        });

        let update = chrome.focus_changed(false, now + Duration::from_millis(1));

        assert!(update.redraw);
        assert_eq!(chrome.phase(), ChromePhase::Hidden);
        assert_eq!(chrome.presentation(), None);
        assert_eq!(chrome.next_deadline(), None);
    }

    #[test]
    fn fullscreen_chrome_control_press_is_captured_until_release() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Instant);
        reveal(&mut chrome, now);

        let pressed = chrome.pointer_button(ChromePointerButton {
            point: point(980.0, 20.0),
            pressed: true,
            now,
        });
        chrome.pointer_moved(point(700.0, 20.0), now + Duration::from_millis(1));
        let released_outside = chrome.pointer_button(ChromePointerButton {
            point: point(700.0, 20.0),
            pressed: false,
            now: now + Duration::from_millis(2),
        });

        assert!(pressed.consumed);
        assert_eq!(pressed.intent, None);
        assert!(released_outside.consumed);
        assert_eq!(released_outside.intent, None);
        assert_eq!(
            chrome
                .presentation()
                .expect("visible chrome")
                .pressed_control,
            None
        );

        chrome.pointer_button(ChromePointerButton {
            point: point(980.0, 20.0),
            pressed: true,
            now: now + Duration::from_millis(3),
        });
        let released_inside = chrome.pointer_button(ChromePointerButton {
            point: point(980.0, 20.0),
            pressed: false,
            now: now + Duration::from_millis(4),
        });
        assert_eq!(released_inside.intent, Some(ChromeIntent::Close));
    }

    #[test]
    fn fullscreen_chrome_non_control_press_begins_drag() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Instant);
        reveal(&mut chrome, now);

        let update = chrome.pointer_button(ChromePointerButton {
            point: point(200.0, 20.0),
            pressed: true,
            now,
        });

        assert!(update.consumed);
        assert_eq!(update.intent, Some(ChromeIntent::BeginDrag));
    }

    #[test]
    fn fullscreen_chrome_double_click_non_control_leaves_fullscreen() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Instant);
        reveal(&mut chrome, now);

        let first = chrome.pointer_button(ChromePointerButton {
            point: point(200.0, 20.0),
            pressed: true,
            now,
        });
        chrome.pointer_button(ChromePointerButton {
            point: point(200.0, 20.0),
            pressed: false,
            now: now + Duration::from_millis(10),
        });
        let second = chrome.pointer_button(ChromePointerButton {
            point: point(202.0, 20.0),
            pressed: true,
            now: now + Duration::from_millis(200),
        });

        assert_eq!(first.intent, Some(ChromeIntent::BeginDrag));
        assert_eq!(second.intent, Some(ChromeIntent::LeaveFullscreen));
    }

    #[test]
    fn fullscreen_chrome_resets_outside_supported_fullscreen_modes() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Smooth);
        reveal(&mut chrome, now);

        let update = chrome.set_active(false);

        assert!(update.redraw);
        assert_eq!(chrome.phase(), ChromePhase::Hidden);
        assert_eq!(chrome.presentation(), None);
        assert_eq!(chrome.next_deadline(), None);
        assert_eq!(
            chrome.pointer_moved(point(20.0, 1.0), now),
            ChromeUpdate::idle()
        );
    }

    #[test]
    fn fullscreen_chrome_disabled_hidden_state_has_zero_idle_work() {
        let now = Instant::now();
        let mut chrome = controller(ChromeMotion::Smooth);
        chrome.set_enabled(false);

        for offset in 0..10 {
            assert_eq!(
                chrome.pointer_moved(point(20.0, 10.0), now + Duration::from_millis(offset),),
                ChromeUpdate::idle()
            );
        }
        assert_eq!(chrome.next_deadline(), None);
        assert_eq!(chrome.presentation(), None);
    }
}
