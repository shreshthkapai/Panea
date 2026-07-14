//! Semantic terminal metadata kept separate from raw terminal output.

pub const LAYER: &str = "semantic meaning";

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticSpan {
    pub start: BufferPosition,
    pub end: BufferPosition,
}

impl SemanticSpan {
    #[must_use]
    pub const fn new(start: BufferPosition, end: BufferPosition) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShellMetadata {
    pub shell: Option<String>,
    pub version: Option<String>,
    pub current_working_directory: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteMetadata {
    pub transport: Option<String>,
    pub remote_host: Option<String>,
    pub remote_user: Option<String>,
    pub remote_current_working_directory: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    Url,
    FilePath,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HintAction {
    OpenUrl(String),
    CopyText(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedHint {
    pub kind: HintKind,
    pub start: BufferPosition,
    pub end: BufferPosition,
    pub text: String,
    pub action: HintAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticRegion {
    pub id: u64,
    pub kind: SemanticRegionKind,
    pub start: BufferPosition,
    pub end: Option<BufferPosition>,
    pub metadata: SemanticMetadata,
}

impl SemanticRegion {
    #[must_use]
    pub const fn span(&self) -> Option<SemanticSpan> {
        match self.end {
            Some(end) => Some(SemanticSpan::new(self.start, end)),
            None => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandBlock {
    pub region_id: u64,
    pub input_region_id: Option<u64>,
    pub output_region_id: Option<u64>,
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
        position: BufferPosition,
        exit_status: CommandStatus,
        duration: Duration,
    },
    CurrentWorkingDirectoryChanged {
        position: BufferPosition,
        directory: String,
        remote: bool,
    },
    ShellMetadataChanged {
        position: BufferPosition,
        metadata: ShellMetadata,
    },
    RemoteMetadataChanged {
        position: BufferPosition,
        metadata: RemoteMetadata,
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

    fn command_finished(
        &mut self,
        position: BufferPosition,
        exit_status: CommandStatus,
        duration: Duration,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationMode {
    Disabled,
    EscapeSequences,
    Heuristic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticDiagnostics {
    pub mode: IntegrationMode,
    pub shell_detected: Option<String>,
    pub integration_active: bool,
    pub last_event: Option<SemanticEventKind>,
    pub last_event_age: Option<Duration>,
    pub command_block_confidence: CommandBlockConfidence,
    pub remote_integration_active: bool,
    pub heuristic_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticEventKind {
    PromptStarted,
    PromptEnded,
    InputStarted,
    InputEnded,
    OutputStarted,
    OutputEnded,
    CommandFinished,
    CurrentWorkingDirectoryChanged,
    ShellMetadataChanged,
    RemoteMetadataChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandBlockConfidence {
    None,
    Heuristic,
    ShellIntegrated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticAction {
    JumpToPreviousCommand,
    JumpToNextCommand,
    SelectCurrentCommandOutput,
    CopyCurrentCommandOutput,
    CopyCommandAndOutput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticActionResult {
    Position(BufferPosition),
    Selection(SemanticSpan),
    Text(String),
    Noop,
}

pub trait TerminalTextProvider {
    fn text_for_span(&self, span: SemanticSpan) -> Option<String>;
}

#[derive(Debug, Clone)]
pub struct SemanticTimelineStore {
    regions: Vec<SemanticRegion>,
    command_blocks: Vec<CommandBlock>,
    metadata: SemanticMetadata,
    next_region_id: u64,
    open_prompt: Option<u64>,
    open_input: Option<u64>,
    open_output: Option<u64>,
    open_command: Option<u64>,
    active_command_started: Option<Instant>,
    last_event: Option<(SemanticEventKind, Instant)>,
    mode: IntegrationMode,
    remote_integration_active: bool,
}

impl Default for SemanticTimelineStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticTimelineStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
            command_blocks: Vec::new(),
            metadata: SemanticMetadata::default(),
            next_region_id: 1,
            open_prompt: None,
            open_input: None,
            open_output: None,
            open_command: None,
            active_command_started: None,
            last_event: None,
            mode: IntegrationMode::EscapeSequences,
            remote_integration_active: false,
        }
    }

    #[must_use]
    pub fn regions(&self) -> &[SemanticRegion] {
        &self.regions
    }

    #[must_use]
    pub fn command_blocks(&self) -> &[CommandBlock] {
        &self.command_blocks
    }

    #[must_use]
    pub const fn metadata(&self) -> &SemanticMetadata {
        &self.metadata
    }

    pub fn set_integration_mode(&mut self, mode: IntegrationMode) {
        self.mode = mode;
    }

    #[must_use]
    pub const fn integration_mode(&self) -> IntegrationMode {
        self.mode
    }

    /// Records transport-provided remote context without claiming that remote
    /// shell integration has emitted a semantic marker.
    pub fn set_remote_session_metadata(&mut self, metadata: RemoteMetadata) {
        self.metadata.remote = Some(metadata);
    }

    /// Marks that a semantic marker was actually observed in a remote byte
    /// stream. Transport metadata alone must not call this method.
    pub fn mark_remote_integration_active(&mut self) {
        self.remote_integration_active = true;
    }

    pub fn apply_event(&mut self, event: SemanticEvent) {
        match event {
            SemanticEvent::PromptStarted { position, metadata } => {
                self.prompt_started(position, metadata);
            }
            SemanticEvent::PromptEnded { position } => self.prompt_ended(position),
            SemanticEvent::InputStarted { position } => self.input_started(position),
            SemanticEvent::InputEnded { position } => self.input_ended(position),
            SemanticEvent::OutputStarted { position } => self.output_started(position),
            SemanticEvent::OutputEnded { position } => self.output_ended(position),
            SemanticEvent::CommandFinished {
                position,
                exit_status,
                duration,
            } => self.command_finished(position, exit_status, duration),
            SemanticEvent::CurrentWorkingDirectoryChanged {
                directory, remote, ..
            } => {
                self.mark_event(SemanticEventKind::CurrentWorkingDirectoryChanged);
                if remote {
                    let remote = self
                        .metadata
                        .remote
                        .get_or_insert_with(RemoteMetadata::default);
                    remote.remote_current_working_directory = Some(directory);
                    self.remote_integration_active = true;
                } else {
                    self.metadata.shell.current_working_directory = Some(directory);
                }
            }
            SemanticEvent::ShellMetadataChanged { metadata, .. } => {
                self.mark_event(SemanticEventKind::ShellMetadataChanged);
                self.metadata.shell = metadata;
            }
            SemanticEvent::RemoteMetadataChanged { metadata, .. } => {
                self.mark_event(SemanticEventKind::RemoteMetadataChanged);
                self.metadata.remote = Some(metadata);
                self.remote_integration_active = true;
            }
        }
    }

    #[must_use]
    pub fn diagnostics(&self, now: Instant) -> SemanticDiagnostics {
        let (last_event, last_event_age) =
            self.last_event.map_or((None, None), |(kind, instant)| {
                (Some(kind), Some(now - instant))
            });

        SemanticDiagnostics {
            mode: self.mode,
            shell_detected: self.metadata.shell.shell.clone(),
            integration_active: last_event.is_some() && self.mode != IntegrationMode::Disabled,
            last_event,
            last_event_age,
            command_block_confidence: if self.command_blocks.is_empty() {
                CommandBlockConfidence::None
            } else if self.mode == IntegrationMode::Heuristic {
                CommandBlockConfidence::Heuristic
            } else {
                CommandBlockConfidence::ShellIntegrated
            },
            remote_integration_active: self.remote_integration_active,
            heuristic_mode: self.mode == IntegrationMode::Heuristic,
        }
    }

    #[must_use]
    pub fn current_command(&self, position: BufferPosition) -> Option<&CommandBlock> {
        self.command_blocks
            .iter()
            .rev()
            .find(|block| block_contains_position(block, position))
    }

    #[must_use]
    pub fn previous_command(&self, position: BufferPosition) -> Option<&CommandBlock> {
        self.command_blocks
            .iter()
            .rev()
            .find(|block| block.started_at < position)
    }

    #[must_use]
    pub fn next_command(&self, position: BufferPosition) -> Option<&CommandBlock> {
        self.command_blocks
            .iter()
            .find(|block| block.started_at > position)
    }

    #[must_use]
    pub fn output_span_for_command(&self, block: &CommandBlock) -> Option<SemanticSpan> {
        let region_id = block.output_region_id?;
        self.regions
            .iter()
            .find(|region| region.id == region_id)
            .and_then(SemanticRegion::span)
    }

    #[must_use]
    pub fn command_span(&self, block: &CommandBlock) -> Option<SemanticSpan> {
        let region = self
            .regions
            .iter()
            .find(|region| region.id == block.region_id)?;
        region.span()
    }

    #[must_use]
    pub fn command_metadata(&self, block: &CommandBlock) -> Option<&SemanticMetadata> {
        self.regions
            .iter()
            .find(|region| region.id == block.region_id)
            .map(|region| &region.metadata)
    }

    #[must_use]
    pub fn command_text(
        &self,
        block: &CommandBlock,
        provider: &impl TerminalTextProvider,
    ) -> Option<String> {
        if !block.command.is_empty() {
            return Some(block.command.clone());
        }
        let region_id = block.input_region_id?;
        let span = self
            .regions
            .iter()
            .find(|region| region.id == region_id)
            .and_then(SemanticRegion::span)?;
        provider.text_for_span(span)
    }

    #[must_use]
    pub fn output_text(
        &self,
        block: &CommandBlock,
        provider: &impl TerminalTextProvider,
    ) -> Option<String> {
        let span = self.output_span_for_command(block)?;
        provider.text_for_span(span)
    }

    #[must_use]
    pub fn run_action(
        &self,
        action: SemanticAction,
        cursor: BufferPosition,
        provider: &impl TerminalTextProvider,
    ) -> SemanticActionResult {
        match action {
            SemanticAction::JumpToPreviousCommand => self
                .previous_command(cursor)
                .map_or(SemanticActionResult::Noop, |block| {
                    SemanticActionResult::Position(block.started_at)
                }),
            SemanticAction::JumpToNextCommand => self
                .next_command(cursor)
                .map_or(SemanticActionResult::Noop, |block| {
                    SemanticActionResult::Position(block.started_at)
                }),
            SemanticAction::SelectCurrentCommandOutput => self
                .current_command(cursor)
                .and_then(|block| self.output_span_for_command(block))
                .map_or(SemanticActionResult::Noop, SemanticActionResult::Selection),
            SemanticAction::CopyCurrentCommandOutput => self
                .current_command(cursor)
                .and_then(|block| self.output_text(block, provider))
                .map_or(SemanticActionResult::Noop, SemanticActionResult::Text),
            SemanticAction::CopyCommandAndOutput => self
                .current_command(cursor)
                .and_then(|block| {
                    let command = self.command_text(block, provider).unwrap_or_default();
                    let output = self.output_text(block, provider).unwrap_or_default();
                    if command.is_empty() && output.is_empty() {
                        None
                    } else if output.is_empty() {
                        Some(command)
                    } else if command.is_empty() {
                        Some(output)
                    } else {
                        Some(format!("{command}\n{output}"))
                    }
                })
                .map_or(SemanticActionResult::Noop, SemanticActionResult::Text),
        }
    }

    fn open_region(
        &mut self,
        kind: SemanticRegionKind,
        position: BufferPosition,
        metadata: SemanticMetadata,
    ) -> u64 {
        let id = self.next_region_id;
        self.next_region_id += 1;
        self.regions.push(SemanticRegion {
            id,
            kind,
            start: position,
            end: None,
            metadata,
        });
        id
    }

    fn close_region(&mut self, id: u64, position: BufferPosition) {
        if let Some(region) = self.regions.iter_mut().find(|region| region.id == id) {
            region.end = Some(position);
        }
    }

    fn mark_event(&mut self, kind: SemanticEventKind) {
        self.last_event = Some((kind, Instant::now()));
    }
}

impl SemanticTimeline for SemanticTimelineStore {
    fn prompt_started(&mut self, position: BufferPosition, metadata: SemanticMetadata) {
        self.mark_event(SemanticEventKind::PromptStarted);
        if let Some(region_id) = self.open_prompt.take() {
            self.close_region(region_id, position);
        }
        self.metadata = merge_metadata(self.metadata.clone(), metadata.clone());
        self.open_prompt = Some(self.open_region(SemanticRegionKind::Prompt, position, metadata));
    }

    fn prompt_ended(&mut self, position: BufferPosition) {
        self.mark_event(SemanticEventKind::PromptEnded);
        if let Some(region_id) = self.open_prompt.take() {
            self.close_region(region_id, position);
        }
    }

    fn input_started(&mut self, position: BufferPosition) {
        self.mark_event(SemanticEventKind::InputStarted);
        if let Some(region_id) = self.open_input.take() {
            self.close_region(region_id, position);
        }
        let command_region_id =
            self.open_region(SemanticRegionKind::Command, position, self.metadata.clone());
        let input_region_id =
            self.open_region(SemanticRegionKind::Input, position, self.metadata.clone());
        self.open_command = Some(command_region_id);
        self.open_input = Some(input_region_id);
        self.active_command_started = Some(Instant::now());
        self.command_blocks.push(CommandBlock {
            region_id: command_region_id,
            input_region_id: Some(input_region_id),
            output_region_id: None,
            command: String::new(),
            status: CommandStatus::Running,
            started_at: position,
            ended_at: None,
            duration: None,
        });
    }

    fn input_ended(&mut self, position: BufferPosition) {
        self.mark_event(SemanticEventKind::InputEnded);
        if let Some(region_id) = self.open_input.take() {
            self.close_region(region_id, position);
        }
    }

    fn output_started(&mut self, position: BufferPosition) {
        self.mark_event(SemanticEventKind::OutputStarted);
        if self.open_command.is_none()
            && !self
                .command_blocks
                .last()
                .is_some_and(|block| matches!(block.status, CommandStatus::Running))
        {
            let command_region_id =
                self.open_region(SemanticRegionKind::Command, position, self.metadata.clone());
            self.open_command = Some(command_region_id);
            self.active_command_started = Some(Instant::now());
            self.command_blocks.push(CommandBlock {
                region_id: command_region_id,
                input_region_id: None,
                output_region_id: None,
                command: String::new(),
                status: CommandStatus::Running,
                started_at: position,
                ended_at: None,
                duration: None,
            });
        }
        if let Some(region_id) = self.open_input.take() {
            self.close_region(region_id, position);
        }
        if let Some(region_id) = self.open_output.take() {
            self.close_region(region_id, position);
        }
        let output_region_id =
            self.open_region(SemanticRegionKind::Output, position, self.metadata.clone());
        self.open_output = Some(output_region_id);
        if let Some(block) = self.command_blocks.last_mut()
            && block.output_region_id.is_none()
        {
            block.output_region_id = Some(output_region_id);
        }
    }

    fn output_ended(&mut self, position: BufferPosition) {
        self.mark_event(SemanticEventKind::OutputEnded);
        if let Some(region_id) = self.open_output.take() {
            self.close_region(region_id, position);
        }
    }

    fn command_finished(
        &mut self,
        position: BufferPosition,
        exit_status: CommandStatus,
        mut duration: Duration,
    ) {
        self.output_ended(position);
        self.mark_event(SemanticEventKind::CommandFinished);
        if let Some(region_id) = self.open_command.take() {
            self.close_region(region_id, position);
        }
        if duration.is_zero()
            && let Some(started) = self.active_command_started
        {
            duration = started.elapsed();
        }
        if let Some(block) = self.command_blocks.last_mut()
            && matches!(block.status, CommandStatus::Running)
        {
            block.status = exit_status;
            block.ended_at = Some(position);
            block.duration = Some(duration);
        }
        self.active_command_started = None;
    }
}

impl SemanticEvent {
    #[must_use]
    pub fn in_remote_session(self) -> Self {
        match self {
            Self::CurrentWorkingDirectoryChanged {
                position,
                directory,
                ..
            } => Self::CurrentWorkingDirectoryChanged {
                position,
                directory,
                remote: true,
            },
            other => other,
        }
    }

    #[must_use]
    pub fn at_position(self, position: BufferPosition) -> Self {
        match self {
            Self::PromptStarted { metadata, .. } => Self::PromptStarted { position, metadata },
            Self::PromptEnded { .. } => Self::PromptEnded { position },
            Self::InputStarted { .. } => Self::InputStarted { position },
            Self::InputEnded { .. } => Self::InputEnded { position },
            Self::OutputStarted { .. } => Self::OutputStarted { position },
            Self::OutputEnded { .. } => Self::OutputEnded { position },
            Self::CommandFinished {
                exit_status,
                duration,
                ..
            } => Self::CommandFinished {
                position,
                exit_status,
                duration,
            },
            Self::CurrentWorkingDirectoryChanged {
                directory, remote, ..
            } => Self::CurrentWorkingDirectoryChanged {
                position,
                directory,
                remote,
            },
            Self::ShellMetadataChanged { metadata, .. } => {
                Self::ShellMetadataChanged { position, metadata }
            }
            Self::RemoteMetadataChanged { metadata, .. } => {
                Self::RemoteMetadataChanged { position, metadata }
            }
        }
    }

    #[must_use]
    pub const fn kind(&self) -> SemanticEventKind {
        match self {
            Self::PromptStarted { .. } => SemanticEventKind::PromptStarted,
            Self::PromptEnded { .. } => SemanticEventKind::PromptEnded,
            Self::InputStarted { .. } => SemanticEventKind::InputStarted,
            Self::InputEnded { .. } => SemanticEventKind::InputEnded,
            Self::OutputStarted { .. } => SemanticEventKind::OutputStarted,
            Self::OutputEnded { .. } => SemanticEventKind::OutputEnded,
            Self::CommandFinished { .. } => SemanticEventKind::CommandFinished,
            Self::CurrentWorkingDirectoryChanged { .. } => {
                SemanticEventKind::CurrentWorkingDirectoryChanged
            }
            Self::ShellMetadataChanged { .. } => SemanticEventKind::ShellMetadataChanged,
            Self::RemoteMetadataChanged { .. } => SemanticEventKind::RemoteMetadataChanged,
        }
    }
}

fn merge_metadata(mut base: SemanticMetadata, next: SemanticMetadata) -> SemanticMetadata {
    if next.shell.shell.is_some() {
        base.shell.shell = next.shell.shell;
    }
    if next.shell.version.is_some() {
        base.shell.version = next.shell.version;
    }
    if next.shell.current_working_directory.is_some() {
        base.shell.current_working_directory = next.shell.current_working_directory;
    }
    if next.shell.prompt.is_some() {
        base.shell.prompt = next.shell.prompt;
    }
    if next.remote.is_some() {
        base.remote = next.remote;
    }
    if next.command.is_some() {
        base.command = next.command;
    }
    base.attributes.extend(next.attributes);
    base
}

fn block_contains_position(block: &CommandBlock, position: BufferPosition) -> bool {
    block.started_at <= position && block.ended_at.is_none_or(|end| position <= end)
}

#[must_use]
pub fn detect_url_hints<'a>(lines: impl IntoIterator<Item = (i64, &'a str)>) -> Vec<DetectedHint> {
    let mut hints = Vec::new();

    for (row, text) in lines {
        for prefix in ["https://", "http://"] {
            let mut search_from = 0;
            while let Some(relative_start) = text[search_from..].find(prefix) {
                let start = search_from + relative_start;
                let end = text[start..]
                    .find(is_url_terminator)
                    .map_or(text.len(), |relative_end| start + relative_end);
                if end > start {
                    let url = trim_url_suffix(&text[start..end]).to_owned();
                    if !url.is_empty() {
                        let col_start = text[..start].chars().count() as u16;
                        let col_end = col_start + url.chars().count() as u16;
                        hints.push(DetectedHint {
                            kind: HintKind::Url,
                            start: BufferPosition::new(row, col_start),
                            end: BufferPosition::new(row, col_end),
                            text: url.clone(),
                            action: HintAction::OpenUrl(url),
                        });
                    }
                }
                search_from = end.max(start + prefix.len());
            }
        }
    }

    hints
}

fn is_url_terminator(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | '[' | ']' | '{' | '}')
}

fn trim_url_suffix(text: &str) -> &str {
    text.trim_end_matches(['.', ',', ')', ';', ':'])
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct FakeTextProvider {
        spans: BTreeMap<SemanticSpanKey, String>,
    }

    impl FakeTextProvider {
        fn with(mut self, span: SemanticSpan, text: &str) -> Self {
            self.spans
                .insert(SemanticSpanKey::from(span), text.to_owned());
            self
        }
    }

    impl TerminalTextProvider for FakeTextProvider {
        fn text_for_span(&self, span: SemanticSpan) -> Option<String> {
            self.spans.get(&SemanticSpanKey::from(span)).cloned()
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    struct SemanticSpanKey {
        start: BufferPosition,
        end: BufferPosition,
    }

    impl From<SemanticSpan> for SemanticSpanKey {
        fn from(value: SemanticSpan) -> Self {
            Self {
                start: value.start,
                end: value.end,
            }
        }
    }

    #[test]
    fn semantics_has_no_runtime_crate_dependencies() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("[dependencies]"),
            "semantics must reference terminal positions without importing terminal, parser, renderer, platform, or transport crates"
        );
    }

    #[test]
    fn detects_url_hints_without_owning_terminal_text() {
        let hints = detect_url_hints([(4, "open https://example.test/path, now")]);

        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].text, "https://example.test/path");
        assert_eq!(hints[0].start, BufferPosition::new(4, 5));
    }

    #[test]
    fn timeline_tracks_command_regions_without_mutating_text() {
        let mut timeline = SemanticTimelineStore::new();
        timeline.apply_event(SemanticEvent::InputStarted {
            position: BufferPosition::new(10, 2),
        });
        timeline.apply_event(SemanticEvent::InputEnded {
            position: BufferPosition::new(10, 9),
        });
        timeline.apply_event(SemanticEvent::OutputStarted {
            position: BufferPosition::new(11, 0),
        });
        timeline.apply_event(SemanticEvent::CommandFinished {
            position: BufferPosition::new(12, 0),
            exit_status: CommandStatus::Code(0),
            duration: Duration::from_millis(12),
        });

        let block = &timeline.command_blocks()[0];
        assert_eq!(block.status, CommandStatus::Code(0));
        assert_eq!(
            timeline.output_span_for_command(block),
            Some(SemanticSpan::new(
                BufferPosition::new(11, 0),
                BufferPosition::new(12, 0)
            ))
        );
    }

    #[test]
    fn output_start_without_input_still_creates_command_block() {
        let mut timeline = SemanticTimelineStore::new();
        timeline.output_started(BufferPosition::new(8, 0));
        timeline.command_finished(
            BufferPosition::new(9, 0),
            CommandStatus::Code(0),
            Duration::from_millis(3),
        );

        let block = &timeline.command_blocks()[0];
        assert_eq!(block.started_at, BufferPosition::new(8, 0));
        assert_eq!(block.input_region_id, None);
        assert_eq!(block.status, CommandStatus::Code(0));
    }

    #[test]
    fn semantic_actions_extract_output_through_provider() {
        let mut timeline = SemanticTimelineStore::new();
        timeline.input_started(BufferPosition::new(1, 0));
        timeline.input_ended(BufferPosition::new(1, 7));
        timeline.output_started(BufferPosition::new(2, 0));
        timeline.command_finished(
            BufferPosition::new(3, 0),
            CommandStatus::Code(0),
            Duration::from_millis(1),
        );

        let provider = FakeTextProvider::default().with(
            SemanticSpan::new(BufferPosition::new(2, 0), BufferPosition::new(3, 0)),
            "panea\n",
        );

        assert_eq!(
            timeline.run_action(
                SemanticAction::CopyCurrentCommandOutput,
                BufferPosition::new(2, 1),
                &provider
            ),
            SemanticActionResult::Text("panea\n".to_owned())
        );
    }

    #[test]
    fn diagnostics_report_shell_integration_status() {
        let mut timeline = SemanticTimelineStore::new();
        timeline.apply_event(SemanticEvent::ShellMetadataChanged {
            position: BufferPosition::new(0, 0),
            metadata: ShellMetadata {
                shell: Some("bash".to_owned()),
                ..ShellMetadata::default()
            },
        });

        let diagnostics = timeline.diagnostics(Instant::now());

        assert!(diagnostics.integration_active);
        assert_eq!(diagnostics.shell_detected.as_deref(), Some("bash"));
        assert_eq!(
            diagnostics.command_block_confidence,
            CommandBlockConfidence::None
        );
    }

    #[test]
    fn transport_remote_metadata_does_not_claim_remote_integration() {
        let mut timeline = SemanticTimelineStore::new();
        timeline.set_remote_session_metadata(RemoteMetadata {
            transport: Some("ssh".to_owned()),
            remote_host: Some("example.test".to_owned()),
            ..RemoteMetadata::default()
        });

        let diagnostics = timeline.diagnostics(Instant::now());
        assert!(!diagnostics.remote_integration_active);
        assert!(!diagnostics.integration_active);
        assert_eq!(
            timeline
                .metadata()
                .remote
                .as_ref()
                .and_then(|remote| remote.remote_host.as_deref()),
            Some("example.test")
        );
    }
}
