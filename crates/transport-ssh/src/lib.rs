//! SSH transport boundary.

pub const LAYER: &str = "session transport";

use std::{
    env,
    error::Error,
    fmt,
    io::{self, Read, Write},
    net::{Shutdown, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use security::{
    AuthMethod, EmptySecretProvider, HostKey, HostKeyDecision, HostKeyTrustAction,
    HostKeyTrustReason, HostKeyTrustRequest, HostTrustProvider, KnownHosts, KnownHostsPolicy,
    RejectingHostTrustProvider, SecretProvider, SecretRequest,
};
use ssh2::{Channel, HostKeyType, Session};
use transport_core::{
    SessionMetadata, TerminalSize, TerminalTransport, TransportError, TransportKind,
    TransportLifecycleEvent, TransportOutput, TransportResult, TransportState, TransportWakeHandle,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshConnectionProfile {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub auth_method: AuthMethod,
    pub identity_file: Option<PathBuf>,
    pub known_hosts_policy: KnownHostsPolicy,
    pub remote_command: Option<String>,
    pub remote_working_directory: Option<String>,
    pub shell_integration: bool,
    pub agent_forwarding: bool,
    pub proxy_jump: Option<String>,
    pub connect_timeout: Duration,
}

impl SshConnectionProfile {
    #[must_use]
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            host: host.into(),
            port: 22,
            username: None,
            auth_method: AuthMethod::Agent,
            identity_file: None,
            known_hosts_policy: KnownHostsPolicy::Ask,
            remote_command: None,
            remote_working_directory: None,
            shell_integration: true,
            agent_forwarding: false,
            proxy_jump: None,
            connect_timeout: Duration::from_secs(10),
        }
    }

    #[must_use]
    pub fn username_or_current_user(&self) -> Option<String> {
        self.username
            .clone()
            .or_else(|| env::var("USER").ok())
            .or_else(|| env::var("USERNAME").ok())
            .filter(|user| !user.trim().is_empty())
    }
}

/// Keepalive interval requested from the remote. Idle sessions behind NAT are
/// otherwise dropped silently.
const KEEPALIVE_INTERVAL_SECS: u32 = 30;

/// Failure text for a configuration this transport cannot honour.
pub const PROXY_JUMP_UNSUPPORTED: &str =
    "SSH proxy_jump is configured but proxy jump transport is not implemented yet";
/// Failure text for a server that refused every offered credential.
pub const AUTHENTICATION_REJECTED: &str = "SSH authentication was rejected";
/// Failure text clauses for the host-key outcomes, all of which need a human.
pub const HOST_KEY_REQUIRES_TRUST: &str = "requires explicit trust";
pub const HOST_KEY_BLOCKED: &str = "connection blocked until explicitly resolved";
pub const HOST_KEY_MISSING: &str = "SSH server did not present a host key";

/// Why a dropped session will not be retried automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshReconnectRefusal {
    /// Automatic reconnection is switched off for this session.
    Disabled,
    /// The retry budget is spent; only a manual reconnect will try again.
    AttemptsExhausted,
    /// Retrying cannot succeed: the failure needs a decision or a config change.
    Permanent,
}

/// What to do about a session that dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshReconnectDecision {
    Retry {
        /// 1 for the first retry after a drop.
        attempt: u32,
        /// How long to wait before trying.
        after: Duration,
    },
    GiveUp(SshReconnectRefusal),
}

/// When and whether to reopen a dropped SSH session.
///
/// Retrying is only ever right for a session that was working and then lost its
/// connection. A rejected credential or an unresolved host key will fail
/// identically forever, and retrying those would bury the prompt that asks the
/// user to resolve it — so those are classified permanent and never retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SshReconnectPolicy {
    pub enabled: bool,
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for SshReconnectPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: 5,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(16),
        }
    }
}

