// Session transport creation, lifecycle, shell activation, and PTY/SSH adaptation.

fn spawn_session_transport(
    config: &AppConfig,
    spec: &SessionSpec,
    size: TransportSize,
    output_waker: &TransportWakeHandle,
) -> transport_core::TransportResult<InitialTransport> {
    match spec.transport {
        SessionTransportKind::LocalPty | SessionTransportKind::WindowsPseudoconsole => {
            if spec.profile_name != "default"
                && !config
                    .shell_profiles
                    .iter()
                    .any(|profile| profile.name == spec.profile_name)
            {
                return Err(transport_core::TransportError::new(format!(
                    "local shell profile '{}' does not exist",
                    spec.profile_name
                )));
            }
            let (mut profile, activation) =
                initial_local_shell_profile(config, Some(&spec.profile_name));
            if let Some(directory) = &spec.working_directory {
                profile.working_directory = Some(PathBuf::from(directory));
            }
            let transport = LocalPtyTransport::spawn(profile, size)?;
            Ok(InitialTransport {
                transport: PaneTransport::Local(PaneTransportLoop::new(
                    transport,
                    output_waker.clone(),
                )),
                semantic_mode: semantic_mode_for_activation(&activation),
                parse_semantic_events: activation.parses_escape_sequences(),
                activation_diagnostics: activation.diagnostics,
                remote_metadata: None,
            })
        }
        SessionTransportKind::Ssh => {
            let profile = config
                .ssh_profiles
                .iter()
                .find(|profile| profile.name == spec.profile_name)
                .ok_or_else(|| {
                    transport_core::TransportError::new(format!(
                        "SSH profile '{}' does not exist",
                        spec.profile_name
                    ))
                })?;
            let semantic_mode = if !config.shell_integration.enabled || !profile.shell_integration {
                IntegrationMode::Disabled
            } else if matches!(
                config.shell_integration.activation,
                ShellIntegrationActivationConfig::Heuristic
            ) {
                IntegrationMode::Heuristic
            } else if matches!(
                config.shell_integration.activation,
                ShellIntegrationActivationConfig::Disabled
            ) {
                IntegrationMode::Disabled
            } else {
                IntegrationMode::EscapeSequences
            };
            let parse_semantic_events = semantic_mode == IntegrationMode::EscapeSequences;
            let mut connection = ssh_connection_profile(profile);
            if let Some(directory) = &spec.working_directory {
                connection.remote_working_directory = Some(directory.clone());
            }
            Ok(InitialTransport {
                transport: PaneTransport::connecting_ssh(connection, size, output_waker.clone()),
                semantic_mode,
                parse_semantic_events,
                activation_diagnostics: vec![match semantic_mode {
                    IntegrationMode::EscapeSequences => format!(
                        "SSH profile '{}' accepts remote semantic markers but remains inactive until a marker is observed; run `panea shell-integration remote-plan --shell <shell> --profile {}` for installation help",
                        profile.name, profile.name
                    ),
                    IntegrationMode::Heuristic => format!(
                        "SSH profile '{}' uses low-confidence input-boundary heuristics; exit status, prompt, and remote cwd metadata are unavailable",
                        profile.name
                    ),
                    IntegrationMode::Disabled => format!(
                        "SSH semantic integration disabled for profile '{}'",
                        profile.name
                    ),
                }],
                remote_metadata: Some(RemoteMetadata {
                    transport: Some("ssh".to_owned()),
                    remote_host: Some(profile.host.clone()),
                    remote_user: profile.username.clone(),
                    remote_current_working_directory: profile.remote_working_directory.clone(),
                }),
            })
        }
        SessionTransportKind::FutureMobileSsh => Err(transport_core::TransportError::new(
            "future mobile SSH transport cannot run in the desktop application",
        )),
    }
}

