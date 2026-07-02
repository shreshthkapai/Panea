//! Platform-neutral terminal state.

pub const LAYER: &str = "core correctness";

use std::{cmp::Ordering, collections::BTreeSet, error::Error, fmt};

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
}

impl Cell {
    #[must_use]
    pub fn blank(attributes: CellAttributes) -> Self {
        Self {
            text: " ".to_owned(),
            attributes,
        }
    }

    #[must_use]
    pub fn text(text: impl Into<String>, attributes: CellAttributes) -> Self {
        Self {
            text: text.into(),
            attributes,
        }
    }

    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.text == " " && self.attributes == CellAttributes::default()
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
    ClearScreen(ClearMode),
    ClearLine(ClearMode),
    SetGraphicRendition(Vec<GraphicRendition>),
    SetMode {
        mode: TerminalMode,
        enabled: bool,
    },
    SetCursorVisible(bool),
    SetCursorShape(CursorShape),
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
            TerminalAction::ClearScreen(mode) => self.clear_screen(mode),
            TerminalAction::ClearLine(mode) => self.clear_line(mode),
            TerminalAction::SetGraphicRendition(renditions) => self.apply_sgr(&renditions),
            TerminalAction::SetMode { mode, enabled } => self.set_mode(mode, enabled),
            TerminalAction::SetCursorVisible(visible) => self.cursor_visible = visible,
            TerminalAction::SetCursorShape(shape) => self.cursor_shape = shape,
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
        let attributes = self.attributes;
        let use_scrollback = self.active_is_primary();
        self.active_mut()
            .print(ch, attributes, autowrap, use_scrollback);
        self.selection = None;
    }

    fn line_feed(&mut self) {
        let use_scrollback = self.active_is_primary();
        self.active_mut().line_feed(use_scrollback);
    }

    fn tab(&mut self) {
        let current = self.active().cursor_col;
        let next_tab = ((current / 8) + 1) * 8;
        let max_col = usize::from(self.active().size.cols.saturating_sub(1));
        self.active_mut().cursor_col = next_tab.min(max_col);
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
                GraphicRendition::NormalIntensity => self.attributes.bold = false,
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

            for cell in &line.cells[from..=to] {
                out.push_str(&cell.text);
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
        append_scrollback: bool,
    ) {
        let cols = usize::from(self.size.cols);
        if self.cursor_col >= cols {
            if autowrap {
                self.wrap_line(append_scrollback);
            } else {
                self.cursor_col = cols.saturating_sub(1);
            }
        }

        if let Some(cell) = self
            .lines
            .get_mut(self.cursor_row)
            .and_then(|line| line.cells.get_mut(self.cursor_col))
        {
            *cell = Cell::text(ch.to_string(), attributes);
        }

        if self.cursor_col + 1 >= cols {
            if autowrap {
                if let Some(line) = self.lines.get_mut(self.cursor_row) {
                    line.hard_wrapped = true;
                }
                self.wrap_line(append_scrollback);
            }
        } else {
            self.cursor_col += 1;
        }
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
        self.cursor_col = self.cursor_col.saturating_sub(1);
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
                self.cursor_col =
                    (self.cursor_col + count).min(usize::from(self.size.cols.saturating_sub(1)));
            }
            CursorDirection::Back => self.cursor_col = self.cursor_col.saturating_sub(count),
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
    }

    fn set_cursor_column(&mut self, col: u16) {
        self.cursor_col = usize::from(col.saturating_sub(1)).min(usize::from(self.size.cols) - 1);
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

        for index in range {
            line.cells[index] = Cell::blank(attributes);
        }
        line.hard_wrapped = false;
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
        if cells.is_empty() {
            out.push(Line::blank(cols as u16));
            continue;
        }

        let mut index = 0;
        while index < cells.len() {
            let end = (index + cols).min(cells.len());
            let mut line = Line {
                cells: cells[index..end].to_vec(),
                hard_wrapped: end < cells.len(),
            };
            line.resize_to(cols as u16, CellAttributes::default());
            out.push(line);
            index = end;
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

    #[test]
    fn term_core_has_no_crate_dependencies() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest.contains("[dependencies]"),
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
}