impl SshReconnectPolicy {
    #[must_use]
    pub fn with_max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts;
        self
    }

    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            max_attempts: 0,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(16),
        }
    }

    /// Decides what to do after a drop.
    ///
    /// `failed_attempts` counts retries already made since the session was last
    /// established, so it is 0 on the first drop.
    #[must_use]
    pub fn decide(&self, failed_attempts: u32, failure: &str) -> SshReconnectDecision {
        if !self.enabled {
            return SshReconnectDecision::GiveUp(SshReconnectRefusal::Disabled);
        }
        if failure_is_permanent(failure) {
            return SshReconnectDecision::GiveUp(SshReconnectRefusal::Permanent);
        }
        if failed_attempts >= self.max_attempts {
            return SshReconnectDecision::GiveUp(SshReconnectRefusal::AttemptsExhausted);
        }

        // Exponential backoff so a host that is down does not get hammered,
        // capped so a long outage still retries at a useful cadence.
        let shift = failed_attempts.min(16);
        let scaled = self
            .initial_backoff
            .saturating_mul(2_u32.saturating_pow(shift));
        SshReconnectDecision::Retry {
            attempt: failed_attempts.saturating_add(1),
            after: scaled.min(self.max_backoff),
        }
    }
}

/// Whether a failure will fail the same way however often it is retried.
#[must_use]
pub fn failure_is_permanent(failure: &str) -> bool {
    // These clauses are the same constants the failures are built from, so the
    // classifier cannot drift from the messages it classifies.
    [
        AUTHENTICATION_REJECTED,
        PROXY_JUMP_UNSUPPORTED,
        HOST_KEY_REQUIRES_TRUST,
        HOST_KEY_BLOCKED,
        HOST_KEY_MISSING,
    ]
    .iter()
    .any(|clause| failure.contains(clause))
}

fn write_nonblocking(writer: &mut (impl Write + ?Sized), bytes: &[u8]) -> io::Result<usize> {
    let mut written = 0;
    while written < bytes.len() {
        match writer.write(&bytes[written..]) {
            Ok(0) => break,
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(_) if written > 0 => break,
            Err(error) => return Err(error),
        }
    }
    Ok(written)
}

#[derive(Default)]
struct ReadinessGate {
    state: Mutex<ReadinessGateState>,
    changed: Condvar,
}

#[derive(Default)]
struct ReadinessGateState {
    pending: bool,
    stopped: bool,
}

impl ReadinessGate {
    fn mark_pending(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.pending || state.stopped {
            return false;
        }
        state.pending = true;
        true
    }

    fn wait_until_acknowledged(&self) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        let Ok(state) = self
            .changed
            .wait_while(state, |state| state.pending && !state.stopped)
        else {
            return false;
        };
        !state.stopped
    }

    fn acknowledge(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.pending = false;
            self.changed.notify_all();
        }
    }

    fn stop(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.pending = false;
            state.stopped = true;
            self.changed.notify_all();
        }
    }
}

fn spawn_socket_readiness_watcher(
    stream: TcpStream,
    output_waker: TransportWakeHandle,
    readiness_gate: Arc<ReadinessGate>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut byte = [0_u8; 1];
        loop {
            match stream.peek(&mut byte) {
                Ok(0) => {
                    if readiness_gate.mark_pending() {
                        output_waker.wake();
                    }
                    break;
                }
                Ok(_) => {
                    if readiness_gate.mark_pending() {
                        output_waker.wake();
                    }
                    if !readiness_gate.wait_until_acknowledged() {
                        break;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => {
                    output_waker.wake();
                    break;
                }
            }
        }
    })
}

pub struct SshTransport {
    session: Session,
    channel: Channel,
    metadata: SessionMetadata,
    state: TransportState,
    pending_lifecycle: Vec<TransportLifecycleEvent>,
    output_closed: bool,
    exit_code: Option<i32>,
    keepalive_sent_at: Option<Instant>,
    readiness_stream: Option<TcpStream>,
    readiness_shutdown: Option<TcpStream>,
    readiness_thread: Option<JoinHandle<()>>,
    readiness_gate: Arc<ReadinessGate>,
}

