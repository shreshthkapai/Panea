//! Semantic terminal metadata kept separate from raw terminal output.

pub const LAYER: &str = "semantic meaning";

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// Upper bound on retained command blocks. A long-lived shell produces roughly
/// five regions per prompt, so an append-only timeline grows for the entire life
/// of the pane; the oldest blocks are dropped together with the regions only
/// they referenced.
pub const MAX_RETAINED_COMMAND_BLOCKS: usize = 1024;

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
    /// A shell reported the command line itself rather than leaving it to be
    /// read back off the screen (VS Code's `OSC 633;E`).
    CommandLineRecorded {
        position: BufferPosition,
        command: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SemanticPositionKey {
    RegionStart(u64),
    RegionEnd(u64),
    CommandStart(u64),
    CommandEnd(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticPositionEntry {
    pub key: SemanticPositionKey,
    pub position: BufferPosition,
}

pub trait TerminalTextProvider {
    fn text_for_span(&self, span: SemanticSpan) -> Option<String>;
}

#[derive(Debug, Clone)]
pub struct SemanticTimelineStore {
    regions: BTreeMap<u64, SemanticRegion>,
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
    revision: u64,
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
            regions: BTreeMap::new(),
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
            revision: 1,
        }
    }

    /// Monotonic revision for render-facing semantic state.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1).max(1);
    }

    pub fn regions(&self) -> impl DoubleEndedIterator<Item = &SemanticRegion> + '_ {
        self.regions.values()
    }

    /// Looks up one region by id without scanning the timeline.
    #[must_use]
    pub fn region(&self, id: u64) -> Option<&SemanticRegion> {
        self.regions.get(&id)
    }

    #[must_use]
    pub fn region_count(&self) -> usize {
        self.regions.len()
    }

    #[must_use]
    pub fn command_blocks(&self) -> &[CommandBlock] {
        &self.command_blocks
    }

    /// Exports every retained row-bearing position with a stable semantic key
    /// so terminal resize can remap it without importing this crate.
    #[must_use]
    pub fn position_entries(&self) -> Vec<SemanticPositionEntry> {
        let mut entries = Vec::with_capacity(
            self.regions.len().saturating_mul(2) + self.command_blocks.len().saturating_mul(2),
        );
        for region in self.regions.values() {
            entries.push(SemanticPositionEntry {
                key: SemanticPositionKey::RegionStart(region.id),
                position: region.start,
            });
            if let Some(position) = region.end {
                entries.push(SemanticPositionEntry {
                    key: SemanticPositionKey::RegionEnd(region.id),
                    position,
                });
            }
        }
        for block in &self.command_blocks {
            entries.push(SemanticPositionEntry {
                key: SemanticPositionKey::CommandStart(block.region_id),
                position: block.started_at,
            });
            if let Some(position) = block.ended_at {
                entries.push(SemanticPositionEntry {
                    key: SemanticPositionKey::CommandEnd(block.region_id),
                    position,
                });
            }
        }
        entries
    }

    pub fn apply_position_entries(&mut self, entries: &[SemanticPositionEntry]) {
        if entries.is_empty() {
            return;
        }
        for entry in entries {
            match entry.key {
                SemanticPositionKey::RegionStart(id) => {
                    if let Some(region) = self.regions.get_mut(&id) {
                        region.start = entry.position;
                    }
                }
                SemanticPositionKey::RegionEnd(id) => {
                    if let Some(region) = self.regions.get_mut(&id)
                        && region.end.is_some()
                    {
                        region.end = Some(entry.position);
                    }
                }
                SemanticPositionKey::CommandStart(id) => {
                    if let Some(block) = self
                        .command_blocks
                        .iter_mut()
                        .find(|block| block.region_id == id)
                    {
                        block.started_at = entry.position;
                    }
                }
                SemanticPositionKey::CommandEnd(id) => {
                    if let Some(block) = self
                        .command_blocks
                        .iter_mut()
                        .find(|block| block.region_id == id)
                        && block.ended_at.is_some()
                    {
                        block.ended_at = Some(entry.position);
                    }
                }
            }
        }
        self.bump_revision();
    }

    #[must_use]
    pub const fn metadata(&self) -> &SemanticMetadata {
        &self.metadata
    }

    pub fn set_integration_mode(&mut self, mode: IntegrationMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.bump_revision();
    }

    #[must_use]
    pub const fn integration_mode(&self) -> IntegrationMode {
        self.mode
    }

    /// Records transport-provided remote context without claiming that remote
    /// shell integration has emitted a semantic marker.
    pub fn set_remote_session_metadata(&mut self, metadata: RemoteMetadata) {
        if self.metadata.remote.as_ref() == Some(&metadata) {
            return;
        }
        self.metadata.remote = Some(metadata);
        self.bump_revision();
    }

    /// Marks that a semantic marker was actually observed in a remote byte
    /// stream. Transport metadata alone must not call this method.
    pub fn mark_remote_integration_active(&mut self) {
        if self.remote_integration_active {
            return;
        }
        self.remote_integration_active = true;
        self.bump_revision();
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
            SemanticEvent::CommandLineRecorded { position, command } => {
                self.command_line_recorded(position, command);
            }
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
        self.regions.get(&region_id).and_then(SemanticRegion::span)
    }

    #[must_use]
    pub fn command_span(&self, block: &CommandBlock) -> Option<SemanticSpan> {
        self.regions.get(&block.region_id)?.span()
    }

    #[must_use]
    pub fn command_metadata(&self, block: &CommandBlock) -> Option<&SemanticMetadata> {
        self.regions
            .get(&block.region_id)
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
            .get(&region_id)
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
        self.regions.insert(
            id,
            SemanticRegion {
                id,
                kind,
                start: position,
                end: None,
                metadata,
            },
        );
        id
    }

    fn close_region(&mut self, id: u64, position: BufferPosition) {
        if let Some(region) = self.regions.get_mut(&id) {
            region.end = Some(position);
        }
    }

    fn is_open_region(&self, id: u64) -> bool {
        [
            self.open_prompt,
            self.open_input,
            self.open_output,
            self.open_command,
        ]
        .contains(&Some(id))
    }

    /// Opens the command and input regions for a new command line.
    ///
    /// Idempotent: a shell that marks the end of its prompt *and* the start of
    /// input (FinalTerm `B` followed by an explicit `I`, or Panea's own
    /// `prompt_end` plus `input_start`) must produce one command block, not two.
    fn begin_input(&mut self, position: BufferPosition) {
        if self.open_input.is_some() {
            return;
        }
        let command_region_id = match self.open_command {
            Some(open) => open,
            None => {
                let id =
                    self.open_region(SemanticRegionKind::Command, position, self.metadata.clone());
                self.open_command = Some(id);
                id
            }
        };
        let input_region_id =
            self.open_region(SemanticRegionKind::Input, position, self.metadata.clone());
        self.open_input = Some(input_region_id);
        self.active_command_started = Some(Instant::now());

        // Reuse the block a prior marker already opened for this command rather
        // than starting a second one for the same prompt.
        if let Some(block) = self.command_blocks.last_mut()
            && block.region_id == command_region_id
        {
            block.input_region_id = Some(input_region_id);
            return;
        }
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
        self.prune_to_capacity();
    }

    /// Records the command line a shell reported out of band (VS Code's
    /// `OSC 633;E`), so command blocks have text even when the input region is
    /// never rendered.
    fn command_line_recorded(&mut self, position: BufferPosition, command: String) {
        self.begin_input(position);
        if let Some(block) = self.command_blocks.last_mut()
            && matches!(block.status, CommandStatus::Running)
        {
            block.command = command;
        }
    }

    /// Drops the oldest command blocks, and every region only they referenced,
    /// once the retained history exceeds [`MAX_RETAINED_COMMAND_BLOCKS`].
    fn prune_to_capacity(&mut self) {
        if self.command_blocks.len() <= MAX_RETAINED_COMMAND_BLOCKS {
            return;
        }
        let excess = self.command_blocks.len() - MAX_RETAINED_COMMAND_BLOCKS;
        self.command_blocks.drain(..excess);
        self.drop_regions_before_retained_blocks();
    }

    /// Region ids increase monotonically, so everything below the lowest id a
    /// retained block still references is unreachable.
    fn drop_regions_before_retained_blocks(&mut self) {
        let lowest_retained = self
            .command_blocks
            .iter()
            .flat_map(|block| {
                [
                    Some(block.region_id),
                    block.input_region_id,
                    block.output_region_id,
                ]
            })
            .flatten()
            .min();
        let Some(lowest_retained) = lowest_retained else {
            let open = self.regions.keys().copied().collect::<Vec<_>>();
            for id in open {
                if !self.is_open_region(id) {
                    self.regions.remove(&id);
                }
            }
            return;
        };
        let unreachable = self
            .regions
            .range(..lowest_retained)
            .map(|(id, _)| *id)
            .filter(|id| !self.is_open_region(*id))
            .collect::<Vec<_>>();
        for id in unreachable {
            self.regions.remove(&id);
        }
    }

    /// The id the next region will be given.
    ///
    /// Callers snapshot it to mark a boundary, then pass it to
    /// [`Self::discard_regions_from`] to drop everything recorded after it.
    #[must_use]
    pub const fn region_id_watermark(&self) -> u64 {
        self.next_region_id
    }

    /// Drops every region and command block recorded at or after `id`.
    pub fn discard_regions_from(&mut self, id: u64) {
        self.regions.retain(|region_id, _| *region_id < id);
        self.command_blocks.retain(|block| block.region_id < id);
        for open in [
            &mut self.open_prompt,
            &mut self.open_input,
            &mut self.open_output,
            &mut self.open_command,
        ] {
            if open.is_some_and(|open| open >= id) {
                *open = None;
            }
        }
    }

    /// Shifts every recorded row up by `lines`, matching a scrollback eviction
    /// of the same size.
    ///
    /// Absolute buffer rows move down as lines leave the top of the buffer, so a
    /// timeline that is not rebased ends up pointing at whatever text later
    /// occupies its old coordinates. Call this with the terminal's eviction
    /// delta (`TerminalState::scrollback_dropped` since the last call) before
    /// [`Self::prune_before_row`], which assumes rows are already current.
    pub fn rebase_rows(&mut self, lines: u64) {
        if lines == 0 {
            return;
        }
        let shift = i64::try_from(lines).unwrap_or(i64::MAX);
        for region in self.regions.values_mut() {
            region.start.row -= shift;
            if let Some(end) = region.end.as_mut() {
                end.row -= shift;
            }
        }
        for block in &mut self.command_blocks {
            block.started_at.row -= shift;
            if let Some(ended_at) = block.ended_at.as_mut() {
                ended_at.row -= shift;
            }
        }
        self.bump_revision();
    }

    /// Drops history that has scrolled out of the retained buffer. Callers pass
    /// the lowest buffer row still addressable after scrollback eviction; rows
    /// below it can no longer be resolved to text, so keeping their regions only
    /// costs memory and slows every lookup.
    pub fn prune_before_row(&mut self, row: i64) {
        self.command_blocks.retain(|block| match block.ended_at {
            Some(ended_at) => ended_at.row >= row,
            None => true,
        });
        let scrolled_out = self
            .regions
            .iter()
            .filter(|(id, region)| {
                !self.is_open_region(**id) && region.end.is_some_and(|end| end.row < row)
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in scrolled_out {
            self.regions.remove(&id);
        }
        self.drop_regions_before_retained_blocks();
        self.bump_revision();
    }

    fn mark_event(&mut self, kind: SemanticEventKind) {
        self.last_event = Some((kind, Instant::now()));
        self.bump_revision();
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
        // FinalTerm `OSC 133;B` marks the end of the prompt *and* the start of
        // user input. Shells that emit it without a separate input marker (fish's
        // built-in integration) otherwise never open an input region, which left
        // their command blocks with no recoverable command text.
        self.begin_input(position);
    }

    fn input_started(&mut self, position: BufferPosition) {
        self.mark_event(SemanticEventKind::InputStarted);
        self.begin_input(position);
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
            self.prune_to_capacity();
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
            Self::CommandLineRecorded { command, .. } => {
                Self::CommandLineRecorded { position, command }
            }
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
            // Reported alongside the start of input, and diagnostics track the
            // boundary rather than the transport detail, so it shares that kind.
            Self::CommandLineRecorded { .. } => SemanticEventKind::InputStarted,
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

    fn run_one_command(timeline: &mut SemanticTimelineStore, row: i64) {
        timeline.prompt_started(BufferPosition::new(row, 0), SemanticMetadata::default());
        timeline.prompt_ended(BufferPosition::new(row, 2));
        timeline.input_started(BufferPosition::new(row, 2));
        timeline.input_ended(BufferPosition::new(row, 9));
        timeline.output_started(BufferPosition::new(row + 1, 0));
        timeline.command_finished(
            BufferPosition::new(row + 2, 0),
            CommandStatus::Code(0),
            Duration::from_millis(1),
        );
    }

    #[test]
    fn long_lived_sessions_stop_accumulating_semantic_regions() {
        let mut timeline = SemanticTimelineStore::new();
        for index in 0..(MAX_RETAINED_COMMAND_BLOCKS as i64 + 200) {
            run_one_command(&mut timeline, index * 3);
        }

        assert_eq!(
            timeline.command_blocks().len(),
            MAX_RETAINED_COMMAND_BLOCKS,
            "retained command history must stay bounded"
        );
        // Four regions per command survive for retained blocks; an unbounded
        // store would hold well over five thousand here.
        assert!(
            timeline.region_count() <= MAX_RETAINED_COMMAND_BLOCKS * 4 + 8,
            "regions must be dropped with the blocks that referenced them, got {}",
            timeline.region_count()
        );
        let newest = timeline
            .command_blocks()
            .last()
            .expect("newest command block");
        assert!(
            timeline.command_span(newest).is_some(),
            "pruning must never drop regions the retained blocks still reference"
        );
    }

    struct StaticText(&'static str);

    impl TerminalTextProvider for StaticText {
        fn text_for_span(&self, _span: SemanticSpan) -> Option<String> {
            Some(self.0.to_owned())
        }
    }

    #[test]
    fn a_prompt_end_marker_alone_opens_the_input_region() {
        // fish's built-in integration emits OSC 133 A/B/C/D with no separate
        // input marker, so B has to start input or the command block never has
        // recoverable text.
        // The exact sequence fish emits: A, B, C, D.
        let mut timeline = SemanticTimelineStore::new();
        timeline.prompt_started(BufferPosition::new(3, 0), SemanticMetadata::default());
        timeline.prompt_ended(BufferPosition::new(3, 2));
        timeline.output_started(BufferPosition::new(4, 0));
        timeline.command_finished(
            BufferPosition::new(5, 0),
            CommandStatus::Code(0),
            Duration::from_millis(4),
        );

        let block = timeline.command_blocks().first().expect("command block");
        assert!(
            block.input_region_id.is_some(),
            "prompt end must open an input region"
        );
        assert_eq!(
            timeline
                .command_text(block, &StaticText("git status"))
                .as_deref(),
            Some("git status"),
            "the input span must be resolvable once output starts"
        );
        assert!(
            timeline.output_span_for_command(block).is_some(),
            "output must still be attributed to the block"
        );
    }

    #[test]
    fn a_prompt_end_followed_by_an_input_marker_makes_one_command_block() {
        let mut timeline = SemanticTimelineStore::new();
        timeline.prompt_started(BufferPosition::new(0, 0), SemanticMetadata::default());
        timeline.prompt_ended(BufferPosition::new(0, 2));
        // Panea's own protocol sends both; two blocks for one prompt would
        // double every command in the block list.
        timeline.input_started(BufferPosition::new(0, 2));
        timeline.input_ended(BufferPosition::new(0, 9));
        timeline.output_started(BufferPosition::new(1, 0));

        assert_eq!(timeline.command_blocks().len(), 1);
    }

    #[test]
    fn a_recorded_command_line_lands_on_the_running_block() {
        let mut timeline = SemanticTimelineStore::new();
        timeline.prompt_started(BufferPosition::new(0, 0), SemanticMetadata::default());
        timeline.apply_event(SemanticEvent::CommandLineRecorded {
            position: BufferPosition::new(0, 2),
            command: "cargo test".to_owned(),
        });

        let block = timeline.command_blocks().first().expect("command block");
        assert_eq!(block.command, "cargo test");
        // A reported command line wins over reading the screen back.
        assert_eq!(
            timeline
                .command_text(block, &StaticText("something else"))
                .as_deref(),
            Some("cargo test")
        );
        assert_eq!(timeline.command_blocks().len(), 1);
    }

    #[test]
    fn rebasing_rows_keeps_regions_on_their_text_after_eviction() {
        let mut timeline = SemanticTimelineStore::new();
        run_one_command(&mut timeline, 100);
        let before = timeline
            .command_blocks()
            .first()
            .expect("command block")
            .started_at
            .row;

        timeline.rebase_rows(40);

        let after = timeline
            .command_blocks()
            .first()
            .expect("command block")
            .started_at
            .row;
        assert_eq!(
            after,
            before - 40,
            "recorded rows must follow their text when the buffer drops lines"
        );
        assert!(
            timeline.regions().all(|region| region.start.row < before),
            "every region row must be rebased, not just command blocks"
        );
    }

    #[test]
    fn semantic_position_entries_round_trip_through_external_reflow_mapping() {
        let mut timeline = SemanticTimelineStore::new();
        run_one_command(&mut timeline, 10);

        let mut entries = timeline.position_entries();
        for entry in &mut entries {
            entry.position.row += 7;
            entry.position.col += 1;
        }
        timeline.apply_position_entries(&entries);

        assert!(
            timeline
                .regions()
                .all(|region| region.start.row >= 17 && region.start.col >= 1)
        );
        assert!(
            timeline
                .command_blocks()
                .iter()
                .all(|block| block.started_at.row >= 17 && block.started_at.col >= 1)
        );
    }

    #[test]
    fn scrolled_out_history_is_pruned_and_open_regions_survive() {
        let mut timeline = SemanticTimelineStore::new();
        run_one_command(&mut timeline, 0);
        run_one_command(&mut timeline, 100);
        // An in-flight command: its prompt region is still open.
        timeline.prompt_started(BufferPosition::new(200, 0), SemanticMetadata::default());
        let regions_before = timeline.region_count();

        timeline.prune_before_row(150);

        assert_eq!(
            timeline.command_blocks().len(),
            0,
            "commands whose rows left the retained buffer must be dropped"
        );
        assert!(timeline.region_count() < regions_before);
        assert!(
            timeline
                .regions()
                .any(|region| region.kind == SemanticRegionKind::Prompt && region.end.is_none()),
            "an open region must survive pruning even when its row is below the watermark"
        );
    }

    #[test]
    fn regions_are_addressable_by_id_without_scanning() {
        let mut timeline = SemanticTimelineStore::new();
        run_one_command(&mut timeline, 4);
        let block = &timeline.command_blocks()[0];

        let region = timeline
            .region(block.region_id)
            .expect("command region by id");

        assert_eq!(region.id, block.region_id);
        assert!(timeline.region(u64::MAX).is_none());
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

    #[test]
    fn revision_advances_for_semantic_and_position_mutations() {
        let mut timeline = SemanticTimelineStore::new();
        let initial = timeline.revision();

        timeline.apply_event(SemanticEvent::PromptStarted {
            position: BufferPosition::new(4, 0),
            metadata: SemanticMetadata::default(),
        });
        let event_revision = timeline.revision();
        assert!(event_revision > initial);

        let mut positions = timeline.position_entries();
        positions[0].position.row = 9;
        timeline.apply_position_entries(&positions);
        assert!(timeline.revision() > event_revision);
    }
}
