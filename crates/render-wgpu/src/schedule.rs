// Demand-driven frame scheduling and animation pacing.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationFramePacerDecision {
    Idle,
    WaitUntil(Instant),
    FrameDue,
}

#[derive(Debug, Default)]
pub struct AnimationFramePacer {
    deadline: Option<Instant>,
}

impl AnimationFramePacer {
    #[must_use]
    pub const fn new() -> Self {
        Self { deadline: None }
    }

    pub fn poll(
        &mut self,
        now: Instant,
        next_frame_after: Option<Duration>,
    ) -> AnimationFramePacerDecision {
        let Some(delay) = next_frame_after else {
            self.deadline = None;
            return AnimationFramePacerDecision::Idle;
        };
        let requested = now + delay;
        let deadline = self
            .deadline
            .map_or(requested, |deadline| deadline.min(requested));
        if now >= deadline {
            self.deadline = None;
            AnimationFramePacerDecision::FrameDue
        } else {
            self.deadline = Some(deadline);
            AnimationFramePacerDecision::WaitUntil(deadline)
        }
    }
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
        if self
            .pending
            .is_none_or(|pending| frame_request_priority(reason) > frame_request_priority(pending))
        {
            self.pending = Some(reason);
        }
    }

    #[must_use]
    pub const fn has_pending_frame(&self) -> bool {
        self.pending.is_some()
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

const fn frame_request_priority(reason: FrameRequestReason) -> u8 {
    match reason {
        FrameRequestReason::Explicit => 6,
        FrameRequestReason::WindowResized => 5,
        FrameRequestReason::TerminalContentChanged => 4,
        FrameRequestReason::SelectionChanged => 3,
        FrameRequestReason::CursorBlink => 2,
        FrameRequestReason::Animation => 1,
    }
}