fn selected_shell_profile(config: &AppConfig) -> Option<&ShellProfile> {
    if let Some(default_shell_profile) = &config.default_shell_profile
        && let Some(profile) = config
            .shell_profiles
            .iter()
            .find(|profile| &profile.name == default_shell_profile)
    {
        return Some(profile);
    }

    config.shell_profiles.first()
}

struct InitialTransport {
    transport: PaneTransport,
    semantic_mode: IntegrationMode,
    parse_semantic_events: bool,
    activation_diagnostics: Vec<String>,
    remote_metadata: Option<RemoteMetadata>,
}

const MAX_PENDING_SSH_INPUT_BYTES: usize = 64 * 1024;

struct PendingSshTransport {
    result: Receiver<TransportResult<SshTransport>>,
    interactions: Receiver<SshInteractionRequest>,
    requested_size: TransportSize,
    pending_input: Vec<u8>,
    output_waker: TransportWakeHandle,
}

enum SshInteractionRequest {
    HostTrust {
        request: HostKeyTrustRequest,
        response: SyncSender<HostKeyTrustAction>,
    },
    Secret {
        request: SecretRequest,
        keychain: KeychainProviderCapability,
        response: SyncSender<Option<SecretPromptResponse>>,
    },
}

struct ChannelHostTrustProvider {
    requests: SyncSender<SshInteractionRequest>,
    output_waker: TransportWakeHandle,
}

impl HostTrustProvider for ChannelHostTrustProvider {
    fn decide_host_trust(
        &mut self,
        request: HostKeyTrustRequest,
    ) -> security::SecurityResult<HostKeyTrustAction> {
        let (response, result) = mpsc::sync_channel(1);
        self.requests
            .send(SshInteractionRequest::HostTrust { request, response })
            .map_err(|_| security::SecurityError::new("SSH trust prompt was cancelled"))?;
        self.output_waker.wake();
        result
            .recv()
            .map_err(|_| security::SecurityError::new("SSH trust prompt was cancelled"))
    }
}

struct ChannelSecretPromptProvider {
    requests: SyncSender<SshInteractionRequest>,
    keychain: KeychainProviderCapability,
    output_waker: TransportWakeHandle,
}

impl SecretPromptProvider for ChannelSecretPromptProvider {
    fn prompt_secret(
        &mut self,
        request: &SecretRequest,
    ) -> security::SecurityResult<Option<SecretPromptResponse>> {
        let (response, result) = mpsc::sync_channel(1);
        self.requests
            .send(SshInteractionRequest::Secret {
                request: request.clone(),
                keychain: self.keychain.clone(),
                response,
            })
            .map_err(|_| security::SecurityError::new("SSH credential prompt was cancelled"))?;
        self.output_waker.wake();
        result
            .recv()
            .map_err(|_| security::SecurityError::new("SSH credential prompt was cancelled"))
    }
}

const MAX_OUTPUT_BYTES_PER_GUI_TICK: usize = 64 * 1024;

/// Upper bound on transport polls per pane per event-loop pass.
const MAX_OUTPUT_DRAIN_PASSES: usize = 64;

/// Wall-clock budget for draining one pane's output. Heavy output is bounded by
/// time rather than by a fixed byte count so a slow parse cannot stretch a
/// single pass past a frame; whatever is left keeps its wake and is picked up on
/// the next pass.
const MAX_OUTPUT_DRAIN_BUDGET: Duration = Duration::from_millis(4);
const SYNCHRONIZED_OUTPUT_TIMEOUT: Duration = Duration::from_millis(150);

struct PaneTransportLoop {
    io: Option<TransportEventLoop>,
    pending_events: VecDeque<TransportEvent>,
    output_waker: TransportWakeHandle,
}

impl PaneTransportLoop {
    fn new<T>(transport: T, output_waker: TransportWakeHandle) -> Self
    where
        T: TerminalTransport + 'static,
    {
        let worker_waker = output_waker.clone();
        Self {
            io: Some(TransportEventLoop::spawn_with_waker(
                transport,
                worker_waker,
            )),
            pending_events: VecDeque::new(),
            output_waker,
        }
    }

