//! Local PTY and Windows pseudoconsole transport boundary.

pub const LAYER: &str = "session transport";

use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use transport_core::{
    SessionMetadata, TerminalSize, TerminalTransport, TransportError, TransportKind,
    TransportLifecycleEvent, TransportOutput, TransportResult, TransportState,
    TransportTerminationHandle, TransportWakeHandle,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalShellKind {
    Default,
    PowerShell,
    Cmd,
    Wsl,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalShellProfile {
    pub name: String,
    pub kind: LocalShellKind,
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub working_directory: Option<PathBuf>,
    pub startup_command: Option<String>,
}

impl LocalShellProfile {
    #[must_use]
    pub fn default_for_platform() -> Self {
        #[cfg(windows)]
        {
            Self::powershell()
        }

        #[cfg(not(windows))]
        {
            let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
            Self {
                name: "default".to_owned(),
                kind: LocalShellKind::Default,
                program,
                args: if cfg!(target_os = "macos") {
                    vec!["-l".to_owned()]
                } else {
                    Vec::new()
                },
                env: BTreeMap::new(),
                working_directory: None,
                startup_command: None,
            }
        }
    }

    #[must_use]
    pub fn powershell() -> Self {
        Self {
            name: "powershell".to_owned(),
            kind: LocalShellKind::PowerShell,
            program: "powershell.exe".to_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
            working_directory: None,
            startup_command: None,
        }
    }

    #[must_use]
    pub fn cmd() -> Self {
        Self {
            name: "cmd".to_owned(),
            kind: LocalShellKind::Cmd,
            program: "cmd.exe".to_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
            working_directory: None,
            startup_command: None,
        }
    }

    #[must_use]
    pub fn wsl(distribution: Option<String>) -> Self {
        let mut args = Vec::new();

        if let Some(distribution) = distribution {
            args.push("--distribution".to_owned());
            args.push(distribution);
        }

        Self {
            name: "wsl".to_owned(),
            kind: LocalShellKind::Wsl,
            program: "wsl.exe".to_owned(),
            args,
            env: BTreeMap::new(),
            working_directory: None,
            startup_command: None,
        }
    }

    #[must_use]
    pub fn custom(name: impl Into<String>, program: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: LocalShellKind::Custom,
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            working_directory: None,
            startup_command: None,
        }
    }

    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    #[must_use]
    pub fn with_working_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(directory.into());
        self
    }

    #[must_use]
    pub fn with_startup_command(mut self, command: impl Into<String>) -> Self {
        self.startup_command = Some(command.into());
        self
    }

    fn command_builder(&self) -> TransportResult<CommandBuilder> {
        if self.program.trim().is_empty() {
            return Err(TransportError::new("shell profile program cannot be empty"));
        }

        let mut command = CommandBuilder::new(&self.program);
        let args = self.effective_args()?;
        command.args(args);

        if !self.env.contains_key("TERM") {
            command.env("TERM", "xterm-256color");
        }
        if !self.env.contains_key("COLORTERM") {
            command.env("COLORTERM", "truecolor");
        }
        if !self.env.contains_key("TERM_PROGRAM") {
            command.env("TERM_PROGRAM", "Panea");
        }
        if !self.env.contains_key("TERM_PROGRAM_VERSION") {
            command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        }

        for (key, value) in &self.env {
            command.env(key, value);
        }

        if let Some(directory) = &self.working_directory {
            command.cwd(directory);
        }

        Ok(command)
    }

    fn command_label(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn effective_args(&self) -> TransportResult<Vec<String>> {
        let mut args = self.args.clone();

        let Some(startup_command) = &self.startup_command else {
            return Ok(args);
        };

        if !args.is_empty() {
            return Err(TransportError::new(
                "startup_command cannot be combined with explicit shell args yet",
            ));
        }

        match self.kind {
            LocalShellKind::PowerShell => {
                args.push("-NoExit".to_owned());
                args.push("-Command".to_owned());
                args.push(startup_command.clone());
            }
            LocalShellKind::Cmd => {
                args.push("/K".to_owned());
                args.push(startup_command.clone());
            }
            LocalShellKind::Wsl => {
                args.push("--exec".to_owned());
                args.push("sh".to_owned());
                args.push("-lc".to_owned());
                args.push(format!("{startup_command}; exec \"$SHELL\" -i"));
            }
            LocalShellKind::Default | LocalShellKind::Custom => {
                if cfg!(windows) {
                    args.push("-NoExit".to_owned());
                    args.push("-Command".to_owned());
                    args.push(startup_command.clone());
                } else {
                    args.push("-lc".to_owned());
                    args.push(format!("{startup_command}; exec \"$SHELL\" -i"));
                }
            }
        }

        Ok(args)
    }
}

pub struct LocalPtyTransport {
    master: Option<Box<dyn MasterPty + Send>>,
    writer_tx: Option<SyncSender<Vec<u8>>>,
    writer_rx: Receiver<WriterMessage>,
    writer_thread: Option<JoinHandle<()>>,
    child_killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    child_rx: Receiver<ChildMessage>,
    child_thread: Option<JoinHandle<()>>,
    reader_rx: Receiver<ReaderMessage>,
    reader_thread: Option<JoinHandle<()>>,
    read_buffer_pool: ReadBufferPool,
    output_waker: Arc<OnceLock<TransportWakeHandle>>,
    diagnostics: LocalPtyDiagnostics,
    metadata: SessionMetadata,
    state: TransportState,
    pending_lifecycle: VecDeque<TransportLifecycleEvent>,
    pending_reader_termination_failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPtyDiagnostics {
    pub command: String,
    pub process_id: Option<u32>,
    pub state: TransportState,
    pub bytes_received: usize,
    pub read_events: usize,
    pub last_bytes_preview: Vec<u8>,
    pub reader_started: bool,
    pub reader_stopped: bool,
    pub reader_error: Option<String>,
    pub child_exited: bool,
    pub kill_attempted: bool,
    pub shutdown_timed_out: bool,
}

impl LocalPtyDiagnostics {
    fn new(command: String, process_id: Option<u32>, state: TransportState) -> Self {
        Self {
            command,
            process_id,
            state,
            bytes_received: 0,
            read_events: 0,
            last_bytes_preview: Vec::new(),
            reader_started: false,
            reader_stopped: false,
            reader_error: None,
            child_exited: false,
            kill_attempted: false,
            shutdown_timed_out: false,
        }
    }

    fn record_bytes(&mut self, chunk: &[u8]) {
        self.bytes_received += chunk.len();
        self.read_events += 1;

        const PREVIEW_LIMIT: usize = 256;
        if chunk.len() >= PREVIEW_LIMIT {
            self.last_bytes_preview.clear();
            self.last_bytes_preview
                .extend_from_slice(&chunk[chunk.len() - PREVIEW_LIMIT..]);
            return;
        }
        let overflow = self
            .last_bytes_preview
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(PREVIEW_LIMIT);
        if overflow > 0 {
            self.last_bytes_preview.copy_within(overflow.., 0);
            self.last_bytes_preview
                .truncate(self.last_bytes_preview.len() - overflow);
        }
        self.last_bytes_preview.extend_from_slice(chunk);
    }
}

impl LocalPtyTransport {
    pub fn spawn_default(size: TerminalSize) -> TransportResult<Self> {
        Self::spawn(LocalShellProfile::default_for_platform(), size)
    }

    pub fn spawn(profile: LocalShellProfile, size: TerminalSize) -> TransportResult<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(to_pty_size(size))
            .map_err(|error| TransportError::new(format!("failed to create PTY: {error}")))?;

        let command_label = profile.command_label();
        let command = profile.command_builder()?;
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| TransportError::new(format!("failed to spawn shell: {error}")))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| TransportError::new(format!("failed to clone PTY reader: {error}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| TransportError::new(format!("failed to open PTY writer: {error}")))?;

        let process_id = child.process_id();
        let output_waker = Arc::new(OnceLock::new());
        let read_buffer_pool = ReadBufferPool::new();
        let (reader_rx, reader_thread) =
            spawn_reader(reader, Arc::clone(&output_waker), read_buffer_pool.clone());
        let (writer_tx, writer_rx, writer_thread) = spawn_writer(writer, Arc::clone(&output_waker));
        let child_killer = Arc::new(Mutex::new(child.clone_killer()));
        let (child_rx, child_thread) = spawn_child_waiter(child, Arc::clone(&output_waker));

        let mut pending_lifecycle = VecDeque::new();
        pending_lifecycle.push_back(TransportLifecycleEvent::Started);

        Ok(Self {
            master: Some(pair.master),
            writer_tx: Some(writer_tx),
            writer_rx,
            writer_thread: Some(writer_thread),
            child_killer,
            child_rx,
            child_thread: Some(child_thread),
            reader_rx,
            reader_thread: Some(reader_thread),
            read_buffer_pool,
            output_waker,
            diagnostics: LocalPtyDiagnostics::new(
                command_label,
                process_id,
                TransportState::Running,
            ),
            metadata: SessionMetadata {
                id: make_session_id(process_id),
                kind: platform_transport_kind(),
                title: Some(profile.name),
                shell: Some(profile.program),
                current_working_directory: profile
                    .working_directory
                    .map(|directory| directory.display().to_string()),
                remote_host: None,
            },
            state: TransportState::Running,
            pending_lifecycle,
            pending_reader_termination_failure: None,
        })
    }

    #[must_use]
    pub fn diagnostics(&self) -> LocalPtyDiagnostics {
        let mut diagnostics = self.diagnostics.clone();
        diagnostics.state = self.state.clone();
        diagnostics
    }

    fn mark_child_exited(&mut self, exit_code: Option<i32>) {
        if !matches!(
            self.state,
            TransportState::DrainingOutput { .. } | TransportState::Closed { .. }
        ) {
            self.state = TransportState::DrainingOutput { exit_code };
            self.diagnostics.child_exited = true;
            self.diagnostics.state = self.state.clone();
            self.writer_tx.take();
            self.close_master_without_blocking();
            self.pending_lifecycle
                .push_back(TransportLifecycleEvent::Exited { exit_code });
        }
    }

    fn mark_closed(&mut self, exit_code: Option<i32>) {
        if !matches!(self.state, TransportState::Closed { .. }) {
            self.state = TransportState::Closed { exit_code };
            self.diagnostics.state = self.state.clone();
            self.pending_lifecycle
                .push_back(TransportLifecycleEvent::Closed);
        }
    }

    fn mark_failed(&mut self, message: impl Into<String>) {
        self.state = TransportState::Failed {
            message: message.into(),
        };
        self.diagnostics.state = self.state.clone();
        self.pending_lifecycle
            .push_back(TransportLifecycleEvent::Closed);
    }

    fn request_child_termination(&mut self) -> TransportResult<()> {
        self.state = TransportState::TerminatingChild;
        self.diagnostics.state = self.state.clone();
        self.diagnostics.kill_attempted = true;
        self.child_killer
            .lock()
            .map_err(|_| TransportError::new("local shell termination handle is poisoned"))?
            .kill()
            .map_err(|error| TransportError::new(format!("failed to terminate shell: {error}")))
    }

    fn close_master_without_blocking(&mut self) {
        if let Some(master) = self.master.take() {
            thread::spawn(move || drop(master));
        }
    }

    fn join_background_threads_if_finished(&mut self) {
        if self
            .reader_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(reader_thread) = self.reader_thread.take()
        {
            let _ = reader_thread.join();
        }
        if self
            .writer_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(writer_thread) = self.writer_thread.take()
        {
            let _ = writer_thread.join();
        }
        if self
            .child_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(child_thread) = self.child_thread.take()
        {
            let _ = child_thread.join();
        }
    }

    fn drain_child_messages(&mut self) {
        while let Ok(message) = self.child_rx.try_recv() {
            match message {
                ChildMessage::Exited(exit_code) => self.mark_child_exited(exit_code),
                ChildMessage::Failed(message) => self.mark_failed(message),
            }
        }
    }

    fn drain_writer_messages(&mut self) {
        while let Ok(message) = self.writer_rx.try_recv() {
            match message {
                WriterMessage::Failed(message) => {
                    self.writer_tx.take();
                    if matches!(
                        self.state,
                        TransportState::Starting | TransportState::Running
                    ) {
                        self.mark_failed(format!("local PTY writer failed: {message}"));
                    }
                }
            }
        }
    }

    fn drain_reader_messages(&mut self) -> TransportOutput {
        let mut bytes = Vec::new();
        let mut lifecycle = Vec::new();
        let mut closed = false;

        while let Some(event) = self.pending_lifecycle.pop_front() {
            if matches!(event, TransportLifecycleEvent::Closed) {
                closed = true;
            }
            lifecycle.push(event);
        }

        while let Ok(message) = self.reader_rx.try_recv() {
            match message {
                ReaderMessage::Started => {
                    self.diagnostics.reader_started = true;
                }
                ReaderMessage::Bytes { buffer, len } => {
                    let payload = &buffer[..len.min(buffer.len())];
                    self.diagnostics.record_bytes(payload);
                    bytes = payload.to_vec();
                    self.read_buffer_pool.recycle(buffer);
                    break;
                }
                ReaderMessage::Closed => {
                    self.diagnostics.reader_stopped = true;
                }
                ReaderMessage::Failed(message) => {
                    self.diagnostics.reader_error = Some(message.clone());
                    self.diagnostics.reader_stopped = true;
                    if reader_failure_requires_child_termination(&self.state)
                        && let Err(error) = self.request_child_termination()
                    {
                        let failure =
                            format!("{message}; child termination request failed: {error}");
                        self.diagnostics.reader_error = Some(failure.clone());
                        self.pending_reader_termination_failure = Some(failure);
                    }
                }
                ReaderMessage::Stopped => {
                    self.diagnostics.reader_stopped = true;
                }
            }
        }

        if !bytes.is_empty() {
            lifecycle.push(TransportLifecycleEvent::OutputReady);
        }

        TransportOutput {
            bytes,
            closed,
            lifecycle,
        }
    }
}

impl TerminalTransport for LocalPtyTransport {
    fn set_output_waker(&mut self, waker: Option<TransportWakeHandle>) {
        if let Some(waker) = waker
            && self.output_waker.set(waker.clone()).is_ok()
        {
            waker.wake();
        }
    }

    fn termination_handle(&self) -> Option<TransportTerminationHandle> {
        let child_killer = Arc::clone(&self.child_killer);
        Some(TransportTerminationHandle::new(move || {
            if let Ok(mut child_killer) = child_killer.lock() {
                let _ = child_killer.kill();
            }
        }))
    }

    fn periodic_poll_interval(&self) -> Option<Duration> {
        None
    }

    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
        if matches!(
            self.state,
            TransportState::ClosingInput
                | TransportState::TerminatingChild
                | TransportState::DrainingOutput { .. }
                | TransportState::Closed { .. }
                | TransportState::Failed { .. }
        ) {
            return Err(TransportError::new("cannot write to a closed local PTY"));
        }

        let writer_tx = self
            .writer_tx
            .as_ref()
            .ok_or_else(|| TransportError::new("local PTY writer is closed"))?;
        queue_writer_input(writer_tx, bytes)
    }

    fn resize(&mut self, size: TerminalSize) -> TransportResult<()> {
        if matches!(
            self.state,
            TransportState::Closed { .. } | TransportState::Failed { .. }
        ) {
            return Err(TransportError::new("cannot resize a closed local PTY"));
        }

        self.master
            .as_ref()
            .ok_or_else(|| TransportError::new("local PTY master is closed"))?
            .resize(to_pty_size(size))
            .map_err(|error| TransportError::new(format!("failed to resize local PTY: {error}")))?;
        self.pending_lifecycle
            .push_back(TransportLifecycleEvent::Resized(size));
        Ok(())
    }

    fn poll_output(&mut self) -> TransportResult<TransportOutput> {
        self.drain_child_messages();
        self.drain_writer_messages();
        let mut output = self.drain_reader_messages();
        self.drain_child_messages();
        self.drain_writer_messages();

        if unresolved_reader_termination_failure_is_terminal(&self.state) {
            if let Some(message) = self.pending_reader_termination_failure.take() {
                self.mark_failed(message);
            }
        } else {
            self.pending_reader_termination_failure.take();
        }

        while let Some(event) = self.pending_lifecycle.pop_front() {
            if matches!(event, TransportLifecycleEvent::Closed) {
                output.closed = true;
            }
            output.lifecycle.push(event);
        }

        if matches!(self.state, TransportState::DrainingOutput { .. })
            && self.diagnostics.reader_stopped
        {
            let exit_code = match self.state {
                TransportState::DrainingOutput { exit_code } => exit_code,
                _ => None,
            };
            self.mark_closed(exit_code);
        }

        while let Some(event) = self.pending_lifecycle.pop_front() {
            if matches!(event, TransportLifecycleEvent::Closed) {
                output.closed = true;
            }
            output.lifecycle.push(event);
        }

        self.join_background_threads_if_finished();
        self.diagnostics.state = self.state.clone();

        Ok(output)
    }

    fn shutdown(&mut self) -> TransportResult<()> {
        if matches!(self.state, TransportState::Closed { .. }) {
            return Ok(());
        }

        self.state = TransportState::ClosingInput;
        self.diagnostics.state = self.state.clone();
        self.pending_lifecycle
            .push_back(TransportLifecycleEvent::ShutdownRequested);
        self.writer_tx.take();
        self.request_child_termination()?;
        self.close_master_without_blocking();
        self.drain_child_messages();
        let _ = self.drain_reader_messages();
        self.join_background_threads_if_finished();
        self.diagnostics.state = self.state.clone();

        Ok(())
    }

    fn session_metadata(&self) -> SessionMetadata {
        self.metadata.clone()
    }

    fn state(&self) -> TransportState {
        self.state.clone()
    }
}

impl Drop for LocalPtyTransport {
    fn drop(&mut self) {
        if matches!(
            self.state,
            TransportState::Running
                | TransportState::ClosingInput
                | TransportState::TerminatingChild
                | TransportState::Failed { .. }
        ) {
            self.writer_tx.take();
            let _ = self.request_child_termination();
            self.close_master_without_blocking();
            self.join_background_threads_if_finished();
        }
    }
}

fn reader_failure_requires_child_termination(state: &TransportState) -> bool {
    // PTY readers can report EIO as a normal child exits, so the child waiter
    // remains the authoritative lifecycle source. If no exit is in progress,
    // force the child toward that waiter instead of leaving a broken reader in
    // Running forever.
    matches!(state, TransportState::Starting | TransportState::Running)
}

fn unresolved_reader_termination_failure_is_terminal(state: &TransportState) -> bool {
    !matches!(
        state,
        TransportState::DrainingOutput { .. }
            | TransportState::Closed { .. }
            | TransportState::Failed { .. }
    )
}

enum WriterMessage {
    Failed(String),
}

enum ChildMessage {
    Exited(Option<i32>),
    Failed(String),
}

enum ReaderMessage {
    Started,
    /// A filled scratch buffer plus the number of valid bytes in it. The buffer
    /// is owned by [`ReadBufferPool`] and must be recycled after the payload is
    /// copied out.
    Bytes {
        buffer: Vec<u8>,
        len: usize,
    },
    Closed,
    Failed(String),
    Stopped,
}

/// Reader chunk size. Larger reads mean proportionally fewer event-loop wakes
/// and syscalls under heavy output than the 8 KiB this used to use.
const READ_BUFFER_BYTES: usize = 64 * 1024;

/// Depth of the reader's queue, in chunks. The send below blocks once it is
/// full, so a child that outruns the parser stalls on the PTY instead of
/// letting this process buffer its output without bound. Draining the queue on
/// the consumer side unblocks the reader again.
const READER_QUEUE_CHUNKS: usize = 8;
const WRITER_QUEUE_CHUNKS: usize = 64;

fn spawn_writer(
    mut writer: Box<dyn Write + Send>,
    output_waker: Arc<OnceLock<TransportWakeHandle>>,
) -> (SyncSender<Vec<u8>>, Receiver<WriterMessage>, JoinHandle<()>) {
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(WRITER_QUEUE_CHUNKS);
    let (message_tx, message_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                if message_tx
                    .send(WriterMessage::Failed(error.to_string()))
                    .is_ok()
                {
                    wake_output(&output_waker);
                }
                break;
            }
        }
    });
    (tx, message_rx, handle)
}

