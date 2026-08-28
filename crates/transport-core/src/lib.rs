//! Transport-agnostic session I/O contracts.

pub const LAYER: &str = "session transport";

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// High-water mark for output bytes that have been handed to the application
/// but not yet consumed. Once the queue reaches it the worker stops draining
/// its transport, which lets the reader's bounded queue fill and pushes
/// backpressure down to the child process instead of growing memory without
/// bound. Mature terminals block the reader rather than buffering an unbounded
/// backlog, because a producer that outruns the parser otherwise turns
/// `cat hugefile` into gigabytes of resident memory.
pub const MAX_QUEUED_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

/// Retry cadence used while output delivery is paused by backpressure. The
/// worker cannot rely on another reader wake to arrive, because the reader is
/// blocked on its own full queue.
const OUTPUT_BACKPRESSURE_RETRY_INTERVAL: Duration = Duration::from_millis(4);
const INPUT_BACKPRESSURE_RETRY_INTERVAL: Duration = Duration::from_millis(4);

/// Platform-neutral callback used to wake an application event loop when a
/// transport has output or lifecycle state ready to consume.
#[derive(Clone)]
pub struct TransportWakeHandle(Arc<TransportWakeInner>);

struct TransportWakeInner {
    wake: Box<dyn Fn() + Send + Sync + 'static>,
    pending: AtomicBool,
}

impl TransportWakeHandle {
    #[must_use]
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(TransportWakeInner {
            wake: Box::new(wake),
            pending: AtomicBool::new(false),
        }))
    }

    /// Requests one application wake for the current pending-output period.
    ///
    /// Returns `true` only for the caller that transitions the shared handle
    /// from idle to pending and invokes the callback.
    pub fn wake(&self) -> bool {
        if self.0.pending.swap(true, Ordering::AcqRel) {
            return false;
        }
        (self.0.wake)();
        true
    }

    /// Rearms the handle after the application starts draining output.
    ///
    /// Clearing before the drain lets a producer racing with that drain queue
    /// another event, so coalescing cannot strand newly arrived bytes.
    pub fn clear_pending(&self) {
        self.0.pending.store(false, Ordering::Release);
    }
}

impl fmt::Debug for TransportWakeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TransportWakeHandle(..)")
    }
}

/// Cloneable, non-blocking emergency termination hook retained outside the
/// transport worker. It lets the application break a backend out of blocked I/O
/// before requesting an asynchronous worker shutdown.
#[derive(Clone)]
pub struct TransportTerminationHandle(Arc<dyn Fn() + Send + Sync + 'static>);

impl TransportTerminationHandle {
    #[must_use]
    pub fn new(terminate: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(terminate))
    }

    pub fn terminate(&self) {
        (self.0)();
    }
}

impl fmt::Debug for TransportTerminationHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TransportTerminationHandle(..)")
    }
}

/// A terminal viewport size expressed in character cells and physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

impl TerminalSize {
    #[must_use]
    pub const fn new(cols: u16, rows: u16, pixel_width: u32, pixel_height: u32) -> Self {
        Self {
            cols,
            rows,
            pixel_width,
            pixel_height,
        }
    }
}

/// The transport backend that owns a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    LocalPty,
    WindowsPseudoconsole,
    Ssh,
    FutureMobileSsh,
}

/// Stable metadata about the connected session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    pub id: String,
    pub kind: TransportKind,
    pub title: Option<String>,
    pub shell: Option<String>,
    pub current_working_directory: Option<String>,
    pub remote_host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportLifecycleEvent {
    Started,
    Resized(TerminalSize),
    OutputReady,
    Exited { exit_code: Option<i32> },
    ShutdownRequested,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportState {
    Starting,
    Running,
    ClosingInput,
    TerminatingChild,
    DrainingOutput { exit_code: Option<i32> },
    Closed { exit_code: Option<i32> },
    Failed { message: String },
}

/// Bytes made available by a transport poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportOutput {
    pub bytes: Vec<u8>,
    pub closed: bool,
    pub lifecycle: Vec<TransportLifecycleEvent>,
}

impl TransportOutput {
    #[must_use]
    pub fn bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            closed: false,
            lifecycle: Vec::new(),
        }
    }

    #[must_use]
    pub const fn closed() -> Self {
        Self {
            bytes: Vec::new(),
            closed: true,
            lifecycle: Vec::new(),
        }
    }

    #[must_use]
    pub fn event(event: TransportLifecycleEvent) -> Self {
        Self {
            bytes: Vec::new(),
            closed: matches!(event, TransportLifecycleEvent::Closed),
            lifecycle: vec![event],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    message: String,
}

impl TransportError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for TransportError {}

pub type TransportResult<T> = Result<T, TransportError>;

/// Commands sent into the transport I/O loop by the window/platform layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportInput {
    Inline { len: u8, bytes: [u8; 32] },
    Heap(Vec<u8>),
}

