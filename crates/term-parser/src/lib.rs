//! ANSI/VT parsing boundary.

pub const LAYER: &str = "core correctness";

use term_core::{
    ClearMode, Color, CursorDirection, CursorShape, CursorState, GraphicRendition, Scrollback,
    SelectionRange, TerminalAction, TerminalCore, TerminalMode, TerminalResult, TerminalSize,
    TerminalState, VisibleGrid,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalEmulator {
    parser: Parser,
    state: TerminalState,
}

impl TerminalEmulator {
    #[must_use]
    pub fn new(size: TerminalSize) -> Self {
        Self {
            parser: Parser::default(),
            state: TerminalState::new(size),
        }
    }

    #[must_use]
    pub const fn state(&self) -> &TerminalState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut TerminalState {
        &mut self.state
    }

    #[must_use]
    pub fn into_state(self) -> TerminalState {
        self.state
    }
}

impl TerminalCore for TerminalEmulator {
    fn apply_bytes(&mut self, bytes: &[u8]) -> TerminalResult<()> {
        let actions = self.parser.parse(bytes);
        self.state.apply_actions(actions)
    }

    fn resize(&mut self, size: TerminalSize) -> TerminalResult<()> {
        <TerminalState as TerminalCore>::resize(&mut self.state, size)
    }

    fn visible_grid(&self) -> VisibleGrid {
        self.state.visible_grid()
    }

    fn scrollback(&self) -> Scrollback {
        self.state.scrollback()
    }

    fn cursor_state(&self) -> CursorState {
        self.state.cursor_state()
    }

    fn modes(&self) -> std::collections::BTreeSet<TerminalMode> {
        self.state.modes()
    }

    fn selection_state(&self) -> Option<SelectionRange> {
        self.state.selection_state()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parser {
    state: ParserState,
    print_buffer: Vec<u8>,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            state: ParserState::Ground,
            print_buffer: Vec::new(),
        }
    }
}

impl Parser {
    #[must_use]
    pub fn parse(&mut self, bytes: &[u8]) -> Vec<TerminalAction> {
        let mut actions = Vec::new();

        for byte in bytes {
            match &mut self.state {
                ParserState::Ground => match *byte {
                    0x1b => {
                        self.flush_print_buffer(&mut actions);
                        self.state = ParserState::Escape;
                    }
                    b'\r' => {
                        self.flush_print_buffer(&mut actions);
                        actions.push(TerminalAction::CarriageReturn);
                    }
                    b'\n' => {
                        self.flush_print_buffer(&mut actions);
                        actions.push(TerminalAction::LineFeed);
                    }
                    0x08 => {
                        self.flush_print_buffer(&mut actions);
                        actions.push(TerminalAction::Backspace);
                    }
                    b'\t' => {
                        self.flush_print_buffer(&mut actions);
                        actions.push(TerminalAction::Tab);
                    }
                    0x00..=0x1f | 0x7f => {}
                    _ => self.print_buffer.push(*byte),
                },
                ParserState::Escape => match *byte {
                    b'[' => self.state = ParserState::Csi(CsiState::default()),
                    b']' => self.state = ParserState::Osc { escape_seen: false },
                    b'c' => {
                        actions.push(TerminalAction::Reset);
                        self.state = ParserState::Ground;
                    }
                    _ => self.state = ParserState::Ground,
                },
                ParserState::Csi(csi) => {
                    if csi.consume(*byte) {
                        let action_set = dispatch_csi(csi);
                        actions.extend(action_set);
                        self.state = ParserState::Ground;
                    }
                }
                ParserState::Osc { escape_seen } => match (*byte, *escape_seen) {
                    (0x07, _) => self.state = ParserState::Ground,
                    (b'\\', true) => self.state = ParserState::Ground,
                    (0x1b, _) => *escape_seen = true,
                    (_, _) => *escape_seen = false,
                },
            }
        }

        if matches!(self.state, ParserState::Ground) {
            self.flush_print_buffer(&mut actions);
        }

        actions
    }

    fn flush_print_buffer(&mut self, actions: &mut Vec<TerminalAction>) {
        if self.print_buffer.is_empty() {
            return;
        }

        let text = String::from_utf8_lossy(&self.print_buffer);
        actions.extend(text.chars().map(TerminalAction::Print));
        self.print_buffer.clear();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParserState {
    Ground,
    Escape,
    Csi(CsiState),
    Osc { escape_seen: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct CsiState {
    private: bool,
    params: String,
    intermediate_space: bool,
    final_byte: Option<u8>,
}

impl CsiState {
    fn consume(&mut self, byte: u8) -> bool {
        match byte {
            b'?' if self.params.is_empty() => self.private = true,
            b'0'..=b'9' | b';' | b':' => self.params.push(char::from(byte)),
            b' ' => self.intermediate_space = true,
            0x40..=0x7e => {
                self.final_byte = Some(byte);
                return true;
            }
            _ => return true,
        }

        false
    }

    fn params(&self) -> Vec<u16> {
        self.params
            .replace(':', ";")
            .split(';')
            .map(|part| {
                if part.is_empty() {
                    0
                } else {
                    part.parse::<u16>().unwrap_or(0)
                }
            })
            .collect()
    }

    fn param_or(&self, index: usize, default: u16) -> u16 {
        self.params()
            .get(index)
            .copied()
            .filter(|value| *value != 0)
            .unwrap_or(default)
    }
}

fn dispatch_csi(csi: &CsiState) -> Vec<TerminalAction> {
    let Some(final_byte) = csi.final_byte else {
        return Vec::new();
    };

    match final_byte {
        b'A' => vec![move_cursor(csi, CursorDirection::Up)],
        b'B' => vec![move_cursor(csi, CursorDirection::Down)],
        b'C' => vec![move_cursor(csi, CursorDirection::Forward)],
        b'D' => vec![move_cursor(csi, CursorDirection::Back)],
        b'E' => vec![move_cursor(csi, CursorDirection::NextLine)],
        b'F' => vec![move_cursor(csi, CursorDirection::PreviousLine)],
        b'G' => vec![TerminalAction::SetCursorColumn(csi.param_or(0, 1))],
        b'H' | b'f' => vec![TerminalAction::SetCursorPosition {
            row: csi.param_or(0, 1),
            col: csi.param_or(1, 1),
        }],
        b'J' => vec![TerminalAction::ClearScreen(clear_mode(csi.param_or(0, 0)))],
        b'K' => vec![TerminalAction::ClearLine(clear_mode(csi.param_or(0, 0)))],
        b'm' => vec![TerminalAction::SetGraphicRendition(parse_sgr(
            &csi.params(),
        ))],
        b'h' | b'l' => mode_actions(csi, final_byte == b'h'),
        b'r' => scroll_region_action(csi),
        b'q' if csi.intermediate_space => cursor_shape_action(csi),
        _ => Vec::new(),
    }
}

fn move_cursor(csi: &CsiState, direction: CursorDirection) -> TerminalAction {
    TerminalAction::MoveCursor {
        direction,
        count: csi.param_or(0, 1),
    }
}

fn clear_mode(param: u16) -> ClearMode {
    match param {
        1 => ClearMode::ToCursor,
        2 => ClearMode::All,
        3 => ClearMode::Saved,
        _ => ClearMode::FromCursor,
    }
}

fn parse_sgr(params: &[u16]) -> Vec<GraphicRendition> {
    if params.is_empty() {
        return vec![GraphicRendition::Reset];
    }

    let mut out = Vec::new();
    let mut index = 0;

    while index < params.len() {
        match params[index] {
            0 => out.push(GraphicRendition::Reset),
            1 => out.push(GraphicRendition::Bold),
            3 => out.push(GraphicRendition::Italic),
            4 => out.push(GraphicRendition::Underline),
            7 => out.push(GraphicRendition::Inverse),
            9 => out.push(GraphicRendition::Strikethrough),
            22 => out.push(GraphicRendition::NormalIntensity),
            23 => out.push(GraphicRendition::NoItalic),
            24 => out.push(GraphicRendition::NoUnderline),
            27 => out.push(GraphicRendition::NoInverse),
            29 => out.push(GraphicRendition::NoStrikethrough),
            30..=37 => out.push(GraphicRendition::Foreground(Color::Indexed(
                (params[index] - 30) as u8,
            ))),
            40..=47 => out.push(GraphicRendition::Background(Color::Indexed(
                (params[index] - 40) as u8,
            ))),
            90..=97 => out.push(GraphicRendition::Foreground(Color::Indexed(
                (params[index] - 90 + 8) as u8,
            ))),
            100..=107 => out.push(GraphicRendition::Background(Color::Indexed(
                (params[index] - 100 + 8) as u8,
            ))),
            38 | 48 => {
                if let Some((rendition, consumed)) = parse_extended_color(params, index) {
                    out.push(rendition);
                    index += consumed;
                }
            }
            39 => out.push(GraphicRendition::DefaultForeground),
            49 => out.push(GraphicRendition::DefaultBackground),
            _ => {}
        }

        index += 1;
    }

    out
}

fn parse_extended_color(params: &[u16], index: usize) -> Option<(GraphicRendition, usize)> {
    let target_is_foreground = params[index] == 38;

    match params.get(index + 1).copied()? {
        5 => {
            let color = Color::Indexed(params.get(index + 2).copied()? as u8);
            Some((color_rendition(target_is_foreground, color), 2))
        }
        2 => {
            let color = Color::Rgb {
                red: params.get(index + 2).copied()? as u8,
                green: params.get(index + 3).copied()? as u8,
                blue: params.get(index + 4).copied()? as u8,
            };
            Some((color_rendition(target_is_foreground, color), 4))
        }
        _ => None,
    }
}

fn color_rendition(foreground: bool, color: Color) -> GraphicRendition {
    if foreground {
        GraphicRendition::Foreground(color)
    } else {
        GraphicRendition::Background(color)
    }
}

fn mode_actions(csi: &CsiState, enabled: bool) -> Vec<TerminalAction> {
    if !csi.private {
        return Vec::new();
    }

    csi.params()
        .into_iter()
        .filter_map(|mode| {
            let mode = match mode {
                1 => TerminalMode::ApplicationCursorKeys,
                25 => return Some(TerminalAction::SetCursorVisible(enabled)),
                66 => TerminalMode::ApplicationKeypad,
                1000 => TerminalMode::MouseReporting,
                1002 => TerminalMode::MouseCellMotion,
                1003 => TerminalMode::MouseAllMotion,
                1004 => TerminalMode::FocusEvents,
                1047 | 1049 => TerminalMode::AlternateScreen,
                2004 => TerminalMode::BracketedPaste,
                _ => return None,
            };

            Some(TerminalAction::SetMode { mode, enabled })
        })
        .collect()
}

fn scroll_region_action(csi: &CsiState) -> Vec<TerminalAction> {
    let params = csi.params();
    if params.is_empty() {
        return vec![TerminalAction::ResetScrollRegion];
    }

    vec![TerminalAction::SetScrollRegion {
        top: csi.param_or(0, 1),
        bottom: csi.param_or(1, u16::MAX),
    }]
}

fn cursor_shape_action(csi: &CsiState) -> Vec<TerminalAction> {
    let shape = match csi.param_or(0, 1) {
        3 | 4 => CursorShape::Underline,
        5 | 6 => CursorShape::Beam,
        _ => CursorShape::Block,
    };

    vec![TerminalAction::SetCursorShape(shape)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_core::{CellAttributes, GridPosition, Selection};

    fn terminal(size: TerminalSize, input: &[u8]) -> TerminalEmulator {
        let mut terminal = TerminalEmulator::new(size);
        terminal.apply_bytes(input).unwrap();
        terminal
    }

    fn line_text(terminal: &TerminalEmulator, row: u16) -> String {
        terminal.state().line(row).unwrap().raw_text()
    }

    #[test]
    fn golden_simple_shell_output() {
        let terminal = terminal(TerminalSize::new(20, 4), b"$ echo hi\r\nhi\r\n");

        assert_eq!(line_text(&terminal, 0), "$ echo hi");
        assert_eq!(line_text(&terminal, 1), "hi");
        assert_eq!(terminal.cursor_state().position, GridPosition::new(2, 0));
    }

    #[test]
    fn golden_color_output() {
        let terminal = terminal(TerminalSize::new(10, 2), b"\x1b[31mred\x1b[0m plain");

        let red_attrs = terminal.state().cell(0, 0).unwrap().attributes;
        let plain_attrs = terminal.state().cell(0, 4).unwrap().attributes;
        assert_eq!(red_attrs.foreground, Some(Color::Indexed(1)));
        assert_eq!(plain_attrs, CellAttributes::default());
    }

    #[test]
    fn golden_wrapping() {
        let terminal = terminal(TerminalSize::new(5, 3), b"abcdef");

        assert_eq!(line_text(&terminal, 0), "abcde");
        assert!(terminal.state().line(0).unwrap().hard_wrapped);
        assert_eq!(line_text(&terminal, 1), "f");
    }

    #[test]
    fn golden_scrolling() {
        let terminal = terminal(TerminalSize::new(10, 2), b"a\r\nb\r\nc");

        assert_eq!(terminal.scrollback().lines[0].raw_text(), "a");
        assert_eq!(line_text(&terminal, 0), "b");
        assert_eq!(line_text(&terminal, 1), "c");
    }

    #[test]
    fn golden_scroll_region_stays_local() {
        let terminal = terminal(
            TerminalSize::new(5, 4),
            b"top\r\none\r\ntwo\r\nbot\x1b[2;3r\x1b[3;1H\r\nnew",
        );

        assert_eq!(line_text(&terminal, 0), "top");
        assert_eq!(line_text(&terminal, 1), "two");
        assert_eq!(line_text(&terminal, 2), "new");
        assert_eq!(line_text(&terminal, 3), "bot");
        assert!(terminal.scrollback().lines.is_empty());
    }

    #[test]
    fn golden_clear_line_and_screen() {
        let mut terminal = terminal(TerminalSize::new(10, 3), b"abc\x1b[2K");

        assert_eq!(line_text(&terminal, 0), "");

        terminal.apply_bytes(b"abc\r\nxyz\x1b[2J").unwrap();
        assert_eq!(line_text(&terminal, 0), "");
        assert_eq!(line_text(&terminal, 1), "");
    }

    #[test]
    fn golden_alternate_screen_enter_exit() {
        let terminal = terminal(TerminalSize::new(10, 2), b"main\x1b[?1049halt\x1b[?1049l");

        assert_eq!(line_text(&terminal, 0), "main");
        assert!(!terminal.modes().contains(&TerminalMode::AlternateScreen));
    }

    #[test]
    fn golden_resize_reflows_primary_screen() {
        let mut terminal = terminal(TerminalSize::new(6, 2), b"abcdef");

        terminal.resize(TerminalSize::new(3, 3)).unwrap();

        assert_eq!(line_text(&terminal, 0), "abc");
        assert_eq!(line_text(&terminal, 1), "def");
        assert_eq!(terminal.cursor_state().position, GridPosition::new(1, 0));
    }

    #[test]
    fn golden_unicode_basics() {
        let terminal = terminal(TerminalSize::new(10, 2), "hé".as_bytes());

        assert_eq!(line_text(&terminal, 0), "hé");
    }

    #[test]
    fn golden_modes_and_cursor_metadata() {
        let terminal = terminal(TerminalSize::new(10, 2), b"\x1b[?2004h\x1b[?25l\x1b[5 q");

        assert!(terminal.modes().contains(&TerminalMode::BracketedPaste));
        assert!(!terminal.cursor_state().visible);
        assert_eq!(terminal.cursor_state().shape, CursorShape::Beam);
    }

    #[test]
    fn golden_selection_extracts_raw_text() {
        let mut terminal = terminal(TerminalSize::new(4, 3), b"abcdef");
        terminal.state_mut().set_selection(Selection::normal(
            GridPosition::new(0, 0),
            GridPosition::new(1, 1),
        ));

        assert_eq!(terminal.state().selected_text().as_deref(), Some("abcdef"));
    }

    #[test]
    fn random_bytes_do_not_panic() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(12, 4));
        let all_bytes: Vec<u8> = (0..=255).collect();

        for _ in 0..8 {
            terminal.apply_bytes(&all_bytes).unwrap();
        }
    }

    #[test]
    fn deterministic_fuzz_streams_do_not_panic() {
        let mut seed = 0x1234_5678_u32;

        for _case in 0..128 {
            let mut input = Vec::with_capacity(96);
            for _ in 0..96 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                input.push((seed >> 24) as u8);
            }

            let mut terminal = TerminalEmulator::new(TerminalSize::new(20, 5));
            terminal.apply_bytes(&input).unwrap();
        }
    }
}