fn queue_writer_input(writer_tx: &SyncSender<Vec<u8>>, bytes: &[u8]) -> TransportResult<usize> {
    match writer_tx.try_send(bytes.to_vec()) {
        Ok(()) => Ok(bytes.len()),
        Err(TrySendError::Full(_)) => Ok(0),
        Err(TrySendError::Disconnected(_)) => Err(TransportError::new("local PTY writer stopped")),
    }
}

fn spawn_child_waiter(
    mut child: Box<dyn Child + Send + Sync>,
    output_waker: Arc<OnceLock<TransportWakeHandle>>,
) -> (Receiver<ChildMessage>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let message = match child.wait() {
            Ok(status) => ChildMessage::Exited(Some(status.exit_code() as i32)),
            Err(error) => ChildMessage::Failed(error.to_string()),
        };
        if tx.send(message).is_ok() {
            wake_output(&output_waker);
        }
    });
    (rx, handle)
}

/// Recycles reader scratch buffers between the reader thread and the transport.
///
/// Allocating a fresh `vec![0; READ_BUFFER_BYTES]` per read cost a 64 KiB
/// allocation *and* a 64 KiB zeroing pass for every chunk, however small — one
/// per keystroke echo. Pooled buffers stay at full length so they never need
/// re-zeroing.
#[derive(Clone)]
struct ReadBufferPool {
    buffers: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl ReadBufferPool {
    fn new() -> Self {
        Self {
            buffers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A buffer of exactly `READ_BUFFER_BYTES` length, ready to be read into.
    fn take(&self) -> Vec<u8> {
        let pooled = self
            .buffers
            .lock()
            .ok()
            .and_then(|mut buffers| buffers.pop());
        match pooled {
            Some(mut buffer) => {
                // Pooled buffers are stored at full length, so this is a no-op
                // in the steady state rather than another zeroing pass.
                if buffer.len() != READ_BUFFER_BYTES {
                    buffer.resize(READ_BUFFER_BYTES, 0);
                }
                buffer
            }
            None => vec![0_u8; READ_BUFFER_BYTES],
        }
    }

    fn recycle(&self, buffer: Vec<u8>) {
        if buffer.len() != READ_BUFFER_BYTES {
            return;
        }
        if let Ok(mut buffers) = self.buffers.lock()
            && buffers.len() < READER_QUEUE_CHUNKS + 1
        {
            buffers.push(buffer);
        }
    }
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    output_waker: Arc<OnceLock<TransportWakeHandle>>,
    pool: ReadBufferPool,
) -> (Receiver<ReaderMessage>, JoinHandle<()>) {
    let (tx, rx) = mpsc::sync_channel(READER_QUEUE_CHUNKS);

    let handle = thread::spawn(move || {
        if tx.send(ReaderMessage::Started).is_ok() {
            wake_output(&output_waker);
        }

        loop {
            let mut buffer = pool.take();
            match reader.read(&mut buffer) {
                Ok(0) => {
                    pool.recycle(buffer);
                    if tx.send(ReaderMessage::Closed).is_ok() {
                        wake_output(&output_waker);
                    }
                    break;
                }
                Ok(count) => {
                    // The buffer keeps its full length and goes back to the
                    // pool; only the bytes actually read are handed onward, so a
                    // small chunk no longer carries a 64 KiB allocation with it
                    // through the queues.
                    if tx
                        .send(ReaderMessage::Bytes { buffer, len: count })
                        .is_err()
                    {
                        break;
                    }
                    wake_output(&output_waker);
                }
                Err(error) => {
                    if tx.send(ReaderMessage::Failed(error.to_string())).is_ok() {
                        wake_output(&output_waker);
                    }
                    break;
                }
            }
        }

        if tx.send(ReaderMessage::Stopped).is_ok() {
            wake_output(&output_waker);
        }
    });

    (rx, handle)
}

fn wake_output(output_waker: &Arc<OnceLock<TransportWakeHandle>>) {
    if let Some(waker) = output_waker.get() {
        waker.wake();
    }
}

fn to_pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows.max(1),
        cols: size.cols.max(1),
        pixel_width: size.pixel_width.min(u16::MAX.into()) as u16,
        pixel_height: size.pixel_height.min(u16::MAX.into()) as u16,
    }
}

fn platform_transport_kind() -> TransportKind {
    if cfg!(windows) {
        TransportKind::WindowsPseudoconsole
    } else {
        TransportKind::LocalPty
    }
}

fn make_session_id(process_id: Option<u32>) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();