    fn send(&self, command: TransportCommand) -> TransportResult<()> {
        self.io
            .as_ref()
            .ok_or_else(|| transport_core::TransportError::new("transport worker is closed"))?
            .send_command(command)
            .map_err(|error| transport_core::TransportError::new(error.to_string()))
    }

    fn poll_output(&mut self) -> TransportResult<TransportOutput> {
        let Some(io) = self.io.as_ref() else {
            return Ok(TransportOutput::closed());
        };
        let mut output = TransportOutput::bytes(Vec::new());
        while output.bytes.len() < MAX_OUTPUT_BYTES_PER_GUI_TICK {
            let Some(event) = self.pending_events.pop_front().or_else(|| io.poll_event()) else {
                break;
            };
            match event {
                TransportEvent::Output(mut bytes) => {
                    let remaining = MAX_OUTPUT_BYTES_PER_GUI_TICK - output.bytes.len();
                    if bytes.len() > remaining {
                        let overflow = bytes.split_off(remaining);
                        self.pending_events
                            .push_front(TransportEvent::Output(overflow));
                    }
                    if output.bytes.is_empty() {
                        output.bytes = bytes;
                    } else {
                        output.bytes.append(&mut bytes);
                    }
                }
                TransportEvent::Lifecycle(event) => {
                    output.closed |=
                        matches!(event, transport_core::TransportLifecycleEvent::Closed);
                    output.lifecycle.push(event);
                }
                TransportEvent::Error(message) => {
                    return Err(transport_core::TransportError::new(message));
                }
            }
        }
        // Stopping at the byte cap means more output may still be queued in the
        // worker even when nothing was split into `pending_events`. Without a
        // wake here that remainder would sit unread until the child happened to
        // produce more output.
        if !self.pending_events.is_empty() || output.bytes.len() >= MAX_OUTPUT_BYTES_PER_GUI_TICK {
            self.output_waker.wake();
        }
        Ok(output)
    }

    fn shutdown(&mut self) -> TransportResult<()> {
        let Some(io) = self.io.take() else {
            return Ok(());
        };
        io.shutdown()
            .map_err(|error| transport_core::TransportError::new(error.to_string()))
    }
}

enum PaneTransport {
    Local(PaneTransportLoop),
    ConnectingSsh(PendingSshTransport),
    Ssh(PaneTransportLoop),
    Failed { message: String, reported: bool },
}

impl PaneTransport {
    fn connecting_ssh(
        profile: SshConnectionProfile,
        size: TransportSize,
        output_waker: TransportWakeHandle,
    ) -> Self {
        let (sender, result) = mpsc::sync_channel(1);
        let (interaction_sender, interactions) = mpsc::sync_channel(1);
        let worker_waker = output_waker.clone();
        thread::spawn(move || {
            let mut trust_provider = ChannelHostTrustProvider {
                requests: interaction_sender.clone(),
                output_waker: worker_waker.clone(),
            };
            let keychain = PlatformKeychainProvider::for_current_platform();
            let prompt_provider = ChannelSecretPromptProvider {
                requests: interaction_sender,
                keychain: keychain.capability(),
                output_waker: worker_waker.clone(),
            };
            let mut secret_provider = KeychainBackedSecretProvider::new(keychain, prompt_provider);
            let transport = SshTransport::connect_with_providers(
                profile,
                size,
                &mut secret_provider,
                &mut trust_provider,
            );
            let _ = sender.send(transport);
            worker_waker.wake();
        });
        Self::ConnectingSsh(PendingSshTransport {
            result,
            interactions,
            requested_size: size,
            pending_input: Vec::new(),
            output_waker,
        })
    }

    fn take_interaction(&mut self) -> Option<SshInteractionRequest> {
        let Self::ConnectingSsh(pending) = self else {
            return None;
        };
        pending.interactions.try_recv().ok()
    }

