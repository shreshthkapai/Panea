// Clipboard actions, paste safety, OSC 52 policy, and terminal response forwarding.

fn paste_bytes(
    text: &str,
    clipboard: &ClipboardConfig,
    legacy_paste: &PasteConfig,
    bracketed_mode: bool,
) -> Vec<u8> {
    let mut text = if clipboard.paste_protection && legacy_paste.normalize_newlines {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_owned()
    };

    if clipboard.paste_protection && legacy_paste.strip_control_characters {
        text.retain(|ch| ch == '\n' || ch == '\t' || !ch.is_control());
    }

    let mut bytes = Vec::new();
    if bracketed_mode && clipboard.bracketed_paste {
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
    } else {
        bytes.extend_from_slice(text.as_bytes());
    }

    bytes
}

fn should_middle_click_paste(
    mouse: &MouseEvent,
    modes: &BTreeSet<TerminalMode>,
    config: &ClipboardConfig,
) -> bool {
    config.enabled
        && config.middle_click_paste
        && !mouse_reporting_enabled(modes)
        && matches!(mouse.kind, MouseEventKind::Pressed(MouseButton::Middle))
}

fn paste_for_middle_click(
    clipboard: &mut ClipboardBridge,
    config: &ClipboardConfig,
) -> Result<String, platform_core::ClipboardDiagnostic> {
    if cfg!(target_os = "linux") && config.prefer_primary_selection_on_linux {
        match clipboard.paste_primary_text() {
            Ok(text) => return Ok(text),
            Err(diagnostic) => {
                eprintln!(
                    "Linux primary selection paste unavailable; falling back to system clipboard: {diagnostic:?}"
                );
            }
        }
    }
    clipboard.paste_text()
}

fn mouse_reporting_enabled(modes: &BTreeSet<TerminalMode>) -> bool {
    modes.contains(&TerminalMode::MouseReporting)
        || modes.contains(&TerminalMode::MouseCellMotion)
        || modes.contains(&TerminalMode::MouseAllMotion)
}

fn focus_report_bytes(focused: bool, modes: &BTreeSet<TerminalMode>) -> Option<&'static [u8]> {
    if !modes.contains(&TerminalMode::FocusEvents) {
        return None;
    }
    Some(if focused { b"\x1b[I" } else { b"\x1b[O" })
}

fn copy_text_with_diagnostics(
    clipboard: &mut ClipboardBridge,
    text: &str,
    config: &ClipboardConfig,
    source: &str,
) {
    match clipboard.copy_text(text) {
        Ok(()) if config.log_operations => {
            eprintln!(
                "clipboard {source}: wrote {} bytes to system clipboard",
                text.len()
            );
        }
        Ok(()) => {}
        Err(diagnostic) => eprintln!("clipboard {source} failed: {diagnostic:?}"),
    }
}

fn process_pending_clipboard_requests(
    terminal: &mut TerminalEmulator,
    clipboard: &mut ClipboardBridge,
    policy: &Osc52ClipboardPolicy,
    config: &ClipboardConfig,
    session_is_remote: bool,
    pending_prompt: &mut Option<Osc52PromptState>,
) {
    if !config.enabled {
        let dropped = terminal.state_mut().take_pending_clipboard_requests();
        if config.log_operations && !dropped.is_empty() {
            eprintln!(
                "clipboard osc52: dropped {} request(s) because clipboard is disabled",
                dropped.len()
            );
        }
        return;
    }

    for request in terminal.state_mut().take_pending_clipboard_requests() {
        let security_request = security_osc52_request(request, session_is_remote);
        match evaluate_osc52_clipboard_write(&security_request, policy) {
            Osc52ClipboardDecision::Allow { text, bytes } => {
                copy_osc52_text_with_diagnostics(
                    clipboard,
                    &text,
                    config,
                    security_request.target,
                    "OSC 52",
                );
                if config.log_operations {
                    eprintln!("clipboard OSC 52: accepted {bytes} byte request");
                }
            }
            Osc52ClipboardDecision::PromptRequired { reason, bytes } => {
                if pending_prompt.is_none() {
                    *pending_prompt = Some(Osc52PromptState {
                        request: security_request,
                        reason,
                        bytes,
                    });
                    eprintln!(
                        "clipboard OSC 52: remote write is waiting for explicit confirmation"
                    );
                } else {
                    eprintln!(
                        "clipboard OSC 52 denied: another remote clipboard decision is already pending"
                    );
                }
            }
            Osc52ClipboardDecision::Deny { reason } => {
                if config.log_operations {
                    eprintln!("clipboard OSC 52 denied: {reason}");
                }
            }
        }
    }
}

