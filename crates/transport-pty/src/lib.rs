//! Local PTY and Windows pseudoconsole transport boundary.

pub const LAYER: &str = "session transport";

use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
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

        for (key, value) in &self.env {
            command.env(key, value);
        }

        if let Some(directory) = &self.working_directory {
            command.cwd(directory);
        }

        Ok(command)
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
    reader_rx: Receiver<ReaderMessage>,
    reader_thread: Option<JoinHandle<()>>,
    metadata: SessionMetadata,
    state: TransportState,
    pending_lifecycle: VecDeque<TransportLifecycleEvent>,
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
            reader_rx,
            reader_thread: Some(reader_thread),
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

    fn mark_exited(&mut self, exit_code: Option<i32>) {
        if !matches!(self.state, TransportState::Exited { .. }) {
            self.state = TransportState::Exited { exit_code };
            self.pending_lifecycle
                .push_back(TransportLifecycleEvent::Exited { exit_code });
            self.pending_lifecycle
                .push_back(TransportLifecycleEvent::Closed);
        }
    }
}

impl TerminalTransport for LocalPtyTransport {
    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<()> {
        if matches!(
            self.state,
            TransportState::Exited { .. } | TransportState::Failed { .. }
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
                ReaderMessage::Bytes(mut chunk) => {
                    bytes.append(&mut chunk);
                }
                ReaderMessage::Closed => {
                    closed = true;
                }
                ReaderMessage::Failed(message) => {
                    self.state = TransportState::Failed {
                        message: message.clone(),
                    };
                    closed = true;
                    return Ok(TransportOutput {
                        bytes,
                        closed,
                        lifecycle: {
                            lifecycle.push(TransportLifecycleEvent::Closed);
                            lifecycle
                        },
                    });
                }
            }
        }

        if !bytes.is_empty() {
            lifecycle.push(TransportLifecycleEvent::OutputReady);
        }

        if matches!(
            self.state,
            TransportState::Running | TransportState::Exiting
        ) {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    let exit_code = Some(status.exit_code() as i32);
                    self.mark_exited(exit_code);
                }
                Ok(None) => {}
                Err(error) => {
                    self.state = TransportState::Failed {
                        message: error.to_string(),
                    };
                    lifecycle.push(TransportLifecycleEvent::Closed);
                    closed = true;
                }
            }
        }

        while let Some(event) = self.pending_lifecycle.pop_front() {
            if matches!(event, TransportLifecycleEvent::Closed) {
                closed = true;
            }
            lifecycle.push(event);
        }

        Ok(TransportOutput {
            bytes,
            closed,
            lifecycle,
        })
    }

    fn shutdown(&mut self) -> TransportResult<()> {
        if matches!(self.state, TransportState::Exited { .. }) {
            return Ok(());
        }

        self.state = TransportState::Exiting;
        self.pending_lifecycle
            .push_back(TransportLifecycleEvent::ShutdownRequested);
        self.writer.take();
        self.master.take();
        self.child
            .kill()
            .map_err(|error| TransportError::new(format!("failed to shut down shell: {error}")))?;
        self.join_reader();
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
            TransportState::Running | TransportState::Exiting
        ) {
            self.writer.take();
            self.master.take();
            let _ = self.child.kill();
            self.join_reader();
        }
    }
}

enum ReaderMessage {
    Bytes(Vec<u8>),
    Closed,
    Failed(String),
}

impl LocalPtyTransport {
    fn join_reader(&mut self) {
        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
        }
    }
}

fn spawn_reader(mut reader: Box<dyn Read + Send>) -> (Receiver<ReaderMessage>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let mut buffer = [0_u8; 8192];

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
    use std::{thread, time::Duration};
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
    fn real_shell_starts_writes_resizes_exits_and_restarts() {
        let mut first =
            LocalPtyTransport::spawn(smoke_profile(), test_size()).expect("spawn shell");
        first
            .write_input(shell_print_command())
            .expect("write command");
        first
            .resize(TerminalSize::new(100, 30, 800, 480))
            .expect("resize shell");

        let output = wait_for_output(&mut first, b"panea-smoke");
        assert!(output);

        first.write_input(shell_exit_command()).expect("write exit");
        assert!(wait_for_exit(&mut first));

        let mut second =
            LocalPtyTransport::spawn(smoke_profile(), test_size()).expect("restart shell");
        second
            .write_input(shell_exit_command())
            .expect("write exit");
        assert!(wait_for_exit(&mut second));
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

        let mut saw_output = false;
        for _ in 0..200 {
            while let Some(event) = event_loop.poll_event() {
                if let TransportEvent::Output(bytes) = event
                    && bytes
                        .windows(b"panea-loop".len())
                        .any(|w| w == b"panea-loop")
                {
                    saw_output = true;
                    break;
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

    fn wait_for_output(transport: &mut LocalPtyTransport, needle: &[u8]) -> bool {
        let mut output = Vec::new();

        for _ in 0..200 {
            let poll = transport.poll_output().expect("poll output");
            output.extend(poll.bytes);

            if output.windows(needle.len()).any(|window| window == needle) {
                return true;
            }

            thread::sleep(Duration::from_millis(10));
        }

        false
    }

    fn wait_for_exit(transport: &mut LocalPtyTransport) -> bool {
        for _ in 0..200 {
            let poll = transport.poll_output().expect("poll output");

            if poll.closed || matches!(transport.state(), TransportState::Exited { .. }) {
                return true;
            }

            thread::sleep(Duration::from_millis(10));
        }

        false
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
}