    fn is_connected(&self) -> bool {
        matches!(self, Self::Local(_) | Self::Ssh(_))
    }

    fn promote_ssh(&mut self) -> TransportResult<()> {
        let Self::ConnectingSsh(pending) = self else {
            return Ok(());
        };
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => Err(transport_core::TransportError::new(
                "SSH connection worker stopped without a result",
            )),
        };
        match result {
            Ok(transport) => {
                let io = PaneTransportLoop::new(transport, pending.output_waker.clone());
                io.send(TransportCommand::Resize(pending.requested_size))?;
                if !pending.pending_input.is_empty() {
                    io.send(TransportCommand::write_input(std::mem::take(
                        &mut pending.pending_input,
                    )))?;
                }
                *self = Self::Ssh(io);
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                *self = Self::Failed {
                    message: message.clone(),
                    reported: false,
                };
                Err(transport_core::TransportError::new(message))
            }
        }
    }

    fn write_input(&mut self, bytes: &[u8]) -> TransportResult<()> {
        self.promote_ssh()?;
        match self {
            Self::Local(transport) | Self::Ssh(transport) => {
                transport.send(TransportCommand::write_input(bytes))
            }
            Self::ConnectingSsh(pending) => {
                if pending.pending_input.len().saturating_add(bytes.len())
                    > MAX_PENDING_SSH_INPUT_BYTES
                {
                    return Err(transport_core::TransportError::new(
                        "SSH input queue is full while connection is pending",
                    ));
                }
                pending.pending_input.extend_from_slice(bytes);
                Ok(())
            }
            Self::Failed { message, .. } => {
                Err(transport_core::TransportError::new(message.clone()))
            }
        }
    }

    fn resize(&mut self, size: TransportSize) -> TransportResult<()> {
        self.promote_ssh()?;
        match self {
            Self::Local(transport) | Self::Ssh(transport) => {
                transport.send(TransportCommand::Resize(size))
            }
            Self::ConnectingSsh(pending) => {
                pending.requested_size = size;
                Ok(())
            }
            Self::Failed { message, .. } => {
                Err(transport_core::TransportError::new(message.clone()))
            }
        }
    }

    fn poll_output(&mut self) -> TransportResult<TransportOutput> {
        self.promote_ssh()?;
        match self {
            Self::Local(transport) | Self::Ssh(transport) => transport.poll_output(),
            Self::ConnectingSsh(_) => Ok(TransportOutput::bytes(Vec::new())),
            Self::Failed { message, reported } => {
                if *reported {
                    Ok(TransportOutput::closed())
                } else {
                    *reported = true;
                    Err(transport_core::TransportError::new(message.clone()))
                }
            }
        }
    }

    fn shutdown(&mut self) -> TransportResult<()> {
        match self {
            Self::Local(transport) | Self::Ssh(transport) => transport.shutdown(),
            Self::ConnectingSsh(_) | Self::Failed { .. } => Ok(()),
        }
    }

    fn requires_periodic_poll(&self) -> bool {
        matches!(self, Self::ConnectingSsh(_))
    }
}

fn initial_local_shell_profile(
    config: &AppConfig,
    requested_profile: Option<&str>,
) -> (LocalShellProfile, ShellIntegrationActivationPlan) {
    let mut profile = requested_profile
        .and_then(|name| {
            config
                .shell_profiles
                .iter()
                .find(|profile| profile.name == name)
        })
        .or_else(|| selected_shell_profile(config))
        .map(local_shell_profile)
        .unwrap_or_else(LocalShellProfile::default_for_platform);
    let shell = shell_kind_for_local_profile(&profile);
    let policy = shell_integration_policy(config);
    let activation = shell_integration::activation_plan(&policy, &profile.name, shell);
    apply_shell_integration_activation(&mut profile, &activation);
    (profile, activation)
}

