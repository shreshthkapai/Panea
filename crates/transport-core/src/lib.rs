//! Transport-agnostic session I/O contracts.

pub const LAYER: &str = "session transport";

use std::{error::Error, fmt};

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
    Exiting,
    Exited { exit_code: Option<i32> },
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

/// Responsible only for bytes in, bytes out, sizing, and lifecycle.
pub trait TerminalTransport {
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