impl TransportInput {
    #[must_use]
    pub fn copy_from_slice(bytes: &[u8]) -> Self {
        if bytes.len() <= 32 {
            let mut inline = [0; 32];
            inline[..bytes.len()].copy_from_slice(bytes);
            Self::Inline {
                len: bytes.len() as u8,
                bytes: inline,
            }
        } else {
            Self::Heap(bytes.to_vec())
        }
    }

    #[must_use]
    pub const fn spilled(&self) -> bool {
        matches!(self, Self::Heap(_))
    }
}

impl AsRef<[u8]> for TransportInput {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Inline { len, bytes } => &bytes[..usize::from(*len)],
            Self::Heap(bytes) => bytes,
        }
    }
}

impl From<Vec<u8>> for TransportInput {
    fn from(bytes: Vec<u8>) -> Self {
        if bytes.len() <= 32 {
            Self::copy_from_slice(&bytes)
        } else {
            Self::Heap(bytes)
        }
    }
}

impl From<&[u8]> for TransportInput {
    fn from(bytes: &[u8]) -> Self {
        Self::copy_from_slice(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportCommand {
    WriteInput(TransportInput),
    Resize(TerminalSize),
    #[doc(hidden)]
    PollOutput,
    Shutdown,
}

impl TransportCommand {
    #[must_use]
    pub fn write_input(bytes: impl Into<TransportInput>) -> Self {
        Self::WriteInput(bytes.into())
    }
}

/// Events emitted by the transport I/O loop for the application to consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportEvent {
    Output(Vec<u8>),
    Lifecycle(TransportLifecycleEvent),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportLoopError {
    Closed,
    Backpressure,
}

impl fmt::Display for TransportLoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("transport event loop is closed"),
            Self::Backpressure => f.write_str("transport event loop queue is full"),
        }
    }
}

impl Error for TransportLoopError {}

/// Non-blocking command/event pump for a single terminal transport.
pub struct TransportEventLoop {
    command_tx: Sender<TransportCommand>,
    event_rx: Receiver<TransportEvent>,
    handle: Option<JoinHandle<()>>,
    queued_output_bytes: Arc<AtomicUsize>,
    termination_handle: Option<TransportTerminationHandle>,
}

impl TransportEventLoop {
    #[must_use]
    pub fn spawn<T>(transport: T) -> Self
    where
        T: TerminalTransport + 'static,
    {
        Self::spawn_with_waker_inner(transport, None)
    }

    #[must_use]
    pub fn spawn_with_waker<T>(transport: T, app_waker: TransportWakeHandle) -> Self
    where
        T: TerminalTransport + 'static,
    {
        Self::spawn_with_waker_inner(transport, Some(app_waker))
    }

