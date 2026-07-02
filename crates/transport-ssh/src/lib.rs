//! SSH transport boundary.

pub const LAYER: &str = "session transport";

use std::{
    env,
    error::Error,
    fmt,
    io::{self, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use security::{
    AuthMethod, EmptySecretProvider, HostKey, HostKeyDecision, KnownHosts, KnownHostsPolicy,
    SecretProvider, SecretRequest,
};
use ssh2::{Channel, HostKeyType, Session};
use transport_core::{
    SessionMetadata, TerminalSize, TerminalTransport, TransportError, TransportKind,
    TransportLifecycleEvent, TransportOutput, TransportResult, TransportState,
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

pub struct SshTransport {
    session: Session,
    channel: Channel,
    metadata: SessionMetadata,
    state: TransportState,
    pending_lifecycle: Vec<TransportLifecycleEvent>,
    output_closed: bool,
    exit_code: Option<i32>,
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
        if profile.proxy_jump.is_some() {
            return Err(TransportError::new(
                "SSH proxy_jump is configured but proxy jump transport is not implemented yet",
            ));
        }

        let tcp = connect_tcp(&profile)?;
        let mut session = Session::new().map_err(transport_error)?;
        session.set_tcp_stream(tcp);
        session.handshake().map_err(transport_error)?;

        verify_host_key(&session, &profile, known_hosts_path)?;
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
    }
}

impl TerminalTransport for SshTransport {
    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<()> {
        if self.output_closed {
            return Err(TransportError::new("SSH session is closed"));
        }

        let mut written = 0;
        while written < bytes.len() {
            match self.channel.write(&bytes[written..]) {
                Ok(0) => return Err(TransportError::new("SSH channel accepted zero bytes")),
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Err(TransportError::new("SSH channel is applying backpressure"));
                }
                Err(error) => return Err(TransportError::new(error.to_string())),
            }
        }
        Ok(())
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
        }

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
) -> TransportResult<()> {
    let (bytes, algorithm) = session
        .host_key()
        .ok_or_else(|| TransportError::new("SSH server did not present a host key"))?;
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
        HostKeyDecision::Unknown { expected_decision } => Err(TransportError::new(format!(
            "SSH host key for {}:{} is unknown: {expected_decision}",
            profile.host, profile.port
        ))),
        HostKeyDecision::Mismatch { expected, actual } => Err(TransportError::new(format!(
            "SSH host key mismatch for {}:{}: expected {expected}, got {actual}",
            profile.host, profile.port
        ))),
    }
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
        return Err(TransportError::new("SSH authentication was rejected"));
    }

    Ok(())
}

fn start_remote(channel: &mut Channel, profile: &SshConnectionProfile) -> TransportResult<()> {
    let command = match (&profile.remote_working_directory, &profile.remote_command) {
        (Some(cwd), Some(command)) => Some(format!("cd {} && exec {}", shell_quote(cwd), command)),
        (Some(cwd), None) => Some(format!("cd {} && exec ${{SHELL:-sh}}", shell_quote(cwd))),
        (None, Some(command)) => Some(format!("exec {command}")),
        (None, None) => None,
    };

    if let Some(command) = command {
        channel.exec(&command).map_err(transport_error)
    } else {
        channel.shell().map_err(transport_error)
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