    match process_id {
        Some(process_id) => format!("local-{process_id}-{millis}"),
        None => format!("local-{millis}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fmt::Write as _,
        io::Cursor,
        sync::{
            Condvar,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };
    use transport_core::{TransportCommand, TransportEvent, TransportEventLoop};

    fn test_size() -> TerminalSize {
        TerminalSize::new(80, 24, 640, 384)
    }

    #[test]
    fn recycled_read_buffers_are_reused_at_full_length() {
        let pool = ReadBufferPool::new();

        let pooled = || pool.buffers.lock().expect("pool lock").len();

        let first = pool.take();
        assert_eq!(first.len(), READ_BUFFER_BYTES);
        assert_eq!(pooled(), 0, "a taken buffer must leave the pool");

        pool.recycle(first);
        assert_eq!(pooled(), 1, "a returned buffer must be retained for reuse");

        // Taking it back must reuse the retained buffer rather than allocating.
        let second = pool.take();
        assert_eq!(pooled(), 0);
        assert_eq!(
            second.len(),
            READ_BUFFER_BYTES,
            "pooled buffers stay at full length so reads need no zeroing pass"
        );
        pool.recycle(second);

        // A short-changed buffer is dropped rather than poisoning the pool.
        let mut truncated = pool.take();
        truncated.truncate(8);
        pool.recycle(truncated);
        assert_eq!(pooled(), 0, "a truncated buffer must not be retained");
        assert_eq!(pool.take().len(), READ_BUFFER_BYTES);

        // The pool is bounded: a burst of returns cannot grow it without limit.
        for _ in 0..(READER_QUEUE_CHUNKS * 4) {
            pool.recycle(vec![0_u8; READ_BUFFER_BYTES]);
        }
        assert_eq!(pooled(), READER_QUEUE_CHUNKS + 1);
    }

    #[test]
    fn a_small_chunk_does_not_carry_a_full_read_buffer_downstream() {
        let output_waker = Arc::new(OnceLock::new());
        let (messages, reader) = spawn_reader(
            Box::new(Cursor::new(b"hi".to_vec())),
            output_waker,
            ReadBufferPool::new(),
        );
        reader.join().expect("reader thread");

        let payload = messages
            .try_iter()
            .find_map(|message| match message {
                ReaderMessage::Bytes { buffer, len } => Some(buffer[..len].to_vec()),
                _ => None,
            })
            .expect("output chunk");

        // The scratch buffer stays behind in the pool; only the bytes actually
        // read travel onward.
        assert_eq!(payload, b"hi");
        assert_eq!(payload.len(), 2);
    }

    #[test]
    fn reader_wakes_consumer_when_output_is_queued() {
        let wake_count = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&wake_count);
        let output_waker = Arc::new(OnceLock::new());
        output_waker
            .set(TransportWakeHandle::new(move || {
                observed.fetch_add(1, Ordering::Relaxed);
            }))
            .expect("set output waker");
        let (messages, reader) = spawn_reader(
            Box::new(Cursor::new(b"panea-output".to_vec())),
            output_waker,
            ReadBufferPool::new(),
        );

        reader.join().expect("reader thread");
        let messages = messages.try_iter().collect::<Vec<_>>();

        assert!(messages.iter().any(|message| {
            matches!(
                message,
                ReaderMessage::Bytes { buffer, len } if &buffer[..*len] == b"panea-output"
            )
        }));
        assert_eq!(wake_count.load(Ordering::Relaxed), 1);
    }

    #[derive(Debug)]
    struct ImmediateExitChild;

    impl portable_pty::ChildKiller for ImmediateExitChild {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(Self)
        }
    }