impl SshTransport {
    fn send_keepalive(&mut self) -> TransportResult<()> {
        let due = self.keepalive_sent_at.is_none_or(|sent_at| {
            sent_at.elapsed() >= Duration::from_secs(u64::from(KEEPALIVE_INTERVAL_SECS))
        });
        if due && classify_keepalive_result(self.session.keepalive_send())? {
            self.keepalive_sent_at = Some(Instant::now());
        }
        Ok(())
    }

    fn stop_readiness_watcher(&mut self) {
        self.readiness_gate.stop();
        self.readiness_stream.take();
        if let Some(stream) = self.readiness_shutdown.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if self
            .readiness_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            && let Some(readiness_thread) = self.readiness_thread.take()
        {
            let _ = readiness_thread.join();
        }
    }
}

impl SshTransport {
    pub fn connect(profile: SshConnectionProfile, size: TerminalSize) -> TransportResult<Self> {
        Self::connect_with_known_hosts(
            profile,
            size,
            &default_known_hosts_path(),
            &mut EmptySecretProvider,
        )
    }

    pub fn connect_with_known_hosts(
        profile: SshConnectionProfile,
        size: TerminalSize,
        known_hosts_path: &Path,
        secret_provider: &mut dyn SecretProvider,
    ) -> TransportResult<Self> {
        let mut trust_provider = RejectingHostTrustProvider;
        Self::connect_with_security(
            profile,
            size,
            known_hosts_path,
            secret_provider,
            &mut trust_provider,
        )
    }

    pub fn connect_with_providers(
        profile: SshConnectionProfile,
        size: TerminalSize,
        secret_provider: &mut dyn SecretProvider,
        trust_provider: &mut dyn HostTrustProvider,
    ) -> TransportResult<Self> {
        Self::connect_with_security(
            profile,
            size,
            &default_known_hosts_path(),
            secret_provider,
            trust_provider,
        )
    }

    pub fn connect_with_security(
        profile: SshConnectionProfile,
        size: TerminalSize,
        known_hosts_path: &Path,
        secret_provider: &mut dyn SecretProvider,
        trust_provider: &mut dyn HostTrustProvider,
    ) -> TransportResult<Self> {
        if profile.proxy_jump.is_some() {
            return Err(TransportError::new(PROXY_JUMP_UNSUPPORTED));
        }

        let tcp = connect_tcp(&profile)?;
        let readiness_stream = tcp
            .try_clone()
            .map_err(|error| TransportError::new(error.to_string()))?;
        let readiness_shutdown = tcp
            .try_clone()
            .map_err(|error| TransportError::new(error.to_string()))?;
        let mut session = Session::new().map_err(transport_error)?;
        session.set_tcp_stream(tcp);
        session.handshake().map_err(transport_error)?;

        verify_host_key(&session, &profile, known_hosts_path, trust_provider)?;
        authenticate(&session, &profile, secret_provider)?;

        let mut channel = session.channel_session().map_err(transport_error)?;
        if profile.agent_forwarding {
            channel
                .request_auth_agent_forwarding()
                .map_err(transport_error)?;
        }
        channel
            .request_pty(
                "xterm-256color",
                None,
                Some((
                    u32::from(size.cols),
                    u32::from(size.rows),
                    size.pixel_width,
                    size.pixel_height,
                )),
            )
            .map_err(transport_error)?;

        start_remote(&mut channel, &profile)?;
        // Ask the remote to keep the connection alive; without this an idle
        // session behind NAT or a stateful firewall dies with no notice.
        session.set_keepalive(true, KEEPALIVE_INTERVAL_SECS);
        session.set_blocking(false);

        Ok(Self {
            metadata: SessionMetadata {
                id: format!("ssh:{}@{}:{}", profile.name, profile.host, profile.port),
                kind: TransportKind::Ssh,
                title: Some(profile.name.clone()),
                shell: profile.remote_command.clone(),
                current_working_directory: profile.remote_working_directory.clone(),
                remote_host: Some(profile.host.clone()),
            },
            session,
            channel,
            state: TransportState::Running,
            pending_lifecycle: vec![TransportLifecycleEvent::Started],
            output_closed: false,
            exit_code: None,
            keepalive_sent_at: None,
            readiness_stream: Some(readiness_stream),
            readiness_shutdown: Some(readiness_shutdown),
            readiness_thread: None,
            readiness_gate: Arc::new(ReadinessGate::default()),
        })
    }

