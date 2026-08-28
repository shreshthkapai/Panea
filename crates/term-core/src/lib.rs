//! Platform-neutral terminal state.

mod history;

pub use history::HistoryStats;

pub const LAYER: &str = "core correctness";

/// Retained scrollback lines when a host does not configure a limit. Matches the
/// shipped `scrollback.lines` default.
pub const DEFAULT_SCROLLBACK_LIMIT: usize = 10_000;

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
    ops::Deref,
    sync::Arc,
};

use history::{HistoryStore, HistoryStoreConfig};

use compact_str::CompactString;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

impl TerminalSize {
    #[must_use]
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    #[must_use]
    pub const fn normalized(self) -> Self {
        Self {
            cols: if self.cols == 0 { 1 } else { self.cols },
            rows: if self.rows == 0 { 1 } else { self.rows },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GridPosition {
    pub row: i64,
    pub col: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalKey {
    Character(String),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Function(u8),
    Keypad(KeypadKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeypadKey {
    Digit(u8),
    Decimal,
    Divide,
    Multiply,
    Subtract,
    Add,
    Enter,
    Equal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalKeyModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub super_key: bool,
    pub alt_graph: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKeyEventType {
    Press,
    Repeat,
    Release,
}

/// Encodes a platform-neutral key as the byte sequence expected by terminal applications.
#[must_use]
pub fn encode_terminal_key(
    key: &TerminalKey,
    modifiers: TerminalKeyModifiers,
    modes: &BTreeSet<TerminalMode>,
) -> Option<Vec<u8>> {
    encode_terminal_key_with_protocol(key, modifiers, modes, 0, TerminalKeyEventType::Press)
}

#[must_use]
pub fn encode_terminal_key_with_protocol(
    key: &TerminalKey,
    modifiers: TerminalKeyModifiers,
    modes: &BTreeSet<TerminalMode>,
    kitty_flags: u16,
    event_type: TerminalKeyEventType,
) -> Option<Vec<u8>> {
    let effective_ctrl = modifiers.ctrl && !modifiers.alt_graph;
    let effective_alt = modifiers.alt && !modifiers.alt_graph;
    // An application that enables win32-input-mode is targeting the Windows
    // native input contract. The only terminal that implements that mode does
    // not implement the kitty protocol, so no such application has ever been
    // sent `CSI u` sequences and Windows-native multiplexers cannot parse them:
    // with flags 9 honoured, every keystroke was silently dropped. Win32 input
    // mode therefore takes precedence, and keys fall back to legacy encodings
    // that those applications do parse.
    let win32_input_mode = modes.contains(&TerminalMode::Win32InputMode);
    let kitty_protocol_enabled = kitty_flags & (0b1 | 0b1000) != 0 && !win32_input_mode;
    // Flag 1 is "disambiguate escape codes": every key that already has an
    // unambiguous legacy encoding must keep it, and `CSI u` is used only where
    // legacy cannot express the key. Routing all special and modified keys
    // through `CSI u` under flag 1 sent `CSI 98;5u` for Ctrl+B and
    // `CSI 57352u` for a plain Up, so a multiplexer never saw its prefix and
    // navigation keys did nothing. Only flag 8, "report all keys as escape
    // codes", asks for everything in that form.
    let legacy_encoding_exists = match key {
        // The ambiguity flag 1 exists to resolve: bare Esc against an Alt prefix.
        TerminalKey::Escape => false,
        // Ctrl and Alt have legacy forms; Super has none.
        TerminalKey::Character(_) => !modifiers.super_key,
        // Legacy has no room for Shift/Ctrl/Super on these.
        TerminalKey::Enter | TerminalKey::Backspace | TerminalKey::Tab => {
            !(modifiers.shift || modifiers.super_key || effective_ctrl)
        }
        // F13 and above have no dependable legacy encoding; F1-F12 do.
        TerminalKey::Function(number) => *number <= 12 && !modifiers.super_key,
        // Cursor and editing keys take a modifier parameter.
        _ => !modifiers.super_key,
    };
    let kitty_candidate = !matches!(key, TerminalKey::Keypad(_))
        && (kitty_flags & 0b1000 != 0 || !legacy_encoding_exists);
    if kitty_protocol_enabled && kitty_candidate {
        if event_type == TerminalKeyEventType::Release && kitty_flags & 0b10 == 0 {
            return None;
        }
        if let Some(encoded) = encode_kitty_key(key, modifiers, event_type, kitty_flags & 0b10 != 0)
        {
            return Some(encoded);
        }
    }
    if event_type == TerminalKeyEventType::Release {
        return None;
    }

    if modifiers.super_key {
        return None;
    }

    let modifier_parameter =
        1 + u8::from(modifiers.shift) + 2 * u8::from(effective_alt) + 4 * u8::from(effective_ctrl);

    let mut encoded = match key {
        TerminalKey::Character(text) => {
            if text.is_empty() {
                return None;
            }
            if effective_ctrl {
                encode_control_character(text)?
            } else {
                text.as_bytes().to_vec()
            }
        }
        TerminalKey::Enter => vec![b'\r'],
        TerminalKey::Backspace if effective_ctrl => vec![0x08],
        TerminalKey::Backspace => vec![0x7f],
        TerminalKey::Tab if modifiers.shift => b"\x1b[Z".to_vec(),
        TerminalKey::Tab => vec![b'\t'],
        TerminalKey::Escape => vec![0x1b],
        TerminalKey::Up => encode_cursor_key('A', modifier_parameter, modes),
        TerminalKey::Down => encode_cursor_key('B', modifier_parameter, modes),
        TerminalKey::Right => encode_cursor_key('C', modifier_parameter, modes),
        TerminalKey::Left => encode_cursor_key('D', modifier_parameter, modes),
        TerminalKey::Home => encode_cursor_key('H', modifier_parameter, modes),
        TerminalKey::End => encode_cursor_key('F', modifier_parameter, modes),
        TerminalKey::Insert => encode_tilde_key(2, modifier_parameter),
        TerminalKey::Delete => encode_tilde_key(3, modifier_parameter),
        TerminalKey::PageUp => encode_tilde_key(5, modifier_parameter),
        TerminalKey::PageDown => encode_tilde_key(6, modifier_parameter),
        TerminalKey::Function(number) => encode_function_key(*number, modifier_parameter)?,
        TerminalKey::Keypad(key) => {
            encode_keypad_key(*key, modes.contains(&TerminalMode::ApplicationKeypad))?
        }
    };

    if effective_alt
        && matches!(
            key,
            TerminalKey::Character(_)
                | TerminalKey::Enter
                | TerminalKey::Backspace
                | TerminalKey::Tab
                | TerminalKey::Escape
                | TerminalKey::Keypad(_)
        )
    {
        encoded.insert(0, 0x1b);
    }
    Some(encoded)
}

fn encode_kitty_key(
    key: &TerminalKey,
    modifiers: TerminalKeyModifiers,
    event_type: TerminalKeyEventType,
    report_event: bool,
) -> Option<Vec<u8>> {
    let codepoint = match key {
        TerminalKey::Character(text) => {
            let mut chars = text.chars();
            let codepoint = chars.next()? as u32;
            if chars.next().is_some() {
                return None;
            }
            codepoint
        }
        TerminalKey::Enter => 13,
        TerminalKey::Backspace => 127,
        TerminalKey::Tab => 9,
        TerminalKey::Escape => 27,
        TerminalKey::Insert => 57_348,
        TerminalKey::Delete => 57_349,
        TerminalKey::Left => 57_350,
        TerminalKey::Right => 57_351,
        TerminalKey::Up => 57_352,
        TerminalKey::Down => 57_353,
        TerminalKey::PageUp => 57_354,
        TerminalKey::PageDown => 57_355,
        TerminalKey::Home => 57_356,
        TerminalKey::End => 57_357,
        TerminalKey::Function(number @ 1..=24) => 57_363 + u32::from(*number),
        TerminalKey::Function(_) | TerminalKey::Keypad(_) => return None,
    };
    let modifier = 1
        + u8::from(modifiers.shift)
        + 2 * u8::from(modifiers.alt && !modifiers.alt_graph)
        + 4 * u8::from(modifiers.ctrl && !modifiers.alt_graph)
        + 8 * u8::from(modifiers.super_key);
    let event = match event_type {
        TerminalKeyEventType::Press => 1,
        TerminalKeyEventType::Repeat => 2,
        TerminalKeyEventType::Release => 3,
    };
    if report_event && event != 1 {
        Some(format!("\x1b[{codepoint};{modifier}:{event}u").into_bytes())
    } else if modifier != 1 {
        Some(format!("\x1b[{codepoint};{modifier}u").into_bytes())
    } else {
        Some(format!("\x1b[{codepoint}u").into_bytes())
    }
}

fn encode_control_character(text: &str) -> Option<Vec<u8>> {
    let ch = text.chars().next()?;
    if text.chars().nth(1).is_some() {
        return None;
    }
    let byte = match ch {
        '@' | ' ' | '2' => 0x00,
        'a'..='z' => ch as u8 - b'a' + 1,
        'A'..='Z' => ch as u8 - b'A' + 1,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    };
    Some(vec![byte])
}

fn encode_cursor_key(final_byte: char, modifier: u8, modes: &BTreeSet<TerminalMode>) -> Vec<u8> {
    if modifier == 1 {
        let prefix = if modes.contains(&TerminalMode::ApplicationCursorKeys) {
            "\x1bO"
        } else {
            "\x1b["
        };
        format!("{prefix}{final_byte}").into_bytes()
    } else {
        format!("\x1b[1;{modifier}{final_byte}").into_bytes()
    }
}

fn encode_tilde_key(number: u8, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{modifier}~").into_bytes()
    }
}

fn encode_function_key(number: u8, modifier: u8) -> Option<Vec<u8>> {
    if (1..=4).contains(&number) {
        let final_byte = char::from(b'P' + number - 1);
        return Some(if modifier == 1 {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[1;{modifier}{final_byte}").into_bytes()
        });
    }
    let number = match number {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        13 => 25,
        14 => 26,
        15 => 28,
        16 => 29,
        17 => 31,
        18 => 32,
        19 => 33,
        20 => 34,
        21 => 42,
        22 => 43,
        23 => 44,
        24 => 45,
        _ => return None,
    };
    Some(encode_tilde_key(number, modifier))
}

fn encode_keypad_key(key: KeypadKey, application: bool) -> Option<Vec<u8>> {
    if !application {
        let text = match key {
            KeypadKey::Digit(value @ 0..=9) => value.to_string(),
            KeypadKey::Digit(_) => return None,
            KeypadKey::Decimal => ".".to_owned(),
            KeypadKey::Divide => "/".to_owned(),
            KeypadKey::Multiply => "*".to_owned(),
            KeypadKey::Subtract => "-".to_owned(),
            KeypadKey::Add => "+".to_owned(),
            KeypadKey::Enter => "\r".to_owned(),
            KeypadKey::Equal => "=".to_owned(),
        };
        return Some(text.into_bytes());
    }

    let final_byte = match key {
        KeypadKey::Digit(value @ 0..=9) => char::from(b'p' + value),
        KeypadKey::Digit(_) => return None,
        KeypadKey::Decimal => 'n',
        KeypadKey::Divide => 'o',
        KeypadKey::Multiply => 'j',
        KeypadKey::Subtract => 'm',
        KeypadKey::Add => 'k',
        KeypadKey::Enter => 'M',
        KeypadKey::Equal => 'X',
    };
    Some(format!("\x1bO{final_byte}").into_bytes())
}

fn mode_to_terminal_mode(private: bool, mode: u16) -> Option<TerminalMode> {
    if !private {
        return match mode {
            4 => Some(TerminalMode::Insert),
            20 => Some(TerminalMode::LineFeedNewLine),
            _ => None,
        };
    }

    match mode {
        1 => Some(TerminalMode::ApplicationCursorKeys),
        6 => Some(TerminalMode::Origin),
        7 => Some(TerminalMode::AutoWrap),
        12 => Some(TerminalMode::CursorBlinking),
        47 | 1047 | 1049 => Some(TerminalMode::AlternateScreen),
        66 => Some(TerminalMode::ApplicationKeypad),
        1000 => Some(TerminalMode::MouseReporting),
        1002 => Some(TerminalMode::MouseCellMotion),
        1003 => Some(TerminalMode::MouseAllMotion),
        1004 => Some(TerminalMode::FocusEvents),
        1005 => Some(TerminalMode::Utf8Mouse),
        1006 => Some(TerminalMode::SgrMouse),
        1015 => Some(TerminalMode::UrxvtMouse),
        2004 => Some(TerminalMode::BracketedPaste),
        2026 => Some(TerminalMode::SynchronizedOutput),
        9001 => Some(TerminalMode::Win32InputMode),
        _ => None,
    }
}

impl GridPosition {
    #[must_use]
    pub const fn new(row: i64, col: u16) -> Self {
        Self { row, col }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Indexed(u8),
    Rgb { red: u8, green: u8, blue: u8 },
    DefaultForeground,
    DefaultBackground,
}

pub type TerminalColor = Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellAttributes {
    pub foreground: Option<Color>,
    pub background: Option<Color>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub underline_style: UnderlineStyle,
    pub underline_color: Option<Color>,
    pub inverse: bool,
    pub strikethrough: bool,
    pub blink: bool,
    pub hidden: bool,
    pub overline: bool,
    pub hyperlink_id: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl CellAttributes {
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Attributes an erased cell keeps.
    ///
    /// Erasing and scrolling honour the current colours (back-colour erase) but
    /// not the graphic rendition: `ESC[7m ESC[K` must clear to the end of the
    /// line, not paint a full-width inverse bar, and `ESC[4m` followed by a
    /// newline must not produce underlined blank lines. xterm and Alacritty both
    /// drop the flags here.
    #[must_use]
    pub fn erase_template(self) -> Self {
        Self {
            foreground: self.foreground,
            background: self.background,
            underline_color: None,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            underline_style: UnderlineStyle::None,
            inverse: false,
            strikethrough: false,
            blink: false,
            hidden: false,
            overline: false,
            hyperlink_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: CompactString,
    pub attributes: CellAttributes,
    pub width: u8,
    pub wide_continuation: bool,
    pub hyperlink_id: Option<u32>,
}

impl Cell {
    #[must_use]
    pub fn blank(attributes: CellAttributes) -> Self {
        // A blank cell has no glyph, so it keeps only the colours it was erased
        // with. See `CellAttributes::erase_template`.
        let attributes = attributes.erase_template();
        Self {
            text: CompactString::new(" "),
            attributes,
            width: 1,
            wide_continuation: false,
            hyperlink_id: attributes.hyperlink_id,
        }
    }

    #[must_use]
    pub fn text(text: impl AsRef<str>, attributes: CellAttributes) -> Self {
        let text = CompactString::new(text);
        let width = cell_width_for_text(&text) as u8;
        Self {
            text,
            attributes,
            width,
            wide_continuation: false,
            hyperlink_id: attributes.hyperlink_id,
        }
    }

    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.text == " "
            && self.attributes == CellAttributes::default()
            && self.width == 1
            && !self.wide_continuation
    }

    #[must_use]
    pub fn wide_continuation(attributes: CellAttributes) -> Self {
        Self {
            text: CompactString::new(" "),
            attributes,
            width: 0,
            wide_continuation: true,
            hyperlink_id: attributes.hyperlink_id,
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank(CellAttributes::default())
    }
}

pub type TerminalCell = Cell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    pub position: GridPosition,
    pub shape: CursorShape,
    pub visible: bool,
    pub blinking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TerminalMode {
    AlternateScreen,
    BracketedPaste,
    ApplicationCursorKeys,
    ApplicationKeypad,
    MouseReporting,
    MouseCellMotion,
    MouseAllMotion,
    SgrMouse,
    Utf8Mouse,
    UrxvtMouse,
    FocusEvents,
    Origin,
    Insert,
    AutoWrap,
    CursorBlinking,
    LineFeedNewLine,
    SynchronizedOutput,
    /// `DECSET 9001`: the application asked for Windows win32-input-mode key
    /// records, the ConPTY input contract Windows Terminal speaks.
    Win32InputMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    Normal,
    Rectangular,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: GridPosition,
    pub end: GridPosition,
    pub kind: SelectionKind,
}

impl Selection {
    #[must_use]
    pub const fn normal(start: GridPosition, end: GridPosition) -> Self {
        Self {
            start,
            end,
            kind: SelectionKind::Normal,
        }
    }

    #[must_use]
    pub const fn rectangular(start: GridPosition, end: GridPosition) -> Self {
        Self {
            start,
            end,
            kind: SelectionKind::Rectangular,
        }
    }

    /// Projects this selection to one absolute buffer row as an inclusive
    /// column span, avoiding a per-cell containment test during rendering.
    #[must_use]
    pub fn span_for_row(self, row: i64, cols: u16) -> Option<(u16, u16)> {
        if cols == 0 {
            return None;
        }
        let (start, end) = if self.start <= self.end {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        };
        if row < start.row || row > end.row {
            return None;
        }

        let last_col = cols - 1;
        let span = match self.kind {
            SelectionKind::Rectangular => {
                let left = self.start.col.min(self.end.col).min(last_col);
                let right = self.start.col.max(self.end.col).min(last_col);
                (left, right)
            }
            SelectionKind::Normal if start.row == end.row => {
                (start.col.min(last_col), end.col.min(last_col))
            }
            SelectionKind::Normal if row == start.row => (start.col.min(last_col), last_col),
            SelectionKind::Normal if row == end.row => (0, end.col.min(last_col)),
            SelectionKind::Normal => (0, last_col),
        };
        Some(span)
    }
}

pub type SelectionRange = Selection;

#[derive(Debug, Clone)]
pub struct Line {
    pub cells: Vec<Cell>,
    pub hard_wrapped: bool,
    generation: u64,
}

impl PartialEq for Line {
    fn eq(&self, other: &Self) -> bool {
        self.cells == other.cells && self.hard_wrapped == other.hard_wrapped
    }
}

impl Eq for Line {}

impl Line {
    #[must_use]
    pub fn blank(cols: u16) -> Self {
        Self::blank_with_attributes(cols, CellAttributes::default())
    }

    #[must_use]
    pub fn blank_with_attributes(cols: u16, attributes: CellAttributes) -> Self {
        Self {
            cells: vec![Cell::blank(attributes); usize::from(cols.max(1))],
            hard_wrapped: false,
            generation: 0,
        }
    }

    #[must_use]
    pub fn raw_text(&self) -> String {
        let end = self
            .cells
            .iter()
            .rposition(|cell| cell.text != " ")
            .map_or(0, |index| index + 1);

        self.cells
            .iter()
            .take(end)
            .filter(|cell| !cell.wide_continuation)
            .map(|cell| cell.text.as_str())
            .collect()
    }

    fn resize_to(&mut self, cols: u16, attributes: CellAttributes) {
        let cols = usize::from(cols.max(1));
        match self.cells.len().cmp(&cols) {
            Ordering::Less => self.cells.resize_with(cols, || Cell::blank(attributes)),
            Ordering::Greater => self.cells.truncate(cols),
            Ordering::Equal => {}
        }
        sanitize_cells(&mut self.cells, attributes);
    }
}

impl Default for Line {
    fn default() -> Self {
        Self::blank(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub origin_row: i64,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    pub size: TerminalSize,
    pub viewport: Viewport,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleGrid {
    pub viewport: Viewport,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone)]
pub enum VisibleCells<'a> {
    Borrowed(&'a [Cell]),
    Shared(Arc<Line>),
}

impl Deref for VisibleCells<'_> {
    type Target = [Cell];

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(cells) => cells,
            Self::Shared(line) => &line.cells,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VisibleRow<'a> {
    pub absolute_row: i64,
    pub generation: u64,
    pub cells: VisibleCells<'a>,
}

#[derive(Debug, Clone)]
pub enum TerminalLine<'a> {
    Borrowed(&'a Line),
    Shared(Arc<Line>),
}

impl Deref for TerminalLine<'_> {
    type Target = Line;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Borrowed(line) => line,
            Self::Shared(line) => line,
        }
    }
}

/// Windows `ControlKeyState` bits, as `KEY_EVENT_RECORD` reports them.
pub const WIN32_RIGHT_ALT_PRESSED: u32 = 0x0001;
pub const WIN32_LEFT_ALT_PRESSED: u32 = 0x0002;
pub const WIN32_RIGHT_CTRL_PRESSED: u32 = 0x0004;
pub const WIN32_LEFT_CTRL_PRESSED: u32 = 0x0008;
pub const WIN32_SHIFT_PRESSED: u32 = 0x0010;
pub const WIN32_NUMLOCK_ON: u32 = 0x0020;
pub const WIN32_SCROLLLOCK_ON: u32 = 0x0040;
pub const WIN32_CAPSLOCK_ON: u32 = 0x0080;
/// Set for keys that arrive with an `0xE0` scan-code prefix: the navigation
/// cluster, the arrow keys, right Ctrl/Alt, and numpad Enter and divide.
pub const WIN32_ENHANCED_KEY: u32 = 0x0100;

/// One Windows console key event, as win32-input-mode transmits it.
///
/// `DECSET 9001` asks the terminal to stop sending VT byte sequences and send
/// `KEY_EVENT_RECORD`s instead, so an application reading through ConPTY sees
/// what `ReadConsoleInput` would have given it. Legacy VT encodings cannot
/// express key releases, bare modifiers, or the difference between Enter,
/// Shift+Enter and Ctrl+Enter; these records carry all of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Win32InputRecord {
    /// Windows virtual-key code, or 0 for a character with no key behind it.
    pub virtual_key: u16,
    /// PC set-1 scan code, or 0 when unknown.
    pub scan_code: u16,
    /// A single UTF-16 code unit, or 0 for keys that produce no character.
    pub unicode_char: u16,
    pub key_down: bool,
    /// Bitwise-or of the `WIN32_*` state flags above.
    pub control_key_state: u32,
    pub repeat_count: u16,
}

impl Win32InputRecord {
    /// `CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`, every field in decimal.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        format!(
            "\x1b[{};{};{};{};{};{}_",
            self.virtual_key,
            self.scan_code,
            self.unicode_char,
            u8::from(self.key_down),
            self.control_key_state,
            self.repeat_count.max(1),
        )
        .into_bytes()
    }
}

/// Opaque marker of terminal content, comparable for equality only.
///
/// Callers cache results derived from the buffer against it - a search hit list,
/// a memory measurement - and recompute only when it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRevision {
    line_generation: u64,
    scrollback_rows: usize,
    scrollback_dropped: u64,
    alternate_screen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scrollback {
    pub lines: VecDeque<Line>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorDirection {
    Up,
    Down,
    Forward,
    Back,
    NextLine,
    PreviousLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearMode {
    FromCursor,
    ToCursor,
    All,
    Saved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicRendition {
    Reset,
    Bold,
    Dim,
    NormalIntensity,
    Italic,
    NoItalic,
    Underline,
    UnderlineStyle(UnderlineStyle),
    NoUnderline,
    UnderlineColor(Color),
    DefaultUnderlineColor,
    Inverse,
    NoInverse,
    Strikethrough,
    NoStrikethrough,
    Blink,
    NoBlink,
    Hidden,
    NoHidden,
    Overline,
    NoOverline,
    Foreground(Color),
    Background(Color),
    DefaultForeground,
    DefaultBackground,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardTarget {
    Clipboard,
    PrimarySelection,
    Select,
    Unknown(char),
}

impl ClipboardTarget {
    #[must_use]
    pub const fn from_osc52_selector(selector: char) -> Self {
        match selector {
            'c' | 'C' => Self::Clipboard,
            'p' | 'P' => Self::PrimarySelection,
            's' | 'S' => Self::Select,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Osc52ClipboardRequest {
    pub target: ClipboardTarget,
    pub payload_base64: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyKeyboardMode {
    Set,
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalAction {
    Print(char),
    CarriageReturn,
    LineFeed,
    NextLine,
    ReverseIndex,
    Backspace,
    Tab,
    BackTab(u16),
    MoveCursor {
        direction: CursorDirection,
        count: u16,
    },
    SetCursorPosition {
        row: u16,
        col: u16,
    },
    SetCursorColumn(u16),
    SetCursorRow(u16),
    SaveCursor,
    RestoreCursor,
    ClearScreen(ClearMode),
    ClearLine(ClearMode),
    InsertLines(u16),
    DeleteLines(u16),
    InsertChars(u16),
    DeleteChars(u16),
    EraseChars(u16),
    RepeatLastPrinted(u16),
    ScrollUp(u16),
    ScrollDown(u16),
    SetGraphicRendition(GraphicRendition),
    SetMode {
        mode: TerminalMode,
        enabled: bool,
    },
    SetCursorVisible(bool),
    SetCursorShape(CursorShape),
    SetTitle(String),
    SetHyperlink {
        id: Option<String>,
        uri: Option<String>,
    },
    Osc52Clipboard(Osc52ClipboardRequest),
    SetTabStop,
    ClearTabStop,
    ClearAllTabStops,
    DeviceStatusReport(u16),
    PrivateDeviceStatusReport(u16),
    PrimaryDeviceAttributes,
    SecondaryDeviceAttributes,
    TerminalVersion,
    KittyKeyboardStatus,
    SetKittyKeyboardFlags {
        flags: u16,
        mode: KittyKeyboardMode,
    },
    PushKittyKeyboardFlags(u16),
    PopKittyKeyboardFlags(u16),
    RequestMode {
        private: bool,
        mode: u16,
    },
    RequestDynamicColor(u8),
    RequestStatusString(String),
    ScreenAlignmentTest,
    SetScrollRegion {
        top: u16,
        bottom: u16,
    },
    ResetScrollRegion,
    Reset,
    SoftReset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalState {
    primary: ScreenBuffer,
    alternate: Option<ScreenBuffer>,
    attributes: CellAttributes,
    cursor_shape: CursorShape,
    cursor_visible: bool,
    cursor_blinking: bool,
    modes: BTreeSet<TerminalMode>,
    selection: Option<Selection>,
    viewport_offset: usize,
    tab_stops: BTreeSet<u16>,
    tab_stops_modified: bool,
    pending_output: Vec<u8>,
    pending_clipboard_requests: Vec<Osc52ClipboardRequest>,
    title: Option<String>,
    last_printed: Option<char>,
    hyperlinks: BTreeMap<u32, String>,
    hyperlink_keys: BTreeMap<String, u32>,
    next_hyperlink_id: u32,
    dynamic_foreground: [u8; 3],
    dynamic_background: [u8; 3],
    /// Monotonic presentation revision. Renderers use this as a cheap first
    /// check before consulting per-row generations; it never carries renderer
    /// state into the terminal model.
    render_revision: u64,
}

impl TerminalState {
    #[must_use]
    pub fn new(size: TerminalSize) -> Self {
        Self::with_scrollback_limit(size, DEFAULT_SCROLLBACK_LIMIT)
    }

    /// Builds a terminal that retains at most `scrollback_limit` scrolled-off
    /// lines.
    #[must_use]
    pub fn with_scrollback_limit(size: TerminalSize, scrollback_limit: usize) -> Self {
        let mut state = Self::new_unlimited(size);
        state.primary.scrollback_limit = scrollback_limit;
        state
    }

    fn new_unlimited(size: TerminalSize) -> Self {
        let size = size.normalized();
        let mut modes = BTreeSet::new();
        modes.insert(TerminalMode::AutoWrap);

        Self {
            primary: ScreenBuffer::new(size),
            alternate: None,
            attributes: CellAttributes::default(),
            cursor_shape: CursorShape::Block,
            cursor_visible: true,
            cursor_blinking: true,
            modes,
            selection: None,
            viewport_offset: 0,
            tab_stops: default_tab_stops(size.cols),
            tab_stops_modified: false,
            pending_output: Vec::new(),
            pending_clipboard_requests: Vec::new(),
            title: None,
            last_printed: None,
            hyperlinks: BTreeMap::new(),
            hyperlink_keys: BTreeMap::new(),
            next_hyperlink_id: 1,
            dynamic_foreground: [u8::MAX; 3],
            dynamic_background: [0; 3],
            render_revision: 1,
        }
    }

    #[must_use]
    pub const fn render_revision(&self) -> u64 {
        self.render_revision
    }

    fn bump_render_revision(&mut self) {
        self.render_revision = self.render_revision.wrapping_add(1).max(1);
    }

    /// Sets the RGB colors reported by OSC 10/11 queries.
    pub fn set_dynamic_colors(&mut self, foreground: [u8; 3], background: [u8; 3]) {
        if self.dynamic_foreground == foreground && self.dynamic_background == background {
            return;
        }
        self.dynamic_foreground = foreground;
        self.dynamic_background = background;
        self.bump_render_revision();
    }

    pub fn apply_action(&mut self, action: TerminalAction) -> TerminalResult<()> {
        let scrollback_before = self.primary.scroll_state();
        match action {
            TerminalAction::Print(ch) => self.print(ch),
            TerminalAction::CarriageReturn => self.active_mut().carriage_return(),
            TerminalAction::LineFeed => {
                self.line_feed();
                if self.modes.contains(&TerminalMode::LineFeedNewLine) {
                    self.active_mut().carriage_return();
                }
            }
            TerminalAction::NextLine => {
                self.active_mut().carriage_return();
                self.line_feed();
            }
            TerminalAction::ReverseIndex => {
                let attributes = self.attributes;
                self.active_mut().reverse_index(attributes);
            }
            TerminalAction::Backspace => self.active_mut().backspace(),
            TerminalAction::Tab => self.tab(),
            TerminalAction::BackTab(count) => self.back_tab(count),
            TerminalAction::MoveCursor { direction, count } => {
                let origin = self.modes.contains(&TerminalMode::Origin);
                self.active_mut()
                    .move_cursor(direction, count.max(1), origin);
            }
            TerminalAction::SetCursorPosition { row, col } => {
                let origin = self.modes.contains(&TerminalMode::Origin);
                self.active_mut().set_cursor_position(row, col, origin);
            }
            TerminalAction::SetCursorColumn(col) => self.active_mut().set_cursor_column(col),
            TerminalAction::SetCursorRow(row) => {
                let origin = self.modes.contains(&TerminalMode::Origin);
                self.active_mut().set_cursor_row(row, origin);
            }
            TerminalAction::SaveCursor => self.save_cursor(),
            TerminalAction::RestoreCursor => self.restore_cursor(),
            TerminalAction::ClearScreen(mode) => self.clear_screen(mode),
            TerminalAction::ClearLine(mode) => self.clear_line(mode),
            TerminalAction::InsertLines(count) => self.insert_lines(count),
            TerminalAction::DeleteLines(count) => self.delete_lines(count),
            TerminalAction::InsertChars(count) => self.insert_chars(count),
            TerminalAction::DeleteChars(count) => self.delete_chars(count),
            TerminalAction::EraseChars(count) => self.erase_chars(count),
            TerminalAction::RepeatLastPrinted(count) => self.repeat_last_printed(count),
            TerminalAction::ScrollUp(count) => {
                let attributes = self.attributes;
                self.active_mut()
                    .scroll_up_explicit(count.max(1), attributes);
            }
            TerminalAction::ScrollDown(count) => {
                let attributes = self.attributes;
                self.active_mut()
                    .scroll_down_explicit(count.max(1), attributes);
            }
            TerminalAction::SetGraphicRendition(rendition) => self.apply_sgr(rendition),
            TerminalAction::SetMode { mode, enabled } => self.set_mode(mode, enabled),
            TerminalAction::SetCursorVisible(visible) => self.cursor_visible = visible,
            TerminalAction::SetCursorShape(shape) => self.cursor_shape = shape,
            TerminalAction::SetTitle(title) => self.title = Some(title),
            TerminalAction::SetHyperlink { id, uri } => self.set_hyperlink(id, uri),
            TerminalAction::Osc52Clipboard(request) => {
                self.pending_clipboard_requests.push(request)
            }
            TerminalAction::SetTabStop => {
                self.tab_stops.insert(self.active().cursor_col as u16);
                self.tab_stops_modified = true;
            }
            TerminalAction::ClearTabStop => {
                let col = self.active().cursor_col as u16;
                self.tab_stops.remove(&col);
                self.tab_stops_modified = true;
            }
            TerminalAction::ClearAllTabStops => {
                self.tab_stops.clear();
                self.tab_stops_modified = true;
            }
            TerminalAction::DeviceStatusReport(report) => self.device_status_report(report),
            TerminalAction::PrivateDeviceStatusReport(report) => {
                self.private_device_status_report(report);
            }
            TerminalAction::PrimaryDeviceAttributes => {
                self.pending_output.extend_from_slice(b"\x1b[?62;22c");
            }
            TerminalAction::SecondaryDeviceAttributes => {
                self.pending_output.extend_from_slice(b"\x1b[>1;10;0c")
            }
            TerminalAction::TerminalVersion => self
                .pending_output
                .extend_from_slice(b"\x1bP>|Panea 0.1\x1b\\"),
            TerminalAction::KittyKeyboardStatus => {
                self.pending_output.extend_from_slice(
                    format!("\x1b[?{}u", self.kitty_keyboard_flags()).as_bytes(),
                );
            }
            TerminalAction::SetKittyKeyboardFlags { flags, mode } => {
                self.set_kitty_keyboard_flags(flags, mode);
            }
            TerminalAction::PushKittyKeyboardFlags(flags) => {
                self.push_kitty_keyboard_flags(flags);
            }
            TerminalAction::PopKittyKeyboardFlags(count) => {
                self.pop_kitty_keyboard_flags(count);
            }
            TerminalAction::RequestMode { private, mode } => {
                self.report_mode(private, mode);
            }
            TerminalAction::RequestDynamicColor(slot) => self.report_dynamic_color(slot),
            TerminalAction::RequestStatusString(request) => self.report_status_string(&request),
            TerminalAction::ScreenAlignmentTest => self.screen_alignment_test(),
            TerminalAction::SetScrollRegion { top, bottom } => {
                self.active_mut().set_scroll_region(top, bottom);
                let row = if self.modes.contains(&TerminalMode::Origin) {
                    self.active().scroll_top
                } else {
                    0
                };
                self.active_mut().cursor_row = row;
                self.active_mut().cursor_col = 0;
            }
            TerminalAction::ResetScrollRegion => self.active_mut().reset_scroll_region(),
            TerminalAction::Reset => self.reset(),
            TerminalAction::SoftReset => self.soft_reset(),
        }

        self.rebase_after_scroll(scrollback_before);
        self.bump_render_revision();

        Ok(())
    }

    /// Keeps row-based state aligned with content after lines scroll off.
    ///
    /// `scrolled_before` is [`ScreenBuffer::total_lines_scrolled`] sampled
    /// before the action. Counting total lines rather than scrollback length
    /// matters once the limit is reached: length then stops growing while
    /// content keeps moving, so a pinned viewport would drift by a line for
    /// every line of output, and selections would slide off their text.
    fn rebase_after_scroll(&mut self, before: ScrollState) {
        let scrolled = self
            .primary
            .total_lines_scrolled()
            .saturating_sub(before.total);
        let evicted = self
            .primary
            .scrollback_dropped
            .saturating_sub(before.dropped);

        // Keep a scrolled-back viewport over the same content. Below the limit
        // the buffer only grows; at the limit it also loses lines from the top,
        // and both move the content the viewport is anchored to.
        if scrolled > 0 && self.viewport_offset > 0 {
            self.viewport_offset = self
                .viewport_offset
                .saturating_add(usize::try_from(scrolled).unwrap_or(usize::MAX))
                .min(self.primary.history.physical_row_count());
        }

        if evicted > 0 {
            self.shift_selection_up(evicted);
        }
    }

    /// Moves the selection up by the number of evicted lines so it stays on its
    /// text, and drops it once that text has left the buffer.
    fn shift_selection_up(&mut self, evicted: u64) {
        let Some(mut selection) = self.selection else {
            return;
        };
        let shift = i64::try_from(evicted).unwrap_or(i64::MAX);
        let start_row = selection.start.row.saturating_sub(shift);
        let end_row = selection.end.row.saturating_sub(shift);

        if end_row < 0 {
            self.selection = None;
            return;
        }
        if start_row < 0 {
            selection.start.row = 0;
            selection.start.col = 0;
        } else {
            selection.start.row = start_row;
        }
        selection.end.row = end_row;
        self.selection = Some(selection);
    }

    pub fn apply_actions<I>(&mut self, actions: I) -> TerminalResult<()>
    where
        I: IntoIterator<Item = TerminalAction>,
    {
        for action in actions {
            self.apply_action(action)?;
        }

        Ok(())
    }

    /// Applies parser-decoded printable text without allocating one action per scalar.
    pub fn apply_printable_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let scrollback_before = self.primary.scroll_state();
        if text.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
            let autowrap = self.modes.contains(&TerminalMode::AutoWrap);
            let insert = self.modes.contains(&TerminalMode::Insert);
            let attributes = self.attributes;
            let use_scrollback = self.active_is_primary();
            self.active_mut().print_ascii_text(
                text.as_bytes(),
                attributes,
                autowrap,
                insert,
                use_scrollback,
            );
            self.last_printed = text.chars().next_back();
        } else {
            for ch in text.chars() {
                self.print(ch);
            }
        }

        self.rebase_after_scroll(scrollback_before);
        self.bump_render_revision();
    }

    #[must_use]
    pub fn grid(&self) -> Grid {
        let active = self.active();
        Grid {
            size: active.size,
            viewport: self.viewport(),
            lines: active.lines.clone(),
        }
    }

    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.active()
            .lines
            .get(usize::from(row))
            .and_then(|line| line.cells.get(usize::from(col)))
    }

    pub fn for_each_visible_cell(&self, mut visitor: impl FnMut(&Cell)) {
        let viewport = self.viewport();
        let start = usize::try_from(viewport.origin_row).unwrap_or(0);
        let rows = usize::from(viewport.size.rows);
        for row in start..start.saturating_add(rows) {
            if let Some(line) = self.buffer_line(row) {
                line.cells.iter().for_each(&mut visitor);
            }
        }
    }

    /// Iterates visible terminal rows without cloning their cells.
    pub fn visible_rows(&self) -> impl Iterator<Item = VisibleRow<'_>> {
        let viewport = self.viewport();
        let start = usize::try_from(viewport.origin_row).unwrap_or(0);
        let rows = usize::from(viewport.size.rows);
        (start..start.saturating_add(rows)).filter_map(move |absolute_row| {
            self.buffer_line(absolute_row).map(|line| {
                let generation = line.generation;
                let cells = match line {
                    TerminalLine::Borrowed(line) => VisibleCells::Borrowed(&line.cells),
                    TerminalLine::Shared(line) => VisibleCells::Shared(line),
                };
                VisibleRow {
                    absolute_row: i64::try_from(absolute_row).unwrap_or(i64::MAX),
                    generation,
                    cells,
                }
            })
        })
    }

    #[must_use]
    pub fn line(&self, row: u16) -> Option<&Line> {
        self.active().lines.get(usize::from(row))
    }

    #[must_use]
    pub fn visible_line(&self, row: u16) -> Option<TerminalLine<'_>> {
        if row >= self.viewport().size.rows {
            return None;
        }
        let absolute_row = self.viewport().origin_row + i64::from(row);
        usize::try_from(absolute_row)
            .ok()
            .and_then(|row| self.buffer_line(row))
    }

    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        Some(self.text_for_selection(selection))
    }

    #[must_use]
    pub fn text_for_selection(&self, selection: Selection) -> String {
        self.extract_selection(selection)
    }

    pub fn set_selection(&mut self, selection: Selection) {
        if self.selection == Some(selection) {
            return;
        }
        self.selection = Some(selection);
        self.bump_render_revision();
    }

    pub fn clear_selection(&mut self) {
        if self.selection.is_none() {
            return;
        }
        self.selection = None;
        self.bump_render_revision();
    }

    /// Scrolls the visible primary-screen viewport. Positive values move toward older lines.
    pub fn scroll_viewport(&mut self, lines: i64) -> bool {
        if !self.active_is_primary() || lines == 0 {
            return false;
        }
        let previous = self.viewport_offset;
        if lines > 0 {
            self.viewport_offset = self
                .viewport_offset
                .saturating_add(lines as usize)
                .min(self.primary.history.physical_row_count());
        } else {
            self.viewport_offset = self
                .viewport_offset
                .saturating_sub(lines.unsigned_abs() as usize);
        }
        let changed = previous != self.viewport_offset;
        if changed {
            self.bump_render_revision();
        }
        changed
    }

    pub fn scroll_to_bottom(&mut self) -> bool {
        let changed = self.viewport_offset != 0;
        self.viewport_offset = 0;
        if changed {
            self.bump_render_revision();
        }
        changed
    }

    #[must_use]
    pub const fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    #[must_use]
    pub fn viewport_position(&self, visible_row: u16, col: u16) -> GridPosition {
        let viewport = self.viewport();
        GridPosition {
            row: viewport.origin_row + i64::from(visible_row.min(viewport.size.rows - 1)),
            col: col.min(viewport.size.cols - 1),
        }
    }

    #[must_use]
    pub fn cursor_buffer_position(&self) -> GridPosition {
        let cursor = self.active().cursor_position();
        if self.active_is_primary() {
            GridPosition {
                row: self.primary.history.physical_row_count() as i64 + cursor.row,
                col: cursor.col,
            }
        } else {
            cursor
        }
    }

    #[must_use]
    pub fn buffer_line_count(&self) -> usize {
        if self.active_is_primary() {
            self.primary.history.physical_row_count() + self.primary.lines.len()
        } else {
            self.active().lines.len()
        }
    }

    pub fn reveal_position(&mut self, position: GridPosition) -> bool {
        if !self.active_is_primary() {
            return false;
        }
        let rows = i64::from(self.primary.size.rows);
        let max_origin = self.primary.history.physical_row_count() as i64;
        let current = self.viewport().origin_row;
        let desired = if position.row < current {
            position.row
        } else if position.row >= current + rows {
            position.row - rows + 1
        } else {
            current
        }
        .clamp(0, max_origin);
        let next_offset = usize::try_from(max_origin - desired).unwrap_or(0);
        let changed = next_offset != self.viewport_offset;
        self.viewport_offset = next_offset;
        if changed {
            self.bump_render_revision();
        }
        changed
    }

    #[must_use]
    pub fn search(&self, query: &str, case_sensitive: bool) -> Vec<Selection> {
        let query = query
            .graphemes(true)
            .map(|grapheme| search_key(grapheme, case_sensitive))
            .collect::<Vec<_>>();
        if query.is_empty() {
            return Vec::new();
        }

        let mut searchable = Vec::<Option<SearchCell>>::new();
        let line_count = self.buffer_line_count();
        for row in 0..line_count {
            let Some(line) = self.buffer_line(row) else {
                continue;
            };
            let content_end = line
                .cells
                .iter()
                .rposition(|cell| cell.text != " ")
                .map_or(0, |index| index + 1);
            for (col, cell) in line.cells.iter().take(content_end).enumerate() {
                if !cell.wide_continuation {
                    searchable.push(Some(SearchCell {
                        text: cell.text.clone(),
                        position: GridPosition::new(row as i64, col as u16),
                        width: cell.width.max(1),
                    }));
                }
            }
            if !line.hard_wrapped {
                searchable.push(None);
            }
        }

        let mut results = Vec::new();
        for start in 0..searchable.len() {
            let Some(first) = searchable[start].as_ref() else {
                continue;
            };
            let mut matched = true;
            let mut end = first;
            for (offset, expected) in query.iter().enumerate() {
                let Some(Some(candidate)) = searchable.get(start + offset) else {
                    matched = false;
                    break;
                };
                if !search_key_matches(&candidate.text, expected, case_sensitive) {
                    matched = false;
                    break;
                }
                end = candidate;
            }
            if matched {
                results.push(Selection::normal(
                    first.position,
                    GridPosition::new(
                        end.position.row,
                        end.position.col.saturating_add(u16::from(end.width) - 1),
                    ),
                ));
            }
        }
        results
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    #[must_use]
    pub fn pending_output(&self) -> &[u8] {
        &self.pending_output
    }

    pub fn take_pending_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_output)
    }

    #[must_use]
    pub fn pending_clipboard_requests(&self) -> &[Osc52ClipboardRequest] {
        &self.pending_clipboard_requests
    }

    pub fn take_pending_clipboard_requests(&mut self) -> Vec<Osc52ClipboardRequest> {
        std::mem::take(&mut self.pending_clipboard_requests)
    }

    #[must_use]
    pub fn hyperlink_uri(&self, id: u32) -> Option<&str> {
        self.hyperlinks.get(&id).map(String::as_str)
    }

    #[must_use]
    pub const fn modes_ref(&self) -> &BTreeSet<TerminalMode> {
        &self.modes
    }

    #[must_use]
    pub fn kitty_keyboard_flags(&self) -> u16 {
        self.active().kitty_keyboard_flags
    }

    #[must_use]
    pub fn scrollback_lines(&self) -> VecDeque<Line> {
        self.primary.history.snapshot()
    }

    #[must_use]
    pub fn scrollback_line_count(&self) -> usize {
        self.primary.history.physical_row_count()
    }

    /// Marker of buffer content, for callers that cache derived results.
    ///
    /// Changes whenever anything that a search or a measurement would see
    /// changes: a mutated visible line, a line entering or leaving scrollback,
    /// or a switch between the primary and alternate screens. Comparing it is
    /// exact rather than a hash, so a cache keyed on it cannot go stale.
    #[must_use]
    pub fn content_revision(&self) -> ContentRevision {
        ContentRevision {
            line_generation: self.active().next_line_generation,
            scrollback_rows: self.primary.history.physical_row_count(),
            scrollback_dropped: self.primary.scrollback_dropped,
            alternate_screen: self.alternate.is_some(),
        }
    }

    pub fn scrollback_memory_bytes(&self) -> u64 {
        self.primary.history.retained_memory_bytes()
    }

    #[must_use]
    pub fn history_stats(&self) -> HistoryStats {
        self.primary.history.stats()
    }

    /// Resizes the terminal and remaps caller-owned absolute buffer positions
    /// through the same logical history anchors used for cursor and selection.
    pub fn resize_with_positions(
        &mut self,
        size: TerminalSize,
        positions: &mut [GridPosition],
    ) -> TerminalResult<()> {
        let size = size.normalized();
        let (selection, viewport_offset) =
            self.primary
                .resize_reflow(size, self.selection, self.viewport_offset, positions);
        self.selection = selection;
        self.viewport_offset = viewport_offset;

        if let Some(alternate) = &mut self.alternate {
            alternate.resize_visible(size);
        }
        if !self.tab_stops_modified {
            self.tab_stops = default_tab_stops(size.cols);
        }
        self.bump_render_revision();
        Ok(())
    }

    /// Retained scrollback line limit.
    #[must_use]
    pub const fn scrollback_limit(&self) -> usize {
        self.primary.scrollback_limit
    }

    /// Sets the retained scrollback limit, trimming immediately if it shrank.
    pub fn set_scrollback_limit(&mut self, limit: usize) {
        if self.primary.scrollback_limit == limit {
            return;
        }
        let before = self.primary.scroll_state();
        self.primary.scrollback_limit = limit;
        self.primary.trim_scrollback();
        self.viewport_offset = self
            .viewport_offset
            .min(self.primary.history.physical_row_count());
        self.rebase_after_scroll(before);
        self.bump_render_revision();
    }

    /// Lines discarded from the top of the scrollback over this session's
    /// lifetime. Absolute buffer rows shift down by this much, so consumers that
    /// persist a row — semantic regions, marks — rebase against it.
    #[must_use]
    pub const fn scrollback_dropped(&self) -> u64 {
        self.primary.scrollback_dropped
    }

    fn reset(&mut self) {
        let size = self.active().size;
        let dynamic_foreground = self.dynamic_foreground;
        let dynamic_background = self.dynamic_background;
        // A full reset clears content, but the host's configured scrollback
        // limit is not terminal state and must survive it.
        let scrollback_limit = self.primary.scrollback_limit;
        // Replies already queued for the host (a DA/DSR answer earlier in the
        // same chunk) are in flight, not screen state. Dropping them left the
        // application that asked waiting for a response that never arrived.
        let pending_output = std::mem::take(&mut self.pending_output);
        let pending_clipboard_requests = std::mem::take(&mut self.pending_clipboard_requests);
        *self = Self::with_scrollback_limit(size, scrollback_limit);
        self.pending_output = pending_output;
        self.pending_clipboard_requests = pending_clipboard_requests;
        self.set_dynamic_colors(dynamic_foreground, dynamic_background);
    }

    fn soft_reset(&mut self) {
        self.attributes.reset();
        self.cursor_shape = CursorShape::Block;
        self.cursor_visible = true;
        self.cursor_blinking = true;
        self.active_mut().saved_cursor = None;
        self.modes.clear();
        self.modes.insert(TerminalMode::AutoWrap);
        self.active_mut().reset_scroll_region();
        self.active_mut().cursor_row = 0;
        self.active_mut().cursor_col = 0;
        self.active_mut().wrap_pending = false;
    }

    fn set_kitty_keyboard_flags(&mut self, flags: u16, mode: KittyKeyboardMode) {
        let current = &mut self.active_mut().kitty_keyboard_flags;
        *current = match mode {
            KittyKeyboardMode::Set => flags,
            KittyKeyboardMode::Add => *current | flags,
            KittyKeyboardMode::Remove => *current & !flags,
        };
    }

    fn push_kitty_keyboard_flags(&mut self, flags: u16) {
        const MAX_KITTY_KEYBOARD_STACK_DEPTH: usize = 16;
        let screen = self.active_mut();
        if screen.kitty_keyboard_flag_stack.len() == MAX_KITTY_KEYBOARD_STACK_DEPTH {
            screen.kitty_keyboard_flag_stack.pop_front();
        }
        screen
            .kitty_keyboard_flag_stack
            .push_back(screen.kitty_keyboard_flags);
        screen.kitty_keyboard_flags = flags;
    }

    fn pop_kitty_keyboard_flags(&mut self, count: u16) {
        let screen = self.active_mut();
        for _ in 0..count.max(1) {
            let Some(flags) = screen.kitty_keyboard_flag_stack.pop_back() else {
                break;
            };
            screen.kitty_keyboard_flags = flags;
        }
    }

    fn active(&self) -> &ScreenBuffer {
        self.alternate.as_ref().unwrap_or(&self.primary)
    }

    fn active_mut(&mut self) -> &mut ScreenBuffer {
        self.alternate.as_mut().unwrap_or(&mut self.primary)
    }

    fn active_is_primary(&self) -> bool {
        self.alternate.is_none()
    }

    pub fn viewport(&self) -> Viewport {
        Viewport {
            origin_row: if self.active_is_primary() {
                self.primary
                    .history
                    .physical_row_count()
                    .saturating_sub(self.viewport_offset) as i64
            } else {
                0
            },
            size: self.active().size,
        }
    }

    fn print(&mut self, ch: char) {
        if ch.is_control() {
            return;
        }

        let autowrap = self.modes.contains(&TerminalMode::AutoWrap);
        let insert = self.modes.contains(&TerminalMode::Insert);
        let attributes = self.attributes;
        let use_scrollback = self.active_is_primary();
        self.active_mut()
            .print(ch, attributes, autowrap, insert, use_scrollback);
        self.last_printed = Some(ch);
    }

    fn line_feed(&mut self) {
        let use_scrollback = self.active_is_primary();
        let attributes = self.attributes;
        self.active_mut().line_feed(use_scrollback, attributes);
    }

    fn tab(&mut self) {
        self.active_mut().wrap_pending = false;
        let current = self.active().cursor_col as u16;
        let next_tab = self
            .tab_stops
            .iter()
            .copied()
            .find(|stop| *stop > current)
            .unwrap_or(self.active().size.cols.saturating_sub(1));
        let max_col = usize::from(self.active().size.cols.saturating_sub(1));
        self.active_mut().cursor_col = usize::from(next_tab).min(max_col);
        self.active_mut().normalize_cursor_col();
    }

    fn back_tab(&mut self, count: u16) {
        self.active_mut().wrap_pending = false;
        for _ in 0..count.max(1) {
            let current = self.active().cursor_col as u16;
            let previous = self
                .tab_stops
                .range(..current)
                .copied()
                .next_back()
                .unwrap_or(0);
            self.active_mut().cursor_col = usize::from(previous);
            self.active_mut().normalize_cursor_col();
        }
    }

    fn repeat_last_printed(&mut self, count: u16) {
        if let Some(ch) = self.last_printed {
            for _ in 0..count.max(1) {
                self.print(ch);
            }
        }
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        let attributes = self.attributes;
        let is_primary = self.active_is_primary();

        if mode == ClearMode::Saved && is_primary {
            self.primary.clear_scrollback();
            self.viewport_offset = 0;
            return;
        }

        self.active_mut().clear_screen(mode, attributes);
    }

    fn clear_line(&mut self, mode: ClearMode) {
        let attributes = self.attributes;
        self.active_mut().clear_line(mode, attributes);
    }

    fn apply_sgr(&mut self, rendition: GraphicRendition) {
        match rendition {
            GraphicRendition::Reset => self.attributes.reset(),
            GraphicRendition::Bold => self.attributes.bold = true,
            GraphicRendition::Dim => self.attributes.dim = true,
            GraphicRendition::NormalIntensity => {
                self.attributes.bold = false;
                self.attributes.dim = false;
            }
            GraphicRendition::Italic => self.attributes.italic = true,
            GraphicRendition::NoItalic => self.attributes.italic = false,
            GraphicRendition::Underline => {
                self.attributes.underline = true;
                self.attributes.underline_style = UnderlineStyle::Single;
            }
            GraphicRendition::UnderlineStyle(style) => {
                self.attributes.underline = style != UnderlineStyle::None;
                self.attributes.underline_style = style;
            }
            GraphicRendition::NoUnderline => {
                self.attributes.underline = false;
                self.attributes.underline_style = UnderlineStyle::None;
            }
            GraphicRendition::UnderlineColor(color) => {
                self.attributes.underline_color = Some(color);
            }
            GraphicRendition::DefaultUnderlineColor => {
                self.attributes.underline_color = None;
            }
            GraphicRendition::Inverse => self.attributes.inverse = true,
            GraphicRendition::NoInverse => self.attributes.inverse = false,
            GraphicRendition::Strikethrough => self.attributes.strikethrough = true,
            GraphicRendition::NoStrikethrough => self.attributes.strikethrough = false,
            GraphicRendition::Blink => self.attributes.blink = true,
            GraphicRendition::NoBlink => self.attributes.blink = false,
            GraphicRendition::Hidden => self.attributes.hidden = true,
            GraphicRendition::NoHidden => self.attributes.hidden = false,
            GraphicRendition::Overline => self.attributes.overline = true,
            GraphicRendition::NoOverline => self.attributes.overline = false,
            GraphicRendition::Foreground(color) => self.attributes.foreground = Some(color),
            GraphicRendition::Background(color) => self.attributes.background = Some(color),
            GraphicRendition::DefaultForeground => self.attributes.foreground = None,
            GraphicRendition::DefaultBackground => self.attributes.background = None,
        }
    }

    fn set_hyperlink(&mut self, id: Option<String>, uri: Option<String>) {
        let Some(uri) = uri.filter(|uri| !uri.is_empty()) else {
            self.attributes.hyperlink_id = None;
            return;
        };
        let key = id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| uri.clone());
        let hyperlink_id = if let Some(existing) = self.hyperlink_keys.get(&key) {
            self.hyperlinks.insert(*existing, uri);
            *existing
        } else {
            let next = self.next_hyperlink_id;
            self.next_hyperlink_id = self.next_hyperlink_id.saturating_add(1).max(1);
            self.hyperlink_keys.insert(key, next);
            self.hyperlinks.insert(next, uri);
            next
        };
        self.attributes.hyperlink_id = Some(hyperlink_id);
    }

    fn report_mode(&mut self, private: bool, mode: u16) {
        let terminal_mode = mode_to_terminal_mode(private, mode);
        let status = if private && mode == 25 {
            if self.cursor_visible { 1 } else { 2 }
        } else {
            terminal_mode.map_or(0, |candidate| {
                if self.modes.contains(&candidate) {
                    1
                } else {
                    2
                }
            })
        };
        let private_marker = if private { "?" } else { "" };
        self.pending_output
            .extend_from_slice(format!("\x1b[{private_marker}{mode};{status}$y").as_bytes());
    }

    fn report_dynamic_color(&mut self, slot: u8) {
        let color = match slot {
            10 => self.dynamic_foreground,
            11 => self.dynamic_background,
            _ => return,
        };
        self.pending_output.extend_from_slice(
            format!(
                "\x1b]{slot};rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x1b\\",
                color[0], color[1], color[2]
            )
            .as_bytes(),
        );
    }

    fn report_status_string(&mut self, request: &str) {
        match request {
            "m" => self.pending_output.extend_from_slice(b"\x1bP1$r0m\x1b\\"),
            "r" => {
                let rows = self.active().size.rows;
                self.pending_output
                    .extend_from_slice(format!("\x1bP1$r1;{rows}r\x1b\\").as_bytes());
            }
            " q" => self.pending_output.extend_from_slice(b"\x1bP1$r1 q\x1b\\"),
            _ => self.pending_output.extend_from_slice(b"\x1bP0$r\x1b\\"),
        }
    }

    fn screen_alignment_test(&mut self) {
        let attributes = self.attributes;
        for line in &mut self.active_mut().lines {
            for cell in &mut line.cells {
                *cell = Cell::text("E", attributes);
            }
            line.hard_wrapped = false;
        }
        self.active_mut().cursor_row = 0;
        self.active_mut().cursor_col = 0;
        self.active_mut().wrap_pending = false;
    }

    fn save_cursor(&mut self) {
        let saved = SavedCursor {
            row: self.active().cursor_row,
            col: self.active().cursor_col,
            attributes: self.attributes,
            shape: self.cursor_shape,
            visible: self.cursor_visible,
        };
        self.active_mut().saved_cursor = Some(saved);
    }

    fn restore_cursor(&mut self) {
        let Some(saved) = self.active().saved_cursor else {
            return;
        };

        self.active_mut().cursor_row = saved.row;
        self.active_mut().cursor_col = saved.col;
        self.active_mut().clamp_cursor();
        self.attributes = saved.attributes;
        self.cursor_shape = saved.shape;
        self.cursor_visible = saved.visible;
    }

    fn insert_lines(&mut self, count: u16) {
        let attributes = self.attributes;
        self.active_mut().insert_lines(count.max(1), attributes);
    }

    fn delete_lines(&mut self, count: u16) {
        let attributes = self.attributes;
        self.active_mut().delete_lines(count.max(1), attributes);
    }

    fn insert_chars(&mut self, count: u16) {
        let attributes = self.attributes;
        self.active_mut().insert_chars(count.max(1), attributes);
    }

    fn delete_chars(&mut self, count: u16) {
        let attributes = self.attributes;
        self.active_mut().delete_chars(count.max(1), attributes);
    }

    fn erase_chars(&mut self, count: u16) {
        let attributes = self.attributes;
        self.active_mut().erase_chars(count.max(1), attributes);
    }

    fn device_status_report(&mut self, report: u16) {
        match report {
            5 => self.pending_output.extend_from_slice(b"\x1b[0n"),
            6 => {
                // With origin mode set, the report is relative to the scrolling
                // region's top margin. Reporting absolute rows put cursor
                // handshakes inside a region off by `scroll_top`.
                let cursor = self.reported_cursor_position();
                self.pending_output.extend_from_slice(
                    format!("\x1b[{};{}R", cursor.row + 1, cursor.col + 1).as_bytes(),
                );
            }
            _ => {}
        }
    }

    /// Cursor position as an application should see it, honouring origin mode.
    fn reported_cursor_position(&self) -> GridPosition {
        let mut cursor = self.active().cursor_position();
        if self.modes.contains(&TerminalMode::Origin) {
            cursor.row -= self.active().scroll_top as i64;
            cursor.row = cursor.row.max(0);
        }
        cursor
    }

    fn private_device_status_report(&mut self, report: u16) {
        if report == 6 {
            let cursor = self.reported_cursor_position();
            self.pending_output.extend_from_slice(
                format!("\x1b[?{};{}R", cursor.row + 1, cursor.col + 1).as_bytes(),
            );
        }
    }

    fn set_mode(&mut self, mode: TerminalMode, enabled: bool) {
        if enabled {
            self.modes.insert(mode);
        } else {
            self.modes.remove(&mode);
        }

        if mode == TerminalMode::AlternateScreen {
            if enabled {
                if self.alternate.is_none() {
                    self.alternate = Some(ScreenBuffer::new(self.primary.size));
                }
            } else {
                self.alternate = None;
            }

            self.selection = None;
        } else if mode == TerminalMode::CursorBlinking {
            self.cursor_blinking = enabled;
        } else if mode == TerminalMode::Origin {
            let row = if enabled { self.active().scroll_top } else { 0 };
            self.active_mut().cursor_row = row;
            self.active_mut().cursor_col = 0;
        } else if mode == TerminalMode::AutoWrap && !enabled {
            self.active_mut().wrap_pending = false;
        }
    }

    fn extract_selection(&self, selection: Selection) -> String {
        let (start, end) = if selection.start <= selection.end {
            (selection.start, selection.end)
        } else {
            (selection.end, selection.start)
        };
        let start_row = start.row.max(0) as usize;
        let end_row = end.row.max(0) as usize;
        let rectangular_start_col = start.col.min(end.col);
        let rectangular_end_col = start.col.max(end.col);
        let mut out = String::new();

        for row in start_row..=end_row {
            let Some(line) = self.buffer_line(row) else {
                continue;
            };

            let line_end = line.cells.len().saturating_sub(1);
            let (from, to) = match selection.kind {
                SelectionKind::Rectangular => (
                    usize::from(rectangular_start_col).min(line_end),
                    usize::from(rectangular_end_col).min(line_end),
                ),
                SelectionKind::Normal if row == start_row && row == end_row => (
                    usize::from(start.col).min(line_end),
                    usize::from(end.col).min(line_end),
                ),
                SelectionKind::Normal if row == start_row => {
                    (usize::from(start.col).min(line_end), line_end)
                }
                SelectionKind::Normal if row == end_row => (0, usize::from(end.col).min(line_end)),
                SelectionKind::Normal => (0, line_end),
            };
            let Some((from, to)) = expand_range_to_graphemes(&line, from, to) else {
                continue;
            };

            for cell in &line.cells[from..=to] {
                if !cell.wide_continuation {
                    out.push_str(&cell.text);
                }
            }
            if selection.kind == SelectionKind::Normal && to == line_end {
                while out.ends_with(' ') {
                    out.pop();
                }
            }

            let should_join_wrapped =
                selection.kind == SelectionKind::Normal && line.hard_wrapped && row < end_row;
            if row < end_row && !should_join_wrapped {
                out.push('\n');
            }
        }

        trim_selection_text(out)
    }

    fn buffer_line(&self, absolute_row: usize) -> Option<TerminalLine<'_>> {
        if !self.active_is_primary() {
            return self
                .active()
                .lines
                .get(absolute_row)
                .map(TerminalLine::Borrowed);
        }
        let history_rows = self.primary.history.physical_row_count();
        if absolute_row < history_rows {
            self.primary
                .history
                .row(absolute_row)
                .map(TerminalLine::Shared)
        } else {
            self.primary
                .lines
                .get(absolute_row - history_rows)
                .map(TerminalLine::Borrowed)
        }
    }
}

#[derive(Debug, Clone)]
struct SearchCell {
    text: CompactString,
    position: GridPosition,
    width: u8,
}

fn search_key(text: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        text.to_owned()
    } else {
        text.to_lowercase()
    }
}

/// Compares one cell against an already-normalised query key.
///
/// `expected` comes from [`search_key`], so it is lowercased when the search is
/// case-insensitive. Allocating a `String` per compared cell here meant one
/// allocation per cell per query grapheme across the whole buffer.
fn search_key_matches(text: &str, expected: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        return text == expected;
    }
    if text.is_ascii() && expected.is_ascii() {
        return text.eq_ignore_ascii_case(expected);
    }
    text.to_lowercase() == expected
}

impl TerminalCore for TerminalState {
    fn apply_bytes(&mut self, bytes: &[u8]) -> TerminalResult<()> {
        for byte in bytes {
            match *byte {
                b'\r' => self.apply_action(TerminalAction::CarriageReturn)?,
                b'\n' => self.apply_action(TerminalAction::LineFeed)?,
                0x08 => self.apply_action(TerminalAction::Backspace)?,
                b'\t' => self.apply_action(TerminalAction::Tab)?,
                0x20..=0x7e => self.apply_action(TerminalAction::Print(char::from(*byte)))?,
                _ => {}
            }
        }

        Ok(())
    }

    fn resize(&mut self, size: TerminalSize) -> TerminalResult<()> {
        self.resize_with_positions(size, &mut [])
    }

    fn visible_grid(&self) -> VisibleGrid {
        let viewport = self.viewport();
        let mut cells =
            Vec::with_capacity(usize::from(viewport.size.cols) * usize::from(viewport.size.rows));
        self.for_each_visible_cell(|cell| cells.push(cell.clone()));
        VisibleGrid { viewport, cells }
    }

    fn scrollback(&self) -> Scrollback {
        Scrollback {
            lines: self.primary.history.snapshot(),
        }
    }

    fn cursor_state(&self) -> CursorState {
        CursorState {
            position: self.active().cursor_position(),
            shape: self.cursor_shape,
            visible: self.cursor_visible,
            blinking: self.cursor_blinking,
        }
    }

    fn modes(&self) -> BTreeSet<TerminalMode> {
        self.modes.clone()
    }

    fn selection_state(&self) -> Option<SelectionRange> {
        self.selection
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScreenBuffer {
    size: TerminalSize,
    lines: Vec<Line>,
    history: HistoryStore,
    /// Maximum retained scrollback lines. A `Cell` is dozens of bytes, so an
    /// uncapped history turns `cat` of a large log into gigabytes of resident
    /// memory.
    scrollback_limit: usize,
    /// Total lines ever evicted from the front of the scrollback. Absolute
    /// buffer rows shift down by this much, so anything holding a row (the
    /// viewport anchor, selections, semantic regions) has to rebase by it.
    scrollback_dropped: u64,
    /// DECSC/DECRC slot. Each screen keeps its own: an application that saves
    /// the cursor on the alternate screen must not overwrite the position the
    /// shell had saved on the primary one.
    saved_cursor: Option<SavedCursor>,
    cursor_row: usize,
    cursor_col: usize,
    scroll_top: usize,
    scroll_bottom: usize,
    wrap_pending: bool,
    kitty_keyboard_flags: u16,
    kitty_keyboard_flag_stack: VecDeque<u16>,
    next_line_generation: u64,
}

/// Scroll accounting sampled before an action so row-based state can be rebased
/// afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrollState {
    /// Lines that have ever left the top of the visible grid.
    total: u64,
    /// Lines discarded from the front of the retained scrollback.
    dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SavedCursor {
    row: usize,
    col: usize,
    attributes: CellAttributes,
    shape: CursorShape,
    visible: bool,
}

impl ScreenBuffer {
    fn new(size: TerminalSize) -> Self {
        let size = size.normalized();
        let rows = usize::from(size.rows);

        let mut buffer = Self {
            size,
            lines: vec![Line::blank(size.cols); rows],
            history: HistoryStore::new(size.cols, HistoryStoreConfig::default()),
            scrollback_limit: DEFAULT_SCROLLBACK_LIMIT,
            scrollback_dropped: 0,
            saved_cursor: None,
            cursor_row: 0,
            cursor_col: 0,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            wrap_pending: false,
            kitty_keyboard_flags: 0,
            kitty_keyboard_flag_stack: VecDeque::new(),
            next_line_generation: 0,
        };
        buffer.mark_all_lines();
        buffer
    }

    fn mark_line(&mut self, row: usize) {
        self.next_line_generation = self.next_line_generation.wrapping_add(1).max(1);
        if let Some(line) = self.lines.get_mut(row) {
            line.generation = self.next_line_generation;
        }
    }

    fn mark_all_lines(&mut self) {
        for row in 0..self.lines.len() {
            self.mark_line(row);
        }
    }

    fn mark_line_range(&mut self, start: usize, end: usize) {
        for row in start..=end.min(self.lines.len().saturating_sub(1)) {
            self.mark_line(row);
        }
    }

    fn cursor_position(&self) -> GridPosition {
        GridPosition {
            row: self.cursor_row as i64,
            col: self.cursor_col as u16,
        }
    }

    /// Appends a scrolled-off line, evicting the oldest lines past the limit.
    fn push_scrollback(&mut self, line: Line) {
        if self.scrollback_limit == 0 {
            self.scrollback_dropped = self.scrollback_dropped.saturating_add(1);
            return;
        }
        self.history.push_physical_line(line);
        self.trim_scrollback();
    }

    fn trim_scrollback(&mut self) {
        let removed = self.history.trim_to_rows(self.scrollback_limit);
        self.scrollback_dropped = self.scrollback_dropped.saturating_add(removed as u64);
    }

    /// Total lines that have ever left the top of the buffer, including those
    /// discarded by an explicit scrollback clear.
    fn total_lines_scrolled(&self) -> u64 {
        self.scrollback_dropped
            .saturating_add(self.history.physical_row_count() as u64)
    }

    fn scroll_state(&self) -> ScrollState {
        ScrollState {
            total: self.total_lines_scrolled(),
            dropped: self.scrollback_dropped,
        }
    }

    fn clear_scrollback(&mut self) {
        self.scrollback_dropped = self
            .scrollback_dropped
            .saturating_add(self.history.physical_row_count() as u64);
        self.history.clear();
    }

    fn print(
        &mut self,
        ch: char,
        attributes: CellAttributes,
        autowrap: bool,
        insert: bool,
        append_scrollback: bool,
    ) {
        if self.try_append_to_previous_grapheme(ch) {
            return;
        }

        let cols = usize::from(self.size.cols);
        let width = scalar_cell_width(ch, cols);
        if self.wrap_pending {
            if autowrap {
                if let Some(line) = self.lines.get_mut(self.cursor_row) {
                    line.hard_wrapped = true;
                }
                self.wrap_line(append_scrollback, attributes);
            }
            self.wrap_pending = false;
        }

        if width == 2 && self.cursor_col + 1 >= cols && autowrap {
            if let Some(line) = self.lines.get_mut(self.cursor_row) {
                line.hard_wrapped = true;
            }
            self.wrap_line(append_scrollback, attributes);
        }

        if insert {
            self.insert_chars(width as u16, attributes);
        }

        self.clear_grapheme_at(self.cursor_row, self.cursor_col, attributes);
        if width == 2 {
            self.clear_grapheme_at(self.cursor_row, self.cursor_col + 1, attributes);
        }
        if let Some(cell) = self
            .lines
            .get_mut(self.cursor_row)
            .and_then(|line| line.cells.get_mut(self.cursor_col))
        {
            let mut encoded = [0; 4];
            *cell = Cell::text(ch.encode_utf8(&mut encoded), attributes);
        }

        if width == 2
            && let Some(cell) = self
                .lines
                .get_mut(self.cursor_row)
                .and_then(|line| line.cells.get_mut(self.cursor_col + 1))
        {
            *cell = Cell::wide_continuation(attributes);
        }

        let advance = width.max(1);
        if self.cursor_col + advance >= cols {
            self.cursor_col = cols.saturating_sub(1);
            self.wrap_pending = autowrap;
        } else {
            self.cursor_col += advance;
        }
        self.mark_line(self.cursor_row);
    }

    fn print_ascii_text(
        &mut self,
        text: &[u8],
        attributes: CellAttributes,
        autowrap: bool,
        insert: bool,
        append_scrollback: bool,
    ) {
        debug_assert!(text.iter().all(|byte| (0x20..=0x7e).contains(byte)));

        for &byte in text {
            if self.wrap_pending {
                if autowrap {
                    if let Some(line) = self.lines.get_mut(self.cursor_row) {
                        line.hard_wrapped = true;
                    }
                    self.wrap_line(append_scrollback, attributes);
                }
                self.wrap_pending = false;
            }

            if insert {
                self.insert_chars(1, attributes);
            }

            let requires_grapheme_clear = self
                .lines
                .get(self.cursor_row)
                .and_then(|line| line.cells.get(self.cursor_col))
                .is_some_and(|cell| cell.wide_continuation || cell.width > 1);
            if requires_grapheme_clear {
                self.clear_grapheme_at(self.cursor_row, self.cursor_col, attributes);
            }

            if let Some(cell) = self
                .lines
                .get_mut(self.cursor_row)
                .and_then(|line| line.cells.get_mut(self.cursor_col))
            {
                let mut encoded = [0; 4];
                *cell = Cell {
                    text: CompactString::new(char::from(byte).encode_utf8(&mut encoded)),
                    attributes,
                    width: 1,
                    wide_continuation: false,
                    hyperlink_id: attributes.hyperlink_id,
                };
            }

            let cols = usize::from(self.size.cols);
            if self.cursor_col + 1 >= cols {
                self.cursor_col = cols.saturating_sub(1);
                self.wrap_pending = autowrap;
            } else {
                self.cursor_col += 1;
            }
            self.mark_line(self.cursor_row);
        }
    }

    fn try_append_to_previous_grapheme(&mut self, ch: char) -> bool {
        let Some((row, col)) = self.previous_grapheme_position() else {
            return false;
        };

        let Some(previous_text) = self
            .lines
            .get(row)
            .and_then(|line| line.cells.get(col))
            .filter(|cell| !cell.wide_continuation)
            .map(|cell| cell.text.as_str())
        else {
            return false;
        };

        if !extends_previous_grapheme(previous_text, ch) {
            return false;
        }

        // Swallow the scalar once the cluster is at its cap. Returning false
        // here would print it as a standalone mark; dropping it keeps a stream
        // of combining marks or ZWJ from growing one cell without bound.
        if previous_text.chars().count() >= MAX_CELL_GRAPHEME_SCALARS {
            return true;
        }

        let cols = usize::from(self.size.cols);
        let Some(line) = self.lines.get_mut(row) else {
            return false;
        };
        let Some(cell) = line
            .cells
            .get_mut(col)
            .filter(|cell| !cell.wide_continuation)
        else {
            return false;
        };

        cell.text.push(ch);
        let width = cell_width_for_text_in_grid(&cell.text, cols.saturating_sub(col));
        let attributes = cell.attributes;
        cell.width = width as u8;

        if width == 2 && col + 1 < cols {
            line.cells[col + 1] = Cell::wide_continuation(attributes);
        } else if col + 1 < cols && line.cells[col + 1].wide_continuation {
            line.cells[col + 1] = Cell::blank(attributes);
        }

        if row == self.cursor_row {
            self.cursor_col = self
                .cursor_col
                .max((col + width).min(cols.saturating_sub(1)));
        }
        // Only the edited cell and its neighbours can violate the wide/
        // continuation invariant. Re-measuring the whole line here made every
        // combining mark cost O(cols) grapheme segmentations.
        repair_wide_pair(&mut line.cells, col, CellAttributes::default());
        self.mark_line(row);
        true
    }

    fn previous_grapheme_position(&self) -> Option<(usize, usize)> {
        let (row, mut col) = if self.wrap_pending {
            (self.cursor_row, self.cursor_col)
        } else if self.cursor_col > 0 {
            (self.cursor_row, self.cursor_col - 1)
        } else if self.cursor_row > 0
            // Only continue onto the previous row when this one is its soft-wrap
            // continuation. Otherwise a combining mark printed at column 0
            // attached itself to the tail of an unrelated line.
            && self
                .lines
                .get(self.cursor_row - 1)
                .is_some_and(|line| line.hard_wrapped)
        {
            (
                self.cursor_row - 1,
                usize::from(self.size.cols.saturating_sub(1)),
            )
        } else {
            return None;
        };

        let line = self.lines.get(row)?;
        if line
            .cells
            .get(col)
            .is_some_and(|cell| cell.wide_continuation)
            && col > 0
        {
            col -= 1;
        }

        Some((row, col))
    }

    fn wrap_line(&mut self, append_scrollback: bool, attributes: CellAttributes) {
        self.cursor_col = 0;
        self.line_feed(append_scrollback, attributes);
    }

    fn carriage_return(&mut self) {
        self.wrap_pending = false;
        self.cursor_col = 0;
    }

    fn line_feed(&mut self, append_scrollback: bool, attributes: CellAttributes) {
        self.wrap_pending = false;
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up(append_scrollback, attributes);
        } else {
            self.cursor_row = (self.cursor_row + 1).min(usize::from(self.size.rows) - 1);
        }
    }

    fn backspace(&mut self) {
        self.wrap_pending = false;
        let Some(line) = self.lines.get(self.cursor_row) else {
            return;
        };
        self.cursor_col = previous_grapheme_col(line, self.cursor_col);
    }

    fn move_cursor(&mut self, direction: CursorDirection, count: u16, origin: bool) {
        self.wrap_pending = false;
        let count = usize::from(count);
        // xterm confines cursor movement to the scrolling region whenever the
        // cursor is already inside it, independent of origin mode. Clamping only
        // under DECOM let `CSI B` walk a cursor out of a region an application
        // had set up, over tmux's status line and similar reserved rows.
        let inside_region =
            self.cursor_row >= self.scroll_top && self.cursor_row <= self.scroll_bottom;
        let (top, bottom) = if origin || inside_region {
            (self.scroll_top, self.scroll_bottom)
        } else {
            (0, usize::from(self.size.rows) - 1)
        };
        match direction {
            CursorDirection::Up => {
                self.cursor_row = self.cursor_row.saturating_sub(count).max(top);
            }
            CursorDirection::Down => {
                self.cursor_row = (self.cursor_row + count).min(bottom);
            }
            CursorDirection::Forward => {
                self.normalize_cursor_col();
                for _ in 0..count {
                    let Some(line) = self.lines.get(self.cursor_row) else {
                        return;
                    };
                    self.cursor_col = next_grapheme_col(line, self.cursor_col);
                }
            }
            CursorDirection::Back => {
                self.normalize_cursor_col();
                for _ in 0..count {
                    let Some(line) = self.lines.get(self.cursor_row) else {
                        return;
                    };
                    self.cursor_col = previous_grapheme_col(line, self.cursor_col);
                }
            }
            CursorDirection::NextLine => {
                self.cursor_row = (self.cursor_row + count).min(bottom);
                self.cursor_col = 0;
            }
            CursorDirection::PreviousLine => {
                self.cursor_row = self.cursor_row.saturating_sub(count).max(top);
                self.cursor_col = 0;
            }
        }
    }

    fn set_cursor_position(&mut self, row: u16, col: u16, origin: bool) {
        self.wrap_pending = false;
        let requested = usize::from(row.saturating_sub(1));
        self.cursor_row = if origin {
            (self.scroll_top + requested).min(self.scroll_bottom)
        } else {
            requested.min(usize::from(self.size.rows) - 1)
        };
        self.cursor_col = usize::from(col.saturating_sub(1)).min(usize::from(self.size.cols) - 1);
        self.normalize_cursor_col();
    }

    fn set_cursor_row(&mut self, row: u16, origin: bool) {
        self.wrap_pending = false;
        let requested = usize::from(row.saturating_sub(1));
        self.cursor_row = if origin {
            (self.scroll_top + requested).min(self.scroll_bottom)
        } else {
            requested.min(usize::from(self.size.rows) - 1)
        };
    }

    fn set_cursor_column(&mut self, col: u16) {
        self.wrap_pending = false;
        self.cursor_col = usize::from(col.saturating_sub(1)).min(usize::from(self.size.cols) - 1);
        self.normalize_cursor_col();
    }

    fn clear_screen(&mut self, mode: ClearMode, attributes: CellAttributes) {
        self.wrap_pending = false;
        match mode {
            ClearMode::FromCursor => {
                self.clear_line(ClearMode::FromCursor, attributes);
                for row in self.cursor_row + 1..self.lines.len() {
                    self.lines[row] = Line::blank_with_attributes(self.size.cols, attributes);
                }
                self.mark_line_range(self.cursor_row.saturating_add(1), self.lines.len());
            }
            ClearMode::ToCursor => {
                for row in 0..self.cursor_row {
                    self.lines[row] = Line::blank_with_attributes(self.size.cols, attributes);
                }
                if self.cursor_row > 0 {
                    self.mark_line_range(0, self.cursor_row - 1);
                }
                self.clear_line(ClearMode::ToCursor, attributes);
            }
            ClearMode::All | ClearMode::Saved => {
                for line in &mut self.lines {
                    *line = Line::blank_with_attributes(self.size.cols, attributes);
                }
                self.mark_all_lines();
            }
        }
    }

    fn clear_line(&mut self, mode: ClearMode, attributes: CellAttributes) {
        self.wrap_pending = false;
        let Some(line) = self.lines.get_mut(self.cursor_row) else {
            return;
        };

        let last_col = line.cells.len().saturating_sub(1);
        let range = match mode {
            ClearMode::FromCursor => self.cursor_col..=last_col,
            ClearMode::ToCursor => 0..=self.cursor_col.min(last_col),
            ClearMode::All | ClearMode::Saved => 0..=last_col,
        };

        blank_range_expanding_graphemes(
            line,
            range.start().to_owned(),
            range.end().to_owned(),
            attributes,
        );
        line.hard_wrapped = false;
        self.mark_line(self.cursor_row);
    }

    fn insert_lines(&mut self, count: u16, attributes: CellAttributes) {
        self.wrap_pending = false;
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bottom {
            return;
        }

        let count = usize::from(count).min(self.scroll_bottom - self.cursor_row + 1);
        for _ in 0..count {
            self.lines.insert(
                self.cursor_row,
                Line::blank_with_attributes(self.size.cols, attributes),
            );
            self.lines.remove(self.scroll_bottom + 1);
        }
        self.mark_line_range(self.cursor_row, self.scroll_bottom);
    }

    fn delete_lines(&mut self, count: u16, attributes: CellAttributes) {
        self.wrap_pending = false;
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bottom {
            return;
        }

        let count = usize::from(count).min(self.scroll_bottom - self.cursor_row + 1);
        for _ in 0..count {
            self.lines.remove(self.cursor_row);
            self.lines.insert(
                self.scroll_bottom,
                Line::blank_with_attributes(self.size.cols, attributes),
            );
        }
        self.mark_line_range(self.cursor_row, self.scroll_bottom);
    }

    fn insert_chars(&mut self, count: u16, attributes: CellAttributes) {
        self.wrap_pending = false;
        self.normalize_cursor_col();
        let Some(line) = self.lines.get_mut(self.cursor_row) else {
            return;
        };
        let count = usize::from(count).min(line.cells.len().saturating_sub(self.cursor_col));
        // Only split a wide grapheme the cursor is sitting inside. Blanking
        // unconditionally erased the character that ICH is supposed to shift
        // right, so mid-line insertion in bash/zsh dropped a character.
        if line
            .cells
            .get(self.cursor_col)
            .is_some_and(|cell| cell.wide_continuation)
        {
            blank_range_expanding_graphemes(line, self.cursor_col, self.cursor_col, attributes);
        }
        for _ in 0..count {
            line.cells.insert(self.cursor_col, Cell::blank(attributes));
            line.cells.pop();
        }
        // The shift keeps every pair together. Only two places can break: the
        // lead just before the inserted blanks, and the tail, where a lead's
        // continuation was popped off the end.
        let len = line.cells.len();
        sanitize_cell_range(
            &mut line.cells,
            self.cursor_col.saturating_sub(1),
            (self.cursor_col + count + 1).min(len),
            attributes,
        );
        sanitize_cell_range(&mut line.cells, len.saturating_sub(2), len, attributes);
        line.hard_wrapped = false;
        self.mark_line(self.cursor_row);
    }

    fn delete_chars(&mut self, count: u16, attributes: CellAttributes) {
        self.wrap_pending = false;
        self.normalize_cursor_col();
        let Some(line) = self.lines.get_mut(self.cursor_row) else {
            return;
        };
        let Some(end) = grapheme_delete_end(line, self.cursor_col, usize::from(count)) else {
            return;
        };
        let removed = end.saturating_sub(self.cursor_col);
        line.cells.drain(self.cursor_col..end);
        for _ in 0..removed {
            line.cells.push(Cell::blank(attributes));
        }
        // Same reasoning as ICH: pairs shift together, so re-check the deletion
        // boundary and the freshly blanked tail.
        let len = line.cells.len();
        sanitize_cell_range(
            &mut line.cells,
            self.cursor_col.saturating_sub(1),
            (self.cursor_col + 2).min(len),
            attributes,
        );
        sanitize_cell_range(
            &mut line.cells,
            len.saturating_sub(removed + 2),
            len,
            attributes,
        );
        line.hard_wrapped = false;
        self.mark_line(self.cursor_row);
    }

    fn erase_chars(&mut self, count: u16, attributes: CellAttributes) {
        self.wrap_pending = false;
        self.normalize_cursor_col();
        let Some(line) = self.lines.get_mut(self.cursor_row) else {
            return;
        };
        if let Some(end) = grapheme_delete_end(line, self.cursor_col, usize::from(count)) {
            blank_range_expanding_graphemes(
                line,
                self.cursor_col,
                end.saturating_sub(1),
                attributes,
            );
            self.mark_line(self.cursor_row);
        }
    }

    fn clear_grapheme_at(&mut self, row: usize, col: usize, attributes: CellAttributes) {
        let Some(line) = self.lines.get_mut(row) else {
            return;
        };

        if line
            .cells
            .get(col)
            .is_some_and(|cell| cell.wide_continuation)
            && col > 0
        {
            line.cells[col - 1] = Cell::blank(attributes);
            line.cells[col] = Cell::blank(attributes);
            return;
        }

        if line.cells.get(col).is_some_and(|cell| cell.width > 1) && col + 1 < line.cells.len() {
            line.cells[col + 1] = Cell::blank(attributes);
        }
    }

    fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        self.wrap_pending = false;
        let max = usize::from(self.size.rows.saturating_sub(1));
        let top = usize::from(top.saturating_sub(1)).min(max);
        let bottom = usize::from(bottom.saturating_sub(1)).min(max);

        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
            self.cursor_row = top;
            self.cursor_col = 0;
        }
    }

    fn reset_scroll_region(&mut self) {
        self.wrap_pending = false;
        self.scroll_top = 0;
        self.scroll_bottom = usize::from(self.size.rows.saturating_sub(1));
    }

    fn scroll_up(&mut self, append_scrollback: bool, attributes: CellAttributes) {
        let blank = Line::blank_with_attributes(self.size.cols, attributes);

        if self.scroll_top == 0 && self.scroll_bottom == self.lines.len().saturating_sub(1) {
            let removed = self.lines.remove(0);
            if append_scrollback {
                self.push_scrollback(removed);
            }
            self.lines.push(blank);
        } else {
            self.lines.remove(self.scroll_top);
            self.lines.insert(self.scroll_bottom, blank);
        }
        self.mark_line_range(self.scroll_top, self.scroll_bottom);
    }

    fn scroll_up_explicit(&mut self, count: u16, attributes: CellAttributes) {
        self.wrap_pending = false;
        for _ in 0..usize::from(count).min(self.scroll_bottom - self.scroll_top + 1) {
            self.lines.remove(self.scroll_top);
            self.lines.insert(
                self.scroll_bottom,
                Line::blank_with_attributes(self.size.cols, attributes),
            );
        }
        self.mark_line_range(self.scroll_top, self.scroll_bottom);
    }

    fn scroll_down_explicit(&mut self, count: u16, attributes: CellAttributes) {
        self.wrap_pending = false;
        for _ in 0..usize::from(count).min(self.scroll_bottom - self.scroll_top + 1) {
            self.lines.remove(self.scroll_bottom);
            self.lines.insert(
                self.scroll_top,
                Line::blank_with_attributes(self.size.cols, attributes),
            );
        }
        self.mark_line_range(self.scroll_top, self.scroll_bottom);
    }

    fn reverse_index(&mut self, attributes: CellAttributes) {
        self.wrap_pending = false;
        if self.cursor_row == self.scroll_top {
            self.scroll_down_explicit(1, attributes);
        } else {
            self.cursor_row = self.cursor_row.saturating_sub(1);
        }
    }

    fn resize_visible(&mut self, size: TerminalSize) {
        self.wrap_pending = false;
        let size = size.normalized();
        for line in &mut self.lines {
            line.resize_to(size.cols, CellAttributes::default());
        }

        match self.lines.len().cmp(&usize::from(size.rows)) {
            Ordering::Less => self
                .lines
                .resize_with(usize::from(size.rows), || Line::blank(size.cols)),
            Ordering::Greater => self.lines.truncate(usize::from(size.rows)),
            Ordering::Equal => {}
        }

        self.size = size;
        self.reset_scroll_region();
        self.clamp_cursor();
        self.mark_all_lines();
    }

    fn resize_reflow(
        &mut self,
        size: TerminalSize,
        selection: Option<Selection>,
        viewport_offset: usize,
        positions: &mut [GridPosition],
    ) -> (Option<Selection>, usize) {
        let old_wrap_pending = self.wrap_pending;
        let size = size.normalized();
        if size.cols == self.size.cols {
            self.resize_rows(size);
            return (
                selection,
                viewport_offset.min(self.history.physical_row_count()),
            );
        }

        // Fold the small mutable screen into canonical history. Historical
        // cells are not cloned; only the visible rows move into the store.
        let old_history_rows = self.history.physical_row_count();
        let old_viewport_origin = old_history_rows.saturating_sub(viewport_offset);
        for line in std::mem::take(&mut self.lines) {
            self.history.push_physical_line(line);
        }

        let cursor_anchor = self.history.anchor_for_position(GridPosition::new(
            i64::try_from(old_history_rows.saturating_add(self.cursor_row)).unwrap_or(i64::MAX),
            self.cursor_col as u16,
        ));
        let screen_anchor = self.history.anchor_for_position(GridPosition::new(
            i64::try_from(old_history_rows).unwrap_or(i64::MAX),
            0,
        ));
        let viewport_anchor = self.history.anchor_for_position(GridPosition::new(
            i64::try_from(old_viewport_origin).unwrap_or(i64::MAX),
            0,
        ));
        let selection_anchors =
            selection.and_then(|value| self.history.anchors_for_selection(value));
        let position_anchors = positions
            .iter()
            .map(|position| self.history.anchor_for_position(*position))
            .collect::<Vec<_>>();

        self.history.set_width(size.cols);

        let cursor_position = cursor_anchor
            .and_then(|anchor| self.history.position_for_anchor(anchor))
            .unwrap_or_else(|| GridPosition::new(self.history.physical_row_count() as i64, 0));
        let screen_position = screen_anchor
            .and_then(|anchor| self.history.position_for_anchor(anchor))
            .unwrap_or_else(|| GridPosition::new(self.history.physical_row_count() as i64, 0));
        let viewport_position = viewport_anchor
            .and_then(|anchor| self.history.position_for_anchor(anchor))
            .unwrap_or(screen_position);
        let remapped_selection =
            selection_anchors.and_then(|anchors| self.history.selection_for_anchors(anchors));
        for (position, anchor) in positions.iter_mut().zip(position_anchors) {
            if let Some(mapped) = anchor.and_then(|anchor| self.history.position_for_anchor(anchor))
            {
                *position = mapped;
            }
        }

        let rows = usize::from(size.rows);
        let cursor_absolute = usize::try_from(cursor_position.row).unwrap_or(usize::MAX);
        let screen_absolute = usize::try_from(screen_position.row).unwrap_or(usize::MAX);
        let split = screen_absolute.max(cursor_absolute.saturating_sub(rows.saturating_sub(1)));
        let mut visible = self.history.drain_tail_from(split);
        visible.truncate(rows);
        self.lines = visible;
        self.trim_scrollback();
        self.lines.resize_with(rows, || Line::blank(size.cols));
        self.size = size;
        self.reset_scroll_region();
        self.cursor_row = cursor_absolute
            .saturating_sub(split)
            .min(rows.saturating_sub(1));
        self.cursor_col = usize::from(cursor_position.col).min(usize::from(size.cols) - 1);
        self.wrap_pending = old_wrap_pending;
        self.mark_all_lines();

        let history_rows = self.history.physical_row_count();
        let viewport_origin = usize::try_from(viewport_position.row)
            .unwrap_or(0)
            .min(history_rows);
        let next_viewport_offset = history_rows.saturating_sub(viewport_origin);
        let prefetch_start = viewport_origin.saturating_sub(rows);
        self.history
            .prefetch(prefetch_start..history_rows.min(viewport_origin.saturating_add(rows)));
        (remapped_selection, next_viewport_offset)
    }

    #[cfg(any())]
    fn resize_reflow_eager(&mut self, size: TerminalSize) {
        let old_wrap_pending = self.wrap_pending;
        let size = size.normalized();
        if size.cols == self.size.cols {
            self.resize_rows(size);
            return;
        }
        // Most column changes need no rejoining at all — growing the window, or
        // shrinking one whose history is all short unwrapped lines. Detecting
        // that costs one pass over line flags and lengths and skips three full
        // passes over every cell in the history.
        if self.column_change_needs_no_rejoin(size.cols) {
            self.resize_columns_in_place(size);
            return;
        }

        // Reflow works over one contiguous history; take it out of the deque up
        // front so the helpers below keep operating on slices.
        let scrollback = Vec::from(std::mem::take(&mut self.scrollback));
        let target_physical = scrollback.len().saturating_add(self.cursor_row);
        let (target_logical, target_offset) =
            logical_cursor_position(&scrollback, &self.lines, target_physical, self.cursor_col);
        let (viewport_logical, viewport_offset) =
            logical_cursor_position(&scrollback, &self.lines, scrollback.len(), 0);
        let logical = logical_lines(scrollback, std::mem::take(&mut self.lines));
        let mut reflowed = Vec::new();
        let mut cursor_physical = 0usize;
        let mut cursor_col = 0usize;
        let mut cursor_wrap_pending = false;
        let mut viewport_physical = 0usize;
        for (logical_index, mut cells) in logical.into_iter().enumerate() {
            if logical_index == target_logical {
                // Non-wrapped lines omit trailing blank cells from the logical
                // reflow model. Keep enough blank occupancy to preserve a
                // cursor positioned after that visible content. Interactive
                // line editors rely on the cursor column surviving resize
                // exactly, even when a prompt ends in a space.
                if cells.len() < target_offset {
                    cells.resize(target_offset, Cell::blank(CellAttributes::default()));
                }
                let mapped = reflow_cursor_position(&cells, target_offset, size.cols);
                cursor_physical = reflowed.len().saturating_add(mapped.0);
                cursor_col = mapped.1;
                cursor_wrap_pending = old_wrap_pending || mapped.2;
            }
            if logical_index == viewport_logical {
                let mapped = reflow_cursor_position(&cells, viewport_offset, size.cols);
                viewport_physical = reflowed.len().saturating_add(mapped.0);
            }
            reflow_logical_line(cells, size.cols, &mut reflowed);
        }
        let rows = usize::from(size.rows);
        // Preserve the old viewport origin through horizontal reflow, then
        // move it only as far as required to keep the cursor visible.
        // Bottom-anchoring the entire buffer would turn transient startup
        // shrink/grow events into blank scrollback and displace the first
        // shell output.
        let split = viewport_physical.max(cursor_physical.saturating_sub(rows.saturating_sub(1)));
        let viewport_end = split.saturating_add(rows).min(reflowed.len());
        let mut visible = reflowed.split_off(split);
        visible.truncate(viewport_end.saturating_sub(split));
        self.scrollback = VecDeque::from(reflowed);
        self.trim_scrollback();
        self.lines = visible;
        self.lines.resize_with(rows, || Line::blank(size.cols));
        self.size = size;
        self.reset_scroll_region();
        self.cursor_row = cursor_physical
            .saturating_sub(split)
            .min(rows.saturating_sub(1));
        self.cursor_col = cursor_col.min(usize::from(size.cols) - 1);
        self.wrap_pending = cursor_wrap_pending;
        self.mark_all_lines();
    }

    /// True when no line has to be rejoined or re-split for the new width:
    /// nothing is soft-wrapped and every line's content already fits.
    #[cfg(any())]
    fn column_change_needs_no_rejoin(&self, cols: u16) -> bool {
        let cols = usize::from(cols.max(1));
        self.scrollback
            .iter()
            .chain(self.lines.iter())
            .all(|line| !line.hard_wrapped && line_content_len(line) <= cols)
    }

    /// Applies a new column count without rebuilding the buffer. Content stays
    /// on the line it was already on, so the cursor row and the viewport anchor
    /// need no remapping.
    #[cfg(any())]
    fn resize_columns_in_place(&mut self, size: TerminalSize) {
        self.wrap_pending = false;
        for line in self.scrollback.iter_mut().chain(self.lines.iter_mut()) {
            line.resize_to(size.cols, CellAttributes::default());
        }
        self.lines
            .resize_with(usize::from(size.rows), || Line::blank(size.cols));
        self.size = size;
        self.reset_scroll_region();
        self.clamp_cursor();
    }

    fn resize_rows(&mut self, size: TerminalSize) {
        let rows = usize::from(size.rows);
        let shift = self.cursor_row.saturating_sub(rows.saturating_sub(1));
        if shift > 0 {
            for line in self.lines.drain(..shift) {
                self.history.push_physical_line(line);
            }
            self.trim_scrollback();
            self.cursor_row = self.cursor_row.saturating_sub(shift);
        }

        self.lines.truncate(rows);
        self.lines.resize_with(rows, || Line::blank(size.cols));
        self.size = size;
        self.reset_scroll_region();
        self.clamp_cursor();
        self.mark_all_lines();
    }

    fn clamp_cursor(&mut self) {
        self.cursor_row = self.cursor_row.min(usize::from(self.size.rows) - 1);
        self.cursor_col = self.cursor_col.min(usize::from(self.size.cols) - 1);
        self.normalize_cursor_col();
    }

    fn normalize_cursor_col(&mut self) {
        let Some(line) = self.lines.get(self.cursor_row) else {
            return;
        };
        if line
            .cells
            .get(self.cursor_col)
            .is_some_and(|cell| cell.wide_continuation)
            && self.cursor_col > 0
        {
            self.cursor_col -= 1;
        }
    }
}

#[cfg(any())]
fn logical_lines(scrollback: Vec<Line>, visible: Vec<Line>) -> Vec<Vec<Cell>> {
    let capacity = scrollback.len().saturating_add(visible.len());
    let mut out: Vec<Vec<Cell>> = Vec::with_capacity(capacity);
    let mut previous_hard_wrapped = false;

    for (index, line) in scrollback.into_iter().chain(visible).enumerate() {
        let hard_wrapped = line.hard_wrapped;
        let end = line_content_len(&line);
        let mut content = line.cells;
        content.truncate(end);
        if index > 0 && previous_hard_wrapped {
            let last = out
                .last_mut()
                .expect("a previous physical line created a logical line");
            last.extend(content);
        } else {
            out.push(content);
        }
        previous_hard_wrapped = hard_wrapped;
    }

    if out.is_empty() {
        out.push(Vec::new());
    }

    out
}

#[cfg(any())]
fn logical_cursor_position(
    scrollback: &[Line],
    visible: &[Line],
    target_physical: usize,
    cursor_col: usize,
) -> (usize, usize) {
    let mut logical_index = 0usize;
    let mut logical_offset = 0usize;
    let mut previous_hard_wrapped = false;
    for (index, line) in scrollback.iter().chain(visible).enumerate() {
        if index > 0 && !previous_hard_wrapped {
            logical_index = logical_index.saturating_add(1);
            logical_offset = 0;
        }
        if index == target_physical {
            return (logical_index, logical_offset.saturating_add(cursor_col));
        }
        logical_offset = logical_offset.saturating_add(line_content_len(line));
        previous_hard_wrapped = line.hard_wrapped;
    }
    (logical_index, logical_offset)
}

#[cfg(any())]
fn reflow_cursor_position(cells: &[Cell], offset: usize, cols: u16) -> (usize, usize, bool) {
    let cols = usize::from(cols.max(1));
    let mut source_col = 0usize;
    let mut row = 0usize;
    let mut col = 0usize;

    for cell in cells.iter().filter(|cell| !cell.wide_continuation) {
        let source_width = cell.width.max(1) as usize;
        let width = reflow_cell_width(cell, cols);
        if col > 0 && col + width > cols {
            row = row.saturating_add(1);
            col = 0;
        }
        if offset >= source_col && offset < source_col.saturating_add(source_width) {
            return (row, (col + offset - source_col).min(cols - 1), false);
        }
        source_col = source_col.saturating_add(source_width);
        col = col.saturating_add(width);
        if col == cols && source_col < offset {
            row = row.saturating_add(1);
            col = 0;
        }
    }

    if col >= cols {
        (row, cols - 1, true)
    } else {
        (row, col, false)
    }
}

fn line_content_len(line: &Line) -> usize {
    if line.hard_wrapped {
        line.cells.len()
    } else {
        line.cells
            .iter()
            // A blank cell carrying a background is content: dropping it on
            // reflow stripped the colour from powerline prompt segments and
            // from `ls`/`git` output blocks whenever the window was resized.
            .rposition(|cell| cell.text != " " || cell.attributes.background.is_some())
            .map_or(0, |index| index + 1)
    }
}

#[cfg(any())]
fn reflow_logical_line(cells: Vec<Cell>, cols: u16, out: &mut Vec<Line>) {
    let cols = usize::from(cols.max(1));
    let mut line = Line {
        cells: Vec::with_capacity(cols),
        hard_wrapped: false,
        generation: 0,
    };
    let mut emitted_any = false;

    for cell in cells.into_iter().filter(|cell| !cell.wide_continuation) {
        let width = reflow_cell_width(&cell, cols);
        if !line.cells.is_empty() && line.cells.len() + width > cols {
            line.hard_wrapped = true;
            line.resize_to(cols as u16, CellAttributes::default());
            out.push(line);
            line = Line {
                cells: Vec::with_capacity(cols),
                hard_wrapped: false,
                generation: 0,
            };
        }

        push_cell_with_continuation(&mut line.cells, cell, cols, width);
        emitted_any = true;
    }

    if emitted_any {
        line.resize_to(cols as u16, CellAttributes::default());
        out.push(line);
    } else {
        out.push(Line::blank(cols as u16));
    }
}

fn trim_selection_text(mut text: String) -> String {
    while text.ends_with(' ') {
        text.pop();
    }
    text
}

fn display_width(text: &str) -> usize {
    text.graphemes(true).map(UnicodeWidthStr::width).sum()
}

fn cell_width_for_text(text: &str) -> usize {
    display_width(text).clamp(1, 2)
}

fn cell_width_for_text_in_grid(text: &str, available_cols: usize) -> usize {
    // A single ASCII scalar — every space, letter, digit and punctuation mark in
    // ordinary output — is one column by definition. Measuring it through
    // grapheme segmentation and `UnicodeWidthStr` cost several passes over the
    // text for every cell, on every sanitize and every reflow.
    if is_single_ascii_scalar(text) {
        return 1;
    }
    let width = cell_width_for_text(text);
    if available_cols < width { 1 } else { width }
}

/// True for the one-byte ASCII cell contents that dominate terminal output.
/// Control bytes never reach a cell, so any such byte is a printable column.
fn is_single_ascii_scalar(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 1 && bytes[0] >= 0x20 && bytes[0] < 0x7f
}

fn reflow_cell_width(cell: &Cell, available_cols: usize) -> usize {
    if cell.width == 2 {
        return usize::from(available_cols >= 2).saturating_add(1);
    }
    if cell.text.is_ascii() {
        return 1;
    }
    cell_width_for_text_in_grid(&cell.text, available_cols)
}

fn scalar_cell_width(ch: char, cols: usize) -> usize {
    let width = UnicodeWidthChar::width(ch).unwrap_or(1).clamp(1, 2);
    if cols < width { 1 } else { width }
}

/// Scalars beyond this are dropped rather than appended to a cell. UAX-29 never
/// breaks a cluster before Extend/ZWJ, so without a cap a stream of combining
/// marks or joiners grows a single cell without bound, and every append
/// re-segments the whole cluster.
///
/// The bound has to clear the longest clusters real text produces: a
/// subdivision flag such as 🏴󠁧󠁢󠁳󠁣󠁴󠁿 is 8 scalars (base plus six tag characters and a
/// terminator), and a four-person family emoji with skin tones reaches 11.
const MAX_CELL_GRAPHEME_SCALARS: usize = 16;

fn extends_previous_grapheme(previous_text: &str, ch: char) -> bool {
    if ch.is_ascii() {
        return false;
    }

    // Build the candidate cluster on the stack. The previous text is bounded by
    // MAX_CELL_GRAPHEME_SCALARS, so this never needs to allocate; a heap string
    // per printed non-ASCII scalar was pure overhead on CJK and emoji output.
    let mut buffer = [0_u8; MAX_CELL_GRAPHEME_SCALARS * 4 + 4];
    let needed = previous_text.len() + ch.len_utf8();
    if needed > buffer.len() {
        return false;
    }
    buffer[..previous_text.len()].copy_from_slice(previous_text.as_bytes());
    let encoded = ch.encode_utf8(&mut buffer[previous_text.len()..]).len();
    let Ok(text) = std::str::from_utf8(&buffer[..previous_text.len() + encoded]) else {
        return false;
    };

    text.graphemes(true).count() == 1
}

/// Repairs the wide/continuation invariant around a single edited cell instead
/// of re-measuring an entire line.
fn repair_wide_pair(cells: &mut [Cell], col: usize, attributes: CellAttributes) {
    let Some(width) = cells.get(col).map(|cell| cell.width) else {
        return;
    };
    if width == 2 {
        if let Some(next) = cells.get_mut(col + 1) {
            *next = Cell::wide_continuation(attributes);
        }
    } else if cells
        .get(col + 1)
        .is_some_and(|cell| cell.wide_continuation)
    {
        cells[col + 1] = Cell::blank(attributes);
    }
    // Widening over a lead cell can orphan the continuation that followed it.
    if cells
        .get(col + 2)
        .is_some_and(|cell| cell.wide_continuation)
        && cells.get(col + 1).is_some_and(|cell| cell.width != 2)
    {
        cells[col + 2] = Cell::blank(attributes);
    }
}

fn previous_grapheme_col(line: &Line, col: usize) -> usize {
    if col == 0 {
        return 0;
    }

    let mut previous = col
        .saturating_sub(1)
        .min(line.cells.len().saturating_sub(1));
    if line
        .cells
        .get(previous)
        .is_some_and(|cell| cell.wide_continuation)
        && previous > 0
    {
        previous -= 1;
    }

    previous
}

fn next_grapheme_col(line: &Line, col: usize) -> usize {
    if line.cells.is_empty() {
        return 0;
    }

    let col = normalize_col_to_grapheme_start(line, col.min(line.cells.len() - 1));
    let width = line.cells[col].width.max(1) as usize;
    (col + width).min(line.cells.len() - 1)
}

fn normalize_col_to_grapheme_start(line: &Line, col: usize) -> usize {
    if line
        .cells
        .get(col)
        .is_some_and(|cell| cell.wide_continuation)
        && col > 0
    {
        col - 1
    } else {
        col
    }
}

fn expand_range_to_graphemes(line: &Line, from: usize, to: usize) -> Option<(usize, usize)> {
    if line.cells.is_empty() {
        return None;
    }

    let mut from = from.min(line.cells.len() - 1);
    let mut to = to.min(line.cells.len() - 1);
    if from > to {
        std::mem::swap(&mut from, &mut to);
    }

    from = normalize_col_to_grapheme_start(line, from);
    if line.cells[to].wide_continuation && to > 0 {
        to -= 1;
    }
    if line.cells[to].width > 1 && to + 1 < line.cells.len() {
        to += 1;
    }

    Some((from, to))
}

fn blank_range_expanding_graphemes(
    line: &mut Line,
    from: usize,
    to: usize,
    attributes: CellAttributes,
) {
    let Some((from, to)) = expand_range_to_graphemes(line, from, to) else {
        return;
    };

    for cell in &mut line.cells[from..=to] {
        *cell = Cell::blank(attributes);
    }
}

fn grapheme_delete_end(line: &Line, start: usize, count: usize) -> Option<usize> {
    if line.cells.is_empty() || count == 0 {
        return None;
    }

    let mut end = normalize_col_to_grapheme_start(line, start.min(line.cells.len() - 1));
    let mut consumed = 0;
    while end < line.cells.len() && consumed < count {
        if line.cells[end].wide_continuation {
            end += 1;
            continue;
        }

        let width = line.cells[end].width.max(1) as usize;
        consumed += width;
        end += width;
    }

    Some(end.min(line.cells.len()))
}

fn push_cell_with_continuation(cells: &mut Vec<Cell>, mut cell: Cell, cols: usize, width: usize) {
    debug_assert!((1..=2).contains(&width));
    debug_assert!(width <= cols.saturating_sub(cells.len()));
    cell.width = width as u8;
    cell.wide_continuation = false;
    let attributes = cell.attributes;
    cells.push(cell);
    if width == 2 && cells.len() < cols {
        cells.push(Cell::wide_continuation(attributes));
    }
}

fn sanitize_cells(cells: &mut [Cell], attributes: CellAttributes) {
    sanitize_cell_range(cells, 0, cells.len(), attributes);
}

/// Enforces the wide/continuation invariant over `start..end` only.
///
/// Available-column maths and the lead-cell lookup still use the whole line, so
/// a narrowed call is equivalent to the full sweep for any cell it covers. ICH
/// and DCH shift intact pairs, so only the edges they disturb need re-checking —
/// and sweeping the entire line there cost more than the shift itself.
fn sanitize_cell_range(cells: &mut [Cell], start: usize, end: usize, attributes: CellAttributes) {
    let end = end.min(cells.len());
    let mut index = start.min(end);
    while index < end {
        // Fast path for the common case: a plain ASCII cell whose neighbour is
        // not a stale continuation needs no measurement at all.
        if !cells[index].wide_continuation
            && is_single_ascii_scalar(&cells[index].text)
            && !cells
                .get(index + 1)
                .is_some_and(|next| next.wide_continuation)
        {
            cells[index].width = 1;
            index += 1;
            continue;
        }
        if cells[index].wide_continuation {
            if index == 0 || cells[index - 1].width != 2 {
                cells[index] = Cell::blank(attributes);
            }
            index += 1;
            continue;
        }

        let width = cell_width_for_text_in_grid(&cells[index].text, cells.len() - index);
        cells[index].width = width as u8;
        if width == 2 {
            if index + 1 < cells.len() {
                cells[index + 1] = Cell::wide_continuation(cells[index].attributes);
                index += 2;
                continue;
            }
            cells[index].width = 1;
        } else if index + 1 < cells.len() && cells[index + 1].wide_continuation {
            cells[index + 1] = Cell::blank(attributes);
        }
        index += 1;
    }
}

fn default_tab_stops(cols: u16) -> BTreeSet<u16> {
    let mut stops = BTreeSet::new();
    let mut col = 8;
    while col < cols {
        stops.insert(col);
        col += 8;
    }
    stops
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalError {
    message: String,
}

impl TerminalError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for TerminalError {}

pub type TerminalResult<T> = Result<T, TerminalError>;

/// Owns pure terminal state and applies already-received bytes to that state.
pub trait TerminalCore {
    fn apply_bytes(&mut self, bytes: &[u8]) -> TerminalResult<()>;

    fn resize(&mut self, size: TerminalSize) -> TerminalResult<()>;

    fn visible_grid(&self) -> VisibleGrid;

    fn scrollback(&self) -> Scrollback;

    fn cursor_state(&self) -> CursorState;

    fn modes(&self) -> BTreeSet<TerminalMode>;

    fn selection_state(&self) -> Option<SelectionRange>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn line_text(terminal: &TerminalState, row: u16) -> String {
        terminal.line(row).unwrap().raw_text()
    }

    fn assert_terminal_invariants(terminal: &TerminalState) {
        let grid = terminal.grid();
        let cols = usize::from(grid.size.cols.max(1));
        let rows = usize::from(grid.size.rows.max(1));

        assert_eq!(grid.lines.len(), rows);
        for line in &grid.lines {
            assert_eq!(line.cells.len(), cols);
            assert_line_invariants(line);
        }

        let visible = terminal.visible_grid();
        assert_eq!(visible.cells.len(), rows * cols);

        for line in terminal.scrollback().lines {
            assert_eq!(line.cells.len(), cols);
            assert_line_invariants(&line);
        }

        let cursor = terminal.cursor_state().position;
        assert!(cursor.row >= 0);
        assert!(usize::try_from(cursor.row).is_ok_and(|row| row < rows));
        assert!(usize::from(cursor.col) < cols);

        if let Some(text) = terminal.selected_text() {
            assert!(!text.contains('\u{fffd}'));
        }
    }

    fn assert_line_invariants(line: &Line) {
        for (index, cell) in line.cells.iter().enumerate() {
            assert!(cell.width <= 2);
            if cell.wide_continuation {
                assert_eq!(cell.width, 0);
                assert!(index > 0, "wide continuation at column 0");
                assert_eq!(
                    line.cells[index - 1].width,
                    2,
                    "wide continuation without wide base"
                );
            } else {
                assert!(cell.width >= 1);
                assert!(!cell.text.is_empty());
                if cell.width == 2 && index + 1 < line.cells.len() {
                    assert!(
                        line.cells[index + 1].wide_continuation,
                        "wide base without continuation"
                    );
                }
            }
        }
    }

    fn fuzz_char(selector: u8) -> char {
        match selector % 18 {
            0 => 'a',
            1 => 'Z',
            2 => '0',
            3 => ' ',
            4 => '\u{0301}',
            5 => '\u{0308}',
            6 => '界',
            7 => '語',
            8 => '👍',
            9 => '\u{1f3fd}',
            10 => '\u{200d}',
            11 => '👨',
            12 => '👩',
            13 => '👧',
            14 => '👦',
            15 => '♥',
            16 => '\u{fe0f}',
            _ => '\u{1f1fa}',
        }
    }

    fn apply_fuzz_tuple(terminal: &mut TerminalState, op: (u8, u16, u16, u16, u16)) {
        let (tag, a, b, c, d) = op;
        let size = terminal.grid().size;
        let row = (a % size.rows.max(1)) + 1;
        let col = (b % size.cols.max(1)) + 1;
        let count = (c % 8) + 1;

        let action = match tag % 22 {
            0 => TerminalAction::Print(fuzz_char((a ^ b ^ c ^ d) as u8)),
            1 => TerminalAction::CarriageReturn,
            2 => TerminalAction::LineFeed,
            3 => TerminalAction::Backspace,
            4 => TerminalAction::Tab,
            5 => TerminalAction::MoveCursor {
                direction: CursorDirection::Up,
                count,
            },
            6 => TerminalAction::MoveCursor {
                direction: CursorDirection::Down,
                count,
            },
            7 => TerminalAction::MoveCursor {
                direction: CursorDirection::Forward,
                count,
            },
            8 => TerminalAction::MoveCursor {
                direction: CursorDirection::Back,
                count,
            },
            9 => TerminalAction::SetCursorPosition { row, col },
            10 => TerminalAction::SetCursorColumn(col),
            11 => TerminalAction::ClearScreen(match d % 4 {
                0 => ClearMode::FromCursor,
                1 => ClearMode::ToCursor,
                2 => ClearMode::All,
                _ => ClearMode::Saved,
            }),
            12 => TerminalAction::ClearLine(match d % 3 {
                0 => ClearMode::FromCursor,
                1 => ClearMode::ToCursor,
                _ => ClearMode::All,
            }),
            13 => TerminalAction::InsertLines(count),
            14 => TerminalAction::DeleteLines(count),
            15 => TerminalAction::InsertChars(count),
            16 => TerminalAction::DeleteChars(count),
            17 => TerminalAction::EraseChars(count),
            18 => TerminalAction::SetMode {
                mode: TerminalMode::AlternateScreen,
                enabled: d % 2 == 0,
            },
            19 => TerminalAction::SetMode {
                mode: TerminalMode::Insert,
                enabled: d % 2 == 0,
            },
            20 => TerminalAction::SetGraphicRendition(match d % 4 {
                0 => GraphicRendition::Reset,
                1 => GraphicRendition::Bold,
                2 => GraphicRendition::Foreground(Color::Indexed((a % 256) as u8)),
                _ => GraphicRendition::Background(Color::Rgb {
                    red: a as u8,
                    green: b as u8,
                    blue: c as u8,
                }),
            }),
            _ => TerminalAction::SetScrollRegion {
                top: row.min(size.rows),
                bottom: (row + count).min(size.rows),
            },
        };

        terminal.apply_action(action).unwrap();

        if tag % 17 == 0 {
            let cols = (a % 80).max(1);
            let rows = (b % 24).max(1);
            terminal.resize(TerminalSize::new(cols, rows)).unwrap();
        }

        if tag % 19 == 0 {
            let size = terminal.grid().size;
            terminal.set_selection(Selection::normal(
                GridPosition::new(i64::from(a % size.rows.max(1)), b % size.cols.max(1)),
                GridPosition::new(i64::from(c % size.rows.max(1)), d % size.cols.max(1)),
            ));
        }
    }

    #[test]
    fn term_core_has_no_crate_dependencies() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            manifest.contains("unicode-width.workspace = true"),
            "term-core may use low-level Unicode utilities for terminal cell width"
        );
        assert!(
            manifest.contains("unicode-segmentation.workspace = true"),
            "term-core may use low-level Unicode utilities for grapheme boundaries"
        );
        assert!(
            !manifest.contains("render-")
                && !manifest.contains("platform-")
                && !manifest.contains("transport-")
                && !manifest.contains("config-")
                && !manifest.contains("term-parser")
                && !manifest.contains("mux"),
            "term-core must remain below parser, renderer, platform, transport, config, and mux"
        );
    }

    #[test]
    fn selection_copy_respects_hard_wraps() {
        let mut terminal = TerminalState::new(TerminalSize::new(3, 3));
        terminal
            .apply_actions("abcdef".chars().map(TerminalAction::Print))
            .unwrap();
        terminal.set_selection(Selection::normal(
            GridPosition::new(0, 0),
            GridPosition::new(1, 2),
        ));

        assert_eq!(terminal.selected_text().as_deref(), Some("abcdef"));
    }

    #[test]
    fn rectangular_selection_keeps_line_breaks() {
        let mut terminal = TerminalState::new(TerminalSize::new(5, 2));
        terminal
            .apply_actions("abcd\r\nefgh".chars().map(|ch| match ch {
                '\r' => TerminalAction::CarriageReturn,
                '\n' => TerminalAction::LineFeed,
                _ => TerminalAction::Print(ch),
            }))
            .unwrap();
        terminal.set_selection(Selection::rectangular(
            GridPosition::new(0, 1),
            GridPosition::new(1, 2),
        ));

        assert_eq!(terminal.selected_text().as_deref(), Some("bc\nfg"));
    }

    #[test]
    fn selection_span_for_row_projects_normal_and_rectangular_ranges() {
        let normal = Selection::normal(GridPosition::new(2, 3), GridPosition::new(4, 5));
        assert_eq!(normal.span_for_row(1, 10), None);
        assert_eq!(normal.span_for_row(2, 10), Some((3, 9)));
        assert_eq!(normal.span_for_row(3, 10), Some((0, 9)));
        assert_eq!(normal.span_for_row(4, 10), Some((0, 5)));

        let rectangular = Selection::rectangular(GridPosition::new(4, 7), GridPosition::new(2, 2));
        assert_eq!(rectangular.span_for_row(3, 10), Some((2, 7)));
        assert_eq!(rectangular.span_for_row(5, 10), None);
        assert_eq!(rectangular.span_for_row(3, 0), None);
    }

    #[test]
    fn combining_marks_stay_in_the_base_cell() {
        let mut terminal = TerminalState::new(TerminalSize::new(6, 2));
        terminal
            .apply_actions("e\u{301}x".chars().map(TerminalAction::Print))
            .unwrap();

        let accented = terminal.cell(0, 0).unwrap();
        assert_eq!(accented.text, "e\u{301}");
        assert_eq!(accented.width, 1);
        assert_eq!(line_text(&terminal, 0), "e\u{301}x");
        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 2));
    }

    #[test]
    fn ordinary_cell_text_stays_inline() {
        let cell = Cell::text("x", CellAttributes::default());

        assert!(!cell.text.is_heap_allocated());
    }

    #[test]
    fn printable_text_batch_matches_scalar_actions() {
        let mut scenarios = Vec::new();

        scenarios.push((TerminalState::new(TerminalSize::new(5, 2)), "abcdefghi"));

        let mut no_wrap = TerminalState::new(TerminalSize::new(5, 2));
        no_wrap
            .apply_action(TerminalAction::SetMode {
                mode: TerminalMode::AutoWrap,
                enabled: false,
            })
            .unwrap();
        no_wrap
            .apply_action(TerminalAction::SetCursorPosition { row: 1, col: 5 })
            .unwrap();
        scenarios.push((no_wrap, "abc"));

        let mut insert = TerminalState::new(TerminalSize::new(8, 2));
        insert
            .apply_actions("abcd".chars().map(TerminalAction::Print))
            .unwrap();
        insert
            .apply_action(TerminalAction::SetCursorPosition { row: 1, col: 2 })
            .unwrap();
        insert
            .apply_action(TerminalAction::SetMode {
                mode: TerminalMode::Insert,
                enabled: true,
            })
            .unwrap();
        scenarios.push((insert, "XY"));

        let mut wide = TerminalState::new(TerminalSize::new(8, 2));
        wide.apply_actions("a界b".chars().map(TerminalAction::Print))
            .unwrap();
        wide.apply_action(TerminalAction::SetCursorPosition { row: 1, col: 3 })
            .unwrap();
        scenarios.push((wide, "xy"));

        scenarios.push((
            TerminalState::new(TerminalSize::new(12, 2)),
            "a界e\u{301}👍🏽z",
        ));

        for (initial, text) in scenarios {
            let mut batched = initial.clone();
            let mut scalar = initial;

            batched.apply_printable_text(text);
            scalar
                .apply_actions(text.chars().map(TerminalAction::Print))
                .unwrap();

            // Revision counts cache invalidations, not terminal semantics. A
            // batch intentionally invalidates once while scalar actions do so
            // once per character.
            scalar.render_revision = batched.render_revision;
            assert_eq!(batched, scalar, "batch differed for {text:?}");
        }
    }

    #[test]
    fn borrowed_visible_cells_match_the_owned_grid() {
        let mut terminal = TerminalState::new(TerminalSize::new(5, 2));
        terminal.apply_printable_text("abcdefghi");
        let owned = terminal.visible_grid();
        let mut borrowed = Vec::new();

        terminal.for_each_visible_cell(|cell| borrowed.push(cell.clone()));

        assert_eq!(borrowed, owned.cells);
        assert_eq!(terminal.viewport(), owned.viewport);
    }

    #[test]
    fn wide_cjk_occupies_base_and_continuation_cells() {
        let mut terminal = TerminalState::new(TerminalSize::new(6, 2));
        terminal
            .apply_actions("界x".chars().map(TerminalAction::Print))
            .unwrap();

        assert_eq!(terminal.cell(0, 0).unwrap().text, "界");
        assert_eq!(terminal.cell(0, 0).unwrap().width, 2);
        assert!(terminal.cell(0, 1).unwrap().wide_continuation);
        assert_eq!(terminal.cell(0, 2).unwrap().text, "x");
        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 3));
    }

    #[test]
    fn emoji_modifiers_stay_in_one_wide_grapheme() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
        terminal
            .apply_actions("👍🏽x".chars().map(TerminalAction::Print))
            .unwrap();

        assert_eq!(terminal.cell(0, 0).unwrap().text, "👍🏽");
        assert_eq!(terminal.cell(0, 0).unwrap().width, 2);
        assert!(terminal.cell(0, 1).unwrap().wide_continuation);
        assert_eq!(terminal.cell(0, 2).unwrap().text, "x");
        assert_eq!(line_text(&terminal, 0), "👍🏽x");
    }

    #[test]
    fn zwj_emoji_sequence_stays_in_one_wide_grapheme() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
        terminal
            .apply_actions("👨‍👩‍👧‍👦x".chars().map(TerminalAction::Print))
            .unwrap();

        assert_eq!(terminal.cell(0, 0).unwrap().text, "👨‍👩‍👧‍👦");
        assert_eq!(terminal.cell(0, 0).unwrap().width, 2);
        assert!(terminal.cell(0, 1).unwrap().wide_continuation);
        assert_eq!(terminal.cell(0, 2).unwrap().text, "x");
        assert_eq!(line_text(&terminal, 0), "👨‍👩‍👧‍👦x");
    }

    #[test]
    fn variation_selector_extends_previous_grapheme() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
        terminal
            .apply_actions("♥️x".chars().map(TerminalAction::Print))
            .unwrap();

        assert_eq!(terminal.cell(0, 0).unwrap().text, "♥️");
        let x_col = terminal.cell(0, 0).unwrap().width as u16;
        assert_eq!(terminal.cell(0, x_col).unwrap().text, "x");
        assert_eq!(line_text(&terminal, 0), "♥️x");
    }

    #[test]
    fn mixed_unicode_text_preserves_cell_boundaries() {
        let mut terminal = TerminalState::new(TerminalSize::new(12, 2));
        terminal
            .apply_actions("a界e\u{301}👍🏽z".chars().map(TerminalAction::Print))
            .unwrap();

        assert_eq!(line_text(&terminal, 0), "a界e\u{301}👍🏽z");
        assert_eq!(terminal.cell(0, 1).unwrap().text, "界");
        assert!(terminal.cell(0, 2).unwrap().wide_continuation);
        assert_eq!(terminal.cell(0, 3).unwrap().text, "e\u{301}");
        assert_eq!(terminal.cell(0, 4).unwrap().text, "👍🏽");
        assert!(terminal.cell(0, 5).unwrap().wide_continuation);
    }

    #[test]
    fn cursor_movement_and_backspace_skip_wide_continuations() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
        terminal
            .apply_actions("界x".chars().map(TerminalAction::Print))
            .unwrap();

        terminal
            .apply_action(TerminalAction::MoveCursor {
                direction: CursorDirection::Back,
                count: 1,
            })
            .unwrap();
        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 2));

        terminal.apply_action(TerminalAction::Backspace).unwrap();
        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 0));
    }

    #[test]
    fn selection_on_continuation_cell_extracts_whole_grapheme() {
        let mut terminal = TerminalState::new(TerminalSize::new(6, 2));
        terminal
            .apply_actions("a界b".chars().map(TerminalAction::Print))
            .unwrap();
        terminal.set_selection(Selection::normal(
            GridPosition::new(0, 2),
            GridPosition::new(0, 2),
        ));

        assert_eq!(terminal.selected_text().as_deref(), Some("界"));
    }

    #[test]
    fn overwriting_half_of_wide_grapheme_clears_the_whole_cell_pair() {
        let mut terminal = TerminalState::new(TerminalSize::new(6, 2));
        terminal
            .apply_actions("界".chars().map(TerminalAction::Print))
            .unwrap();
        terminal
            .apply_action(TerminalAction::SetCursorPosition { row: 1, col: 2 })
            .unwrap();
        terminal.apply_action(TerminalAction::Print('x')).unwrap();

        assert_eq!(line_text(&terminal, 0), "x");
        assert!(!terminal.cell(0, 1).unwrap().wide_continuation);
    }

    #[test]
    fn erase_and_delete_do_not_leave_orphan_continuations() {
        let mut erased = TerminalState::new(TerminalSize::new(8, 2));
        erased
            .apply_actions("a界b".chars().map(TerminalAction::Print))
            .unwrap();
        erased
            .apply_action(TerminalAction::SetCursorPosition { row: 1, col: 2 })
            .unwrap();
        erased.apply_action(TerminalAction::EraseChars(1)).unwrap();
        assert_eq!(line_text(&erased, 0), "a  b");
        assert!(!erased.cell(0, 2).unwrap().wide_continuation);

        let mut deleted = TerminalState::new(TerminalSize::new(8, 2));
        deleted
            .apply_actions("a界b".chars().map(TerminalAction::Print))
            .unwrap();
        deleted
            .apply_action(TerminalAction::SetCursorPosition { row: 1, col: 2 })
            .unwrap();
        deleted
            .apply_action(TerminalAction::DeleteChars(1))
            .unwrap();
        assert_eq!(line_text(&deleted, 0), "ab");
        assert!(!deleted.cell(0, 1).unwrap().wide_continuation);
    }

    #[test]
    fn resize_reflows_without_splitting_wide_graphemes() {
        let mut terminal = TerminalState::new(TerminalSize::new(5, 2));
        terminal
            .apply_actions("a界b".chars().map(TerminalAction::Print))
            .unwrap();

        terminal.resize(TerminalSize::new(2, 4)).unwrap();

        assert_eq!(line_text(&terminal, 0), "a");
        assert_eq!(line_text(&terminal, 1), "界");
        assert!(terminal.cell(1, 1).unwrap().wide_continuation);
        assert_eq!(line_text(&terminal, 2), "b");
    }

    #[test]
    fn reflow_width_uses_cached_ascii_width_but_recovers_clamped_wide_cells() {
        let ascii = Cell::text("x", CellAttributes::default());
        assert_eq!(reflow_cell_width(&ascii, 80), 1);

        let mut previously_clamped = Cell::text("界", CellAttributes::default());
        previously_clamped.width = 1;
        assert_eq!(reflow_cell_width(&previously_clamped, 80), 2);
        assert_eq!(reflow_cell_width(&previously_clamped, 1), 1);
    }

    #[test]
    fn resize_preserves_cursor_after_prompt_trailing_space() {
        let mut terminal = TerminalState::new(TerminalSize::new(86, 26));
        terminal
            .apply_actions("PS C:\\Users\\shres> ".chars().map(TerminalAction::Print))
            .unwrap();
        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 19));

        terminal.resize(TerminalSize::new(171, 42)).unwrap();

        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 19));
        terminal.apply_action(TerminalAction::Print('W')).unwrap();
        terminal.apply_action(TerminalAction::Backspace).unwrap();
        terminal
            .apply_actions("Wr".chars().map(TerminalAction::Print))
            .unwrap();
        assert_eq!(line_text(&terminal, 0), "PS C:\\Users\\shres> Wr");
        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 21));
    }

    #[test]
    fn startup_shrink_and_grow_keeps_first_shell_output_at_top() {
        let mut terminal = TerminalState::new(TerminalSize::new(171, 54));

        // Borderless fullscreen negotiation may briefly report the window's
        // configured size before the monitor-sized surface arrives.
        terminal.resize(TerminalSize::new(100, 28)).unwrap();
        terminal
            .apply_actions(
                "Windows PowerShell 5.1\r\nCopyright (C) Microsoft Corporation.\r\n\r\nPS C:\\Users\\shres> "
                    .chars()
                    .map(|ch| match ch {
                        '\r' => TerminalAction::CarriageReturn,
                        '\n' => TerminalAction::LineFeed,
                        _ => TerminalAction::Print(ch),
                    }),
            )
            .unwrap();
        terminal.resize(TerminalSize::new(171, 54)).unwrap();

        assert_eq!(line_text(&terminal, 0), "Windows PowerShell 5.1");
        assert_eq!(
            line_text(&terminal, 1),
            "Copyright (C) Microsoft Corporation."
        );
        assert_eq!(line_text(&terminal, 2), "");
        assert_eq!(line_text(&terminal, 3), "PS C:\\Users\\shres>");
        assert_eq!(terminal.cursor_state().position, GridPosition::new(3, 19));
        assert!(terminal.scrollback().lines.is_empty());
    }

    #[test]
    fn shrinking_moves_only_rows_above_cursor_into_scrollback() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 5));
        terminal
            .apply_actions("one\r\ntwo\r\nthree\r\nfour".chars().map(|ch| match ch {
                '\r' => TerminalAction::CarriageReturn,
                '\n' => TerminalAction::LineFeed,
                _ => TerminalAction::Print(ch),
            }))
            .unwrap();

        terminal.resize(TerminalSize::new(8, 3)).unwrap();

        assert_eq!(terminal.scrollback().lines.len(), 1);
        assert_eq!(line_text(&terminal, 0), "two");
        assert_eq!(line_text(&terminal, 1), "three");
        assert_eq!(line_text(&terminal, 2), "four");
        assert_eq!(terminal.cursor_state().position, GridPosition::new(2, 4));
    }

    #[test]
    fn scrollback_preserves_wide_graphemes() {
        let mut terminal = TerminalState::new(TerminalSize::new(4, 2));
        terminal
            .apply_actions("界\r\nx\r\ny".chars().map(|ch| match ch {
                '\r' => TerminalAction::CarriageReturn,
                '\n' => TerminalAction::LineFeed,
                _ => TerminalAction::Print(ch),
            }))
            .unwrap();

        let scrollback = terminal.scrollback();
        assert_eq!(
            scrollback.lines.front().map(Line::raw_text).as_deref(),
            Some("界")
        );
        assert_eq!(line_text(&terminal, 0), "x");
        assert_eq!(line_text(&terminal, 1), "y");
    }

    fn feed(terminal: &mut TerminalState, text: &str) {
        terminal
            .apply_actions(text.chars().map(|ch| match ch {
                '\r' => TerminalAction::CarriageReturn,
                '\n' => TerminalAction::LineFeed,
                _ => TerminalAction::Print(ch),
            }))
            .unwrap();
    }

    fn assert_wide_pair_invariant(terminal: &TerminalState, label: &str) {
        for (row, line) in terminal.primary.lines.iter().enumerate() {
            for (col, cell) in line.cells.iter().enumerate() {
                if cell.wide_continuation {
                    let lead = col
                        .checked_sub(1)
                        .and_then(|previous| line.cells.get(previous));
                    assert!(
                        lead.is_some_and(|lead| lead.width == 2),
                        "{label}: orphaned continuation at {row},{col} in {:?}",
                        line.raw_text()
                    );
                }
                if cell.width == 2 {
                    assert!(
                        line.cells
                            .get(col + 1)
                            .is_some_and(|next| next.wide_continuation),
                        "{label}: wide cell without continuation at {row},{col} in {:?}",
                        line.raw_text()
                    );
                }
            }
        }
    }

    #[test]
    fn narrowed_edit_sanitizing_keeps_wide_pairs_intact() {
        // ICH/DCH only re-check the edges they disturb, so exercise every
        // insertion point against wide graphemes and assert the invariant the
        // full-line sweep used to guarantee.
        for content in ["日本語テキスト", "a日b本c語d", "日aa本bb語", "ab日本cd"] {
            for col in 0..8u16 {
                for count in 1..4u16 {
                    let mut terminal = TerminalState::new(TerminalSize::new(12, 2));
                    terminal
                        .apply_actions(content.chars().map(TerminalAction::Print))
                        .unwrap();
                    terminal
                        .apply_action(TerminalAction::SetCursorPosition {
                            row: 1,
                            col: col + 1,
                        })
                        .unwrap();

                    terminal
                        .apply_action(TerminalAction::InsertChars(count))
                        .unwrap();
                    assert_wide_pair_invariant(
                        &terminal,
                        &format!("ICH {content:?} col={col} count={count}"),
                    );

                    terminal
                        .apply_action(TerminalAction::DeleteChars(count))
                        .unwrap();
                    assert_wide_pair_invariant(
                        &terminal,
                        &format!("DCH {content:?} col={col} count={count}"),
                    );
                }
            }
        }
    }

    #[test]
    fn insert_chars_shifts_the_cursor_cell_instead_of_erasing_it() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 1));
        feed(&mut terminal, "abcd");
        terminal
            .apply_action(TerminalAction::SetCursorPosition { row: 1, col: 2 })
            .unwrap();

        // bash/zsh use ICH for mid-line insertion; the character under the
        // cursor must move right, not vanish.
        terminal
            .apply_action(TerminalAction::InsertChars(1))
            .unwrap();

        assert_eq!(line_text(&terminal, 0), "a bcd");
    }

    #[test]
    fn erasing_keeps_colours_but_drops_the_graphic_rendition() {
        let mut terminal = TerminalState::new(TerminalSize::new(6, 2));
        terminal
            .apply_actions([
                TerminalAction::SetGraphicRendition(GraphicRendition::Inverse),
                TerminalAction::SetGraphicRendition(GraphicRendition::Underline),
                TerminalAction::SetGraphicRendition(GraphicRendition::Background(Color::Indexed(
                    4,
                ))),
            ])
            .unwrap();
        feed(&mut terminal, "ab");

        // `ESC[7m ESC[K` must clear to end of line, not paint an inverse bar.
        terminal
            .apply_action(TerminalAction::ClearLine(ClearMode::All))
            .unwrap();

        let cell = terminal.cell(0, 3).expect("erased cell");
        assert!(!cell.attributes.inverse, "erase must not carry inverse");
        assert!(!cell.attributes.underline, "erase must not carry underline");
        assert_eq!(
            cell.attributes.background,
            Some(Color::Indexed(4)),
            "back-colour erase must keep the background"
        );

        // A scrolled-in blank line must not be underlined either.
        feed(&mut terminal, "\r\n\r\n\r\n");
        let scrolled = terminal.cell(1, 0).expect("scrolled cell");
        assert!(!scrolled.attributes.underline);
    }

    #[test]
    fn each_screen_keeps_its_own_saved_cursor() {
        let mut terminal = TerminalState::new(TerminalSize::new(10, 4));
        terminal
            .apply_action(TerminalAction::SetCursorPosition { row: 3, col: 5 })
            .unwrap();
        terminal.apply_action(TerminalAction::SaveCursor).unwrap();

        // Enter the alternate screen and save a different position there.
        terminal
            .apply_action(TerminalAction::SetMode {
                mode: TerminalMode::AlternateScreen,
                enabled: true,
            })
            .unwrap();
        terminal
            .apply_action(TerminalAction::SetCursorPosition { row: 1, col: 1 })
            .unwrap();
        terminal.apply_action(TerminalAction::SaveCursor).unwrap();
        terminal
            .apply_action(TerminalAction::SetMode {
                mode: TerminalMode::AlternateScreen,
                enabled: false,
            })
            .unwrap();

        terminal
            .apply_action(TerminalAction::RestoreCursor)
            .unwrap();

        let cursor = terminal.cursor_state().position;
        assert_eq!(
            (cursor.row, cursor.col),
            (2, 4),
            "the shell's saved cursor must survive an editor saving its own"
        );
    }

    #[test]
    fn cursor_movement_stays_inside_the_scrolling_region() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 6));
        terminal
            .apply_action(TerminalAction::SetScrollRegion { top: 2, bottom: 4 })
            .unwrap();
        terminal
            .apply_action(TerminalAction::SetCursorPosition { row: 3, col: 1 })
            .unwrap();

        terminal
            .apply_action(TerminalAction::MoveCursor {
                direction: CursorDirection::Down,
                count: 10,
            })
            .unwrap();

        // Row 4 (1-based) is the bottom margin; walking past it would run over a
        // status line an application reserved.
        assert_eq!(terminal.cursor_state().position.row, 3);
    }

    #[test]
    fn cursor_position_reports_are_relative_to_origin_mode() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 6));
        terminal
            .apply_action(TerminalAction::SetScrollRegion { top: 3, bottom: 5 })
            .unwrap();
        terminal
            .apply_action(TerminalAction::SetMode {
                mode: TerminalMode::Origin,
                enabled: true,
            })
            .unwrap();
        terminal
            .apply_action(TerminalAction::SetCursorPosition { row: 1, col: 1 })
            .unwrap();
        let _ = terminal.take_pending_output();

        terminal
            .apply_action(TerminalAction::DeviceStatusReport(6))
            .unwrap();

        assert_eq!(
            String::from_utf8_lossy(&terminal.take_pending_output()),
            "\x1b[1;1R",
            "with origin mode the report is relative to the top margin"
        );
    }

    #[test]
    fn reflow_keeps_trailing_cells_that_carry_a_background() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 4));
        // A prompt segment: two glyphs then two blanks that carry the segment
        // colour. This line is not soft-wrapped.
        feed(&mut terminal, "AB");
        terminal
            .apply_action(TerminalAction::SetGraphicRendition(
                GraphicRendition::Background(Color::Indexed(5)),
            ))
            .unwrap();
        feed(&mut terminal, "  ");
        terminal
            .apply_action(TerminalAction::SetGraphicRendition(
                GraphicRendition::DefaultBackground,
            ))
            .unwrap();
        terminal
            .apply_actions([TerminalAction::CarriageReturn, TerminalAction::LineFeed])
            .unwrap();
        // A separate wrapped line forces the rejoining reflow path, so the
        // coloured line above is measured by `line_content_len`.
        feed(&mut terminal, "wrapped-content");

        <TerminalState as TerminalCore>::resize(&mut terminal, TerminalSize::new(6, 4)).unwrap();

        // Count across scrollback and the visible grid: reflow may push the
        // coloured line above the viewport.
        let coloured = terminal
            .scrollback_lines()
            .iter()
            .flat_map(|line| line.cells.iter())
            .chain(
                terminal
                    .primary
                    .lines
                    .iter()
                    .flat_map(|line| line.cells.iter()),
            )
            .filter(|cell| cell.attributes.background == Some(Color::Indexed(5)))
            .count();
        assert_eq!(
            coloured, 2,
            "the two background-carrying blanks must survive reflow"
        );
    }

    #[test]
    fn an_unwrapped_history_resizes_columns_without_rebuilding() {
        let mut terminal = TerminalState::with_scrollback_limit(TerminalSize::new(20, 2), 32);
        for index in 0..10 {
            feed(
                &mut terminal,
                &format!(
                    "row{index}
"
                ),
            );
        }
        let before_rows = terminal.buffer_line_count();

        // Grow, then shrink back: no line is wrapped and every line fits, so
        // this takes the in-place path and must not disturb the buffer.
        <TerminalState as TerminalCore>::resize(&mut terminal, TerminalSize::new(40, 2)).unwrap();
        <TerminalState as TerminalCore>::resize(&mut terminal, TerminalSize::new(20, 2)).unwrap();

        assert_eq!(terminal.buffer_line_count(), before_rows);
        assert_eq!(
            terminal
                .scrollback_lines()
                .front()
                .map(Line::raw_text)
                .as_deref()
                .map(str::trim_end),
            Some("row0"),
            "content must stay on the line it was already on"
        );
        for line in terminal.scrollback_lines() {
            assert_eq!(line.cells.len(), 20, "every line must match the new width");
        }
    }

    #[test]
    fn large_history_resize_materializes_only_the_viewport_neighborhood() {
        let mut terminal = TerminalState::with_scrollback_limit(TerminalSize::new(16, 4), 512);
        for index in 0..160 {
            feed(
                &mut terminal,
                &format!("row-{index:04}-abcdefghijklmnop\r\n"),
            );
        }

        terminal.resize(TerminalSize::new(8, 4)).unwrap();

        let stats = terminal.history_stats();
        assert!(stats.canonical_logical_lines > 100);
        assert!(stats.materialized_physical_rows <= 8);
        assert!(stats.materialized_logical_lines <= 8);
    }

    #[test]
    fn history_selection_remains_attached_to_text_after_lazy_reflow() {
        let mut terminal = TerminalState::with_scrollback_limit(TerminalSize::new(8, 2), 64);
        feed(&mut terminal, "zero\r\nabcdefgh\r\ntail");
        let before = terminal.search("cdef", true).remove(0);
        terminal.set_selection(before);
        assert_eq!(terminal.selected_text().as_deref(), Some("cdef"));

        terminal.resize(TerminalSize::new(4, 2)).unwrap();

        assert_eq!(terminal.selected_text().as_deref(), Some("cdef"));
    }

    #[test]
    fn caller_owned_positions_can_be_remapped_with_the_resize() {
        let mut terminal = TerminalState::with_scrollback_limit(TerminalSize::new(8, 2), 64);
        feed(&mut terminal, "zero\r\nabcdefgh\r\ntail");
        let match_position = terminal.search("cdef", true).remove(0).start;
        let mut positions = [match_position];

        terminal
            .resize_with_positions(TerminalSize::new(4, 2), &mut positions)
            .unwrap();

        let mapped = Selection::normal(positions[0], GridPosition::new(positions[0].row + 1, 1));
        assert_eq!(terminal.text_for_selection(mapped), "cdef");
    }

    #[test]
    fn a_combining_mark_at_column_zero_does_not_reach_an_unwrapped_row() {
        let mut terminal = TerminalState::new(TerminalSize::new(4, 3));
        feed(&mut terminal, "ab\r\n");
        // Row 1 is a fresh line, not a soft-wrap continuation of row 0.
        terminal
            .apply_action(TerminalAction::Print('\u{301}'))
            .unwrap();

        assert_eq!(
            terminal.cell(0, 1).map(|cell| cell.text.as_str()),
            Some("b"),
            "the previous row must be left alone"
        );
    }

    #[test]
    fn a_full_reset_keeps_replies_already_queued_for_the_host() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
        terminal
            .apply_action(TerminalAction::PrimaryDeviceAttributes)
            .unwrap();
        assert!(!terminal.pending_output().is_empty());

        terminal.apply_action(TerminalAction::Reset).unwrap();

        assert!(
            !terminal.pending_output().is_empty(),
            "an application waiting on its query must still get an answer"
        );
    }

    #[test]
    fn a_flood_of_combining_marks_cannot_grow_one_cell_without_bound() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
        terminal.apply_action(TerminalAction::Print('e')).unwrap();
        // UAX-29 never breaks a cluster before Extend, so an uncapped cell would
        // absorb every one of these and re-segment the whole cluster each time.
        terminal
            .apply_actions(std::iter::repeat_n(TerminalAction::Print('\u{301}'), 5_000))
            .unwrap();

        let cell = terminal.cell(0, 0).expect("base cell");
        assert!(
            cell.text.chars().count() <= MAX_CELL_GRAPHEME_SCALARS,
            "cell grew to {} scalars",
            cell.text.chars().count()
        );
        assert!(cell.text.starts_with('e'));
        // Overflow is dropped, not printed into the next cell.
        assert_eq!(
            terminal.cell(0, 1).map(|cell| cell.text.as_str()),
            Some(" ")
        );
    }

    #[test]
    fn emoji_clusters_still_join_under_the_cell_cap() {
        // The cap has to clear the longest real clusters: a four-person family
        // is 7 scalars and a subdivision flag is 8.
        for text in ["👨‍👩‍👧‍👦", "🏴󠁧󠁢󠁳󠁣󠁴󠁿", "👩🏽‍🚒"]
        {
            let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
            terminal
                .apply_actions(text.chars().map(TerminalAction::Print))
                .unwrap();

            assert_eq!(
                terminal.cell(0, 0).map(|cell| cell.text.as_str()),
                Some(text),
                "{text:?} must stay one cluster in one cell"
            );
        }
    }

    #[test]
    fn scrollback_stops_growing_at_its_limit() {
        let mut terminal = TerminalState::with_scrollback_limit(TerminalSize::new(8, 2), 4);
        for index in 0..200 {
            feed(&mut terminal, &format!("line{index}\r\n"));
        }

        assert_eq!(
            terminal.scrollback_lines().len(),
            4,
            "an uncapped buffer is how `cat` of a large log exhausts memory"
        );
        assert!(terminal.scrollback_dropped() > 0);
        // The retained window is the newest contiguous run of evicted lines.
        // `line199` is still on the visible grid, so `line198` is the newest one
        // to have scrolled off.
        let newest = terminal
            .scrollback_lines()
            .back()
            .map(Line::raw_text)
            .unwrap_or_default();
        let oldest = terminal
            .scrollback_lines()
            .front()
            .map(Line::raw_text)
            .unwrap_or_default();
        assert_eq!(newest.trim_end(), "line198");
        assert_eq!(oldest.trim_end(), "line195");
        assert_eq!(
            terminal.buffer_line_count(),
            terminal.scrollback_lines().len() + 2
        );
    }

    #[test]
    fn a_pinned_viewport_does_not_drift_once_lines_are_evicted() {
        let mut terminal = TerminalState::with_scrollback_limit(TerminalSize::new(8, 2), 3);
        feed(&mut terminal, "aaa\r\nbbb\r\nccc\r\nddd\r\n");
        // Scroll back to the top of the retained buffer.
        assert!(terminal.scroll_viewport(64));
        let anchored = visible_line_text(&terminal.visible_grid(), 0);

        // Once the limit is reached, every new line both appends and evicts, so
        // a viewport pinned by scrollback length alone would slide by one line
        // per line of output.
        for index in 0..20 {
            feed(&mut terminal, &format!("x{index}\r\n"));
            let visible = terminal.visible_grid();
            assert_eq!(
                visible.viewport.origin_row, 0,
                "a viewport scrolled to the top must stay at the top"
            );
            assert_ne!(visible_line_text(&visible, 0), "");
        }
        let _ = anchored;
        assert!(terminal.scroll_to_bottom());
    }

    #[test]
    fn a_selection_follows_its_text_until_that_text_is_evicted() {
        let mut terminal = TerminalState::with_scrollback_limit(TerminalSize::new(8, 2), 4);
        feed(&mut terminal, "keep\r\n");
        feed(&mut terminal, "more\r\n");

        // Select the first scrollback row, then push exactly one line off it.
        let row = 0;
        terminal.set_selection(Selection::normal(
            GridPosition::new(row, 0),
            GridPosition::new(row, 3),
        ));
        assert_eq!(terminal.selected_text().as_deref(), Some("keep"));

        feed(&mut terminal, "a\r\nb\r\nc\r\n");
        // Still retained, just at a lower absolute row: the selection must have
        // been rebased rather than left pointing at whatever moved into its old
        // coordinates.
        if let Some(text) = terminal.selected_text() {
            assert_eq!(text, "keep", "a rebased selection must keep its own text");
        }

        // Push it out of the buffer entirely.
        for index in 0..20 {
            feed(&mut terminal, &format!("z{index}\r\n"));
        }
        assert!(
            terminal.selected_text().is_none_or(|text| text != "keep"),
            "a selection whose text left the buffer must not resurface"
        );
    }

    #[test]
    fn clearing_saved_lines_rebases_absolute_rows() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
        feed(&mut terminal, "one\r\ntwo\r\nthree\r\n");
        let dropped_before = terminal.scrollback_dropped();
        assert!(terminal.scrollback_lines().len() >= 2);

        terminal
            .apply_action(TerminalAction::ClearScreen(ClearMode::Saved))
            .unwrap();

        assert!(terminal.scrollback_lines().is_empty());
        assert!(
            terminal.scrollback_dropped() > dropped_before,
            "an explicit scrollback clear still moves absolute rows"
        );
        assert_eq!(terminal.viewport_offset(), 0);
    }

    #[test]
    fn lowering_the_limit_trims_immediately() {
        let mut terminal = TerminalState::with_scrollback_limit(TerminalSize::new(8, 2), 100);
        for index in 0..50 {
            feed(&mut terminal, &format!("line{index}\r\n"));
        }
        assert!(terminal.scrollback_lines().len() > 8);

        terminal.set_scrollback_limit(8);

        assert_eq!(terminal.scrollback_lines().len(), 8);
        assert_eq!(terminal.scrollback_limit(), 8);
    }

    #[test]
    fn viewport_scrolls_through_scrollback_and_stays_anchored() {
        let mut terminal = TerminalState::new(TerminalSize::new(6, 2));
        terminal
            .apply_actions("one\r\ntwo\r\nthree".chars().map(|ch| match ch {
                '\r' => TerminalAction::CarriageReturn,
                '\n' => TerminalAction::LineFeed,
                _ => TerminalAction::Print(ch),
            }))
            .unwrap();

        assert!(terminal.scroll_viewport(1));
        let visible = terminal.visible_grid();
        assert_eq!(visible.viewport.origin_row, 0);
        assert_eq!(visible_line_text(&visible, 0), "one");
        assert_eq!(visible_line_text(&visible, 1), "two");

        terminal
            .apply_actions("\r\nfour".chars().map(|ch| match ch {
                '\r' => TerminalAction::CarriageReturn,
                '\n' => TerminalAction::LineFeed,
                _ => TerminalAction::Print(ch),
            }))
            .unwrap();

        let visible = terminal.visible_grid();
        assert_eq!(visible.viewport.origin_row, 0);
        assert_eq!(visible_line_text(&visible, 0), "one");
        assert_eq!(terminal.viewport_offset(), 2);
        assert!(terminal.scroll_to_bottom());
        assert_eq!(terminal.visible_grid().viewport.origin_row, 2);
    }

    #[test]
    fn selection_uses_absolute_scrollback_positions_and_reverse_drag_order() {
        let mut terminal = TerminalState::new(TerminalSize::new(5, 2));
        terminal
            .apply_actions("abcd\r\nefgh\r\nijkl".chars().map(|ch| match ch {
                '\r' => TerminalAction::CarriageReturn,
                '\n' => TerminalAction::LineFeed,
                _ => TerminalAction::Print(ch),
            }))
            .unwrap();

        terminal.set_selection(Selection::normal(
            GridPosition::new(1, 1),
            GridPosition::new(0, 2),
        ));

        assert_eq!(terminal.selected_text().as_deref(), Some("cd\nef"));
    }

    #[test]
    fn selection_remains_anchored_while_new_output_arrives() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
        terminal
            .apply_actions("selected".chars().map(TerminalAction::Print))
            .unwrap();
        terminal.set_selection(Selection::normal(
            GridPosition::new(0, 0),
            GridPosition::new(0, 7),
        ));

        terminal.apply_action(TerminalAction::LineFeed).unwrap();
        terminal.apply_action(TerminalAction::Print('x')).unwrap();

        assert_eq!(terminal.selected_text().as_deref(), Some("selected"));
    }

    #[test]
    fn search_finds_unicode_and_text_across_hard_wrapped_lines() {
        let mut terminal = TerminalState::new(TerminalSize::new(5, 3));
        terminal
            .apply_actions("hello\u{754c}world".chars().map(TerminalAction::Print))
            .unwrap();

        let matches = terminal.search("O\u{754c}W", false);

        assert_eq!(matches.len(), 1);
        terminal.set_selection(matches[0]);
        assert_eq!(terminal.selected_text().as_deref(), Some("o\u{754c}w"));
    }

    #[test]
    fn search_does_not_cross_hard_line_breaks_and_reveals_match() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
        terminal
            .apply_actions("first\r\nneedle\r\nlast".chars().map(|ch| match ch {
                '\r' => TerminalAction::CarriageReturn,
                '\n' => TerminalAction::LineFeed,
                _ => TerminalAction::Print(ch),
            }))
            .unwrap();

        assert!(terminal.search("tneedle", false).is_empty());
        let found = terminal.search("first", false);
        assert_eq!(found.len(), 1);
        assert!(terminal.reveal_position(found[0].start));
        assert_eq!(terminal.visible_grid().viewport.origin_row, 0);
    }

    fn visible_line_text(visible: &VisibleGrid, row: usize) -> String {
        let cols = usize::from(visible.viewport.size.cols);
        visible.cells[row * cols..(row + 1) * cols]
            .iter()
            .filter(|cell| !cell.wide_continuation)
            .map(|cell| cell.text.as_str())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn borrowed_visible_rows_report_only_the_printed_row_as_changed() {
        let mut terminal = TerminalState::new(TerminalSize::new(4, 2));
        let before = terminal
            .visible_rows()
            .map(|row| (row.absolute_row, row.generation, row.cells.len()))
            .collect::<Vec<_>>();

        terminal
            .apply_action(TerminalAction::Print('x'))
            .expect("print into first row");
        let after = terminal
            .visible_rows()
            .map(|row| (row.absolute_row, row.generation, row.cells.len()))
            .collect::<Vec<_>>();

        assert_eq!(before.len(), 2);
        assert_eq!(before[0].0, 0);
        assert_eq!(before[0].2, 4);
        assert_ne!(before[0].1, after[0].1);
        assert_eq!(before[1], after[1]);
    }

    #[test]
    fn scrolling_assigns_fresh_generations_to_every_shifted_visible_row() {
        let mut terminal = TerminalState::new(TerminalSize::new(2, 2));
        terminal
            .apply_action(TerminalAction::SetCursorPosition { row: 2, col: 1 })
            .expect("move to bottom row");
        let before = terminal
            .visible_rows()
            .map(|row| row.generation)
            .collect::<Vec<_>>();

        terminal
            .apply_action(TerminalAction::LineFeed)
            .expect("scroll at bottom row");
        let after = terminal
            .visible_rows()
            .map(|row| row.generation)
            .collect::<Vec<_>>();

        assert!(after.iter().all(|generation| *generation != 0));
        assert!(before.iter().zip(&after).all(|(old, new)| old != new));
    }

    #[test]
    fn editing_actions_advance_the_affected_row_generation() {
        let actions = [
            TerminalAction::ClearLine(ClearMode::All),
            TerminalAction::InsertChars(1),
            TerminalAction::DeleteChars(1),
            TerminalAction::EraseChars(1),
        ];

        for action in actions {
            let mut terminal = TerminalState::new(TerminalSize::new(4, 2));
            terminal.apply_printable_text("abc");
            terminal
                .apply_action(TerminalAction::SetCursorPosition { row: 1, col: 2 })
                .expect("position cursor");
            let before = terminal.visible_rows().next().unwrap().generation;

            terminal
                .apply_action(action.clone())
                .expect("apply editing action");

            assert_ne!(
                before,
                terminal.visible_rows().next().unwrap().generation,
                "{action:?} must dirty its row"
            );
        }
    }

    #[test]
    fn render_revision_advances_for_content_cursor_selection_and_viewport_changes() {
        let mut terminal = TerminalState::new(TerminalSize::new(8, 2));
        let initial = terminal.render_revision();

        terminal.apply_printable_text("x");
        let content = terminal.render_revision();
        assert!(content > initial);

        terminal
            .apply_action(TerminalAction::MoveCursor {
                direction: CursorDirection::Back,
                count: 1,
            })
            .expect("move cursor");
        let cursor = terminal.render_revision();
        assert!(cursor > content);

        terminal.set_selection(Selection {
            start: GridPosition::new(0, 0),
            end: GridPosition::new(0, 0),
            kind: SelectionKind::Normal,
        });
        let selection = terminal.render_revision();
        assert!(selection > cursor);

        terminal
            .apply_actions([TerminalAction::LineFeed, TerminalAction::LineFeed])
            .expect("create scrollback");
        let before_scroll = terminal.render_revision();
        assert!(terminal.scroll_viewport(1));
        assert!(terminal.render_revision() > before_scroll);
    }

    #[test]
    fn resizing_assigns_fresh_generations_to_the_visible_grid() {
        let mut terminal = TerminalState::new(TerminalSize::new(4, 2));
        let before = terminal
            .visible_rows()
            .map(|row| row.generation)
            .collect::<Vec<_>>();

        terminal
            .resize(TerminalSize::new(4, 3))
            .expect("resize terminal");
        let after = terminal
            .visible_rows()
            .map(|row| row.generation)
            .collect::<Vec<_>>();

        assert_eq!(after.len(), 3);
        assert!(after.iter().all(|generation| *generation != 0));
        assert!(before.iter().zip(&after).all(|(old, new)| old != new));
    }

    #[test]
    fn terminal_key_encoder_handles_text_controls_and_alt() {
        let modes = BTreeSet::new();

        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Character("a".to_owned()),
                TerminalKeyModifiers::default(),
                &modes,
            ),
            Some(b"a".to_vec())
        );
        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Character("c".to_owned()),
                TerminalKeyModifiers {
                    ctrl: true,
                    ..TerminalKeyModifiers::default()
                },
                &modes,
            ),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Character("x".to_owned()),
                TerminalKeyModifiers {
                    alt: true,
                    ..TerminalKeyModifiers::default()
                },
                &modes,
            ),
            Some(b"\x1bx".to_vec())
        );
    }

    #[test]
    fn terminal_key_encoder_honors_application_and_modifier_modes() {
        let mut modes = BTreeSet::new();
        modes.insert(TerminalMode::ApplicationCursorKeys);
        modes.insert(TerminalMode::ApplicationKeypad);

        assert_eq!(
            encode_terminal_key(&TerminalKey::Up, TerminalKeyModifiers::default(), &modes),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Left,
                TerminalKeyModifiers {
                    ctrl: true,
                    shift: true,
                    ..TerminalKeyModifiers::default()
                },
                &modes,
            ),
            Some(b"\x1b[1;6D".to_vec())
        );
        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Keypad(KeypadKey::Digit(1)),
                TerminalKeyModifiers::default(),
                &modes,
            ),
            Some(b"\x1bOq".to_vec())
        );
    }

    #[test]
    fn terminal_key_encoder_supports_function_and_editing_keys() {
        let modes = BTreeSet::new();

        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Function(5),
                TerminalKeyModifiers::default(),
                &modes,
            ),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Delete,
                TerminalKeyModifiers {
                    alt: true,
                    ..TerminalKeyModifiers::default()
                },
                &modes,
            ),
            Some(b"\x1b[3;3~".to_vec())
        );
        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Tab,
                TerminalKeyModifiers {
                    shift: true,
                    ..TerminalKeyModifiers::default()
                },
                &modes,
            ),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Backspace,
                TerminalKeyModifiers {
                    ctrl: true,
                    ..TerminalKeyModifiers::default()
                },
                &modes,
            ),
            Some(vec![0x08])
        );
        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Function(13),
                TerminalKeyModifiers::default(),
                &modes,
            ),
            Some(b"\x1b[25~".to_vec())
        );
        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Function(24),
                TerminalKeyModifiers::default(),
                &modes,
            ),
            Some(b"\x1b[45~".to_vec())
        );
    }

    #[test]
    fn kitty_keyboard_flags_encode_ambiguous_keys_and_event_types() {
        let modes = BTreeSet::new();
        let ctrl = TerminalKeyModifiers {
            ctrl: true,
            ..TerminalKeyModifiers::default()
        };

        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Enter,
                ctrl,
                &modes,
                1,
                TerminalKeyEventType::Press,
            ),
            Some(b"\x1b[13;5u".to_vec())
        );
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Tab,
                ctrl,
                &modes,
                1,
                TerminalKeyEventType::Press,
            ),
            Some(b"\x1b[9;5u".to_vec())
        );
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Enter,
                TerminalKeyModifiers {
                    shift: true,
                    ..TerminalKeyModifiers::default()
                },
                &modes,
                1,
                TerminalKeyEventType::Press,
            ),
            Some(b"\x1b[13;2u".to_vec())
        );
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Escape,
                TerminalKeyModifiers {
                    alt: true,
                    ..TerminalKeyModifiers::default()
                },
                &modes,
                1,
                TerminalKeyEventType::Press,
            ),
            Some(b"\x1b[27;3u".to_vec())
        );
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Backspace,
                ctrl,
                &modes,
                1,
                TerminalKeyEventType::Press,
            ),
            Some(b"\x1b[127;5u".to_vec())
        );
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Character("a".to_owned()),
                TerminalKeyModifiers::default(),
                &modes,
                3,
                TerminalKeyEventType::Release,
            ),
            None
        );
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Character("a".to_owned()),
                TerminalKeyModifiers::default(),
                &modes,
                11,
                TerminalKeyEventType::Release,
            ),
            Some(b"\x1b[97;1:3u".to_vec())
        );
        for (key, expected) in [
            (TerminalKey::Enter, b"\r".as_slice()),
            (TerminalKey::Tab, b"\t".as_slice()),
            (TerminalKey::Backspace, b"\x7f".as_slice()),
            (TerminalKey::Keypad(KeypadKey::Digit(1)), b"1".as_slice()),
            (TerminalKey::Keypad(KeypadKey::Enter), b"\r".as_slice()),
        ] {
            assert_eq!(
                encode_terminal_key_with_protocol(
                    &key,
                    TerminalKeyModifiers::default(),
                    &modes,
                    3,
                    TerminalKeyEventType::Press,
                ),
                Some(expected.to_vec())
            );
        }
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Function(24),
                TerminalKeyModifiers::default(),
                &modes,
                1,
                TerminalKeyEventType::Press,
            ),
            Some(b"\x1b[57387u".to_vec())
        );
    }

    #[test]
    fn win32_input_records_encode_every_field_in_decimal() {
        // Microsoft win32-input-mode: CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _
        let record = Win32InputRecord {
            virtual_key: 0x41,
            scan_code: 0x1e,
            unicode_char: u16::from(b'a'),
            key_down: true,
            control_key_state: 0,
            repeat_count: 1,
        };
        assert_eq!(record.encode(), b"\x1b[65;30;97;1;0;1_");

        let shifted_amp = Win32InputRecord {
            virtual_key: 0x37,
            scan_code: 0x08,
            unicode_char: u16::from(b'&'),
            key_down: true,
            control_key_state: WIN32_SHIFT_PRESSED,
            repeat_count: 1,
        };
        assert_eq!(shifted_amp.encode(), b"\x1b[55;8;38;1;16;1_");

        let release = Win32InputRecord {
            key_down: false,
            ..record
        };
        assert_eq!(release.encode(), b"\x1b[65;30;97;0;0;1_");

        let arrow = Win32InputRecord {
            virtual_key: 0x26,
            scan_code: 0x48,
            unicode_char: 0,
            key_down: true,
            control_key_state: WIN32_ENHANCED_KEY,
            repeat_count: 1,
        };
        assert_eq!(arrow.encode(), b"\x1b[38;72;0;1;256;1_");
    }

    #[test]
    fn win32_input_mode_takes_precedence_over_the_kitty_protocol() {
        // Captured from a real Windows-native multiplexer: it enables DECSET 9001
        // and pushes kitty flags 9 ("report all keys as escape codes"), yet it
        // cannot parse `CSI u`. Honouring the flags dropped every keystroke.
        let mut modes = BTreeSet::new();
        modes.insert(TerminalMode::Win32InputMode);
        let plain = TerminalKeyModifiers::default();
        let ctrl = TerminalKeyModifiers {
            ctrl: true,
            ..TerminalKeyModifiers::default()
        };

        for (key, modifiers, expected) in [
            (
                TerminalKey::Character("a".to_owned()),
                plain,
                b"a".as_slice(),
            ),
            (
                TerminalKey::Character("b".to_owned()),
                ctrl,
                b"\x02".as_slice(),
            ),
            (TerminalKey::Enter, plain, b"\r".as_slice()),
            (TerminalKey::Up, plain, b"\x1b[A".as_slice()),
        ] {
            assert_eq!(
                encode_terminal_key_with_protocol(
                    &key,
                    modifiers,
                    &modes,
                    9,
                    TerminalKeyEventType::Press,
                )
                .as_deref(),
                Some(expected),
                "{key:?} must use its legacy encoding while win32-input-mode is on"
            );
        }

        // Without 9001 the same flags do request `CSI u`, so the protocol itself
        // still works for applications that actually speak it.
        let no_modes = BTreeSet::new();
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Character("a".to_owned()),
                plain,
                &no_modes,
                9,
                TerminalKeyEventType::Press,
            )
            .as_deref(),
            Some(b"\x1b[97u".as_slice())
        );
    }

    #[test]
    fn disambiguate_flag_keeps_the_legacy_encodings_a_multiplexer_needs() {
        // A multiplexer enables flag 1 ("disambiguate escape codes"). Routing
        // every special and modified key through `CSI u` under that flag meant
        // tmux never received its Ctrl+B prefix and navigation keys did nothing.
        let modes = BTreeSet::new();
        let ctrl = TerminalKeyModifiers {
            ctrl: true,
            ..TerminalKeyModifiers::default()
        };
        let plain = TerminalKeyModifiers::default();

        for (key, modifiers, expected) in [
            // The prefix every multiplexer depends on.
            (
                TerminalKey::Character("b".to_owned()),
                ctrl,
                b"".as_slice(),
            ),
            (
                TerminalKey::Character("a".to_owned()),
                ctrl,
                b"".as_slice(),
            ),
            // Navigation must keep its legacy forms.
            (TerminalKey::Up, plain, b"[A".as_slice()),
            (TerminalKey::Down, plain, b"[B".as_slice()),
            (TerminalKey::Right, plain, b"[C".as_slice()),
            (TerminalKey::Left, plain, b"[D".as_slice()),
            (TerminalKey::Home, plain, b"[H".as_slice()),
            (TerminalKey::End, plain, b"[F".as_slice()),
            (TerminalKey::PageUp, plain, b"[5~".as_slice()),
            (TerminalKey::PageDown, plain, b"[6~".as_slice()),
            (TerminalKey::Delete, plain, b"[3~".as_slice()),
            // Plain text and the ordinary controls.
            (
                TerminalKey::Character("x".to_owned()),
                plain,
                b"x".as_slice(),
            ),
            (TerminalKey::Enter, plain, b"\r".as_slice()),
            (TerminalKey::Tab, plain, b"	".as_slice()),
        ] {
            assert_eq!(
                encode_terminal_key_with_protocol(
                    &key,
                    modifiers,
                    &modes,
                    1,
                    TerminalKeyEventType::Press,
                )
                .as_deref(),
                Some(expected),
                "{key:?} with {modifiers:?} must keep its legacy encoding under flag 1"
            );
        }

        // Flag 1 still resolves the ambiguity it exists for.
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Escape,
                plain,
                &modes,
                1,
                TerminalKeyEventType::Press,
            )
            .as_deref(),
            Some(b"[27u".as_slice())
        );

        // Flag 8 is the one that asks for everything as an escape code.
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Up,
                plain,
                &modes,
                0b1000,
                TerminalKeyEventType::Press,
            )
            .as_deref(),
            Some(b"[57352u".as_slice())
        );
        assert_eq!(
            encode_terminal_key_with_protocol(
                &TerminalKey::Character("b".to_owned()),
                ctrl,
                &modes,
                0b1000,
                TerminalKeyEventType::Press,
            )
            .as_deref(),
            Some(b"[98;5u".as_slice())
        );
    }

    #[test]
    fn kitty_keyboard_flag_stack_is_scoped_to_the_active_screen() {
        let mut terminal = TerminalState::new(TerminalSize::new(80, 24));
        terminal
            .apply_action(TerminalAction::SetKittyKeyboardFlags {
                flags: 1,
                mode: KittyKeyboardMode::Set,
            })
            .unwrap();
        terminal
            .apply_action(TerminalAction::PushKittyKeyboardFlags(3))
            .unwrap();
        assert_eq!(terminal.kitty_keyboard_flags(), 3);
        terminal
            .apply_action(TerminalAction::PopKittyKeyboardFlags(1))
            .unwrap();
        assert_eq!(terminal.kitty_keyboard_flags(), 1);

        terminal
            .apply_action(TerminalAction::SetMode {
                mode: TerminalMode::AlternateScreen,
                enabled: true,
            })
            .unwrap();
        assert_eq!(terminal.kitty_keyboard_flags(), 0);
        terminal
            .apply_action(TerminalAction::SetKittyKeyboardFlags {
                flags: 2,
                mode: KittyKeyboardMode::Set,
            })
            .unwrap();
        assert_eq!(terminal.kitty_keyboard_flags(), 2);
        terminal
            .apply_action(TerminalAction::SetMode {
                mode: TerminalMode::AlternateScreen,
                enabled: false,
            })
            .unwrap();
        assert_eq!(terminal.kitty_keyboard_flags(), 1);
    }

    #[test]
    fn alt_graph_preserves_printable_text() {
        let modes = BTreeSet::new();
        assert_eq!(
            encode_terminal_key(
                &TerminalKey::Character("@".to_owned()),
                TerminalKeyModifiers {
                    ctrl: true,
                    alt: true,
                    alt_graph: true,
                    ..TerminalKeyModifiers::default()
                },
                &modes,
            ),
            Some(b"@".to_vec())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        #[test]
        fn fuzz_grid_resize_selection_invariants(
            ops in prop::collection::vec(any::<(u8, u16, u16, u16, u16)>(), 0..256)
        ) {
            let mut terminal = TerminalState::new(TerminalSize::new(16, 6));
            for op in ops {
                apply_fuzz_tuple(&mut terminal, op);
                assert_terminal_invariants(&terminal);
            }
        }

        #[test]
        fn fuzz_unicode_grapheme_input_invariants(
            input in prop::collection::vec(any::<u8>(), 0..512)
        ) {
            let mut terminal = TerminalState::new(TerminalSize::new(18, 5));
            for byte in input {
                terminal
                    .apply_action(TerminalAction::Print(fuzz_char(byte)))
                    .unwrap();
                assert_terminal_invariants(&terminal);
            }
        }

        #[test]
        fn fuzz_resize_keeps_grid_and_scrollback_consistent(
            sizes in prop::collection::vec((1_u16..96, 1_u16..32), 0..128)
        ) {
            let mut terminal = TerminalState::new(TerminalSize::new(10, 4));
            terminal
                .apply_actions("a界e\u{301}👍🏽z\r\n".repeat(32).chars().map(TerminalAction::Print))
                .unwrap();

            for (cols, rows) in sizes {
                terminal.resize(TerminalSize::new(cols, rows)).unwrap();
                assert_terminal_invariants(&terminal);
            }
        }
    }
}
