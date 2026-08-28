// Platform-neutral keyboard, IME, wheel, and binding translation.

#[derive(Debug, Clone, Copy, PartialEq)]
struct ImeCursorArea {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl ImeCursorArea {
    const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Debug, Default)]
struct ImeCursorAreaTracker {
    last: Option<ImeCursorArea>,
}

impl ImeCursorAreaTracker {
    fn update(&mut self, area: ImeCursorArea) -> bool {
        if self.last == Some(area) {
            return false;
        }
        self.last = Some(area);
        true
    }
}

/// Logical key names that are modifiers, never text.
///
/// They arrive as their own key events and stringify to their name, so any path
/// that turns a logical key into text has to reject them explicitly.
const MODIFIER_KEY_NAMES: [&str; 10] = [
    "Alt",
    "AltGraph",
    "Control",
    "Shift",
    "Super",
    "Meta",
    "Hyper",
    "CapsLock",
    "NumLock",
    "ScrollLock",
];

fn is_modifier_key_name(name: &str) -> bool {
    MODIFIER_KEY_NAMES.contains(&name)
}

/// Windows control-key state for a key event.
fn win32_control_key_state(event: &KeyEvent, enhanced: bool) -> u32 {
    let mut state = 0;
    if event.modifiers.shift {
        state |= WIN32_SHIFT_PRESSED;
    }
    // AltGr is reported by Windows as right Alt together with left Ctrl.
    if event.modifiers.alt_graph {
        state |= WIN32_RIGHT_ALT_PRESSED | WIN32_LEFT_CTRL_PRESSED;
    } else {
        if event.modifiers.alt {
            state |= WIN32_LEFT_ALT_PRESSED;
        }
        if event.modifiers.ctrl {
            state |= WIN32_LEFT_CTRL_PRESSED;
        }
    }
    if enhanced {
        state |= WIN32_ENHANCED_KEY;
    }
    state
}

/// The character a key event carries, as Windows would report it.
///
/// Keys that produce no character (navigation, function keys, bare modifiers)
/// report 0; the application reads the virtual key instead.
fn win32_unicode_text(event: &KeyEvent) -> Option<String> {
    let named = match event.logical_key.as_str() {
        "Enter" | "NumpadEnter" => {
            // Ctrl+Enter is LF where plain Enter is CR: the distinction legacy
            // encodings cannot carry.
            return Some(if event.modifiers.ctrl && !event.modifiers.alt_graph {
                "\n".to_owned()
            } else {
                "\r".to_owned()
            });
        }
        "Tab" => Some("\t"),
        "Backspace" => Some("\u{8}"),
        "Escape" => Some("\u{1b}"),
        "Space" => Some(" "),
        _ => None,
    };
    if let Some(text) = named {
        return Some(text.to_owned());
    }
    if is_modifier_key_name(event.logical_key.as_str()) {
        return None;
    }

    // Ctrl+letter carries the control character, matching Windows.
    if event.modifiers.ctrl
        && !event.modifiers.alt_graph
        && let Some(control) = control_character_for(event.logical_key.as_str())
    {
        return Some(control.to_string());
    }

    let candidate = event
        .text
        .as_deref()
        .filter(|text| !text.is_empty())
        .unwrap_or(event.logical_key.as_str());
    let mut chars = candidate.chars();
    let first = chars.next()?;
    // A multi-character logical name ("ArrowUp", "F5") is a key, not text.
    if chars.next().is_some() || first.is_control() && candidate.chars().count() > 1 {
        return None;
    }
    Some(first.to_string())
}