    fn spawn_with_waker_inner<T>(mut transport: T, app_waker: Option<TransportWakeHandle>) -> Self
    where
        T: TerminalTransport + 'static,
    {
        // Input is intentionally unbounded, matching mature terminal event
        // loops: a temporarily busy PTY must never make the window thread
        // block or discard already accepted keystrokes.
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let output_wake_tx = command_tx.clone();
        let backend_waker = TransportWakeHandle::new(move || {
            let _ = output_wake_tx.send(TransportCommand::PollOutput);
        });
        transport.set_output_waker(Some(backend_waker.clone()));
        let termination_handle = transport.termination_handle();
        let periodic_poll_interval = transport.periodic_poll_interval();
        let queued_output_bytes = Arc::new(AtomicUsize::new(0));
        let worker_queued_output_bytes = Arc::clone(&queued_output_bytes);

        let handle = thread::spawn(move || {
            let mut running = true;
            let mut pending_input = Vec::with_capacity(256);
            // Set while output delivery is paused because the application has
            // not consumed what it already has. The wait below becomes bounded
            // so the pause is re-evaluated without needing a reader wake.
            let mut output_backpressure = false;
            let mut poll_again = false;

            while running {
                let wait_interval = if poll_again {
                    Some(Duration::ZERO)
                } else if output_backpressure || !pending_input.is_empty() {
                    Some(match periodic_poll_interval {
                        Some(interval) => interval.min(if output_backpressure {
                            OUTPUT_BACKPRESSURE_RETRY_INTERVAL
                        } else {
                            INPUT_BACKPRESSURE_RETRY_INTERVAL
                        }),
                        None if output_backpressure => OUTPUT_BACKPRESSURE_RETRY_INTERVAL,
                        None => INPUT_BACKPRESSURE_RETRY_INTERVAL,
                    })
                } else {
                    periodic_poll_interval
                };
                let first_command = match wait_interval {
                    Some(interval) => match command_rx.recv_timeout(interval) {
                        Ok(command) => Some(command),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => {
                            running = false;
                            None
                        }
                    },
                    None => match command_rx.recv() {
                        Ok(command) => Some(command),
                        Err(_) => {
                            running = false;
                            None
                        }
                    },
                };
                let commands = first_command
                    .into_iter()
                    .chain(std::iter::from_fn(|| command_rx.try_recv().ok()));
                for command in commands {
                    match command {
                        TransportCommand::WriteInput(bytes) => {
                            pending_input.extend_from_slice(bytes.as_ref());
                        }
                        TransportCommand::Resize(size) => {
                            flush_pending_transport_input(
                                &mut transport,
                                &mut pending_input,
                                &event_tx,
                                app_waker.as_ref(),
                            );
                            if let Err(error) = transport.resize(size) {
                                send_transport_event(
                                    &event_tx,
                                    app_waker.as_ref(),
                                    TransportEvent::Error(error.to_string()),
                                );
                            }
                        }
                        TransportCommand::PollOutput => {
                            flush_pending_transport_input(
                                &mut transport,
                                &mut pending_input,
                                &event_tx,
                                app_waker.as_ref(),
                            );
                        }
                        TransportCommand::Shutdown => {
                            flush_pending_transport_input(
                                &mut transport,
                                &mut pending_input,
                                &event_tx,
                                app_waker.as_ref(),
                            );
                            if let Err(error) = transport.shutdown() {
                                send_transport_event(
                                    &event_tx,
                                    app_waker.as_ref(),
                                    TransportEvent::Error(error.to_string()),
                                );
                            }
                            running = false;
                        }
                    }
                }
                flush_pending_transport_input(
                    &mut transport,
                    &mut pending_input,
                    &event_tx,
                    app_waker.as_ref(),
                );
                if !running {
                    break;
                }

                // Leave unread output in the transport (and, below it, in the
                // reader's bounded queue and the OS pipe) until the
                // application catches up. Draining here regardless would move
                // an unbounded backlog into this process's memory.
                if worker_queued_output_bytes.load(Ordering::Acquire) >= MAX_QUEUED_OUTPUT_BYTES {
                    output_backpressure = true;
                    poll_again = false;
                    continue;
                }
                output_backpressure = false;

                // Rearm before draining so readiness that races with this poll
                // schedules another pass instead of becoming stranded.
                backend_waker.clear_pending();
                poll_again = false;
                match transport.poll_output() {
                    Ok(output) => {
                        let had_bytes = !output.bytes.is_empty();
                        if had_bytes {
                            let queued = output.bytes.len();
                            if send_transport_event(
                                &event_tx,
                                app_waker.as_ref(),
                                TransportEvent::Output(output.bytes),
                            ) {
                                worker_queued_output_bytes.fetch_add(queued, Ordering::AcqRel);
                            }
                        }

                        for event in output.lifecycle {
                            send_transport_event(
                                &event_tx,
                                app_waker.as_ref(),
                                TransportEvent::Lifecycle(event),
                            );
                        }

                        if output.closed {
                            running = false;
                        } else if had_bytes {
                            // Some transports return one owned chunk at a
                            // time. Poll again without sleeping so coalesced
                            // notifications cannot strand later chunks or
                            // closure messages.
                            poll_again = true;
                        }
                    }
                    Err(error) => {
                        send_transport_event(
                            &event_tx,
                            app_waker.as_ref(),
                            TransportEvent::Error(error.to_string()),
                        );
                        running = false;
                    }
                }
            }
        });

        Self {
            command_tx,
            event_rx,
            handle: Some(handle),
            queued_output_bytes,
            termination_handle,
        }
    }

    pub fn send_command(&self, command: TransportCommand) -> Result<(), TransportLoopError> {
        self.command_tx
            .send(command)
            .map_err(|_| TransportLoopError::Closed)
    }

    #[must_use]
    pub fn poll_event(&self) -> Option<TransportEvent> {
        let event = self.event_rx.try_recv().ok()?;
        if let TransportEvent::Output(bytes) = &event {
            let consumed = bytes.len();
            let _ = self.queued_output_bytes.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |queued| Some(queued.saturating_sub(consumed)),
            );
        }
        Some(event)
    }

    /// Output bytes delivered to the application but not yet consumed. Reaching
    /// [`MAX_QUEUED_OUTPUT_BYTES`] pauses output delivery until the application
    /// drains what it already holds.
    #[must_use]
    pub fn queued_output_bytes(&self) -> usize {
        self.queued_output_bytes.load(Ordering::Acquire)
    }

    pub fn shutdown(mut self) -> Result<(), TransportLoopError> {
        if let Some(termination_handle) = self.termination_handle.take() {
            termination_handle.terminate();
        }
        let _ = self.command_tx.send(TransportCommand::Shutdown);
        let _ = self.handle.take();
        Ok(())
    }
}

