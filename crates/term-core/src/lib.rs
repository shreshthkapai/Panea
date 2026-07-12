//! Platform-neutral terminal state.

pub const LAYER: &str = "core correctness";

use std::{cmp::Ordering, collections::BTreeSet, error::Error, fmt};

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

/// Encodes a platform-neutral key as the byte sequence expected by terminal applications.
#[must_use]
pub fn encode_terminal_key(
    key: &TerminalKey,
    modifiers: TerminalKeyModifiers,
    modes: &BTreeSet<TerminalMode>,
) -> Option<Vec<u8>> {
    if modifiers.super_key {
        return None;
    }

    let effective_ctrl = modifiers.ctrl && !modifiers.alt_graph;
    let effective_alt = modifiers.alt && !modifiers.alt_graph;
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
    pub inverse: bool,
    pub strikethrough: bool,
}

impl CellAttributes {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub attributes: CellAttributes,
    pub width: u8,
    pub wide_continuation: bool,
}

impl Cell {
    #[must_use]
    pub fn blank(attributes: CellAttributes) -> Self {
        Self {
            text: " ".to_owned(),
            attributes,
            width: 1,
            wide_continuation: false,
        }
    }

    #[must_use]
    pub fn text(text: impl Into<String>, attributes: CellAttributes) -> Self {
        let text = text.into();
        let width = cell_width_for_text(&text) as u8;
        Self {
            text,
            attributes,
            width,
            wide_continuation: false,
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
            text: " ".to_owned(),
            attributes,
            width: 0,
            wide_continuation: true,
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
    FocusEvents,
    Origin,
    Insert,
    AutoWrap,
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
}

pub type SelectionRange = Selection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub cells: Vec<Cell>,
    pub hard_wrapped: bool,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scrollback {
    pub lines: Vec<Line>,
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
    NoUnderline,
    Inverse,
    NoInverse,
    Strikethrough,
    NoStrikethrough,
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
    SetGraphicRendition(Vec<GraphicRendition>),
    SetMode {
        mode: TerminalMode,
        enabled: bool,
    },
    SetCursorVisible(bool),
    SetCursorShape(CursorShape),
    SetTitle(String),
    Osc52Clipboard(Osc52ClipboardRequest),
    SetTabStop,
    ClearTabStop,
    ClearAllTabStops,
    DeviceStatusReport(u16),
    PrivateDeviceStatusReport(u16),
    PrimaryDeviceAttributes,
    SetScrollRegion {
        top: u16,
        bottom: u16,
    },
    ResetScrollRegion,
    Reset,
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
    saved_cursor: Option<SavedCursor>,
    tab_stops: BTreeSet<u16>,
    tab_stops_modified: bool,
    pending_output: Vec<u8>,
    pending_clipboard_requests: Vec<Osc52ClipboardRequest>,
    title: Option<String>,
    last_printed: Option<char>,
}

impl TerminalState {
    #[must_use]
    pub fn new(size: TerminalSize) -> Self {
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
            saved_cursor: None,
            tab_stops: default_tab_stops(size.cols),
            tab_stops_modified: false,
            pending_output: Vec::new(),
            pending_clipboard_requests: Vec::new(),
            title: None,
            last_printed: None,
        }
    }

    pub fn apply_action(&mut self, action: TerminalAction) -> TerminalResult<()> {
        let scrollback_before = self.primary.scrollback.len();
        match action {
            TerminalAction::Print(ch) => self.print(ch),
            TerminalAction::CarriageReturn => self.active_mut().carriage_return(),
            TerminalAction::LineFeed => self.line_feed(),
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
            TerminalAction::SetGraphicRendition(renditions) => self.apply_sgr(&renditions),
            TerminalAction::SetMode { mode, enabled } => self.set_mode(mode, enabled),
            TerminalAction::SetCursorVisible(visible) => self.cursor_visible = visible,
            TerminalAction::SetCursorShape(shape) => self.cursor_shape = shape,
            TerminalAction::SetTitle(title) => self.title = Some(title),
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
                self.pending_output.extend_from_slice(b"\x1b[?1;2c");
            }
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
        }

        if self.viewport_offset > 0 {
            self.viewport_offset = self
                .viewport_offset
                .saturating_add(
                    self.primary
                        .scrollback
                        .len()
                        .saturating_sub(scrollback_before),
                )
                .min(self.primary.scrollback.len());
        }

        Ok(())
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

    #[must_use]
    pub fn line(&self, row: u16) -> Option<&Line> {
        self.active().lines.get(usize::from(row))
    }

    #[must_use]
    pub fn visible_line(&self, row: u16) -> Option<&Line> {
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
        Some(self.extract_selection(selection))
    }

    pub fn set_selection(&mut self, selection: Selection) {
        self.selection = Some(selection);
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
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
                .min(self.primary.scrollback.len());
        } else {
            self.viewport_offset = self
                .viewport_offset
                .saturating_sub(lines.unsigned_abs() as usize);
        }
        previous != self.viewport_offset
    }