    fn close_channel_bounded(&mut self, timeout: Duration) {
        self.state = TransportState::ClosingInput;
        let _ = self.channel.send_eof();

        self.state = TransportState::TerminatingChild;
        let deadline = Instant::now() + timeout;
        loop {
            match self.channel.close() {
                Ok(()) => break,
                Err(error) if is_would_block(&error) && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }

        self.exit_code = self.channel.exit_status().ok();
        self.state = TransportState::Closed {
            exit_code: self.exit_code,
        };
        self.output_closed = true;
        self.pending_lifecycle.push(TransportLifecycleEvent::Closed);
        let _ = self.session.disconnect(None, "session closed", None);
        self.stop_readiness_watcher();
    }
}

impl TerminalTransport for SshTransport {
    fn set_output_waker(&mut self, waker: Option<TransportWakeHandle>) {
        if self.readiness_thread.is_none()
            && let Some(waker) = waker
            && let Some(stream) = self.readiness_stream.take()
        {
            self.readiness_thread = Some(spawn_socket_readiness_watcher(
                stream,
                waker,
                Arc::clone(&self.readiness_gate),
            ));
        }
    }

    fn periodic_poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(u64::from(KEEPALIVE_INTERVAL_SECS)))
    }

    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<usize> {
        if self.output_closed {
            return Err(TransportError::new("SSH session is closed"));
        }

        // Report exactly what the channel took. This previously wrote part of
        // the buffer and then returned an error, so the caller discarded the
        // remainder and large pastes arrived truncated.
        let written = write_nonblocking(&mut self.channel, bytes)
            .map_err(|error| TransportError::new(error.to_string()))?;

        if written > 0 {
            let _ = self.channel.flush();
        }
        Ok(written)
    }

    fn resize(&mut self, size: TerminalSize) -> TransportResult<()> {
        self.channel
            .request_pty_size(
                u32::from(size.cols),
                u32::from(size.rows),
                Some(size.pixel_width),
                Some(size.pixel_height),
            )
            .map_err(transport_error)?;
        self.pending_lifecycle
            .push(TransportLifecycleEvent::Resized(size));
        Ok(())
    }

    fn poll_output(&mut self) -> TransportResult<TransportOutput> {
        if !self.output_closed {
            self.send_keepalive()?;
        }

        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 8192];

        loop {
            match self.channel.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    bytes.extend_from_slice(&buffer[..count]);
                    if bytes.len() >= 64 * 1024 {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(TransportError::new(error.to_string())),
            }
        }

        let mut lifecycle = std::mem::take(&mut self.pending_lifecycle);
        if !bytes.is_empty() {
            lifecycle.push(TransportLifecycleEvent::OutputReady);
        }

        if self.channel.eof() && !self.output_closed {
            self.exit_code = self.channel.exit_status().ok();
            self.state = TransportState::Closed {
                exit_code: self.exit_code,
            };
            lifecycle.push(TransportLifecycleEvent::Exited {
                exit_code: self.exit_code,
            });
            lifecycle.push(TransportLifecycleEvent::Closed);
            self.output_closed = true;
            self.stop_readiness_watcher();
        }

        self.readiness_gate.acknowledge();

        Ok(TransportOutput {
            bytes,
            closed: self.output_closed,
            lifecycle,
        })
    }

    fn shutdown(&mut self) -> TransportResult<()> {
        if !self.output_closed {
            self.pending_lifecycle
                .push(TransportLifecycleEvent::ShutdownRequested);
            self.close_channel_bounded(Duration::from_millis(500));
        }
        Ok(())
    }

    fn session_metadata(&self) -> SessionMetadata {
        self.metadata.clone()
    }

    fn state(&self) -> TransportState {
        self.state.clone()
    }
}

impl Drop for SshTransport {
    fn drop(&mut self) {
        self.stop_readiness_watcher();
        if !self.output_closed {
            self.close_channel_bounded(Duration::from_millis(25));
        }
    }
}