/// `Ctrl` plus a letter or the usual symbol set, as a control character.
fn control_character_for(logical: &str) -> Option<char> {
    let mut chars = logical.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let byte = match ch {
        'a'..='z' => ch as u8 - b'a' + 1,
        'A'..='Z' => ch as u8 - b'A' + 1,
        '@' | ' ' => 0x00,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' | '/' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    };
    Some(char::from(byte))
}

/// Encodes a key event as win32-input-mode records.
///
/// A character outside the basic multilingual plane needs two UTF-16 code units,
/// so it becomes two records — the same thing Windows does.
fn win32_input_records(event: &KeyEvent) -> Option<Vec<u8>> {
    let mapped = event
        .physical_key
        .as_deref()
        .and_then(win32_virtual_key);
    let enhanced = mapped.is_some_and(|key| key.enhanced);
    let control_key_state = win32_control_key_state(event, enhanced);
    let key_down = event.state == KeyState::Pressed;
    let text = win32_unicode_text(event);

    // No key and no character: nothing Windows would report.
    if mapped.is_none() && text.is_none() {
        return None;
    }

    let units: Vec<u16> = text
        .as_deref()
        .map(|text| text.encode_utf16().collect())
        .unwrap_or_default();

    let record_for = |unicode_char: u16| Win32InputRecord {
        virtual_key: mapped.map_or(0, |key| key.virtual_key),
        scan_code: mapped.map_or(0, |key| key.scan_code),
        unicode_char,
        key_down,
        control_key_state,
        repeat_count: 1,
    };

    let mut out = Vec::new();
    if units.is_empty() {
        out.extend_from_slice(&record_for(0).encode());
    } else {
        for unit in units {
            out.extend_from_slice(&record_for(unit).encode());
        }
    }
    Some(out)
}

/// Encodes a key for a terminal, honouring whichever input protocol it asked for.
///
/// win32-input-mode wins when set: an application that enabled it is reading
/// Windows console records, and legacy or kitty sequences would be discarded.
fn encode_key_for_terminal(terminal: &TerminalEmulator, event: &KeyEvent) -> Option<Vec<u8>> {
    if terminal
        .modes_ref()
        .contains(&TerminalMode::Win32InputMode)
    {
        return win32_input_records(event);
    }
    let key = terminal_key(event)?;
    let event_type = if event.state == KeyState::Released {
        TerminalKeyEventType::Release
    } else if event.repeat {
        TerminalKeyEventType::Repeat
    } else {
        TerminalKeyEventType::Press
    };
    encode_terminal_key_with_protocol(
        &key,
        terminal_modifiers(event.modifiers),
        terminal.modes_ref(),
        terminal.state().kitty_keyboard_flags(),
        event_type,
    )
}

fn terminal_key(event: &KeyEvent) -> Option<TerminalKey> {
    if let Some(keypad) = event.physical_key.as_deref().and_then(keypad_key) {
        return Some(TerminalKey::Keypad(keypad));
    }
    // A modifier on its own produces no terminal input; it only changes what the
    // next key means.
    if is_modifier_key_name(event.logical_key.as_str()) {
        return None;
    }

    let key = match event.logical_key.as_str() {
        "Enter" => TerminalKey::Enter,
        "Backspace" => TerminalKey::Backspace,
        "Tab" => TerminalKey::Tab,
        "Escape" => TerminalKey::Escape,
        "ArrowUp" => TerminalKey::Up,
        "ArrowDown" => TerminalKey::Down,
        "ArrowLeft" => TerminalKey::Left,
        "ArrowRight" => TerminalKey::Right,
        "Home" => TerminalKey::Home,
        "End" => TerminalKey::End,
        "Insert" => TerminalKey::Insert,
        "Delete" => TerminalKey::Delete,
        "PageUp" => TerminalKey::PageUp,
        "PageDown" => TerminalKey::PageDown,
        logical if logical.len() > 1 && logical.starts_with('F') => {
            TerminalKey::Function(logical[1..].parse().ok()?)
        }
        _ => TerminalKey::Character(terminal_character_text(event)?),
    };
    Some(key)
}

fn remember_consumed_key(consumed_keys: &mut HashSet<String>, event: &KeyEvent) {
    if let Some(physical_key) = &event.physical_key {
        consumed_keys.insert(physical_key.clone());
    }
}

fn take_consumed_key_release(consumed_keys: &mut HashSet<String>, event: &KeyEvent) -> bool {
    event
        .physical_key
        .as_ref()
        .is_some_and(|physical_key| consumed_keys.remove(physical_key))
}

fn terminal_character_text(event: &KeyEvent) -> Option<String> {
    if event.modifiers.ctrl && !event.modifiers.alt_graph {
        let logical = event.logical_key.as_str();
        if logical.chars().count() == 1 && logical.chars().next().is_some_and(|ch| !ch.is_control())
        {
            return Some(logical.to_owned());
        }
        return None;
    }

    if event.modifiers.alt && !event.modifiers.alt_graph {
        // Alt+key sends the *unmodified* key so Option+a is ESC a rather than
        // ESC å. Named keys stringify to their name here ("Alt", "Shift", "F5"),
        // so without a single-character guard a bare Alt press typed the word
        // "Alt" — once on press and again on release, while Alt was still held.
        let logical = event.logical_key_without_modifiers.as_str();
        return (logical.chars().count() == 1 && !logical.chars().any(char::is_control))
            .then(|| logical.to_owned());
    }

    if event.state == KeyState::Released {
        let logical = event.logical_key.as_str();
        return (logical.chars().count() == 1 && !logical.chars().any(char::is_control))
            .then(|| logical.to_owned());
    }

    event
        .text
        .as_ref()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
        .cloned()
}

fn scroll_delta_components(delta: MouseScrollDelta) -> (f64, f64) {
    match delta {
        MouseScrollDelta::Lines { x, y } | MouseScrollDelta::Pixels { x, y } => (x, y),
    }
}

fn accumulated_scroll_lines(
    delta: MouseScrollDelta,
    metrics: CellMetrics,
    remainder: &mut f64,
) -> i64 {
    const LINES_PER_WHEEL_STEP: f64 = 3.0;
    let (_, y) = scroll_delta_components(delta);
    let lines = match delta {
        MouseScrollDelta::Lines { .. } => y * LINES_PER_WHEEL_STEP,
        MouseScrollDelta::Pixels { .. } => y / f64::from(metrics.cell_height),
    };
    *remainder += lines;
    let whole = remainder.trunc() as i64;
    *remainder -= whole as f64;
    whole
}

fn keypad_key(physical_key: &str) -> Option<KeypadKey> {
    let name = physical_key
        .strip_prefix("Code(")
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(physical_key);
    match name {
        "Numpad0" => Some(KeypadKey::Digit(0)),
        "Numpad1" => Some(KeypadKey::Digit(1)),
        "Numpad2" => Some(KeypadKey::Digit(2)),
        "Numpad3" => Some(KeypadKey::Digit(3)),
        "Numpad4" => Some(KeypadKey::Digit(4)),
        "Numpad5" => Some(KeypadKey::Digit(5)),
        "Numpad6" => Some(KeypadKey::Digit(6)),
        "Numpad7" => Some(KeypadKey::Digit(7)),
        "Numpad8" => Some(KeypadKey::Digit(8)),
        "Numpad9" => Some(KeypadKey::Digit(9)),
        "NumpadDecimal" => Some(KeypadKey::Decimal),
        "NumpadDivide" => Some(KeypadKey::Divide),
        "NumpadMultiply" => Some(KeypadKey::Multiply),
        "NumpadSubtract" => Some(KeypadKey::Subtract),
        "NumpadAdd" => Some(KeypadKey::Add),
        "NumpadEnter" => Some(KeypadKey::Enter),
        "NumpadEqual" => Some(KeypadKey::Equal),
        _ => None,
    }
}

fn terminal_modifiers(modifiers: KeyModifiers) -> TerminalKeyModifiers {
    TerminalKeyModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        ctrl: modifiers.ctrl,
        super_key: modifiers.super_key,
        alt_graph: modifiers.alt_graph,
    }
}