fn flush_pending_transport_input<T>(
    transport: &mut T,
    pending_input: &mut Vec<u8>,
    event_tx: &Sender<TransportEvent>,
    app_waker: Option<&TransportWakeHandle>,
) where
    T: TerminalTransport + ?Sized,
{
    if pending_input.is_empty() {
        return;
    }
    match transport.write_input(pending_input) {
        Ok(written) if written <= pending_input.len() => {
            pending_input.drain(..written);
        }
        Ok(written) => {
            send_transport_event(
                event_tx,
                app_waker,
                TransportEvent::Error(format!(
                    "transport reported writing {written} bytes from a {} byte input buffer",
                    pending_input.len()
                )),
            );
            pending_input.clear();
        }
        Err(error) => {
            send_transport_event(
                event_tx,
                app_waker,
                TransportEvent::Error(error.to_string()),
            );
            // `Ok(0)` is the retryable backpressure signal. An error is
            // terminal for this buffered write; retaining it would emit the
            // same error and wake the UI every retry tick forever.
            pending_input.clear();
        }
    }
}

/// Returns whether the event was accepted by the application queue.
fn send_transport_event(
    event_tx: &Sender<TransportEvent>,
    app_waker: Option<&TransportWakeHandle>,
    event: TransportEvent,
) -> bool {
    if event_tx.send(event).is_err() {
        return false;
    }
    if let Some(app_waker) = app_waker {
        app_waker.wake();
    }
    true
}

impl Drop for TransportEventLoop {
    fn drop(&mut self) {
        if let Some(termination_handle) = self.termination_handle.take() {
            termination_handle.terminate();
        }
        let _ = self.command_tx.send(TransportCommand::Shutdown);
        let _ = self.handle.take();
    }
}

/// Responsible only for bytes in, bytes out, sizing, and lifecycle.
pub trait TerminalTransport: Send {
    /// Registers a readiness callback. Backends with reader threads should
    /// invoke it after queueing output or lifecycle changes.
    fn set_output_waker(&mut self, _waker: Option<TransportWakeHandle>) {}

    /// Returns a hook that can terminate a child or connection independently
    /// of this transport, including while the worker is blocked in I/O.
    fn termination_handle(&self) -> Option<TransportTerminationHandle> {
        None
    }