fn local_shell_profile(profile: &ShellProfile) -> LocalShellProfile {
    let profile = resolved_shell_profile(profile);
    let kind = match profile.kind {
        ShellProfileKind::Default => LocalShellKind::Default,
        ShellProfileKind::PowerShell => LocalShellKind::PowerShell,
        ShellProfileKind::Cmd => LocalShellKind::Cmd,
        ShellProfileKind::Wsl => LocalShellKind::Wsl,
        ShellProfileKind::Custom => LocalShellKind::Custom,
    };
    let program = if profile.program.trim().is_empty() {
        match profile.kind {
            ShellProfileKind::PowerShell => "powershell.exe",
            ShellProfileKind::Cmd => "cmd.exe",
            ShellProfileKind::Wsl => "wsl.exe",
            ShellProfileKind::Default | ShellProfileKind::Custom => {
                if cfg!(windows) {
                    "powershell.exe"
                } else {
                    "/bin/sh"
                }
            }
        }
        .to_owned()
    } else {
        profile.program.clone()
    };

    LocalShellProfile {
        name: profile.name.clone(),
        kind,
        program,
        args: profile.args.clone(),
        env: profile.env.clone(),
        working_directory: profile.working_directory.as_ref().map(PathBuf::from),
        startup_command: profile.startup_command.clone(),
    }
}

fn resolved_shell_profile(profile: &ShellProfile) -> ShellProfile {
    let mut resolved = profile.clone();
    let override_config = match ConfigPlatform::current() {
        ConfigPlatform::MacOs => profile.platform_overrides.macos.as_ref(),
        ConfigPlatform::Windows => profile.platform_overrides.windows.as_ref(),
        ConfigPlatform::Linux => profile.platform_overrides.linux.as_ref(),
        ConfigPlatform::LinuxX11 => profile
            .platform_overrides
            .linux_x11
            .as_ref()
            .or(profile.platform_overrides.linux.as_ref()),
        ConfigPlatform::LinuxWayland => profile
            .platform_overrides
            .linux_wayland
            .as_ref()
            .or(profile.platform_overrides.linux.as_ref()),
        ConfigPlatform::Unknown => None,
    };
    if let Some(override_config) = override_config {
        if let Some(program) = &override_config.program {
            resolved.program = program.clone();
        }
        if let Some(args) = &override_config.args {
            resolved.args = args.clone();
        }
        if let Some(env) = &override_config.env {
            resolved.env.extend(env.clone());
        }
        if let Some(working_directory) = &override_config.working_directory {
            resolved.working_directory = Some(working_directory.clone());
        }
        if let Some(startup_command) = &override_config.startup_command {
            resolved.startup_command = Some(startup_command.clone());
        }
    }
    resolved
}

fn shell_integration_policy(config: &AppConfig) -> ShellIntegrationPolicy {
    let activation = if !config.shell_integration.enabled {
        IntegrationActivation::Disabled
    } else {
        match config.shell_integration.activation {
            ShellIntegrationActivationConfig::Full => IntegrationActivation::Full,
            ShellIntegrationActivationConfig::AutoDetect => IntegrationActivation::AutoDetect,
            ShellIntegrationActivationConfig::Manual => IntegrationActivation::Manual,
            ShellIntegrationActivationConfig::Heuristic => IntegrationActivation::Heuristic,
            ShellIntegrationActivationConfig::Disabled => IntegrationActivation::Disabled,
        }
    };

    ShellIntegrationPolicy {
        enabled: config.shell_integration.enabled,
        activation,
        auto_install: config.shell_integration.auto_install,
        enabled_shells: config
            .shell_integration
            .enabled_shells
            .iter()
            .map(|shell| ShellKind::parse(shell))
            .filter(|shell| *shell != ShellKind::Unknown)
            .collect(),
        disabled_profiles: config.shell_integration.disabled_shell_profiles.clone(),
        remote_instructions: config.shell_integration.remote_instructions,
    }
}