    impl portable_pty::Child for ImmediateExitChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(Some(portable_pty::ExitStatus::with_exit_code(7)))
        }

        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(7))
        }

        fn process_id(&self) -> Option<u32> {
            Some(42)
        }

        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }

    #[test]
    fn child_exit_waiter_wakes_without_pty_output() {
        let wake_count = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&wake_count);
        let output_waker = Arc::new(OnceLock::new());
        output_waker
            .set(TransportWakeHandle::new(move || {
                observed.fetch_add(1, Ordering::Relaxed);
            }))
            .expect("set output waker");

        let (messages, waiter) = spawn_child_waiter(Box::new(ImmediateExitChild), output_waker);
        waiter.join().expect("child waiter thread");

        assert!(matches!(
            messages.recv_timeout(Duration::from_millis(50)),
            Ok(ChildMessage::Exited(Some(7)))
        ));
        assert_eq!(wake_count.load(Ordering::Relaxed), 1);
    }

    struct GatedWriter {
        started: mpsc::Sender<()>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "writer failed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Write for GatedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let _ = self.started.send(());
            let (released, ready) = &*self.release;
            let guard = released.lock().expect("release lock");
            let _ = ready
                .wait_timeout_while(guard, Duration::from_millis(300), |released| !*released)
                .expect("release wait");
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writer_queue_accepts_input_without_waiting_for_the_pty_pipe() {
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let output_waker = Arc::new(OnceLock::new());
        let (writer_tx, _writer_messages, writer_thread) = spawn_writer(
            Box::new(GatedWriter {
                started: started_tx,
                release: Arc::clone(&release),
            }),
            output_waker,
        );

        let started = Instant::now();
        assert_eq!(queue_writer_input(&writer_tx, b"large paste"), Ok(11));
        assert!(started.elapsed() < Duration::from_millis(50));
        started_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("writer thread starts OS write");

        release
            .0
            .lock()
            .map(|mut released| *released = true)
            .unwrap();
        release.1.notify_all();
        drop(writer_tx);
        writer_thread.join().expect("writer thread");
    }

    #[test]
    fn writer_failure_is_reported_and_wakes_the_transport() {
        let wake_count = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&wake_count);
        let output_waker = Arc::new(OnceLock::new());
        output_waker
            .set(TransportWakeHandle::new(move || {
                observed.fetch_add(1, Ordering::Relaxed);
            }))
            .expect("set output waker");
        let (writer_tx, writer_messages, writer_thread) =
            spawn_writer(Box::new(FailingWriter), output_waker);

        assert_eq!(queue_writer_input(&writer_tx, b"input"), Ok(5));
        writer_thread.join().expect("writer thread");

        assert!(matches!(
            writer_messages.recv_timeout(Duration::from_millis(50)),
            Ok(WriterMessage::Failed(message)) if message.contains("writer failed")
        ));
        assert_eq!(wake_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn reader_failure_only_terminates_a_child_that_is_still_running() {
        assert!(!reader_failure_requires_child_termination(
            &TransportState::DrainingOutput { exit_code: Some(0) }
        ));
        assert!(reader_failure_requires_child_termination(
            &TransportState::Running
        ));
        assert!(!reader_failure_requires_child_termination(
            &TransportState::TerminatingChild
        ));
        assert!(unresolved_reader_termination_failure_is_terminal(
            &TransportState::TerminatingChild
        ));
        assert!(!unresolved_reader_termination_failure_is_terminal(
            &TransportState::DrainingOutput { exit_code: Some(0) }
        ));
    }

    #[test]
    fn default_profile_matches_platform() {
        let profile = LocalShellProfile::default_for_platform();

        assert!(!profile.name.is_empty());
        assert!(!profile.program.is_empty());

        if cfg!(windows) {
            assert_eq!(profile.kind, LocalShellKind::PowerShell);
        } else {
            assert_eq!(profile.kind, LocalShellKind::Default);
        }
    }

    #[test]
    fn windows_profile_groundwork_is_present() {
        assert_eq!(LocalShellProfile::powershell().program, "powershell.exe");
        assert_eq!(LocalShellProfile::cmd().program, "cmd.exe");

        let wsl = LocalShellProfile::wsl(Some("Ubuntu".to_owned()));
        assert_eq!(wsl.program, "wsl.exe");
        assert_eq!(wsl.args, ["--distribution", "Ubuntu"]);
    }

    #[test]
    fn startup_command_rejects_explicit_args_for_now() {
        let result = LocalShellProfile::custom("custom", "shell")
            .with_args(["--login"])
            .with_startup_command("echo panea")
            .command_builder();

        assert!(result.is_err());
    }

    #[test]
    #[ignore = "spawns a real local shell"]
    fn one_shot_shell_outputs_and_exits() {
        let mut transport =
            LocalPtyTransport::spawn(one_shot_smoke_profile(), test_size()).expect("spawn shell");

        assert_spawned(&transport);
        wait_for_output(&mut transport, b"panea-smoke", Duration::from_secs(3))
            .unwrap_or_else(|message| panic!("{message}"));
        wait_for_closed(&mut transport, Duration::from_secs(2))
            .unwrap_or_else(|message| panic!("{message}"));
        transport
            .shutdown()
            .unwrap_or_else(|error| panic!("shutdown after one-shot close failed: {error}"));
    }

    #[test]
    #[ignore = "spawns a real local shell"]
    fn real_shell_starts_writes_resizes_exits_and_restarts() {
        let mut first =
            LocalPtyTransport::spawn(smoke_profile(), test_size()).expect("spawn shell");
        assert_spawned(&first);
        first
            .write_input(shell_print_command())
            .expect("write command");
        first
            .resize(TerminalSize::new(100, 30, 800, 480))
            .expect("resize shell");

        wait_for_output(&mut first, b"panea-smoke", Duration::from_secs(3))
            .unwrap_or_else(|message| panic!("{message}"));

        first.write_input(shell_exit_command()).expect("write exit");
        wait_for_closed(&mut first, Duration::from_secs(2))
            .unwrap_or_else(|message| panic!("{message}"));

        let mut second =
            LocalPtyTransport::spawn(smoke_profile(), test_size()).expect("restart shell");
        assert_spawned(&second);
        second
            .write_input(shell_exit_command())
            .expect("write exit");
        wait_for_closed(&mut second, Duration::from_secs(2))
            .unwrap_or_else(|message| panic!("{message}"));
    }

    #[test]
    #[ignore = "spawns a real local shell"]
    fn event_loop_emits_shell_output_without_blocking() {
        let transport =
            LocalPtyTransport::spawn(smoke_profile(), test_size()).expect("spawn shell");
        let event_loop = TransportEventLoop::spawn(transport);

        event_loop
            .send_command(TransportCommand::write_input(shell_print_command()))
            .expect("send command");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw_output = false;
        while Instant::now() < deadline {
            while let Some(event) = event_loop.poll_event() {
                if let TransportEvent::Output(bytes) = event {
                    if bytes
                        .windows(b"\x1b[6n".len())
                        .any(|window| window == b"\x1b[6n")
                    {
                        event_loop
                            .send_command(TransportCommand::write_input(b"\x1b[1;1R".as_slice()))
                            .expect("answer terminal cursor query");
                    }

                    if bytes
                        .windows(b"panea-loop".len())
                        .any(|w| w == b"panea-loop")
                    {
                        saw_output = true;
                        break;
                    }
                }
            }

            if saw_output {
                break;
            }

            thread::sleep(Duration::from_millis(10));
        }

        event_loop
            .send_command(TransportCommand::Shutdown)
            .expect("send shutdown");
        assert!(saw_output);
    }

    fn assert_spawned(transport: &LocalPtyTransport) {
        let diagnostics = transport.diagnostics();
        assert!(
            !diagnostics.command.is_empty(),
            "spawned command should be recorded"
        );
        assert!(
            matches!(transport.state(), TransportState::Running),
            "transport should start running: {}",
            format_diagnostics(&diagnostics, &[], &[])
        );
    }

    fn wait_for_output(
        transport: &mut LocalPtyTransport,
        needle: &[u8],
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let mut lifecycle = Vec::new();

        while Instant::now() < deadline {
            let poll = transport.poll_output().expect("poll output");
            answer_terminal_queries(transport, &poll.bytes)?;
            lifecycle.extend(poll.lifecycle);
            output.extend(poll.bytes);

            if output.windows(needle.len()).any(|window| window == needle) {
                return Ok(());
            }

            thread::sleep(Duration::from_millis(10));
        }

        let before_shutdown = transport.diagnostics();
        let shutdown_result = transport.shutdown();
        let after_shutdown = transport.diagnostics();
        Err(format!(
            "timed out waiting for output {:?}\nBefore shutdown:\n{}\nAfter shutdown:\n{}\nShutdown result: {:?}",
            String::from_utf8_lossy(needle),
            format_diagnostics(&before_shutdown, &output, &lifecycle),
            format_diagnostics(&after_shutdown, &output, &lifecycle),
            shutdown_result.map_err(|error| error.to_string())
        ))
    }

    fn wait_for_closed(transport: &mut LocalPtyTransport, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let mut lifecycle = Vec::new();

        while Instant::now() < deadline {
            let poll = transport.poll_output().expect("poll output");
            answer_terminal_queries(transport, &poll.bytes)?;
            lifecycle.extend(poll.lifecycle);
            output.extend(poll.bytes);

            if poll.closed || matches!(transport.state(), TransportState::Closed { .. }) {
                return Ok(());
            }

            thread::sleep(Duration::from_millis(10));
        }

        let before_shutdown = transport.diagnostics();
        let shutdown_result = transport.shutdown();
        let after_shutdown = transport.diagnostics();
        Err(format!(
            "timed out waiting for transport close\nBefore shutdown:\n{}\nAfter shutdown:\n{}\nShutdown result: {:?}",
            format_diagnostics(&before_shutdown, &output, &lifecycle),
            format_diagnostics(&after_shutdown, &output, &lifecycle),
            shutdown_result.map_err(|error| error.to_string())
        ))
    }

    fn format_diagnostics(
        diagnostics: &LocalPtyDiagnostics,
        output: &[u8],
        lifecycle: &[TransportLifecycleEvent],
    ) -> String {
        let mut message = String::new();
        let _ = writeln!(message, "command: {}", diagnostics.command);
        let _ = writeln!(message, "pid: {:?}", diagnostics.process_id);
        let _ = writeln!(message, "state: {:?}", diagnostics.state);
        let _ = writeln!(message, "bytes_received: {}", diagnostics.bytes_received);
        let _ = writeln!(message, "read_events: {}", diagnostics.read_events);
        let _ = writeln!(
            message,
            "last_reader_preview: {:?}",
            String::from_utf8_lossy(&diagnostics.last_bytes_preview)
        );
        let _ = writeln!(
            message,
            "accumulated_preview: {:?}",
            String::from_utf8_lossy(output)
        );
        let _ = writeln!(message, "reader_started: {}", diagnostics.reader_started);
        let _ = writeln!(message, "reader_stopped: {}", diagnostics.reader_stopped);
        let _ = writeln!(message, "reader_error: {:?}", diagnostics.reader_error);
        let _ = writeln!(message, "child_exited: {}", diagnostics.child_exited);
        let _ = writeln!(message, "kill_attempted: {}", diagnostics.kill_attempted);
        let _ = writeln!(
            message,
            "shutdown_timed_out: {}",
            diagnostics.shutdown_timed_out
        );
        let _ = writeln!(message, "lifecycle: {lifecycle:?}");
        message
    }

    fn answer_terminal_queries(
        transport: &mut LocalPtyTransport,
        bytes: &[u8],
    ) -> Result<(), String> {
        if bytes
            .windows(b"\x1b[6n".len())
            .any(|window| window == b"\x1b[6n")
        {
            transport
                .write_input(b"\x1b[1;1R")
                .map_err(|error| format!("failed to answer terminal cursor query: {error}"))?;
        }

        Ok(())
    }

    fn shell_print_command() -> &'static [u8] {
        if cfg!(windows) {
            b"echo panea-smoke\r\necho panea-loop\r\n"
        } else {
            b"printf 'panea-smoke\\npanea-loop\\n'\n"
        }
    }

    fn shell_exit_command() -> &'static [u8] {
        if cfg!(windows) {
            b"exit\r\n"
        } else {
            b"exit\n"
        }
    }

    fn smoke_profile() -> LocalShellProfile {
        if cfg!(windows) {
            LocalShellProfile::cmd()
        } else {
            LocalShellProfile::default_for_platform()
        }
    }

    fn one_shot_smoke_profile() -> LocalShellProfile {
        if cfg!(windows) {
            LocalShellProfile::cmd().with_args(["/D", "/C", "echo panea-smoke"])
        } else {
            LocalShellProfile::custom("sh", "sh").with_args(["-lc", "printf '%s\\n' panea-smoke"])
        }
    }
}