    pub fn scroll_to_bottom(&mut self) -> bool {
        let changed = self.viewport_offset != 0;
        self.viewport_offset = 0;
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
                row: self.primary.scrollback.len() as i64 + cursor.row,
                col: cursor.col,
            }
        } else {
            cursor
        }
    }

    #[must_use]
    pub fn buffer_line_count(&self) -> usize {
        if self.active_is_primary() {
            self.primary.scrollback.len() + self.primary.lines.len()
        } else {
            self.active().lines.len()
        }
    }

    pub fn reveal_position(&mut self, position: GridPosition) -> bool {
        if !self.active_is_primary() {
            return false;
        }
        let rows = i64::from(self.primary.size.rows);
        let max_origin = self.primary.scrollback.len() as i64;
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

        let mut searchable = Vec::<Option<SearchCell<'_>>>::new();
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
                        text: &cell.text,
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
            let Some(first) = searchable[start] else {
                continue;
            };
            let mut matched = true;
            let mut end = first;
            for (offset, expected) in query.iter().enumerate() {
                let Some(Some(candidate)) = searchable.get(start + offset) else {
                    matched = false;
                    break;
                };
                if search_key(candidate.text, case_sensitive) != *expected {
                    matched = false;
                    break;
                }
                end = *candidate;
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

    fn reset(&mut self) {
        let size = self.active().size;
        *self = Self::new(size);
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

    fn viewport(&self) -> Viewport {
        Viewport {
            origin_row: if self.active_is_primary() {
                self.primary
                    .scrollback
                    .len()
                    .saturating_sub(self.viewport_offset) as i64
            } else {
                0
            },
            size: self.active().size,
        }
    }

    fn print(&mut self, ch: char) {
        if ch == '\u{fffd}' || ch.is_control() {
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
            self.primary.scrollback.clear();
            return;
        }

        self.active_mut().clear_screen(mode, attributes);
    }

    fn clear_line(&mut self, mode: ClearMode) {
        let attributes = self.attributes;
        self.active_mut().clear_line(mode, attributes);
    }

    fn apply_sgr(&mut self, renditions: &[GraphicRendition]) {
        if renditions.is_empty() {
            self.attributes.reset();
            return;
        }

        for rendition in renditions {
            match *rendition {
                GraphicRendition::Reset => self.attributes.reset(),
                GraphicRendition::Bold => self.attributes.bold = true,
                GraphicRendition::Dim => self.attributes.dim = true,
                GraphicRendition::NormalIntensity => {
                    self.attributes.bold = false;
                    self.attributes.dim = false;
                }
                GraphicRendition::Italic => self.attributes.italic = true,
                GraphicRendition::NoItalic => self.attributes.italic = false,
                GraphicRendition::Underline => self.attributes.underline = true,
                GraphicRendition::NoUnderline => self.attributes.underline = false,
                GraphicRendition::Inverse => self.attributes.inverse = true,
                GraphicRendition::NoInverse => self.attributes.inverse = false,
                GraphicRendition::Strikethrough => self.attributes.strikethrough = true,
                GraphicRendition::NoStrikethrough => self.attributes.strikethrough = false,
                GraphicRendition::Foreground(color) => self.attributes.foreground = Some(color),
                GraphicRendition::Background(color) => self.attributes.background = Some(color),
                GraphicRendition::DefaultForeground => self.attributes.foreground = None,
                GraphicRendition::DefaultBackground => self.attributes.background = None,
            }
        }
    }

    fn save_cursor(&mut self) {
        self.saved_cursor = Some(SavedCursor {
            row: self.active().cursor_row,
            col: self.active().cursor_col,
            attributes: self.attributes,
            shape: self.cursor_shape,
            visible: self.cursor_visible,
        });
    }

    fn restore_cursor(&mut self) {
        let Some(saved) = self.saved_cursor else {
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
                let cursor = self.active().cursor_position();
                self.pending_output.extend_from_slice(
                    format!("\x1b[{};{}R", cursor.row + 1, cursor.col + 1).as_bytes(),
                );
            }
            _ => {}
        }
    }

    fn private_device_status_report(&mut self, report: u16) {
        if report == 6 {
            let cursor = self.active().cursor_position();
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
            let Some((from, to)) = expand_range_to_graphemes(line, from, to) else {
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

    fn buffer_line(&self, absolute_row: usize) -> Option<&Line> {
        if !self.active_is_primary() {
            return self.active().lines.get(absolute_row);
        }
        self.primary.scrollback.get(absolute_row).or_else(|| {
            self.primary
                .lines
                .get(absolute_row - self.primary.scrollback.len())
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct SearchCell<'a> {
    text: &'a str,
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
        let size = size.normalized();
        self.primary.resize_reflow(size);
        self.viewport_offset = self.viewport_offset.min(self.primary.scrollback.len());

        if let Some(alternate) = &mut self.alternate {
            alternate.resize_visible(size);
        }
        if !self.tab_stops_modified {
            self.tab_stops = default_tab_stops(size.cols);
        }

        Ok(())
    }

    fn visible_grid(&self) -> VisibleGrid {
        let viewport = self.viewport();
        let start = usize::try_from(viewport.origin_row).unwrap_or(0);
        let rows = usize::from(viewport.size.rows);
        VisibleGrid {
            viewport,
            cells: (start..start.saturating_add(rows))
                .filter_map(|row| self.buffer_line(row))
                .flat_map(|line| line.cells.iter().cloned())
                .collect(),
        }
    }

    fn scrollback(&self) -> Scrollback {
        Scrollback {
            lines: self.primary.scrollback.clone(),
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
    scrollback: Vec<Line>,
    cursor_row: usize,
    cursor_col: usize,
    scroll_top: usize,
    scroll_bottom: usize,
    wrap_pending: bool,
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

        Self {
            size,
            lines: vec![Line::blank(size.cols); rows],
            scrollback: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            wrap_pending: false,
        }
    }

    fn cursor_position(&self) -> GridPosition {
        GridPosition {
            row: self.cursor_row as i64,
            col: self.cursor_col as u16,
        }
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
            *cell = Cell::text(ch.to_string(), attributes);
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
        sanitize_cells(&mut line.cells, CellAttributes::default());
        true
    }

    fn previous_grapheme_position(&self) -> Option<(usize, usize)> {
        let (row, mut col) = if self.wrap_pending {
            (self.cursor_row, self.cursor_col)
        } else if self.cursor_col > 0 {
            (self.cursor_row, self.cursor_col - 1)
        } else if self.cursor_row > 0 {
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
        let top = if origin { self.scroll_top } else { 0 };
        let bottom = if origin {
            self.scroll_bottom
        } else {
            usize::from(self.size.rows) - 1
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
            }
            ClearMode::ToCursor => {
                for row in 0..self.cursor_row {
                    self.lines[row] = Line::blank_with_attributes(self.size.cols, attributes);
                }
                self.clear_line(ClearMode::ToCursor, attributes);
            }
            ClearMode::All | ClearMode::Saved => {
                for line in &mut self.lines {
                    *line = Line::blank_with_attributes(self.size.cols, attributes);
                }
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
    }

    fn insert_chars(&mut self, count: u16, attributes: CellAttributes) {
        self.wrap_pending = false;
        self.normalize_cursor_col();
        let Some(line) = self.lines.get_mut(self.cursor_row) else {
            return;
        };
        let count = usize::from(count).min(line.cells.len().saturating_sub(self.cursor_col));
        blank_range_expanding_graphemes(line, self.cursor_col, self.cursor_col, attributes);
        for _ in 0..count {
            line.cells.insert(self.cursor_col, Cell::blank(attributes));
            line.cells.pop();
        }
        sanitize_cells(&mut line.cells, attributes);
        line.hard_wrapped = false;
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
        sanitize_cells(&mut line.cells, attributes);
        line.hard_wrapped = false;
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
                self.scrollback.push(removed);
            }
            self.lines.push(blank);
        } else {
            self.lines.remove(self.scroll_top);
            self.lines.insert(self.scroll_bottom, blank);
        }
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
    }

    fn resize_reflow(&mut self, size: TerminalSize) {
        let old_wrap_pending = self.wrap_pending;
        let size = size.normalized();
        let logical = logical_lines(&self.scrollback, &self.lines);
        let target_physical = self.scrollback.len().saturating_add(self.cursor_row);
        let (target_logical, target_offset) = logical_cursor_position(
            &self.scrollback,
            &self.lines,
            target_physical,
            self.cursor_col,
        );
        let mut reflowed = Vec::new();
        let mut cursor_physical = 0usize;
        let mut cursor_col = 0usize;
        let mut cursor_wrap_pending = false;
        for (logical_index, cells) in logical.into_iter().enumerate() {
            if logical_index == target_logical {
                let mapped = reflow_cursor_position(&cells, target_offset, size.cols);
                cursor_physical = reflowed.len().saturating_add(mapped.0);
                cursor_col = mapped.1;
                cursor_wrap_pending = old_wrap_pending || mapped.2;
            }
            reflowed.extend(reflow_logical_lines(vec![cells], size.cols));
        }
        let rows = usize::from(size.rows);

        while reflowed.len() < rows {
            reflowed.push(Line::blank(size.cols));
        }

        let split = reflowed.len().saturating_sub(rows);
        self.scrollback = reflowed[..split].to_vec();
        self.lines = reflowed[split..].to_vec();
        self.size = size;
        self.reset_scroll_region();
        self.cursor_row = cursor_physical
            .saturating_sub(split)
            .min(rows.saturating_sub(1));
        self.cursor_col = cursor_col.min(usize::from(size.cols) - 1);
        self.wrap_pending = cursor_wrap_pending;
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

fn logical_lines(scrollback: &[Line], visible: &[Line]) -> Vec<Vec<Cell>> {
    let mut out: Vec<Vec<Cell>> = Vec::new();
    let lines: Vec<&Line> = scrollback.iter().chain(visible.iter()).collect();

    for (index, line) in lines.iter().enumerate() {
        let content = line_content(line);
        if index > 0 && lines[index - 1].hard_wrapped {
            let last = out
                .last_mut()
                .expect("a previous physical line created a logical line");
            last.extend(content);
            continue;
        }

        out.push(content);
    }

    if out.is_empty() {
        out.push(Vec::new());
    }

    out
}

fn logical_cursor_position(
    scrollback: &[Line],
    visible: &[Line],
    target_physical: usize,
    cursor_col: usize,
) -> (usize, usize) {
    let lines = scrollback.iter().chain(visible.iter()).collect::<Vec<_>>();
    let mut logical_index = 0usize;
    let mut logical_offset = 0usize;
    for (index, line) in lines.iter().enumerate() {
        if index > 0 && !lines[index - 1].hard_wrapped {
            logical_index = logical_index.saturating_add(1);
            logical_offset = 0;
        }
        if index == target_physical {
            return (logical_index, logical_offset.saturating_add(cursor_col));
        }
        logical_offset = logical_offset.saturating_add(line_content(line).len());
    }
    (logical_index, logical_offset)
}

fn reflow_cursor_position(cells: &[Cell], offset: usize, cols: u16) -> (usize, usize, bool) {
    let cols = usize::from(cols.max(1));
    let mut source_col = 0usize;
    let mut row = 0usize;
    let mut col = 0usize;

    for cell in cells.iter().filter(|cell| !cell.wide_continuation) {
        let source_width = cell.width.max(1) as usize;
        let width = cell_width_for_text_in_grid(&cell.text, cols);
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

fn line_content(line: &Line) -> Vec<Cell> {
    let end = if line.hard_wrapped {
        line.cells.len()
    } else {
        line.cells
            .iter()
            .rposition(|cell| cell.text != " ")
            .map_or(0, |index| index + 1)
    };

    line.cells.iter().take(end).cloned().collect()
}

fn reflow_logical_lines(logical: Vec<Vec<Cell>>, cols: u16) -> Vec<Line> {
    let cols = usize::from(cols.max(1));
    let mut out = Vec::new();

    for cells in logical {
        let mut line = Line {
            cells: Vec::with_capacity(cols),
            hard_wrapped: false,
        };
        let mut emitted_any = false;

        for cell in cells.into_iter().filter(|cell| !cell.wide_continuation) {
            let width = cell_width_for_text_in_grid(&cell.text, cols);
            if !line.cells.is_empty() && line.cells.len() + width > cols {
                line.hard_wrapped = true;
                line.resize_to(cols as u16, CellAttributes::default());
                out.push(line);
                line = Line {
                    cells: Vec::with_capacity(cols),
                    hard_wrapped: false,
                };
            }

            push_cell_with_continuation(&mut line.cells, cell, cols);
            emitted_any = true;
        }

        if emitted_any {
            line.resize_to(cols as u16, CellAttributes::default());
            out.push(line);
        } else {
            out.push(Line::blank(cols as u16));
        }
    }

    out
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
    let width = cell_width_for_text(text);
    if available_cols < width { 1 } else { width }
}

fn scalar_cell_width(ch: char, cols: usize) -> usize {
    let width = UnicodeWidthChar::width(ch).unwrap_or(1).clamp(1, 2);
    if cols < width { 1 } else { width }
}

fn extends_previous_grapheme(previous_text: &str, ch: char) -> bool {
    if ch.is_ascii() {
        return false;
    }

    let mut text = String::with_capacity(previous_text.len() + ch.len_utf8());
    text.push_str(previous_text);
    text.push(ch);

    text.graphemes(true).count() == 1
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

fn push_cell_with_continuation(cells: &mut Vec<Cell>, mut cell: Cell, cols: usize) {
    let available = cols.saturating_sub(cells.len());
    let width = cell_width_for_text_in_grid(&cell.text, available);
    cell.width = width as u8;
    cell.wide_continuation = false;
    let attributes = cell.attributes;
    cells.push(cell);
    if width == 2 && cells.len() < cols {
        cells.push(Cell::wide_continuation(attributes));
    }
}

fn sanitize_cells(cells: &mut [Cell], attributes: CellAttributes) {
    let mut index = 0;
    while index < cells.len() {
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
            20 => TerminalAction::SetGraphicRendition(vec![match d % 4 {
                0 => GraphicRendition::Reset,
                1 => GraphicRendition::Bold,
                2 => GraphicRendition::Foreground(Color::Indexed((a % 256) as u8)),
                _ => GraphicRendition::Background(Color::Rgb {
                    red: a as u8,
                    green: b as u8,
                    blue: c as u8,
                }),
            }]),
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
            scrollback.lines.first().map(Line::raw_text).as_deref(),
            Some("界")
        );
        assert_eq!(line_text(&terminal, 0), "x");
        assert_eq!(line_text(&terminal, 1), "y");
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