fn copy_osc52_text_with_diagnostics(
    clipboard: &mut ClipboardBridge,
    text: &str,
    config: &ClipboardConfig,
    target: Osc52ClipboardTarget,
    source: &str,
) {
    if matches!(target, Osc52ClipboardTarget::PrimarySelection) {
        match clipboard.copy_primary_text(text) {
            Ok(()) => {
                if config.log_operations {
                    eprintln!(
                        "clipboard {source}: wrote {} bytes to primary selection",
                        text.len()
                    );
                }
                return;
            }
            Err(diagnostic) => eprintln!(
                "clipboard {source}: primary selection unavailable; falling back to system clipboard: {diagnostic:?}"
            ),
        }
    }
    copy_text_with_diagnostics(clipboard, text, config, source);
}

fn security_osc52_request(request: Osc52ClipboardRequest, remote: bool) -> SecurityOsc52Request {
    SecurityOsc52Request {
        target: security_clipboard_target(request.target),
        payload_base64: request.payload_base64,
        remote,
    }
}

fn security_clipboard_target(target: ClipboardTarget) -> Osc52ClipboardTarget {
    match target {
        ClipboardTarget::Clipboard => Osc52ClipboardTarget::Clipboard,
        ClipboardTarget::PrimarySelection => Osc52ClipboardTarget::PrimarySelection,
        ClipboardTarget::Select => Osc52ClipboardTarget::Select,
        ClipboardTarget::Unknown(ch) => Osc52ClipboardTarget::Unknown(ch),
    }
}

fn osc52_policy(config: &ClipboardConfig) -> Osc52ClipboardPolicy {
    Osc52ClipboardPolicy {
        enabled: config.osc52.enabled,
        allow_local: config.osc52.allow_local,
        allow_remote: config.osc52.allow_remote,
        max_bytes: config.osc52.max_bytes,
        confirm_remote_writes: config.osc52.confirm_remote_writes,
    }
}

fn shutdown_transport(transport: Option<&mut PaneTransport>) {
    if let Some(transport) = transport {
        match catch_unwind(AssertUnwindSafe(|| transport.shutdown())) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("transport shutdown error: {error}"),
            Err(panic) => eprintln!(
                "transport shutdown panic boundary: {}",
                panic_payload(panic)
            ),
        }
    }
}

trait TerminalInputSink {
    fn write_terminal_bytes(&mut self, bytes: &[u8]) -> TransportResult<()>;
}

impl TerminalInputSink for PaneTransport {
    fn write_terminal_bytes(&mut self, bytes: &[u8]) -> TransportResult<()> {
        self.write_input(bytes)
    }
}

#[cfg(test)]
impl TerminalInputSink for LocalPtyTransport {
    fn write_terminal_bytes(&mut self, bytes: &[u8]) -> TransportResult<()> {
        self.write_input(bytes).map(|_| ())
    }
}

fn flush_terminal_responses<T>(terminal: &mut TerminalEmulator, transport: &mut T)
where
    T: TerminalInputSink + ?Sized,
{
    let responses = terminal.state_mut().take_pending_output();
    if !responses.is_empty() {
        write_transport_input(transport, &responses);
    }
}

fn write_terminal_input<T>(terminal: &mut TerminalEmulator, transport: &mut T, bytes: &[u8])
where
    T: TerminalInputSink + ?Sized,
{
    flush_terminal_responses(terminal, transport);
    write_transport_input(transport, bytes);
}

fn write_transport_input<T>(transport: &mut T, bytes: &[u8])
where
    T: TerminalInputSink + ?Sized,
{
    match catch_unwind(AssertUnwindSafe(|| transport.write_terminal_bytes(bytes))) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("transport input error: {error}"),
        Err(panic) => eprintln!("transport input panic boundary: {}", panic_payload(panic)),
    }
}
