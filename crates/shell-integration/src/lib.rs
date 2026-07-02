//! Shell integration hooks and semantic event contracts.

pub const LAYER: &str = "semantic meaning";

use std::time::Duration;

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
    AutoDetect,
    Manual,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegrationPolicy {
    pub activation: IntegrationActivation,
    pub enabled_shells: Vec<ShellKind>,
    pub disabled_profiles: Vec<String>,
    pub remote_instructions: bool,
}

impl Default for ShellIntegrationPolicy {
    fn default() -> Self {
        Self {
            activation: IntegrationActivation::AutoDetect,
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
        self.activation != IntegrationActivation::Disabled
            && self.enabled_shells.contains(&shell)
            && !self
                .disabled_profiles
                .iter()
                .any(|profile| profile == profile_name)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticParseError {
    UnsupportedOsc(String),
    InvalidPayload(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSemanticEvent {
    pub raw_osc: String,
    pub event: SemanticEvent,
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

        for byte in bytes {
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
                        if let Some(event) = parse_osc_payload(content, position).ok().flatten() {
                            events.push(event);
                        }
                        self.state = ParserState::Ground;
                    }
                    (b'\\', true) => {
                        if content.last() == Some(&ESC) {
                            content.pop();
                        }
                        if let Some(event) = parse_osc_payload(content, position).ok().flatten() {
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
__panea_prompt_start() { __panea_osc "prompt_start;shell=bash;cwd=$PWD"; }
__panea_prompt_end() { __panea_osc "prompt_end"; }
__panea_preexec() { __panea_osc "input_start"; }
__panea_precmd() {
  local status=$?
  __panea_osc "command_finished;status=$status"
  __panea_osc "cwd;path=$PWD"
}
PROMPT_COMMAND="__panea_precmd;__panea_prompt_start;${PROMPT_COMMAND:-};__panea_prompt_end"
trap '__panea_preexec' DEBUG
"#;

pub const ZSH_SCRIPT: &str = r#"# Panea shell integration for zsh.
__panea_osc() { printf '\033]777;%s\007' "$1"; }
precmd_functions+=(__panea_precmd)
preexec_functions+=(__panea_preexec)
__panea_precmd() {
  local status=$?
  __panea_osc "command_finished;status=$status"
  __panea_osc "cwd;path=$PWD"
  __panea_osc "prompt_start;shell=zsh;cwd=$PWD"
}
__panea_preexec() {
  __panea_osc "prompt_end"
  __panea_osc "input_start"
  __panea_osc "input_end"
  __panea_osc "output_start"
}
"#;

pub const FISH_SCRIPT: &str = r#"# Panea shell integration for fish.
function __panea_osc
    printf '\e]777;%s\a' $argv[1]
end
function __panea_prompt --on-event fish_prompt
    __panea_osc "prompt_start;shell=fish;cwd=$PWD"
end
function __panea_preexec --on-event fish_preexec
    __panea_osc "prompt_end"
    __panea_osc "input_start"
    __panea_osc "input_end"
    __panea_osc "output_start"
end
function __panea_postexec --on-event fish_postexec
    __panea_osc "command_finished;status=$status"
    __panea_osc "cwd;path=$PWD"
end
"#;

pub const POWERSHELL_SCRIPT: &str = r#"# Panea shell integration for PowerShell.
function global:__PaneaOsc([string] $Payload) {
    [Console]::Write("$([char]27)]777;$Payload$([char]7)")
}
if (Test-Path function:\prompt) {
    Copy-Item function:\prompt function:\__PaneaOriginalPrompt -Force
}
function global:prompt {
    $lastStatus = if ($?) { 0 } else { 1 }
    __PaneaOsc "command_finished;status=$lastStatus"
    __PaneaOsc "cwd;path=$PWD"
    __PaneaOsc "prompt_start;shell=powershell;cwd=$PWD"
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
    fn scripts_exist_for_baseline_shells() {
        for shell in [
            ShellKind::Bash,
            ShellKind::Zsh,
            ShellKind::Fish,
            ShellKind::PowerShell,
        ] {
            let script = script_for_shell(shell).expect("script");
            assert!(script.contents.contains("777"));
        }
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
