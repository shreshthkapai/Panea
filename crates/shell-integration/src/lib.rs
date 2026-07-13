//! Shell integration hooks and semantic event contracts.

pub const LAYER: &str = "semantic meaning";

use std::{collections::BTreeMap, path::Path, time::Duration};

use semantics::{
    BufferPosition, CommandStatus, RemoteMetadata, SemanticEvent, SemanticMetadata, ShellMetadata,
};

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const MAX_OSC_PAYLOAD_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Pwsh,
    Nushell,
    Cmd,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationActivation {
    Full,
    AutoDetect,
    Manual,
    Heuristic,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegrationPolicy {
    pub enabled: bool,
    pub activation: IntegrationActivation,
    pub auto_install: bool,
    pub enabled_shells: Vec<ShellKind>,
    pub disabled_profiles: Vec<String>,
    pub remote_instructions: bool,
}

impl Default for ShellIntegrationPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            activation: IntegrationActivation::AutoDetect,
            auto_install: false,
            enabled_shells: vec![
                ShellKind::Bash,
                ShellKind::Zsh,
                ShellKind::Fish,
                ShellKind::PowerShell,
                ShellKind::Pwsh,
            ],
            disabled_profiles: Vec::new(),
            remote_instructions: true,
        }
    }
}

impl ShellIntegrationPolicy {
    #[must_use]
    pub fn should_enable_for_profile(&self, profile_name: &str, shell: ShellKind) -> bool {
        self.enabled
            && self.activation != IntegrationActivation::Disabled
            && self.enabled_shells.contains(&shell)
            && !self
                .disabled_profiles
                .iter()
                .any(|profile| profile == profile_name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellIntegrationRuntimeMode {
    Full,
    Auto,
    Heuristic,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellIntegrationActivationAction {
    InjectRuntimeScript,
    DetectExisting,
    ManualInstructions,
    Heuristic,
    Disabled,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegrationActivationPlan {
    pub shell: ShellKind,
    pub mode: ShellIntegrationRuntimeMode,
    pub action: ShellIntegrationActivationAction,
    pub script: Option<ShellIntegrationScript>,
    pub environment: BTreeMap<String, String>,
    pub diagnostics: Vec<String>,
}

impl ShellIntegrationActivationPlan {
    #[must_use]
    pub fn parses_escape_sequences(&self) -> bool {
        matches!(
            self.action,
            ShellIntegrationActivationAction::InjectRuntimeScript
                | ShellIntegrationActivationAction::DetectExisting
                | ShellIntegrationActivationAction::ManualInstructions
        )
    }
}

#[must_use]
pub fn activation_plan(
    policy: &ShellIntegrationPolicy,
    profile_name: &str,
    shell: ShellKind,
) -> ShellIntegrationActivationPlan {
    let mut environment = BTreeMap::from([
        ("PANEA_TERMINAL".to_owned(), "1".to_owned()),
        (
            "PANEA_SHELL".to_owned(),
            shell.as_str().unwrap_or("unknown").to_owned(),
        ),
    ]);
    let mut diagnostics = Vec::new();

    if !policy.enabled || policy.activation == IntegrationActivation::Disabled {
        environment.insert("PANEA_SHELL_INTEGRATION".to_owned(), "0".to_owned());
        diagnostics.push("shell integration disabled by config".to_owned());
        return ShellIntegrationActivationPlan {
            shell,
            mode: ShellIntegrationRuntimeMode::Off,
            action: ShellIntegrationActivationAction::Disabled,
            script: None,
            environment,
            diagnostics,
        };
    }

    if !policy.enabled_shells.contains(&shell) {
        environment.insert("PANEA_SHELL_INTEGRATION".to_owned(), "0".to_owned());
        diagnostics.push(format!(
            "shell integration not enabled for shell {}",
            shell.as_str().unwrap_or("unknown")
        ));
        return ShellIntegrationActivationPlan {
            shell,
            mode: ShellIntegrationRuntimeMode::Off,
            action: ShellIntegrationActivationAction::Unsupported,
            script: None,
            environment,
            diagnostics,
        };
    }

    if policy
        .disabled_profiles
        .iter()
        .any(|profile| profile == profile_name)
    {
        environment.insert("PANEA_SHELL_INTEGRATION".to_owned(), "0".to_owned());
        diagnostics.push(format!(
            "shell integration disabled for shell profile '{profile_name}'"
        ));
        return ShellIntegrationActivationPlan {
            shell,
            mode: ShellIntegrationRuntimeMode::Off,
            action: ShellIntegrationActivationAction::Disabled,
            script: None,
            environment,
            diagnostics,
        };
    }

    if policy.activation == IntegrationActivation::Heuristic {
        environment.insert("PANEA_SHELL_INTEGRATION".to_owned(), "heuristic".to_owned());
        diagnostics.push("shell integration heuristic mode requested".to_owned());
        return ShellIntegrationActivationPlan {
            shell,
            mode: ShellIntegrationRuntimeMode::Heuristic,
            action: ShellIntegrationActivationAction::Heuristic,
            script: None,
            environment,
            diagnostics,
        };
    }

    let Some(script) = script_for_shell(shell) else {
        environment.insert("PANEA_SHELL_INTEGRATION".to_owned(), "0".to_owned());
        diagnostics.push(format!(
            "no runtime shell integration script exists for {}",
            shell.as_str().unwrap_or("unknown")
        ));
        return ShellIntegrationActivationPlan {
            shell,
            mode: ShellIntegrationRuntimeMode::Off,
            action: ShellIntegrationActivationAction::Unsupported,
            script: None,
            environment,
            diagnostics,
        };
    };

    let (mode, action) = match policy.activation {
        IntegrationActivation::Full => (
            ShellIntegrationRuntimeMode::Full,
            ShellIntegrationActivationAction::InjectRuntimeScript,
        ),
        IntegrationActivation::AutoDetect if policy.auto_install => (
            ShellIntegrationRuntimeMode::Auto,
            ShellIntegrationActivationAction::InjectRuntimeScript,
        ),
        IntegrationActivation::AutoDetect => (
            ShellIntegrationRuntimeMode::Auto,
            ShellIntegrationActivationAction::DetectExisting,
        ),
        IntegrationActivation::Manual => (
            ShellIntegrationRuntimeMode::Auto,
            ShellIntegrationActivationAction::ManualInstructions,
        ),
        IntegrationActivation::Heuristic | IntegrationActivation::Disabled => unreachable!(),
    };

    environment.insert(
        "PANEA_SHELL_INTEGRATION".to_owned(),
        match mode {
            ShellIntegrationRuntimeMode::Full => "full",
            ShellIntegrationRuntimeMode::Auto => "auto",
            ShellIntegrationRuntimeMode::Heuristic => "heuristic",
            ShellIntegrationRuntimeMode::Off => "0",
        }
        .to_owned(),
    );

    if action == ShellIntegrationActivationAction::DetectExisting {
        diagnostics.push(
            "auto-detect mode will accept semantic escape sequences but will not inject hooks"
                .to_owned(),
        );
    }
    if action == ShellIntegrationActivationAction::ManualInstructions
        && let Some(instructions) = manual_install_instructions(shell)
    {
        diagnostics.push(instructions.to_owned());
    }

    ShellIntegrationActivationPlan {
        shell,
        mode,
        action,
        script: Some(script),
        environment,
        diagnostics,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegrationScript {
    pub shell: ShellKind,
    pub file_name: &'static str,
    pub contents: &'static str,
}

#[must_use]
pub fn script_for_shell(shell: ShellKind) -> Option<ShellIntegrationScript> {
    match shell {
        ShellKind::Bash => Some(ShellIntegrationScript {
            shell,
            file_name: "panea.bash",
            contents: BASH_SCRIPT,
        }),
        ShellKind::Zsh => Some(ShellIntegrationScript {
            shell,
            file_name: "panea.zsh",
            contents: ZSH_SCRIPT,
        }),
        ShellKind::Fish => Some(ShellIntegrationScript {
            shell,
            file_name: "panea.fish",
            contents: FISH_SCRIPT,
        }),
        ShellKind::PowerShell | ShellKind::Pwsh => Some(ShellIntegrationScript {
            shell,
            file_name: "panea.ps1",
            contents: POWERSHELL_SCRIPT,
        }),
        ShellKind::Nushell | ShellKind::Cmd | ShellKind::Unknown => None,
    }
}

impl ShellKind {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "bash" => Self::Bash,
            "zsh" => Self::Zsh,
            "fish" => Self::Fish,
            "powershell" | "windows_powershell" | "powershell.exe" => Self::PowerShell,
            "pwsh" | "pwsh.exe" => Self::Pwsh,
            "nu" | "nushell" => Self::Nushell,
            "cmd" | "cmd.exe" => Self::Cmd,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Bash => Some("bash"),
            Self::Zsh => Some("zsh"),
            Self::Fish => Some("fish"),
            Self::PowerShell => Some("powershell"),
            Self::Pwsh => Some("pwsh"),
            Self::Nushell => Some("nushell"),
            Self::Cmd => Some("cmd"),
            Self::Unknown => None,
        }
    }
}

#[must_use]
pub fn detect_shell_kind(program_or_name: &str) -> ShellKind {
    let path = Path::new(program_or_name);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program_or_name);
    let without_extension = file_name
        .strip_suffix(".exe")
        .or_else(|| file_name.strip_suffix(".EXE"))
        .unwrap_or(file_name);

    ShellKind::parse(without_extension)
}

#[must_use]
pub fn manual_install_instructions(shell: ShellKind) -> Option<&'static str> {
    match shell {
        ShellKind::Bash => Some("source /path/to/panea.bash from ~/.bashrc"),
        ShellKind::Zsh => Some("source /path/to/panea.zsh from ~/.zshrc"),
        ShellKind::Fish => Some("source /path/to/panea.fish from ~/.config/fish/config.fish"),
        ShellKind::PowerShell | ShellKind::Pwsh => {
            Some(". /path/to/panea.ps1 from your PowerShell profile")
        }
        ShellKind::Nushell => Some("nushell integration is planned but not enabled yet"),
        ShellKind::Cmd => {
            Some("cmd has limited shell integration and currently runs without hooks")
        }
        ShellKind::Unknown => None,
    }
}

#[must_use]
pub fn verification_sequence(shell: ShellKind, marker: &str) -> Option<String> {
    let escaped_marker = marker.replace('\'', "'\\''");
    match shell {
        ShellKind::Bash | ShellKind::Zsh => Some(format!(
            "printf '\\033]777;shell;shell={}\\007\\033]777;prompt_start;shell={}\\007\\033]777;prompt_end\\007\\033]777;input_start\\007\\033]777;input_end\\007\\033]777;output_start\\007%s\\n\\033]777;output_end\\007\\033]777;command_finished;status=0;duration_ms=1\\007' '{}'",
            shell.as_str().unwrap_or("sh"),
            shell.as_str().unwrap_or("sh"),
            escaped_marker
        )),
        ShellKind::Fish => Some(format!(
            "printf '\\e]777;shell;shell=fish\\a\\e]777;prompt_start;shell=fish\\a\\e]777;prompt_end\\a\\e]777;input_start\\a\\e]777;input_end\\a\\e]777;output_start\\a%s\\n\\e]777;output_end\\a\\e]777;command_finished;status=0;duration_ms=1\\a' '{}'",
            escaped_marker
        )),
        ShellKind::PowerShell | ShellKind::Pwsh => Some(format!(
            "$e=[char]27; Write-Host -NoNewline \"$e]777;shell;shell=powershell$([char]7)$e]777;prompt_start;shell=powershell$([char]7)$e]777;prompt_end$([char]7)$e]777;input_start$([char]7)$e]777;input_end$([char]7)$e]777;output_start$([char]7)\"; Write-Output '{}'; Write-Host -NoNewline \"$e]777;output_end$([char]7)$e]777;command_finished;status=0;duration_ms=1$([char]7)\"",
            marker.replace('\"', "`\"")
        )),
        ShellKind::Cmd | ShellKind::Nushell | ShellKind::Unknown => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticParseError {
    UnsupportedOsc(String),
    InvalidPayload(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSemanticEvent {
    pub raw_osc: String,
    pub event: SemanticEvent,
    /// Byte offset immediately after the marker in the current input batch.
    /// Zero is used by direct payload parsing where no source batch exists.
    pub source_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEscapeParser {
    state: ParserState,
}

impl Default for SemanticEscapeParser {
    fn default() -> Self {
        Self {
            state: ParserState::Ground,
        }
    }
}

impl SemanticEscapeParser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse(&mut self, bytes: &[u8], position: BufferPosition) -> Vec<ParsedSemanticEvent> {
        let mut events = Vec::new();

        for (index, byte) in bytes.iter().enumerate() {
            match &mut self.state {
                ParserState::Ground => {
                    if *byte == ESC {
                        self.state = ParserState::Escape;
                    }
                }
                ParserState::Escape => {
                    if *byte == b']' {
                        self.state = ParserState::Osc {
                            escape_seen: false,
                            content: Vec::new(),
                        };
                    } else {
                        self.state = ParserState::Ground;
                    }
                }
                ParserState::Osc {
                    escape_seen,
                    content,
                } => match (*byte, *escape_seen) {
                    (BEL, _) => {
                        if let Some(mut event) = parse_osc_payload(content, position).ok().flatten()
                        {
                            event.source_end = index.saturating_add(1);
                            events.push(event);
                        }
                        self.state = ParserState::Ground;
                    }
                    (b'\\', true) => {
                        if content.last() == Some(&ESC) {
                            content.pop();
                        }
                        if let Some(mut event) = parse_osc_payload(content, position).ok().flatten()
                        {
                            event.source_end = index.saturating_add(1);
                            events.push(event);
                        }
                        self.state = ParserState::Ground;
                    }
                    (ESC, _) => {
                        if content.len() >= MAX_OSC_PAYLOAD_BYTES {
                            self.state = ParserState::IgnoringOsc { escape_seen: true };
                        } else {
                            content.push(*byte);
                            *escape_seen = true;
                        }
                    }
                    (_, _) => {
                        if content.len() >= MAX_OSC_PAYLOAD_BYTES {
                            self.state = ParserState::IgnoringOsc { escape_seen: false };
                        } else {
                            content.push(*byte);
                            *escape_seen = false;
                        }
                    }
                },
                ParserState::IgnoringOsc { escape_seen } => match (*byte, *escape_seen) {
                    (BEL, _) | (b'\\', true) => self.state = ParserState::Ground,
                    (ESC, _) => *escape_seen = true,
                    (_, _) => *escape_seen = false,
                },
            }
        }

        events
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParserState {
    Ground,
    Escape,
    Osc { escape_seen: bool, content: Vec<u8> },
    IgnoringOsc { escape_seen: bool },
}

pub fn parse_osc_payload(
    payload: &[u8],
    position: BufferPosition,
) -> Result<Option<ParsedSemanticEvent>, SemanticParseError> {
    let raw = String::from_utf8_lossy(payload).to_string();
    let mut parts = raw.split(';');
    let Some(command) = parts.next() else {
        return Ok(None);
    };
    let fields: Vec<&str> = parts.collect();

    let event = match command {
        "7" => cwd_event(&fields.join(";"), position, false)?,
        "133" => osc_133_event(&raw, &fields, position)?,
        "633" => osc_633_event(&raw, &fields, position)?,
        "777" => panea_event(&raw, &fields, position)?,
        _ => return Ok(None),
    };

    Ok(Some(ParsedSemanticEvent {
        raw_osc: raw,
        event,
        source_end: 0,
    }))
}

fn osc_133_event(
    raw: &str,
    fields: &[&str],
    position: BufferPosition,
) -> Result<SemanticEvent, SemanticParseError> {
    match fields.first().copied() {
        Some("A") => Ok(SemanticEvent::PromptStarted {
            position,
            metadata: metadata_from_kv(&fields[1..]),
        }),
        Some("B") => Ok(SemanticEvent::PromptEnded { position }),
        Some("C") => Ok(SemanticEvent::OutputStarted { position }),
        Some("D") => Ok(SemanticEvent::CommandFinished {
            position,
            exit_status: parse_exit_status(fields.get(1).copied()),
            duration: parse_duration(fields.get(2).copied()),
        }),
        Some("I") => Ok(SemanticEvent::InputStarted { position }),
        Some("J") => Ok(SemanticEvent::InputEnded { position }),
        _ => Err(SemanticParseError::UnsupportedOsc(raw.to_owned())),
    }
}

fn osc_633_event(
    raw: &str,
    fields: &[&str],
    position: BufferPosition,
) -> Result<SemanticEvent, SemanticParseError> {
    match fields.first().copied() {
        Some("A") => Ok(SemanticEvent::PromptStarted {
            position,
            metadata: metadata_from_kv(&fields[1..]),
        }),
        Some("B") => Ok(SemanticEvent::PromptEnded { position }),
        Some("C") => Ok(SemanticEvent::InputEnded { position }),
        Some("D") => Ok(SemanticEvent::CommandFinished {
            position,
            exit_status: parse_exit_status(fields.get(1).copied()),
            duration: parse_duration(fields.get(2).copied()),
        }),
        Some("E") => Ok(SemanticEvent::InputStarted { position }),
        Some("P") => panea_event(raw, &fields[1..], position),
        _ => Err(SemanticParseError::UnsupportedOsc(raw.to_owned())),
    }
}

fn panea_event(
    raw: &str,
    fields: &[&str],
    position: BufferPosition,
) -> Result<SemanticEvent, SemanticParseError> {
    match fields.first().copied() {
        Some("prompt_start") => Ok(SemanticEvent::PromptStarted {
            position,
            metadata: metadata_from_kv(&fields[1..]),
        }),
        Some("prompt_end") => Ok(SemanticEvent::PromptEnded { position }),
        Some("input_start") => Ok(SemanticEvent::InputStarted { position }),
        Some("input_end") => Ok(SemanticEvent::InputEnded { position }),
        Some("output_start") => Ok(SemanticEvent::OutputStarted { position }),
        Some("output_end") => Ok(SemanticEvent::OutputEnded { position }),
        Some("command_finished") => Ok(SemanticEvent::CommandFinished {
            position,
            exit_status: parse_exit_status(kv_value(fields, "status")),
            duration: parse_duration(kv_value(fields, "duration_ms")),
        }),
        Some("cwd") => cwd_event(
            kv_value(fields, "path").unwrap_or_default(),
            position,
            false,
        ),
        Some("remote_cwd") => {
            cwd_event(kv_value(fields, "path").unwrap_or_default(), position, true)
        }
        Some("shell") => Ok(SemanticEvent::ShellMetadataChanged {
            position,
            metadata: shell_metadata_from_kv(&fields[1..]),
        }),
        Some("remote") => Ok(SemanticEvent::RemoteMetadataChanged {
            position,
            metadata: remote_metadata_from_kv(&fields[1..]),
        }),
        _ => Err(SemanticParseError::UnsupportedOsc(raw.to_owned())),
    }
}

fn cwd_event(
    payload: &str,
    position: BufferPosition,
    remote: bool,
) -> Result<SemanticEvent, SemanticParseError> {
    let directory = parse_cwd_payload(payload);
    if directory.is_empty() {
        return Err(SemanticParseError::InvalidPayload(
            "current working directory event had no path".to_owned(),
        ));
    }
    Ok(SemanticEvent::CurrentWorkingDirectoryChanged {
        position,
        directory,
        remote,
    })
}

fn metadata_from_kv(fields: &[&str]) -> SemanticMetadata {
    SemanticMetadata {
        shell: shell_metadata_from_kv(fields),
        remote: None,
        command: kv_value(fields, "command").map(ToOwned::to_owned),
        attributes: fields
            .iter()
            .filter_map(|field| field.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
    }
}

fn shell_metadata_from_kv(fields: &[&str]) -> ShellMetadata {
    ShellMetadata {
        shell: kv_value(fields, "shell").map(ToOwned::to_owned),
        version: kv_value(fields, "version").map(ToOwned::to_owned),
        current_working_directory: kv_value(fields, "cwd")
            .or_else(|| kv_value(fields, "path"))
            .map(ToOwned::to_owned),
        prompt: kv_value(fields, "prompt").map(ToOwned::to_owned),
    }
}

fn remote_metadata_from_kv(fields: &[&str]) -> RemoteMetadata {
    RemoteMetadata {
        transport: kv_value(fields, "transport").map(ToOwned::to_owned),
        remote_host: kv_value(fields, "host").map(ToOwned::to_owned),
        remote_user: kv_value(fields, "user").map(ToOwned::to_owned),
        remote_current_working_directory: kv_value(fields, "cwd")
            .or_else(|| kv_value(fields, "path"))
            .map(ToOwned::to_owned),
    }
}

fn kv_value<'a>(fields: &'a [&str], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| {
            field
                .split_once('=')
                .filter(|(candidate, _)| *candidate == key)
        })
        .map(|(_, value)| value)
}

fn parse_exit_status(value: Option<&str>) -> CommandStatus {
    match value.and_then(|value| value.parse::<i32>().ok()) {
        Some(code) => CommandStatus::Code(code),
        None => CommandStatus::Unknown,
    }
}

fn parse_duration(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_default()
}

fn parse_cwd_payload(payload: &str) -> String {
    if let Some(rest) = payload.strip_prefix("file://") {
        let path_start = rest.find('/').unwrap_or(0);
        return percent_decode(&rest[path_start..]);
    }
    percent_decode(payload)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(hex) = u8::from_str_radix(&value[index + 1..index + 3], 16)
        {
            out.push(hex);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

pub const BASH_SCRIPT: &str = r#"# Panea shell integration for bash.
__panea_osc() { printf '\033]777;%s\007' "$1"; }
__panea_active=0
__panea_ready=0
__panea_original_prompt_command=${PROMPT_COMMAND:-}
__panea_precmd() {
  local status=$?
  if [ "$__panea_active" = 1 ]; then
    __panea_osc "output_end"
    __panea_osc "command_finished;status=$status"
    __panea_active=0
  fi
  __panea_osc "cwd;path=$PWD"
  __panea_osc "shell;shell=bash;cwd=$PWD"
  __panea_osc "prompt_start;shell=bash;cwd=$PWD"
}
__panea_prompt_cycle() {
  __panea_ready=0
  __panea_precmd
  if [ -n "$__panea_original_prompt_command" ]; then
    eval "$__panea_original_prompt_command"
  fi
  __panea_ready=1
}
__panea_preexec() {
  if [ "$__panea_ready" = 1 ]; then
    __panea_ready=0
    __panea_active=1
    __panea_osc "input_end"
    __panea_osc "output_start"
  fi
}
PROMPT_COMMAND=__panea_prompt_cycle
PS1="${PS1}"$'\[\e]777;prompt_end\a\]\[\e]777;input_start\a\]'
trap '__panea_preexec' DEBUG
"#;

pub const ZSH_SCRIPT: &str = r#"# Panea shell integration for zsh.
__panea_osc() { printf '\033]777;%s\007' "$1"; }
typeset -g __panea_active=0
precmd_functions+=(__panea_precmd)
preexec_functions+=(__panea_preexec)
__panea_precmd() {
  local status=$?
  if (( __panea_active )); then
    __panea_osc "output_end"
    __panea_osc "command_finished;status=$status"
    __panea_active=0
  fi
  __panea_osc "cwd;path=$PWD"
  __panea_osc "shell;shell=zsh;cwd=$PWD"
  __panea_osc "prompt_start;shell=zsh;cwd=$PWD"
}
__panea_preexec() {
  __panea_osc "input_end"
  __panea_osc "output_start"
  __panea_active=1
}
PROMPT="${PROMPT}"$'%{\e]777;prompt_end\a%}%{\e]777;input_start\a%}'
"#;

pub const FISH_SCRIPT: &str = r#"# Panea shell integration for fish.
function __panea_osc
    printf '\e]777;%s\a' $argv[1]
end
function __panea_prompt --on-event fish_prompt
    if set -q __panea_active
        __panea_osc "output_end"
        __panea_osc "command_finished;status=$__panea_status"
        set -e __panea_active
    end
    __panea_osc "cwd;path=$PWD"
    __panea_osc "shell;shell=fish;cwd=$PWD"
    __panea_osc "prompt_start;shell=fish;cwd=$PWD"
end
function __panea_preexec --on-event fish_preexec
    __panea_osc "prompt_end"
    __panea_osc "input_start"
    __panea_osc "input_end"
    __panea_osc "output_start"
    set -g __panea_active 1
end
function __panea_postexec --on-event fish_postexec
    set -g __panea_status $status
end
"#;

pub const POWERSHELL_SCRIPT: &str = r#"# Panea shell integration for PowerShell.
function global:__PaneaOsc([string] $Payload) {
    [Console]::Write("$([char]27)]777;$Payload$([char]7)")
}
if (Test-Path function:\prompt) {
    Copy-Item function:\prompt function:\__PaneaOriginalPrompt -Force
}
$global:__PaneaCommandActive = $false
if (Get-Module PSReadLine) {
    $paneaEnterHandler = Get-PSReadLineKeyHandler -Bound |
      Where-Object { ($_.Key -eq 'Enter') -or ($_.Key -contains 'Enter') } |
      Select-Object -First 1
    if (($null -eq $paneaEnterHandler) -or ($paneaEnterHandler.Function -eq 'AcceptLine')) {
      Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
        __PaneaOsc "input_end"
        __PaneaOsc "output_start"
        $global:__PaneaCommandActive = $true
        [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
      }
    } else {
      __PaneaOsc "shell;shell=powershell;integration=prompt_only;reason=custom_enter_handler"
    }
}
function global:prompt {
    $lastStatus = if ($?) { 0 } else { 1 }
    if ($global:__PaneaCommandActive) {
        __PaneaOsc "output_end"
        __PaneaOsc "command_finished;status=$lastStatus"
        $global:__PaneaCommandActive = $false
    }
    __PaneaOsc "cwd;path=$PWD"
    __PaneaOsc "shell;shell=powershell;cwd=$PWD;prompt_marker=fallback_prefix"
    __PaneaOsc "prompt_start;shell=powershell;cwd=$PWD"
    __PaneaOsc "prompt_end"
    __PaneaOsc "input_start"
    if (Test-Path function:\__PaneaOriginalPrompt) {
        & function:\__PaneaOriginalPrompt
    } else {
        "PS $PWD> "
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use semantics::SemanticEventKind;

    #[test]
    fn parses_osc_133_boundaries() {
        let position = BufferPosition::new(4, 2);
        let parsed = parse_osc_payload(b"133;D;7;42", position)
            .expect("parse")
            .expect("event");

        assert_eq!(
            parsed.event,
            SemanticEvent::CommandFinished {
                position,
                exit_status: CommandStatus::Code(7),
                duration: Duration::from_millis(42)
            }
        );
    }

    #[test]
    fn streaming_parser_handles_bel_and_st_terminated_osc() {
        let mut parser = SemanticEscapeParser::new();
        let position = BufferPosition::new(1, 0);
        let events = parser.parse(
            b"\x1b]777;input_start\x07text\x1b]777;output_start\x1b\\",
            position,
        );

        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].event,
            SemanticEvent::InputStarted { .. }
        ));
        assert!(matches!(
            events[1].event,
            SemanticEvent::OutputStarted { .. }
        ));
    }

    #[test]
    fn parses_cwd_and_shell_metadata() {
        let position = BufferPosition::new(0, 0);
        let cwd = parse_osc_payload(b"7;file://host/home/me/project%20x", position)
            .expect("parse")
            .expect("event");
        assert_eq!(
            cwd.event,
            SemanticEvent::CurrentWorkingDirectoryChanged {
                position,
                directory: "/home/me/project x".to_owned(),
                remote: false
            }
        );

        let shell = parse_osc_payload(b"777;shell;shell=bash;version=5.2", position)
            .expect("parse")
            .expect("event");
        assert!(matches!(
            shell.event,
            SemanticEvent::ShellMetadataChanged {
                metadata: ShellMetadata { shell: Some(_), .. },
                ..
            }
        ));
    }

    #[test]
    fn policy_can_disable_specific_profiles() {
        let policy = ShellIntegrationPolicy {
            disabled_profiles: vec!["plain".to_owned()],
            ..ShellIntegrationPolicy::default()
        };

        assert!(policy.should_enable_for_profile("default", ShellKind::Bash));
        assert!(!policy.should_enable_for_profile("plain", ShellKind::Bash));
        assert!(!policy.should_enable_for_profile("default", ShellKind::Cmd));
    }

    #[test]
    fn activation_plan_distinguishes_full_auto_manual_heuristic_and_off() {
        let full = activation_plan(
            &ShellIntegrationPolicy {
                activation: IntegrationActivation::Full,
                ..ShellIntegrationPolicy::default()
            },
            "default",
            ShellKind::Bash,
        );
        assert_eq!(full.mode, ShellIntegrationRuntimeMode::Full);
        assert_eq!(
            full.action,
            ShellIntegrationActivationAction::InjectRuntimeScript
        );
        assert!(full.script.is_some());

        let auto = activation_plan(
            &ShellIntegrationPolicy::default(),
            "default",
            ShellKind::Bash,
        );
        assert_eq!(auto.mode, ShellIntegrationRuntimeMode::Auto);
        assert_eq!(
            auto.action,
            ShellIntegrationActivationAction::DetectExisting
        );
        assert!(auto.parses_escape_sequences());

        let manual = activation_plan(
            &ShellIntegrationPolicy {
                activation: IntegrationActivation::Manual,
                ..ShellIntegrationPolicy::default()
            },
            "default",
            ShellKind::Bash,
        );
        assert_eq!(
            manual.action,
            ShellIntegrationActivationAction::ManualInstructions
        );
        assert!(
            manual
                .diagnostics
                .iter()
                .any(|line| line.contains("bashrc"))
        );

        let heuristic = activation_plan(
            &ShellIntegrationPolicy {
                activation: IntegrationActivation::Heuristic,
                ..ShellIntegrationPolicy::default()
            },
            "default",
            ShellKind::Bash,
        );
        assert_eq!(heuristic.mode, ShellIntegrationRuntimeMode::Heuristic);
        assert!(!heuristic.parses_escape_sequences());

        let off = activation_plan(
            &ShellIntegrationPolicy {
                enabled: false,
                ..ShellIntegrationPolicy::default()
            },
            "default",
            ShellKind::Bash,
        );
        assert_eq!(off.mode, ShellIntegrationRuntimeMode::Off);
        assert_eq!(off.action, ShellIntegrationActivationAction::Disabled);
    }

    #[test]
    fn auto_install_injects_script_only_for_supported_shells() {
        let policy = ShellIntegrationPolicy {
            auto_install: true,
            ..ShellIntegrationPolicy::default()
        };

        let bash = activation_plan(&policy, "default", ShellKind::Bash);
        assert_eq!(
            bash.action,
            ShellIntegrationActivationAction::InjectRuntimeScript
        );

        let cmd = activation_plan(&policy, "cmd", ShellKind::Cmd);
        assert_eq!(cmd.action, ShellIntegrationActivationAction::Unsupported);
        assert_eq!(cmd.mode, ShellIntegrationRuntimeMode::Off);
    }

    #[test]
    fn detects_shell_from_program_path() {
        assert_eq!(detect_shell_kind("/bin/bash"), ShellKind::Bash);
        assert_eq!(
            detect_shell_kind("C:\\Program Files\\PowerShell\\7\\pwsh.exe"),
            ShellKind::Pwsh
        );
        assert_eq!(detect_shell_kind("unknown-shell"), ShellKind::Unknown);
    }

    #[test]
    fn verification_sequence_contains_semantic_markers_and_visible_text() {
        let sequence =
            verification_sequence(ShellKind::Bash, "panea-shell-integration-smoke").unwrap();

        assert!(sequence.contains("777;shell"));
        assert!(sequence.contains("777;output_start"));
        assert!(sequence.contains("panea-shell-integration-smoke"));
    }

    #[test]
    fn scripts_exist_for_baseline_shells() {
        for shell in [
            ShellKind::Bash,
            ShellKind::Zsh,
            ShellKind::Fish,
            ShellKind::PowerShell,
        ] {
            let script = script_for_shell(shell).expect("script");
            assert!(script.contents.contains("777"));
            for marker in [
                "prompt_start",
                "prompt_end",
                "input_start",
                "input_end",
                "output_start",
                "output_end",
                "command_finished",
                "cwd",
                "shell",
            ] {
                assert!(
                    script.contents.contains(marker),
                    "{shell:?} script is missing {marker}"
                );
            }
        }
    }

    #[test]
    fn parsed_events_report_their_end_offset_in_the_current_batch() {
        let input = b"visible\x1b]777;output_start\x07more";
        let mut parser = SemanticEscapeParser::new();
        let events = parser.parse(input, BufferPosition::new(0, 0));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source_end, input.len() - 4);
        assert_eq!(events[0].event.kind(), SemanticEventKind::OutputStarted);
    }

    #[test]
    fn unterminated_semantic_osc_payload_is_bounded_and_dropped() {
        let mut parser = SemanticEscapeParser::new();
        let mut input = vec![ESC, b']'];
        input.extend(std::iter::repeat_n(b'a', MAX_OSC_PAYLOAD_BYTES + 512));
        input.extend_from_slice(b"\x07\x1b]777;shell;shell=bash\x07");

        let events = parser.parse(&input, BufferPosition::new(0, 0));

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].event.kind(),
            SemanticEventKind::ShellMetadataChanged
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        #[test]
        fn fuzz_semantic_escape_parser_never_panics(
            input in prop::collection::vec(any::<u8>(), 0..2048),
            row in 0_i64..4096,
            col in 0_u16..512,
        ) {
            let mut parser = SemanticEscapeParser::new();
            let events = parser.parse(&input, BufferPosition::new(row, col));
            for parsed in events {
                assert!(!parsed.raw_osc.is_empty());
            }
        }

        #[test]
        fn fuzz_semantic_osc_payload_parser_never_panics(
            payload in prop::collection::vec(any::<u8>(), 0..2048)
        ) {
            let _ = parse_osc_payload(&payload, BufferPosition::new(0, 0));
        }
    }
}