fn shell_kind_for_local_profile(profile: &LocalShellProfile) -> ShellKind {
    match profile.kind {
        LocalShellKind::PowerShell => ShellKind::PowerShell,
        LocalShellKind::Cmd => ShellKind::Cmd,
        LocalShellKind::Wsl => ShellKind::Bash,
        LocalShellKind::Default | LocalShellKind::Custom => {
            let detected = detect_shell_kind(&profile.program);
            if detected == ShellKind::Unknown && cfg!(windows) {
                ShellKind::PowerShell
            } else {
                detected
            }
        }
    }
}

fn semantic_mode_for_activation(activation: &ShellIntegrationActivationPlan) -> IntegrationMode {
    match activation.mode {
        ShellIntegrationRuntimeMode::Full | ShellIntegrationRuntimeMode::Auto => {
            IntegrationMode::EscapeSequences
        }
        ShellIntegrationRuntimeMode::Heuristic => IntegrationMode::Heuristic,
        ShellIntegrationRuntimeMode::Off => IntegrationMode::Disabled,
    }
}

fn apply_shell_integration_activation(
    profile: &mut LocalShellProfile,
    activation: &ShellIntegrationActivationPlan,
) {
    profile.env.extend(activation.environment.clone());

    if activation.action != ShellIntegrationActivationAction::InjectRuntimeScript {
        return;
    }

    if !profile.args.is_empty() {
        eprintln!(
            "shell integration fallback: profile '{}' has explicit args, runtime hook injection skipped",
            profile.name
        );
        return;
    }

    let Some(script) = activation.script.as_ref() else {
        return;
    };
    let existing_startup = profile.startup_command.take();
    let hook = combine_shell_startup(script.contents, existing_startup.as_deref());

    match activation.shell {
        ShellKind::Bash => {
            if let Ok(path) = write_runtime_shell_hook(&profile.name, "bashrc", &hook) {
                profile.args = vec![
                    "--init-file".to_owned(),
                    path.display().to_string(),
                    "-i".to_owned(),
                ];
            } else {
                profile.startup_command = existing_startup;
            }
        }
        ShellKind::Zsh => {
            if let Ok(path) = write_runtime_shell_hook(&profile.name, "zshrc", &hook)
                && let Some(directory) = path.parent()
            {
                profile.env.insert(
                    "PANEA_ORIGINAL_ZDOTDIR".to_owned(),
                    std::env::var("ZDOTDIR").unwrap_or_else(|_| {
                        std::env::var("HOME").unwrap_or_else(|_| "~".to_owned())
                    }),
                );
                profile
                    .env
                    .insert("ZDOTDIR".to_owned(), directory.display().to_string());
                profile.args = vec!["-i".to_owned()];
            } else {
                profile.startup_command = existing_startup;
            }
        }
        ShellKind::Fish => {
            profile.args = vec!["-C".to_owned(), hook];
        }
        ShellKind::PowerShell | ShellKind::Pwsh => {
            profile.args = vec![
                "-NoLogo".to_owned(),
                "-NoExit".to_owned(),
                "-Command".to_owned(),
                hook,
            ];
            profile.startup_command = None;
        }
        ShellKind::Cmd | ShellKind::Nushell | ShellKind::Unknown => {
            profile.startup_command = existing_startup;
        }
    }
}

fn combine_shell_startup(script: &str, existing_startup: Option<&str>) -> String {
    let mut combined = String::from(script);
    if let Some(existing_startup) = existing_startup
        && !existing_startup.trim().is_empty()
    {
        combined.push('\n');
        combined.push_str(existing_startup);
        combined.push('\n');
    }
    combined
}