fn connect_tcp(profile: &SshConnectionProfile) -> TransportResult<TcpStream> {
    let address = format!("{}:{}", profile.host, profile.port);
    let mut last_error = None;
    for socket_addr in address.to_socket_addrs().map_err(|error| {
        TransportError::new(format!(
            "failed to resolve SSH host '{}': {error}",
            profile.host
        ))
    })? {
        match TcpStream::connect_timeout(&socket_addr, profile.connect_timeout) {
            Ok(stream) => {
                stream
                    .set_nodelay(true)
                    .map_err(|error| TransportError::new(error.to_string()))?;
                stream
                    .set_read_timeout(Some(profile.connect_timeout))
                    .map_err(|error| TransportError::new(error.to_string()))?;
                stream
                    .set_write_timeout(Some(profile.connect_timeout))
                    .map_err(|error| TransportError::new(error.to_string()))?;
                return Ok(stream);
            }
            Err(error) => last_error = Some(error),
        }
    }

    Err(TransportError::new(format!(
        "failed to connect to SSH host '{}:{}': {}",
        profile.host,
        profile.port,
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no socket addresses".to_owned())
    )))
}

fn verify_host_key(
    session: &Session,
    profile: &SshConnectionProfile,
    known_hosts_path: &Path,
    trust_provider: &mut dyn HostTrustProvider,
) -> TransportResult<()> {
    let (bytes, algorithm) = session
        .host_key()
        .ok_or_else(|| TransportError::new(HOST_KEY_MISSING))?;
    let key = HostKey::from_raw(
        &profile.host,
        profile.port,
        host_key_algorithm(algorithm),
        bytes,
    );
    let mut known_hosts = KnownHosts::load(known_hosts_path)
        .map_err(|error| TransportError::new(error.to_string()))?;

    match known_hosts.verify(&key, &profile.known_hosts_policy) {
        HostKeyDecision::Trusted => Ok(()),
        HostKeyDecision::TrustAndStore => {
            known_hosts.trust(&key);
            known_hosts
                .save(known_hosts_path)
                .map_err(|error| TransportError::new(error.to_string()))
        }
        HostKeyDecision::Unknown { expected_decision }
            if matches!(profile.known_hosts_policy, KnownHostsPolicy::Ask) =>
        {
            match trust_provider
                .decide_host_trust(HostKeyTrustRequest::unknown(key.clone(), expected_decision))
                .map_err(|error| TransportError::new(error.to_string()))?
            {
                HostKeyTrustAction::TrustOnce => Ok(()),
                HostKeyTrustAction::TrustAndStore => {
                    known_hosts.trust(&key);
                    known_hosts
                        .save(known_hosts_path)
                        .map_err(|error| TransportError::new(error.to_string()))
                }
                HostKeyTrustAction::Reject | HostKeyTrustAction::ReplaceStoredKey => {
                    Err(unknown_host_error(profile, &key))
                }
            }
        }
        HostKeyDecision::Unknown { .. } => Err(unknown_host_error(profile, &key)),
        HostKeyDecision::Mismatch { expected, actual }
            if !matches!(
                profile.known_hosts_policy,
                KnownHostsPolicy::PinFingerprint { .. }
            ) =>
        {
            let request = HostKeyTrustRequest::changed(key.clone(), expected.clone());
            match trust_provider
                .decide_host_trust(request)
                .map_err(|error| TransportError::new(error.to_string()))?
            {
                HostKeyTrustAction::ReplaceStoredKey => {
                    known_hosts.trust(&key);
                    known_hosts
                        .save(known_hosts_path)
                        .map_err(|error| TransportError::new(error.to_string()))
                }
                HostKeyTrustAction::Reject
                | HostKeyTrustAction::TrustOnce
                | HostKeyTrustAction::TrustAndStore => Err(changed_host_error(
                    profile,
                    &expected,
                    &actual,
                    HostKeyTrustReason::ChangedHostKey,
                )),
            }
        }
        HostKeyDecision::Mismatch { expected, actual } => {
            let _request = HostKeyTrustRequest::pinned_mismatch(key, expected.clone());
            Err(changed_host_error(
                profile,
                &expected,
                &actual,
                HostKeyTrustReason::PinnedFingerprintMismatch,
            ))
        }
    }
}

