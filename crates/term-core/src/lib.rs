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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalAction {
    Print(char),
    CarriageReturn,
    LineFeed,
    Backspace,
    Tab,
    MoveCursor {
        direction: CursorDirection,
        count: u16,
    },
    SetCursorPosition {
        row: u16,
        col: u16,
    },
    SetCursorColumn(u16),
    SaveCursor,
    RestoreCursor,
    ClearScreen(ClearMode),
    ClearLine(ClearMode),
    InsertLines(u16),
    DeleteLines(u16),
    InsertChars(u16),
    DeleteChars(u16),
    EraseChars(u16),
    SetGraphicRendition(Vec<GraphicRendition>),
    SetMode {
        mode: TerminalMode,
        enabled: bool,
    },
    SetCursorVisible(bool),
    SetCursorShape(CursorShape),
    SetTitle(String),
    SetTabStop,
    ClearTabStop,
    ClearAllTabStops,
    DeviceStatusReport(u16),
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
    saved_cursor: Option<SavedCursor>,
    tab_stops: BTreeSet<u16>,
    tab_stops_modified: bool,
    pending_output: Vec<u8>,
    title: Option<String>,
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
            saved_cursor: None,
            tab_stops: default_tab_stops(size.cols),
            tab_stops_modified: false,
            pending_output: Vec::new(),
            title: None,
        }
    }

    pub fn apply_action(&mut self, action: TerminalAction) -> TerminalResult<()> {
        match action {
            TerminalAction::Print(ch) => self.print(ch),
            TerminalAction::CarriageReturn => self.active_mut().carriage_return(),
            TerminalAction::LineFeed => self.line_feed(),
            TerminalAction::Backspace => self.active_mut().backspace(),
            TerminalAction::Tab => self.tab(),
            TerminalAction::MoveCursor { direction, count } => {
                self.active_mut().move_cursor(direction, count.max(1));
            }
            TerminalAction::SetCursorPosition { row, col } => {
                self.active_mut().set_cursor_position(row, col);
            }
            TerminalAction::SetCursorColumn(col) => self.active_mut().set_cursor_column(col),
            TerminalAction::SaveCursor => self.save_cursor(),
            TerminalAction::RestoreCursor => self.restore_cursor(),
            TerminalAction::ClearScreen(mode) => self.clear_screen(mode),
            TerminalAction::ClearLine(mode) => self.clear_line(mode),
            TerminalAction::InsertLines(count) => self.insert_lines(count),
            TerminalAction::DeleteLines(count) => self.delete_lines(count),
            TerminalAction::InsertChars(count) => self.insert_chars(count),
            TerminalAction::DeleteChars(count) => self.delete_chars(count),
            TerminalAction::EraseChars(count) => self.erase_chars(count),
            TerminalAction::SetGraphicRendition(renditions) => self.apply_sgr(&renditions),
            TerminalAction::SetMode { mode, enabled } => self.set_mode(mode, enabled),
            TerminalAction::SetCursorVisible(visible) => self.cursor_visible = visible,
            TerminalAction::SetCursorShape(shape) => self.cursor_shape = shape,
            TerminalAction::SetTitle(title) => self.title = Some(title),
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
            TerminalAction::SetScrollRegion { top, bottom } => {
                self.active_mut().set_scroll_region(top, bottom);
            }
            TerminalAction::ResetScrollRegion => self.active_mut().reset_scroll_region(),
            TerminalAction::Reset => self.reset(),
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
                self.primary.scrollback.len() as i64
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
        self.selection = None;
    }

    fn line_feed(&mut self) {
        let use_scrollback = self.active_is_primary();
        self.active_mut().line_feed(use_scrollback);
    }

    fn tab(&mut self) {
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
        }
    }

    fn extract_selection(&self, selection: Selection) -> String {
        let start_row = selection.start.row.min(selection.end.row).max(0) as usize;
        let end_row = selection.start.row.max(selection.end.row).max(0) as usize;
        let start_col = selection.start.col.min(selection.end.col);
        let end_col = selection.start.col.max(selection.end.col);
        let lines = &self.active().lines;
        let mut out = String::new();

        for row in start_row..=end_row {
            let Some(line) = lines.get(row) else {
                continue;
            };

            let line_end = line.cells.len().saturating_sub(1);
            let (from, to) = match selection.kind {
                SelectionKind::Rectangular => (
                    usize::from(start_col).min(line_end),
                    usize::from(end_col).min(line_end),
                ),
                SelectionKind::Normal if row == start_row && row == end_row => (
                    usize::from(start_col).min(line_end),
                    usize::from(end_col).min(line_end),
                ),
                SelectionKind::Normal if row == start_row => {
                    (usize::from(start_col).min(line_end), line_end)
                }
                SelectionKind::Normal if row == end_row => (0, usize::from(end_col).min(line_end)),
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

            let should_join_wrapped =
                selection.kind == SelectionKind::Normal && line.hard_wrapped && row < end_row;
            if row < end_row && !should_join_wrapped {
                out.push('\n');
            }
        }

        trim_selection_text(out)
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

        if let Some(alternate) = &mut self.alternate {
            alternate.resize_visible(size);
        }
        if !self.tab_stops_modified {
            self.tab_stops = default_tab_stops(size.cols);
        }

        Ok(())
    }

    fn visible_grid(&self) -> VisibleGrid {
        VisibleGrid {
            viewport: self.viewport(),
            cells: self
                .active()
                .lines
                .iter()
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
        if self.cursor_col >= cols {
            if autowrap {
                self.wrap_line(append_scrollback);
            } else {
                self.cursor_col = cols.saturating_sub(1);
            }
        }

        if width == 2 && self.cursor_col + 1 >= cols && autowrap {
            if let Some(line) = self.lines.get_mut(self.cursor_row) {
                line.hard_wrapped = true;
            }
            self.wrap_line(append_scrollback);
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
            if autowrap {
                if let Some(line) = self.lines.get_mut(self.cursor_row) {
                    line.hard_wrapped = true;
                }
                self.wrap_line(append_scrollback);
            }
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
        let (row, mut col) = if self.cursor_col > 0 {
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

    fn wrap_line(&mut self, append_scrollback: bool) {
        self.cursor_col = 0;
        self.line_feed(append_scrollback);
    }

    fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }

    fn line_feed(&mut self, append_scrollback: bool) {
        if self.cursor_row == self.scroll_bottom {
            self.scroll_up(append_scrollback);
        } else {
            self.cursor_row = (self.cursor_row + 1).min(usize::from(self.size.rows) - 1);
        }
    }

    fn backspace(&mut self) {
        let Some(line) = self.lines.get(self.cursor_row) else {
            return;
        };
        self.cursor_col = previous_grapheme_col(line, self.cursor_col);
    }

    fn move_cursor(&mut self, direction: CursorDirection, count: u16) {
        let count = usize::from(count);
        match direction {
            CursorDirection::Up => {
                self.cursor_row = self.cursor_row.saturating_sub(count).max(self.scroll_top);
            }
            CursorDirection::Down => {
                self.cursor_row = (self.cursor_row + count).min(self.scroll_bottom);
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
                self.cursor_row = (self.cursor_row + count).min(self.scroll_bottom);
                self.cursor_col = 0;
            }
            CursorDirection::PreviousLine => {
                self.cursor_row = self.cursor_row.saturating_sub(count).max(self.scroll_top);
                self.cursor_col = 0;
            }
        }
    }

    fn set_cursor_position(&mut self, row: u16, col: u16) {
        self.cursor_row = usize::from(row.saturating_sub(1)).min(usize::from(self.size.rows) - 1);
        self.cursor_col = usize::from(col.saturating_sub(1)).min(usize::from(self.size.cols) - 1);
        self.normalize_cursor_col();
    }

    fn set_cursor_column(&mut self, col: u16) {
        self.cursor_col = usize::from(col.saturating_sub(1)).min(usize::from(self.size.cols) - 1);
        self.normalize_cursor_col();
    }

    fn clear_screen(&mut self, mode: ClearMode, attributes: CellAttributes) {
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
        self.scroll_top = 0;
        self.scroll_bottom = usize::from(self.size.rows.saturating_sub(1));
    }

    fn scroll_up(&mut self, append_scrollback: bool) {
        let blank = Line::blank(self.size.cols);

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

    fn resize_visible(&mut self, size: TerminalSize) {
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
        let size = size.normalized();
        let logical = logical_lines(&self.scrollback, &self.lines);
        let mut reflowed = reflow_logical_lines(logical, size.cols);
        let rows = usize::from(size.rows);

        while reflowed.len() < rows {
            reflowed.push(Line::blank(size.cols));
        }

        let split = reflowed.len().saturating_sub(rows);
        self.scrollback = reflowed[..split].to_vec();
        self.lines = reflowed[split..].to_vec();
        self.size = size;
        self.reset_scroll_region();
        self.clamp_cursor();
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

    fn line_text(terminal: &TerminalState, row: u16) -> String {
        terminal.line(row).unwrap().raw_text()
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
}
