//! Transport-agnostic session I/O contracts.

pub const LAYER: &str = "session transport";

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Platform-neutral callback used to wake an application event loop when a
/// transport has output or lifecycle state ready to consume.
#[derive(Clone)]
pub struct TransportWakeHandle(Arc<dyn Fn() + Send + Sync + 'static>);

impl TransportWakeHandle {
    #[must_use]
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(wake))
    }

    pub fn wake(&self) {
        (self.0)();
    }
}

impl fmt::Debug for TransportWakeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TransportWakeHandle(..)")
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
        transport.set_output_waker(Some(TransportWakeHandle::new(move || {
            let _ = output_wake_tx.send(TransportCommand::PollOutput);
        })));
        let periodic_poll_interval = transport.periodic_poll_interval();

        let handle = thread::spawn(move || {
            let mut running = true;
            let mut pending_input = Vec::with_capacity(256);

            while running {
                let first_command = match periodic_poll_interval {
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

                match transport.poll_output() {
                    Ok(output) => {
                        if !output.bytes.is_empty() {
                            send_transport_event(
                                &event_tx,
                                app_waker.as_ref(),
                                TransportEvent::Output(output.bytes),
                            );
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
        }
    }

    pub fn send_command(&self, command: TransportCommand) -> Result<(), TransportLoopError> {
        self.command_tx
            .send(command)
            .map_err(|_| TransportLoopError::Closed)
    }

    #[must_use]
    pub fn poll_event(&self) -> Option<TransportEvent> {
        self.event_rx.try_recv().ok()
    }

    pub fn shutdown(mut self) -> Result<(), TransportLoopError> {
        self.send_command(TransportCommand::Shutdown)?;

        if let Some(handle) = self.handle.take() {
            handle.join().map_err(|_| TransportLoopError::Closed)?;
        }

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
    if let Err(error) = transport.write_input(pending_input) {
        send_transport_event(
            event_tx,
            app_waker,
            TransportEvent::Error(error.to_string()),
        );
    }
    pending_input.clear();
}

fn send_transport_event(
    event_tx: &Sender<TransportEvent>,
    app_waker: Option<&TransportWakeHandle>,
    event: TransportEvent,
) {
    if event_tx.send(event).is_ok()
        && let Some(app_waker) = app_waker
    {
        app_waker.wake();
    }
}

impl Drop for TransportEventLoop {
    fn drop(&mut self) {
        let _ = self.command_tx.send(TransportCommand::Shutdown);
        let _ = self.handle.take();
    }
}

/// Responsible only for bytes in, bytes out, sizing, and lifecycle.
pub trait TerminalTransport: Send {
    /// Registers a readiness callback. Backends with reader threads should
    /// invoke it after queueing output or lifecycle changes.
    fn set_output_waker(&mut self, _waker: Option<TransportWakeHandle>) {}

    /// Periodic output polling for backends without readiness callbacks.
    /// Wake-driven transports return `None` and sleep until explicitly woken.
    fn periodic_poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_millis(8))
    }

    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<()>;

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
        time::Duration,
    };

    use super::{
        SessionMetadata, TerminalSize, TerminalTransport, TransportCommand, TransportEvent,
        TransportEventLoop, TransportInput, TransportKind, TransportOutput, TransportResult,
        TransportState, TransportWakeHandle,
    };

    #[test]
    fn ordinary_terminal_input_stays_inline() {
        let input = TransportInput::copy_from_slice(b"\x1b[1;5D");

        assert_eq!(input.as_ref(), b"\x1b[1;5D");
        assert!(!input.spilled());
    }

    struct WakeDrivenTransport {
        output: VecDeque<Vec<u8>>,
        waker: Option<TransportWakeHandle>,
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    struct IdleWakeTransport {
        polls: Arc<AtomicUsize>,
    }

    impl TerminalTransport for IdleWakeTransport {
        fn periodic_poll_interval(&self) -> Option<Duration> {
            None
        }

        fn write_input(&mut self, _bytes: &[u8]) -> TransportResult<()> {
            Ok(())
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

        fn write_input(&mut self, bytes: &[u8]) -> TransportResult<()> {
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
            Ok(())
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

        fn write_input(&mut self, bytes: &[u8]) -> TransportResult<()> {
            self.writes
                .lock()
                .expect("writes lock")
                .push(bytes.to_vec());
            self.output.push_back(bytes.to_vec());
            if let Some(waker) = &self.waker {
                waker.wake();
            }
            Ok(())
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
    fn transport_wake_handle_is_cloneable_and_backend_agnostic() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let wake = TransportWakeHandle::new(move || {
            observed.fetch_add(1, Ordering::Relaxed);
        });

        wake.wake();
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