fn unknown_host_error(profile: &SshConnectionProfile, key: &HostKey) -> TransportError {
    TransportError::new(format!(
        "SSH host key for {}:{} is unknown and {HOST_KEY_REQUIRES_TRUST}: {} {}",
        profile.host, profile.port, key.algorithm, key.sha256_fingerprint
    ))
}

fn changed_host_error(
    profile: &SshConnectionProfile,
    expected: &str,
    actual: &str,
    reason: HostKeyTrustReason,
) -> TransportError {
    let detail = match reason {
        HostKeyTrustReason::UnknownHost => "unknown host key",
        HostKeyTrustReason::ChangedHostKey => "changed host key",
        HostKeyTrustReason::PinnedFingerprintMismatch => "pinned fingerprint mismatch",
    };
    TransportError::new(format!(
        "SSH {detail} for {}:{}: expected {expected}, got {actual}; {HOST_KEY_BLOCKED}",
        profile.host, profile.port
    ))
}

fn authenticate(
    session: &Session,
    profile: &SshConnectionProfile,
    secret_provider: &mut dyn SecretProvider,
) -> TransportResult<()> {
    let username = profile
        .username_or_current_user()
        .ok_or_else(|| TransportError::new("SSH username is required"))?;

    match profile.auth_method {
        AuthMethod::Agent => session
            .userauth_agent(&username)
            .map_err(|error| TransportError::new(format!("SSH agent auth failed: {error}")))?,
        AuthMethod::PublicKey => {
            let identity_file = profile
                .identity_file
                .as_deref()
                .ok_or_else(|| TransportError::new("SSH public key auth requires identity_file"))?;
            let passphrase = secret_provider
                .request_secret(SecretRequest::SshKeyPassphrase {
                    profile: profile.name.clone(),
                    host: profile.host.clone(),
                    identity_file: identity_file.to_owned(),
                })
                .map_err(|error| TransportError::new(error.to_string()))?;
            session
                .userauth_pubkey_file(
                    &username,
                    None,
                    identity_file,
                    passphrase.as_ref().map(|secret| secret.expose()),
                )
                .map_err(|error| {
                    TransportError::new(format!("SSH public key auth failed: {error}"))
                })?;
        }
        AuthMethod::Password | AuthMethod::KeyboardInteractive => {
            let secret = secret_provider
                .request_secret(SecretRequest::SshPassword {
                    profile: profile.name.clone(),
                    host: profile.host.clone(),
                    username: username.clone(),
                })
                .map_err(|error| TransportError::new(error.to_string()))?
                .ok_or_else(|| TransportError::new("SSH password was required but unavailable"))?;
            session
                .userauth_password(&username, secret.expose())
                .map_err(|error| {
                    TransportError::new(format!("SSH password auth failed: {error}"))
                })?;
        }
        AuthMethod::None => {
            return Err(TransportError::new(
                "SSH none authentication is not supported by this backend",
            ));
        }
    }

    if !session.authenticated() {
        return Err(TransportError::new(AUTHENTICATION_REJECTED));
    }

    Ok(())
}

fn start_remote(channel: &mut Channel, profile: &SshConnectionProfile) -> TransportResult<()> {
    if let Some(command) = remote_command_line(profile) {
        channel.exec(&command).map_err(transport_error)
    } else {
        channel.shell().map_err(transport_error)
    }
}