fn write_runtime_shell_hook(
    profile_name: &str,
    file_name: &str,
    contents: &str,
) -> std::io::Result<PathBuf> {
    let directory = std::env::temp_dir()
        .join("panea-shell-integration")
        .join(std::process::id().to_string())
        .join(sanitize_file_component(profile_name));
    let path = match file_name {
        "zshrc" => directory.join(".zshrc"),
        "bashrc" => directory.join("panea.bashrc"),
        _ => directory.join(file_name),
    };
    let wrapped = wrap_runtime_shell_hook(file_name, contents);
    let content_hash = stable_bytes_hash(wrapped.as_bytes());
    static WRITTEN_HOOKS: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();
    let hashes = WRITTEN_HOOKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut hashes = hashes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if hashes.get(&path) == Some(&content_hash) {
        return Ok(path);
    }

    fs::create_dir_all(&directory)?;
    fs::write(&path, wrapped)?;
    hashes.insert(path.clone(), content_hash);
    Ok(path)
}

fn stable_bytes_hash(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

fn wrap_runtime_shell_hook(file_name: &str, contents: &str) -> String {
    match file_name {
        "bashrc" => {
            format!("if [ -r \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi\n{contents}\n")
        }
        "zshrc" => format!(
            "if [ -n \"$PANEA_ORIGINAL_ZDOTDIR\" ] && [ -r \"$PANEA_ORIGINAL_ZDOTDIR/.zshrc\" ]; then . \"$PANEA_ORIGINAL_ZDOTDIR/.zshrc\"; fi\n{contents}\n"
        ),
        _ => contents.to_owned(),
    }
}

fn sanitize_file_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn ssh_connection_profile(profile: &SshProfile) -> SshConnectionProfile {
    let mut connection = SshConnectionProfile::new(profile.name.clone(), profile.host.clone());
    connection.port = profile.port;
    connection.username = profile.username.clone();
    connection.auth_method = match profile.auth_method {
        SshAuthMethod::Agent => security::AuthMethod::Agent,
        SshAuthMethod::PublicKey => security::AuthMethod::PublicKey,
        SshAuthMethod::Password => security::AuthMethod::Password,
        SshAuthMethod::KeyboardInteractive => security::AuthMethod::KeyboardInteractive,
        SshAuthMethod::None => security::AuthMethod::None,
    };
    connection.identity_file = profile.identity_file.as_ref().map(PathBuf::from);
    connection.known_hosts_policy = match &profile.known_hosts_policy {
        SshKnownHostsPolicy::Ask => security::KnownHostsPolicy::Ask,
        SshKnownHostsPolicy::RequireKnown => security::KnownHostsPolicy::RequireKnown,
        SshKnownHostsPolicy::TrustOnFirstUse => security::KnownHostsPolicy::TrustOnFirstUse,
        SshKnownHostsPolicy::PinFingerprint { sha256 } => {
            security::KnownHostsPolicy::PinFingerprint {
                sha256: sha256.clone(),
            }
        }
    };
    connection.remote_command = profile.remote_command.clone();
    connection.remote_working_directory = profile.remote_working_directory.clone();
    connection.shell_integration = profile.shell_integration;
    connection.agent_forwarding = profile.agent_forwarding;
    connection.proxy_jump = profile.proxy_jump.clone();
    connection
}

fn cols_for_width(width: u32, metrics: CellMetrics) -> u16 {
    ((width as f32 / metrics.cell_width).floor() as u16).max(1)
}

fn horizontal_content_inset(config: &AppConfig) -> u32 {
    u32::from(config.window.padding_x).saturating_add(u32::from(config.window.margin_x))
}

fn vertical_content_inset(config: &AppConfig) -> u32 {
    u32::from(config.window.padding_y).saturating_add(u32::from(config.window.margin_y))
}

fn content_extent(extent: u32, inset: u32) -> u32 {
    extent.saturating_sub(inset.saturating_mul(2)).max(1)
}

fn rows_for_height(height: u32, metrics: CellMetrics) -> u16 {
    ((height as f32 / metrics.cell_height).floor() as u16).max(1)
}