fn keybinding_action(event: &KeyEvent, config: &AppConfig) -> Option<String> {
    if event.state != KeyState::Pressed {
        return None;
    }
    let event_key = canonical_key_event(event);
    config
        .keyboard
        .keybindings
        .iter()
        .find(|binding| canonical_key_spec(&binding.keys) == event_key)
        .map(|binding| binding.action.clone())
}

fn parse_send_bytes_action(action: &str) -> Result<Option<Vec<u8>>, String> {
    const PREFIX: &str = "send_bytes:";
    const MAX_BYTES: usize = 4096;

    let Some(payload) = action.strip_prefix(PREFIX) else {
        return Ok(None);
    };
    if !payload.is_ascii() {
        return Err("hex payload must contain only ASCII characters".to_owned());
    }

    let compact = payload
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    if compact.is_empty() {
        return Err("hex payload cannot be empty".to_owned());
    }
    if compact.len() % 2 != 0 {
        return Err("hex payload must contain complete byte pairs".to_owned());
    }
    if compact.len() / 2 > MAX_BYTES {
        return Err(format!("hex payload exceeds the {MAX_BYTES}-byte limit"));
    }

    let mut bytes = Vec::with_capacity(compact.len() / 2);
    for pair in compact.chunks_exact(2) {
        let pair = std::str::from_utf8(pair)
            .map_err(|_| "hex payload must contain only ASCII characters".to_owned())?;
        let byte = u8::from_str_radix(pair, 16)
            .map_err(|_| format!("invalid hexadecimal byte '{pair}'"))?;
        bytes.push(byte);
    }
    Ok(Some(bytes))
}

