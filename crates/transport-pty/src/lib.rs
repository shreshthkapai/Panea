//! Local PTY and Windows pseudoconsole transport boundary.

pub const LAYER: &str = "session transport";

use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use transport_core::{
    SessionMetadata, TerminalSize, TerminalTransport, TransportError, TransportKind,
    TransportLifecycleEvent, TransportOutput, TransportResult, TransportState,
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
                args: Vec::new(),
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
    writer: Option<Box<dyn Write + Send>>,
    child: Box<dyn Child + Send + Sync>,
    process_id: Option<u32>,
    reader_rx: Receiver<ReaderMessage>,
    reader_thread: Option<JoinHandle<()>>,
    diagnostics: LocalPtyDiagnostics,
    metadata: SessionMetadata,
    state: TransportState,
    pending_lifecycle: VecDeque<TransportLifecycleEvent>,
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
        self.last_bytes_preview.extend_from_slice(chunk);
        if self.last_bytes_preview.len() > PREVIEW_LIMIT {
            let drain_count = self.last_bytes_preview.len() - PREVIEW_LIMIT;
            self.last_bytes_preview.drain(..drain_count);
        }
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
        let (reader_rx, reader_thread) = spawn_reader(reader);

        let mut pending_lifecycle = VecDeque::new();
        pending_lifecycle.push_back(TransportLifecycleEvent::Started);

        Ok(Self {
            master: Some(pair.master),
            writer: Some(writer),
            child,
            process_id,
            reader_rx,
            reader_thread: Some(reader_thread),
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
            self.writer.take();
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

    fn wait_for_child_exit(&mut self, timeout: Duration) -> TransportResult<Option<i32>> {
        if let TransportState::Closed { exit_code } | TransportState::DrainingOutput { exit_code } =
            self.state
        {
            return Ok(exit_code);
        }

        let deadline = Instant::now() + timeout;

        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    let exit_code = Some(status.exit_code() as i32);
                    self.mark_child_exited(exit_code);
                    return Ok(exit_code);
                }
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => return Ok(None),
                Err(error) => {
                    self.mark_failed(error.to_string());
                    return Err(TransportError::new(format!(
                        "failed to wait for shell termination: {error}"
                    )));
                }
            }
        }
    }

    fn request_child_termination(&mut self) -> TransportResult<()> {
        self.state = TransportState::TerminatingChild;
        self.diagnostics.state = self.state.clone();
        self.diagnostics.kill_attempted = true;
        self.child
            .kill()
            .map_err(|error| TransportError::new(format!("failed to terminate shell: {error}")))
    }

    fn close_master_without_blocking(&mut self) {
        if let Some(master) = self.master.take() {
            thread::spawn(move || drop(master));
        }
    }

    fn join_reader_if_finished(&mut self) {
        if self
            .reader_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(reader_thread) = self.reader_thread.take()
        {
            let _ = reader_thread.join();
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
                ReaderMessage::Bytes(mut chunk) => {
                    self.diagnostics.record_bytes(&chunk);
                    bytes.append(&mut chunk);
                }
                ReaderMessage::Closed => {
                    self.diagnostics.reader_stopped = true;
                }
                ReaderMessage::Failed(message) => {
                    self.diagnostics.reader_error = Some(message.clone());
                    self.mark_failed(message);
                    closed = true;
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
    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<()> {
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

        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| TransportError::new("local PTY writer is closed"))?;
        writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .map_err(|error| TransportError::new(format!("failed to write to local PTY: {error}")))
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
        let mut output = self.drain_reader_messages();

        if matches!(
            self.state,
            TransportState::Running
                | TransportState::ClosingInput
                | TransportState::TerminatingChild
        ) {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    let exit_code = Some(status.exit_code() as i32);
                    self.mark_child_exited(exit_code);
                }
                Ok(None) => {}
                Err(error) => {
                    self.mark_failed(error.to_string());
                }
            }
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

        self.join_reader_if_finished();
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
        self.writer.take();

        if self
            .wait_for_child_exit(Duration::from_millis(250))?
            .is_none()
        {
            self.request_child_termination()?;

            if self
                .wait_for_child_exit(Duration::from_millis(1000))?
                .is_none()
            {
                self.diagnostics.shutdown_timed_out = true;
                self.mark_failed(format!(
                    "timed out terminating local shell process {:?}",
                    self.process_id
                ));
                self.close_master_without_blocking();
                self.join_reader_if_finished();
                self.diagnostics.state = self.state.clone();
                return Err(TransportError::new(format!(
                    "timed out terminating local shell process {:?}",
                    self.process_id
                )));
            }
        }

        self.close_master_without_blocking();
        let _ = self.drain_reader_messages();
        self.join_reader_if_finished();

        let exit_code = match self.state {
            TransportState::DrainingOutput { exit_code } | TransportState::Closed { exit_code } => {
                exit_code
            }
            _ => None,
        };
        self.mark_closed(exit_code);
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
        ) {
            self.writer.take();
            self.diagnostics.kill_attempted = true;
            let _ = self.child.kill();
            if let Ok(Some(status)) = self.child.try_wait() {
                self.mark_child_exited(Some(status.exit_code() as i32));
            }
            self.close_master_without_blocking();
            self.join_reader_if_finished();
        }
    }
}

enum ReaderMessage {
    Started,
    Bytes(Vec<u8>),
    Closed,
    Failed(String),
    Stopped,
}

fn spawn_reader(mut reader: Box<dyn Read + Send>) -> (Receiver<ReaderMessage>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        let _ = tx.send(ReaderMessage::Started);

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = tx.send(ReaderMessage::Closed);
                    break;
                }
                Ok(count) => {
                    if tx
                        .send(ReaderMessage::Bytes(buffer[..count].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(ReaderMessage::Failed(error.to_string()));
                    break;
                }
            }
        }

        let _ = tx.send(ReaderMessage::Stopped);
    });

    (rx, handle)
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
        thread,
        time::{Duration, Instant},
    };
    use transport_core::{TransportCommand, TransportEvent, TransportEventLoop};

    fn test_size() -> TerminalSize {
        TerminalSize::new(80, 24, 640, 384)
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
        let event_loop = TransportEventLoop::spawn_with_queue_bound(transport, 16);

        event_loop
            .send_command(TransportCommand::WriteInput(shell_print_command().to_vec()))
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
                            .send_command(TransportCommand::WriteInput(b"\x1b[1;1R".to_vec()))
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