fn remote_command_line(profile: &SshConnectionProfile) -> Option<String> {
    match (&profile.remote_working_directory, &profile.remote_command) {
        (Some(cwd), Some(command)) => Some(format!("cd {} && exec {}", shell_quote(cwd), command)),
        (Some(cwd), None) => Some(format!("cd {} && exec ${{SHELL:-sh}}", shell_quote(cwd))),
        (None, Some(command)) => Some(command.clone()),
        (None, None) => None,
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn host_key_algorithm(kind: HostKeyType) -> &'static str {
    match kind {
        HostKeyType::Rsa => "ssh-rsa",
        HostKeyType::Dss => "ssh-dss",
        HostKeyType::Ecdsa256 => "ecdsa-sha2-nistp256",
        HostKeyType::Ecdsa384 => "ecdsa-sha2-nistp384",
        HostKeyType::Ecdsa521 => "ecdsa-sha2-nistp521",
        HostKeyType::Ed25519 => "ssh-ed25519",
        HostKeyType::Unknown => "unknown",
    }
}

fn default_known_hosts_path() -> PathBuf {
    if let Ok(path) = env::var("PANEA_KNOWN_HOSTS") {
        return PathBuf::from(path);
    }

    let base = env::var("APPDATA")
        .or_else(|_| env::var("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|_| env::var("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|_| PathBuf::from("."));

    base.join("panea").join("known-hosts.json")
}

fn is_would_block(error: &ssh2::Error) -> bool {
    matches!(error.code(), ssh2::ErrorCode::Session(-37))
}

fn classify_keepalive_result(result: Result<u32, ssh2::Error>) -> TransportResult<bool> {
    match result {
        Ok(_) => Ok(true),
        Err(error) if is_would_block(&error) => Ok(false),
        Err(error) => Err(transport_error(error)),
    }
}

fn transport_error(error: impl fmt::Display) -> TransportError {
    TransportError::new(error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshProfileBuildError {
    message: String,
}

impl SshProfileBuildError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SshProfileBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for SshProfileBuildError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::{Shutdown, TcpListener},
        sync::mpsc,
    };

    struct PartialThenBlockedWriter {
        accepted: Vec<u8>,
        blocked: bool,
    }

    impl Write for PartialThenBlockedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.blocked {
                self.blocked = false;
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            let count = bytes.len().min(3);
            self.accepted.extend_from_slice(&bytes[..count]);
            self.blocked = true;
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn nonblocking_write_reports_the_prefix_accepted_before_backpressure() {
        let mut writer = PartialThenBlockedWriter {
            accepted: Vec::new(),
            blocked: false,
        };

        let written = write_nonblocking(&mut writer, b"abcdef").expect("partial write");

        assert_eq!(written, 3);
        assert_eq!(writer.accepted, b"abc");
    }

    #[test]
    fn keepalive_result_only_suppresses_nonblocking_retry() {
        assert!(classify_keepalive_result(Ok(30)).expect("sent"));
        assert!(
            !classify_keepalive_result(Err(ssh2::Error::new(
                ssh2::ErrorCode::Session(-37),
                "would block",
            )))
            .expect("retryable")
        );
        assert!(
            classify_keepalive_result(Err(ssh2::Error::new(
                ssh2::ErrorCode::Session(-1),
                "connection failed",
            )))
            .is_err()
        );
    }

    #[test]
    fn socket_readiness_wakes_the_transport_worker() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let client = TcpStream::connect(listener.local_addr().expect("listener address"))
            .expect("connect loopback client");
        let mut server = listener.accept().expect("accept loopback client").0;
        let shutdown = client.try_clone().expect("clone client socket");
        let (wake_tx, wake_rx) = mpsc::channel();
        let readiness_gate = Arc::new(ReadinessGate::default());
        let watcher = spawn_socket_readiness_watcher(
            client,
            transport_core::TransportWakeHandle::new(move || {
                let _ = wake_tx.send(());
            }),
            Arc::clone(&readiness_gate),
        );

        server.write_all(b"x").expect("write readiness byte");
        wake_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("socket readability should wake transport");
        assert!(
            matches!(
                wake_rx.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "one readable socket must not produce repeated unacknowledged wakes"
        );

        readiness_gate.stop();
        shutdown.shutdown(Shutdown::Both).expect("stop watcher");
        watcher.join().expect("readiness watcher");
    }

    #[test]
    fn socket_readiness_is_coalesced_until_the_worker_acknowledges_it() {
        let readiness = ReadinessGate::default();

        assert!(readiness.mark_pending());
        assert!(!readiness.mark_pending());
        readiness.acknowledge();
        assert!(readiness.mark_pending());
    }

    #[test]
    fn a_dropped_session_retries_with_widening_backoff() {
        let policy = SshReconnectPolicy::default();
        let transient = "SSH channel read failed: connection reset";

        let mut waits = Vec::new();
        for attempt in 0..policy.max_attempts {
            match policy.decide(attempt, transient) {
                SshReconnectDecision::Retry { attempt: n, after } => {
                    assert_eq!(n, attempt + 1);
                    waits.push(after);
                }
                other => panic!("attempt {attempt} must retry, got {other:?}"),
            }
        }

        // Widening, and capped so a long outage still retries usefully.
        assert_eq!(waits[0], Duration::from_secs(1));
        assert_eq!(waits[1], Duration::from_secs(2));
        assert_eq!(waits[2], Duration::from_secs(4));
        assert!(waits.windows(2).all(|pair| pair[1] >= pair[0]));
        assert!(waits.iter().all(|wait| *wait <= policy.max_backoff));

        // The budget is finite: past it only a manual reconnect tries again.
        assert_eq!(
            policy.decide(policy.max_attempts, transient),
            SshReconnectDecision::GiveUp(SshReconnectRefusal::AttemptsExhausted)
        );
    }

    #[test]
    fn failures_that_need_a_human_are_never_retried() {
        let policy = SshReconnectPolicy::default();

        // Each of these fails identically however often it is retried, and
        // retrying would bury the prompt or error that resolves it.
        for failure in [
            AUTHENTICATION_REJECTED,
            PROXY_JUMP_UNSUPPORTED,
            "SSH host key for example.test:22 is unknown and requires explicit trust: ssh-ed25519 abc",
            "SSH changed host key for example.test:22: expected a, got b; connection blocked until explicitly resolved",
        ] {
            assert!(
                failure_is_permanent(failure),
                "{failure:?} must be classified permanent"
            );
            assert_eq!(
                policy.decide(0, failure),
                SshReconnectDecision::GiveUp(SshReconnectRefusal::Permanent),
                "{failure:?} must not be retried"
            );
        }

        // A lost connection is exactly what retrying is for.
        assert!(!failure_is_permanent("SSH session is closed"));
        assert!(!failure_is_permanent(
            "failed to write to SSH channel: broken pipe"
        ));
    }

    #[test]
    fn reconnection_can_be_switched_off() {
        assert_eq!(
            SshReconnectPolicy::disabled().decide(0, "connection reset"),
            SshReconnectDecision::GiveUp(SshReconnectRefusal::Disabled)
        );
        assert_eq!(
            SshReconnectPolicy::default()
                .with_max_attempts(0)
                .decide(0, "connection reset"),
            SshReconnectDecision::GiveUp(SshReconnectRefusal::AttemptsExhausted)
        );
    }

    #[test]
    fn profile_defaults_require_explicit_host_key_decision() {
        let profile = SshConnectionProfile::new("prod", "example.com");

        assert_eq!(profile.known_hosts_policy, KnownHostsPolicy::Ask);
        assert_eq!(profile.auth_method, AuthMethod::Agent);
    }

    #[test]
    fn quoted_remote_working_directory_is_shell_safe() {
        assert_eq!(shell_quote("/tmp/it's here"), "'/tmp/it'\\''s here'");
    }

    #[test]
    fn remote_command_is_not_forced_through_posix_exec_without_cwd() {
        let mut profile = SshConnectionProfile::new("prod", "example.com");
        profile.remote_command = Some("echo panea".to_owned());

        assert_eq!(remote_command_line(&profile).as_deref(), Some("echo panea"));
    }

    #[test]
    fn transport_ssh_does_not_import_high_layers() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("render-")
                && !manifest.contains("platform-")
                && !manifest.contains("semantics")
                && !manifest.contains("mux")
                && !manifest.contains("config-"),
            "transport-ssh must stay behind the transport/security contract"
        );
    }
}