fn mousebinding_action(event: &MouseEvent, config: &config_core::MouseConfig) -> Option<String> {
    let event_gesture = canonical_mouse_event(event)?;
    config
        .bindings
        .iter()
        .find(|binding| canonical_mouse_spec(&binding.gesture).as_deref() == Some(&event_gesture))
        .map(|binding| binding.action.trim().to_ascii_lowercase())
}

fn canonical_mouse_event(event: &MouseEvent) -> Option<String> {
    let name = match event.kind {
        MouseEventKind::Pressed(button) => format!("{}press", mouse_button_name(button)?),
        MouseEventKind::Released(button) => format!("{}release", mouse_button_name(button)?),
        MouseEventKind::Wheel(delta) => {
            let (delta_x, delta_y) = scroll_delta_components(delta);
            if delta_y > 0.0 {
                "wheelup".to_owned()
            } else if delta_y < 0.0 {
                "wheeldown".to_owned()
            } else if delta_x > 0.0 {
                "wheelright".to_owned()
            } else if delta_x < 0.0 {
                "wheelleft".to_owned()
            } else {
                return None;
            }
        }
        MouseEventKind::Moved => return None,
    };
    Some(canonical_mouse_parts(event.modifiers, &name))
}

fn canonical_mouse_spec(spec: &str) -> Option<String> {
    let mut modifiers = KeyModifiers::default();
    let mut event = None;
    for part in spec.split('+') {
        let normalized = part.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "alt" | "option" => modifiers.alt = true,
            "shift" => modifiers.shift = true,
            "super" | "cmd" | "command" | "meta" => modifiers.super_key = true,
            _ if event.is_none() => event = Some(normalized),
            _ => return None,
        }
    }
    event.map(|event| canonical_mouse_parts(modifiers, &event))
}

fn canonical_mouse_parts(modifiers: KeyModifiers, event: &str) -> String {
    let mut parts = Vec::new();
    if modifiers.ctrl {
        parts.push("ctrl");
    }
    if modifiers.alt {
        parts.push("alt");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    if modifiers.super_key {
        parts.push("super");
    }
    parts.push(event);
    parts.join("+")
}

fn mouse_button_name(button: MouseButton) -> Option<&'static str> {
    match button {
        MouseButton::Left => Some("left"),
        MouseButton::Middle => Some("middle"),
        MouseButton::Right => Some("right"),
        MouseButton::Back => Some("back"),
        MouseButton::Forward => Some("forward"),
        MouseButton::Other(_) => None,
    }
}

fn canonical_key_event(event: &KeyEvent) -> String {
    let mut parts = Vec::new();
    if event.modifiers.ctrl {
        parts.push("ctrl".to_owned());
    }
    if event.modifiers.alt {
        parts.push("alt".to_owned());
    }
    if event.modifiers.shift {
        parts.push("shift".to_owned());
    }
    if event.modifiers.super_key {
        parts.push("super".to_owned());
    }
    parts.push(canonical_key_name(&event.logical_key));
    parts.join("+")
}

fn canonical_key_spec(spec: &str) -> String {
    let mut modifiers = BTreeSet::new();
    let mut key = String::new();
    for part in spec.split('+') {
        match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => {
                modifiers.insert("ctrl");
            }
            "alt" | "option" => {
                modifiers.insert("alt");
            }
            "shift" => {
                modifiers.insert("shift");
            }
            "super" | "cmd" | "command" | "meta" => {
                modifiers.insert("super");
            }
            other if !other.is_empty() => key = canonical_key_name(other),
            _ => {}
        }
    }

    let mut parts = modifiers.into_iter().collect::<Vec<_>>();
    parts.push(key.as_str());
    parts.join("+")
}

fn canonical_key_name(key: &str) -> String {
    match key.trim().to_ascii_lowercase().as_str() {
        "pagedown" | "page_down" | "page down" => "pagedown".to_owned(),
        "pageup" | "page_up" | "page up" => "pageup".to_owned(),
        "arrowleft" | "left" => "left".to_owned(),
        "arrowright" | "right" => "right".to_owned(),
        "arrowup" | "up" => "up".to_owned(),
        "arrowdown" | "down" => "down".to_owned(),
        " " | "space" => "space".to_owned(),
        other => other.to_owned(),
    }
}
