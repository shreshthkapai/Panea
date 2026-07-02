//! Transport-agnostic session I/O contracts.

pub const LAYER: &str = "session transport";

use std::{
    error::Error,
    fmt,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
    time::Duration,
};

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
pub enum TransportCommand {
    WriteInput(Vec<u8>),
    Resize(TerminalSize),
    Shutdown,
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
    command_tx: SyncSender<TransportCommand>,
    event_rx: Receiver<TransportEvent>,
    handle: Option<JoinHandle<()>>,
}

impl TransportEventLoop {
    const DEFAULT_QUEUE_BOUND: usize = 1024;
    const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(2);

    #[must_use]
    pub fn spawn<T>(transport: T) -> Self
    where
        T: TerminalTransport + 'static,
    {
        Self::spawn_with_queue_bound(transport, Self::DEFAULT_QUEUE_BOUND)
    }

    #[must_use]
    pub fn spawn_with_queue_bound<T>(mut transport: T, queue_bound: usize) -> Self
    where
        T: TerminalTransport + 'static,
    {
        let queue_bound = queue_bound.max(1);
        let (command_tx, command_rx) = mpsc::sync_channel(queue_bound);
        let (event_tx, event_rx) = mpsc::sync_channel(queue_bound);

        let handle = thread::spawn(move || {
            let mut running = true;

            while running {
                let mut did_work = false;

                loop {
                    match command_rx.try_recv() {
                        Ok(command) => {
                            did_work = true;
                            match command {
                                TransportCommand::WriteInput(bytes) => {
                                    if let Err(error) = transport.write_input(&bytes) {
                                        let _ = event_tx
                                            .try_send(TransportEvent::Error(error.to_string()));
                                    }
                                }
                                TransportCommand::Resize(size) => {
                                    if let Err(error) = transport.resize(size) {
                                        let _ = event_tx
                                            .try_send(TransportEvent::Error(error.to_string()));
                                    }
                                }
                                TransportCommand::Shutdown => {
                                    if let Err(error) = transport.shutdown() {
                                        let _ = event_tx
                                            .try_send(TransportEvent::Error(error.to_string()));
                                    }
                                    running = false;
                                }
                            }
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            running = false;
                            break;
                        }
                    }
                }

                match transport.poll_output() {
                    Ok(output) => {
                        if !output.bytes.is_empty() {
                            did_work = true;
                            let _ = event_tx.try_send(TransportEvent::Output(output.bytes));
                        }

                        for event in output.lifecycle {
                            did_work = true;
                            let _ = event_tx.try_send(TransportEvent::Lifecycle(event));
                        }

                        if output.closed {
                            running = false;
                        }
                    }
                    Err(error) => {
                        let _ = event_tx.try_send(TransportEvent::Error(error.to_string()));
                        running = false;
                    }
                }

                if !did_work && running {
                    thread::sleep(Self::IDLE_POLL_INTERVAL);
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
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => TransportLoopError::Backpressure,
                TrySendError::Disconnected(_) => TransportLoopError::Closed,
            })
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

impl Drop for TransportEventLoop {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(TransportCommand::Shutdown);
        let _ = self.handle.take();
    }
}

/// Responsible only for bytes in, bytes out, sizing, and lifecycle.
pub trait TerminalTransport: Send {
    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<()>;

    fn resize(&mut self, size: TerminalSize) -> TransportResult<()>;

    fn poll_output(&mut self) -> TransportResult<TransportOutput>;

    fn shutdown(&mut self) -> TransportResult<()>;

    fn session_metadata(&self) -> SessionMetadata;

    fn state(&self) -> TransportState;
}

#[cfg(test)]
mod tests {
    #[test]
    fn transport_core_has_no_crate_dependencies() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("[dependencies]"),
            "transport-core must not depend on platform, windows, rendering, parser, or config"
        );
    }
}