    /// Periodic output polling for backends without readiness callbacks.
    /// Wake-driven transports return `None` and sleep until explicitly woken.
    fn periodic_poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_millis(8))
    }

    /// Attempts to accept input without blocking output delivery and returns
    /// the number of bytes accepted. Returning zero requests a later retry.
    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize>;

    fn resize(&mut self, size: TerminalSize) -> TransportResult<()>;

    fn poll_output(&mut self) -> TransportResult<TransportOutput>;

    fn shutdown(&mut self) -> TransportResult<()>;

    fn session_metadata(&self) -> SessionMetadata;

    fn state(&self) -> TransportState;
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    use super::{
        SessionMetadata, TerminalSize, TerminalTransport, TransportCommand, TransportError,
        TransportEvent, TransportEventLoop, TransportInput, TransportKind, TransportOutput,
        TransportResult, TransportState, TransportTerminationHandle, TransportWakeHandle,
    };

    struct PartialWriteTransport {
        accepted: Arc<Mutex<Vec<u8>>>,
    }

    impl TerminalTransport for PartialWriteTransport {
        fn periodic_poll_interval(&self) -> Option<Duration> {
            Some(Duration::from_millis(1))
        }

        fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
            let accepted = bytes.len().min(2);
            self.accepted
                .lock()
                .expect("accepted input lock")
                .extend_from_slice(&bytes[..accepted]);
            Ok(accepted)
        }

        fn resize(&mut self, _size: TerminalSize) -> TransportResult<()> {
            Ok(())
        }

        fn poll_output(&mut self) -> TransportResult<TransportOutput> {
            Ok(TransportOutput::bytes(Vec::new()))
        }

        fn shutdown(&mut self) -> TransportResult<()> {
            Ok(())
        }

        fn session_metadata(&self) -> SessionMetadata {
            test_metadata()
        }

        fn state(&self) -> TransportState {
            TransportState::Running
        }
    }

    struct StalledWriteTransport {
        started: Option<mpsc::Sender<()>>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    impl TerminalTransport for StalledWriteTransport {
        fn periodic_poll_interval(&self) -> Option<Duration> {
            None
        }

        fn termination_handle(&self) -> Option<TransportTerminationHandle> {
            let release = Arc::clone(&self.release);
            Some(TransportTerminationHandle::new(move || {
                let (released, ready) = &*release;
                *released.lock().expect("release lock") = true;
                ready.notify_all();
            }))
        }

        fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            let (released, ready) = &*self.release;
            let guard = released.lock().expect("release lock");
            let _ = ready
                .wait_timeout_while(guard, Duration::from_millis(300), |released| !*released)
                .expect("release wait");
            Ok(bytes.len())
        }

        fn resize(&mut self, _size: TerminalSize) -> TransportResult<()> {
            Ok(())
        }

        fn poll_output(&mut self) -> TransportResult<TransportOutput> {
            Ok(TransportOutput::bytes(Vec::new()))
        }

        fn shutdown(&mut self) -> TransportResult<()> {
            Ok(())
        }

        fn session_metadata(&self) -> SessionMetadata {
            test_metadata()
        }

        fn state(&self) -> TransportState {
            TransportState::Running
        }
    }

    struct FloodTransport {
        polls: Arc<AtomicUsize>,
        chunk: usize,
    }

    struct FailingWriteTransport {
        writes: Arc<AtomicUsize>,
    }

    impl TerminalTransport for FailingWriteTransport {
        fn periodic_poll_interval(&self) -> Option<Duration> {
            Some(Duration::from_millis(1))
        }

        fn write_input(&mut self, _bytes: &[u8]) -> TransportResult<usize> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Err(TransportError::new("permanent input failure"))
        }

        fn resize(&mut self, _size: TerminalSize) -> TransportResult<()> {
            Ok(())
        }

        fn poll_output(&mut self) -> TransportResult<TransportOutput> {
            Ok(TransportOutput::bytes(Vec::new()))
        }

        fn shutdown(&mut self) -> TransportResult<()> {
            Ok(())
        }

        fn session_metadata(&self) -> SessionMetadata {
            test_metadata()
        }

        fn state(&self) -> TransportState {
            TransportState::Running
        }
    }

    impl TerminalTransport for FloodTransport {
        fn periodic_poll_interval(&self) -> Option<Duration> {
            Some(Duration::from_millis(1))
        }

        fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
            Ok(bytes.len())
        }

        fn resize(&mut self, _size: TerminalSize) -> TransportResult<()> {
            Ok(())
        }

        fn poll_output(&mut self) -> TransportResult<TransportOutput> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            Ok(TransportOutput::bytes(vec![b'x'; self.chunk]))
        }

        fn shutdown(&mut self) -> TransportResult<()> {
            Ok(())
        }

        fn session_metadata(&self) -> SessionMetadata {
            test_metadata()
        }

        fn state(&self) -> TransportState {
            TransportState::Running
        }
    }

    #[test]
    fn unconsumed_output_pauses_transport_polling_until_the_application_catches_up() {
        let polls = Arc::new(AtomicUsize::new(0));
        let events = TransportEventLoop::spawn(FloodTransport {
            polls: Arc::clone(&polls),
            chunk: 256 * 1024,
        });

        // A producer that is never read must stop being drained rather than
        // moving an unbounded backlog into this process.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while events.queued_output_bytes() < super::MAX_QUEUED_OUTPUT_BYTES {
            assert!(
                std::time::Instant::now() < deadline,
                "queued output never reached the backpressure threshold"
            );
            thread::sleep(Duration::from_millis(5));
        }

        let paused_at = polls.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(60));
        assert_eq!(
            polls.load(Ordering::SeqCst),
            paused_at,
            "polling must stay paused while the application has not consumed its output"
        );

        let mut drained = 0usize;
        while let Some(event) = events.poll_event() {
            if let TransportEvent::Output(bytes) = event {
                drained += bytes.len();
            }
        }
        assert!(drained > 0);
        assert_eq!(events.queued_output_bytes(), 0);

        // Draining releases the pause, so output resumes flowing.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while polls.load(Ordering::SeqCst) <= paused_at {
            assert!(
                std::time::Instant::now() < deadline,
                "polling never resumed after the backlog was consumed"
            );
            thread::sleep(Duration::from_millis(5));
        }

        let _ = events.shutdown();
    }

    #[test]
    fn ordinary_terminal_input_stays_inline() {
        let input = TransportInput::copy_from_slice(b"\x1b[1;5D");

        assert_eq!(input.as_ref(), b"\x1b[1;5D");
        assert!(!input.spilled());
    }

    #[test]
    fn permanent_input_failure_is_reported_once_without_a_retry_storm() {
        let writes = Arc::new(AtomicUsize::new(0));
        let event_loop = TransportEventLoop::spawn(FailingWriteTransport {
            writes: Arc::clone(&writes),
        });
        event_loop
            .send_command(TransportCommand::write_input(b"lost".as_slice()))
            .expect("queue failing input");

        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        loop {
            if matches!(event_loop.poll_event(), Some(TransportEvent::Error(_))) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "input error was not reported"
            );
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(30));
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        event_loop.shutdown().expect("shutdown transport worker");
    }

    #[test]
    fn transport_worker_retries_only_the_unwritten_input_tail() {
        let accepted = Arc::new(Mutex::new(Vec::new()));
        let event_loop = TransportEventLoop::spawn(PartialWriteTransport {
            accepted: Arc::clone(&accepted),
        });

        event_loop
            .send_command(TransportCommand::write_input(b"abcdef".as_slice()))
            .expect("queue input");

        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        while accepted.lock().expect("accepted input lock").len() < 6
            && std::time::Instant::now() < deadline
        {
            thread::yield_now();
        }

        assert_eq!(
            accepted.lock().expect("accepted input lock").as_slice(),
            b"abcdef"
        );
        event_loop.shutdown().expect("shutdown transport worker");
    }

    #[test]
    fn shutdown_terminates_and_detaches_a_stalled_transport_worker() {
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let event_loop = TransportEventLoop::spawn(StalledWriteTransport {
            started: Some(started_tx),
            release: Arc::clone(&release),
        });
        event_loop
            .send_command(TransportCommand::write_input(b"blocked".as_slice()))
            .expect("queue stalled input");
        started_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("backend write starts");

        let started = std::time::Instant::now();
        event_loop.shutdown().expect("shutdown transport worker");

        assert!(
            started.elapsed() < Duration::from_millis(50),
            "shutdown must not join a stalled worker: {:?}",
            started.elapsed()
        );
        assert!(*release.0.lock().expect("release lock"));
    }

    struct WakeDrivenTransport {
        output: VecDeque<Vec<u8>>,
        waker: Option<TransportWakeHandle>,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    struct IdleWakeTransport {
        polls: Arc<AtomicUsize>,
    }

    struct ExternalWakeTransport {
        output: Arc<Mutex<VecDeque<Vec<u8>>>>,
        waker: Arc<Mutex<Option<TransportWakeHandle>>>,
    }

    impl TerminalTransport for ExternalWakeTransport {
        fn set_output_waker(&mut self, waker: Option<TransportWakeHandle>) {
            *self.waker.lock().expect("backend waker lock") = waker;
        }

        fn periodic_poll_interval(&self) -> Option<Duration> {
            None
        }

        fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
            Ok(bytes.len())
        }

        fn resize(&mut self, _size: TerminalSize) -> TransportResult<()> {
            Ok(())
        }

        fn poll_output(&mut self) -> TransportResult<TransportOutput> {
            Ok(TransportOutput::bytes(
                self.output
                    .lock()
                    .expect("external output lock")
                    .pop_front()
                    .unwrap_or_default(),
            ))
        }

        fn shutdown(&mut self) -> TransportResult<()> {
            Ok(())
        }

        fn session_metadata(&self) -> SessionMetadata {
            test_metadata()
        }

        fn state(&self) -> TransportState {
            TransportState::Running
        }
    }

    impl TerminalTransport for IdleWakeTransport {
        fn periodic_poll_interval(&self) -> Option<Duration> {
            None
        }

        fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
            Ok(bytes.len())
        }

        fn resize(&mut self, _size: TerminalSize) -> TransportResult<()> {
            Ok(())
        }

        fn poll_output(&mut self) -> TransportResult<TransportOutput> {
            self.polls.fetch_add(1, Ordering::Relaxed);
            Ok(TransportOutput::bytes(Vec::new()))
        }

        fn shutdown(&mut self) -> TransportResult<()> {
            Ok(())
        }

        fn session_metadata(&self) -> SessionMetadata {
            test_metadata()
        }

        fn state(&self) -> TransportState {
            TransportState::Running
        }
    }

    struct SlowFirstWriteTransport {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        first_write_started: mpsc::Sender<()>,
        release_first_write: mpsc::Receiver<()>,
    }

    impl TerminalTransport for SlowFirstWriteTransport {
        fn periodic_poll_interval(&self) -> Option<Duration> {
            None
        }

        fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
            let first = {
                let mut writes = self.writes.lock().expect("writes lock");
                let first = writes.is_empty();
                writes.push(bytes.to_vec());
                first
            };
            if first {
                let _ = self.first_write_started.send(());
                let _ = self
                    .release_first_write
                    .recv_timeout(Duration::from_millis(200));
            }
            Ok(bytes.len())
        }

        fn resize(&mut self, _size: TerminalSize) -> TransportResult<()> {
            Ok(())
        }

        fn poll_output(&mut self) -> TransportResult<TransportOutput> {
            Ok(TransportOutput::bytes(Vec::new()))
        }

        fn shutdown(&mut self) -> TransportResult<()> {
            Ok(())
        }

        fn session_metadata(&self) -> SessionMetadata {
            test_metadata()
        }

        fn state(&self) -> TransportState {
            TransportState::Running
        }
    }

    fn test_metadata() -> SessionMetadata {
        SessionMetadata {
            id: "test".to_owned(),
            kind: TransportKind::LocalPty,
            title: None,
            shell: None,
            current_working_directory: None,
            remote_host: None,
        }
    }

    impl TerminalTransport for WakeDrivenTransport {
        fn set_output_waker(&mut self, waker: Option<TransportWakeHandle>) {
            self.waker = waker;
        }

        fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
            self.writes
                .lock()
                .expect("writes lock")
                .push(bytes.to_vec());
            self.output.push_back(bytes.to_vec());
            if let Some(waker) = &self.waker {
                waker.wake();
            }
            Ok(bytes.len())
        }

        fn resize(&mut self, _size: TerminalSize) -> TransportResult<()> {
            Ok(())
        }

        fn poll_output(&mut self) -> TransportResult<TransportOutput> {
            Ok(TransportOutput::bytes(
                self.output.pop_front().unwrap_or_default(),
            ))
        }

        fn shutdown(&mut self) -> TransportResult<()> {
            Ok(())
        }

        fn session_metadata(&self) -> SessionMetadata {
            SessionMetadata {
                id: "test".to_owned(),
                kind: TransportKind::LocalPty,
                title: None,
                shell: None,
                current_working_directory: None,
                remote_host: None,
            }
        }

        fn state(&self) -> TransportState {
            TransportState::Running
        }
    }

    #[test]
    fn transport_wake_handle_coalesces_clones_until_cleared() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let wake = TransportWakeHandle::new(move || {
            observed.fetch_add(1, Ordering::Relaxed);
        });

        wake.wake();
        wake.clone().wake();

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        wake.clear_pending();
        wake.clone().wake();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn transport_core_has_no_crate_dependencies() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("[dependencies]"),
            "transport-core must not depend on platform, windows, rendering, parser, or config"
        );
    }

    #[test]
    fn transport_worker_wakes_the_app_only_after_output_is_pollable() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let transport = WakeDrivenTransport {
            output: VecDeque::new(),
            waker: None,
            writes: Arc::clone(&writes),
        };
        let (wake_tx, wake_rx) = mpsc::channel();
        let app_waker = TransportWakeHandle::new(move || {
            let _ = wake_tx.send(());
        });
        let app_waker_rearm = app_waker.clone();
        let event_loop = TransportEventLoop::spawn_with_waker(transport, app_waker);

        event_loop
            .send_command(TransportCommand::write_input(b"panea".as_slice()))
            .expect("queue input without blocking the caller");
        wake_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("worker should wake the app immediately");

        assert_eq!(
            event_loop.poll_event(),
            Some(TransportEvent::Output(b"panea".to_vec()))
        );
        assert_eq!(
            writes.lock().expect("writes lock").as_slice(),
            &[b"panea".to_vec()]
        );

        app_waker_rearm.clear_pending();
        event_loop
            .send_command(TransportCommand::write_input(b"again".as_slice()))
            .expect("queue second input");
        wake_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("backend wake should rearm after output is polled");
        assert_eq!(
            event_loop.poll_event(),
            Some(TransportEvent::Output(b"again".to_vec()))
        );
    }

    #[test]
    fn backend_readiness_rearms_after_each_transport_poll() {
        let output = Arc::new(Mutex::new(VecDeque::new()));
        let backend_waker = Arc::new(Mutex::new(None));
        let (app_wake_tx, app_wake_rx) = mpsc::channel();
        let app_waker = TransportWakeHandle::new(move || {
            let _ = app_wake_tx.send(());
        });
        let app_waker_rearm = app_waker.clone();
        let event_loop = TransportEventLoop::spawn_with_waker(
            ExternalWakeTransport {
                output: Arc::clone(&output),
                waker: Arc::clone(&backend_waker),
            },
            app_waker,
        );
        let wake_backend = || {
            backend_waker
                .lock()
                .expect("backend waker lock")
                .as_ref()
                .expect("backend waker installed")
                .wake();
        };

        output
            .lock()
            .expect("external output lock")
            .push_back(b"first".to_vec());
        wake_backend();
        app_wake_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("first app wake");
        assert_eq!(
            event_loop.poll_event(),
            Some(TransportEvent::Output(b"first".to_vec()))
        );

        app_waker_rearm.clear_pending();
        output
            .lock()
            .expect("external output lock")
            .push_back(b"second".to_vec());
        wake_backend();
        app_wake_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("second app wake");
        assert_eq!(
            event_loop.poll_event(),
            Some(TransportEvent::Output(b"second".to_vec()))
        );
    }

    #[test]
    fn one_coalesced_wake_drains_all_prequeued_backend_output() {
        let output = Arc::new(Mutex::new(VecDeque::from([
            b"first".to_vec(),
            b"second".to_vec(),
        ])));
        let backend_waker = Arc::new(Mutex::new(None));
        let (app_wake_tx, app_wake_rx) = mpsc::channel();
        let event_loop = TransportEventLoop::spawn_with_waker(
            ExternalWakeTransport {
                output: Arc::clone(&output),
                waker: Arc::clone(&backend_waker),
            },
            TransportWakeHandle::new(move || {
                let _ = app_wake_tx.send(());
            }),
        );
        backend_waker
            .lock()
            .expect("backend waker lock")
            .as_ref()
            .expect("backend waker installed")
            .wake();
        app_wake_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("coalesced app wake");

        let deadline = std::time::Instant::now() + Duration::from_millis(100);
        let mut received = Vec::new();
        while received.len() < 2 && std::time::Instant::now() < deadline {
            if let Some(TransportEvent::Output(bytes)) = event_loop.poll_event() {
                received.push(bytes);
            } else {
                thread::yield_now();
            }
        }
        assert_eq!(received, [b"first".to_vec(), b"second".to_vec()]);
    }

    #[test]
    fn wake_driven_transport_does_not_poll_while_idle() {
        let polls = Arc::new(AtomicUsize::new(0));
        let event_loop = TransportEventLoop::spawn(IdleWakeTransport {
            polls: Arc::clone(&polls),
        });

        std::thread::sleep(Duration::from_millis(40));

        assert_eq!(polls.load(Ordering::Relaxed), 0);
        event_loop.shutdown().expect("shutdown idle worker");
    }

    #[test]
    fn transport_worker_coalesces_a_ready_input_burst() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let event_loop = TransportEventLoop::spawn(SlowFirstWriteTransport {
            writes: Arc::clone(&writes),
            first_write_started: started_tx,
            release_first_write: release_rx,
        });

        event_loop
            .send_command(TransportCommand::write_input(vec![0]))
            .expect("queue first input");
        started_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("first write starts");
        for value in 1..=50_u8 {
            event_loop
                .send_command(TransportCommand::write_input(vec![value]))
                .expect("queue burst input");
        }
        release_tx.send(()).expect("release first write");

        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        while writes
            .lock()
            .expect("writes lock")
            .iter()
            .map(Vec::len)
            .sum::<usize>()
            < 51
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let writes = writes.lock().expect("writes lock");
        assert_eq!(
            writes.iter().flatten().copied().collect::<Vec<_>>(),
            (0..=50).collect::<Vec<_>>()
        );
        assert!(
            writes.len() <= 2,
            "ready key events should become at most one additional backend write; got {}",
            writes.len()
        );
        drop(writes);
        event_loop.shutdown().expect("shutdown burst worker");
    }

    #[test]
    fn transport_command_queue_does_not_drop_input_while_backend_is_stalled() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let event_loop = TransportEventLoop::spawn(SlowFirstWriteTransport {
            writes: Arc::clone(&writes),
            first_write_started: started_tx,
            release_first_write: release_rx,
        });

        event_loop
            .send_command(TransportCommand::write_input(vec![0]))
            .expect("queue first input");
        started_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("backend write starts");

        let results = (1..=50_u8)
            .map(|value| event_loop.send_command(TransportCommand::write_input(vec![value])))
            .collect::<Vec<_>>();
        release_tx.send(()).expect("release backend write");

        assert!(
            results.iter().all(Result::is_ok),
            "the window thread must not lose keystrokes because the backend is temporarily busy: {results:?}"
        );

        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        while writes
            .lock()
            .expect("writes lock")
            .iter()
            .map(Vec::len)
            .sum::<usize>()
            < 51
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(
            writes
                .lock()
                .expect("writes lock")
                .iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>(),
            (0..=50).collect::<Vec<_>>()
        );
        event_loop.shutdown().expect("shutdown stalled worker");
    }
}
