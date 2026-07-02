//! Semantic terminal metadata kept separate from raw terminal output.

pub const LAYER: &str = "semantic meaning";

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BufferPosition {
    pub row: i64,
    pub col: u16,
}

impl BufferPosition {
    #[must_use]
    pub const fn new(row: i64, col: u16) -> Self {
        Self { row, col }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShellMetadata {
    pub shell: Option<String>,
    pub current_working_directory: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteMetadata {
    pub transport: Option<String>,
    pub remote_host: Option<String>,
    pub remote_user: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SemanticMetadata {
    pub shell: ShellMetadata,
    pub remote: Option<RemoteMetadata>,
    pub command: Option<String>,
    pub attributes: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandStatus {
    Running,
    Code(i32),
    Signal(String),
    Unknown,
}

pub type CommandExitStatus = CommandStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticRegionKind {
    Prompt,
    Input,
    Output,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticRegion {
    pub id: u64,
    pub kind: SemanticRegionKind,
    pub start: BufferPosition,
    pub end: Option<BufferPosition>,
    pub metadata: SemanticMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBlock {
    pub region_id: u64,
    pub command: String,
    pub status: CommandStatus,
    pub started_at: BufferPosition,
    pub ended_at: Option<BufferPosition>,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticEvent {
    PromptStarted {
        position: BufferPosition,
        metadata: SemanticMetadata,
    },
    PromptEnded {
        position: BufferPosition,
    },
    InputStarted {
        position: BufferPosition,
    },
    InputEnded {
        position: BufferPosition,
    },
    OutputStarted {
        position: BufferPosition,
    },
    OutputEnded {
        position: BufferPosition,
    },
    CommandFinished {
        exit_status: CommandStatus,
        duration: Duration,
    },
}

/// Records meaning over terminal positions without mutating terminal content.
pub trait SemanticTimeline {
    fn prompt_started(&mut self, position: BufferPosition, metadata: SemanticMetadata);

    fn prompt_ended(&mut self, position: BufferPosition);

    fn input_started(&mut self, position: BufferPosition);

    fn input_ended(&mut self, position: BufferPosition);

    fn output_started(&mut self, position: BufferPosition);

    fn output_ended(&mut self, position: BufferPosition);

    fn command_finished(&mut self, exit_status: CommandStatus, duration: Duration);
}

#[cfg(test)]
mod tests {
    #[test]
    fn semantics_has_no_crate_dependencies() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("[dependencies]"),
            "semantics must reference terminal positions without importing terminal, parser, or renderer crates"
        );
    }
}
