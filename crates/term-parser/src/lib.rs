//! ANSI/VT parsing boundary.

pub const LAYER: &str = "core correctness";

use term_core::{
    ClearMode, ClipboardTarget, Color, CursorDirection, CursorShape, CursorState, GraphicRendition,
    Osc52ClipboardRequest, Scrollback, SelectionRange, TerminalAction, TerminalCore, TerminalMode,
    TerminalResult, TerminalSize, TerminalState, VisibleGrid,
};

const MAX_CSI_PARAM_BYTES: usize = 256;
const MAX_OSC_PAYLOAD_BYTES: usize = 16 * 1024;
const MAX_STRING_PAYLOAD_BYTES: usize = 64 * 1024;

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

    pub fn apply_bytes_and_take_pending_output(&mut self, bytes: &[u8]) -> TerminalResult<Vec<u8>> {
        self.apply_bytes(bytes)?;
        Ok(self.state.take_pending_output())
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
    g0_charset: CharacterSet,
    g1_charset: CharacterSet,
    active_charset: CharacterSetSlot,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            state: ParserState::Ground,
            print_buffer: Vec::new(),
            g0_charset: CharacterSet::Ascii,
            g1_charset: CharacterSet::Ascii,
            active_charset: CharacterSetSlot::G0,
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
                        self.flush_print_buffer(&mut actions, false);
                        self.state = ParserState::Escape;
                    }
                    b'\r' => {
                        self.flush_print_buffer(&mut actions, false);
                        actions.push(TerminalAction::CarriageReturn);
                    }
                    b'\n' => {
                        self.flush_print_buffer(&mut actions, false);
                        actions.push(TerminalAction::LineFeed);
                    }
                    0x08 => {
                        self.flush_print_buffer(&mut actions, false);
                        actions.push(TerminalAction::Backspace);
                    }
                    b'\t' => {
                        self.flush_print_buffer(&mut actions, false);
                        actions.push(TerminalAction::Tab);
                    }
                    0x0e => {
                        self.flush_print_buffer(&mut actions, false);
                        self.active_charset = CharacterSetSlot::G1;
                    }
                    0x0f => {
                        self.flush_print_buffer(&mut actions, false);
                        self.active_charset = CharacterSetSlot::G0;
                    }
                    0x00..=0x1f | 0x7f => {}
                    byte @ 0x20..=0x7e
                        if match self.active_charset {
                            CharacterSetSlot::G0 => self.g0_charset,
                            CharacterSetSlot::G1 => self.g1_charset,
                        } == CharacterSet::DecSpecial =>
                    {
                        self.flush_print_buffer(&mut actions, false);
                        actions.push(TerminalAction::Print(dec_special_graphic(byte)));
                    }
                    _ => self.print_buffer.push(*byte),
                },
                ParserState::Escape => match *byte {
                    b'[' => self.state = ParserState::Csi(CsiState::default()),
                    b']' => {
                        self.state = ParserState::Osc {
                            escape_seen: false,
                            content: Vec::new(),
                        }
                    }
                    b'P' => {
                        self.state = ParserState::StringControl {
                            kind: StringControlKind::Dcs,
                            escape_seen: false,
                            content: Vec::new(),
                        }
                    }
                    b'_' | b'^' | b'X' => {
                        self.state = ParserState::StringControl {
                            kind: StringControlKind::Ignored,
                            escape_seen: false,
                            content: Vec::new(),
                        }
                    }
                    b'7' => {
                        actions.push(TerminalAction::SaveCursor);
                        self.state = ParserState::Ground;
                    }
                    b'8' => {
                        actions.push(TerminalAction::RestoreCursor);
                        self.state = ParserState::Ground;
                    }
                    b'H' => {
                        actions.push(TerminalAction::SetTabStop);
                        self.state = ParserState::Ground;
                    }
                    b'D' => {
                        actions.push(TerminalAction::LineFeed);
                        self.state = ParserState::Ground;
                    }
                    b'E' => {
                        actions.push(TerminalAction::NextLine);
                        self.state = ParserState::Ground;
                    }
                    b'M' => {
                        actions.push(TerminalAction::ReverseIndex);
                        self.state = ParserState::Ground;
                    }
                    b'(' | b')' | b'*' | b'+' => {
                        self.state = ParserState::CharacterSetDesignation(match *byte {
                            b')' | b'+' => CharacterSetSlot::G1,
                            _ => CharacterSetSlot::G0,
                        });
                    }
                    b'=' => {
                        actions.push(TerminalAction::SetMode {
                            mode: TerminalMode::ApplicationKeypad,
                            enabled: true,
                        });
                        self.state = ParserState::Ground;
                    }
                    b'>' => {
                        actions.push(TerminalAction::SetMode {
                            mode: TerminalMode::ApplicationKeypad,
                            enabled: false,
                        });
                        self.state = ParserState::Ground;
                    }
                    b'c' => {
                        actions.push(TerminalAction::Reset);
                        self.g0_charset = CharacterSet::Ascii;
                        self.g1_charset = CharacterSet::Ascii;
                        self.active_charset = CharacterSetSlot::G0;
                        self.state = ParserState::Ground;
                    }
                    _ => self.state = ParserState::Ground,
                },
                ParserState::Csi(csi) => {
                    if matches!(*byte, b'0'..=b'9' | b';' | b':')
                        && csi.params.len() >= MAX_CSI_PARAM_BYTES
                    {
                        self.state = ParserState::IgnoringCsi;
                        continue;
                    }
                    if csi.consume(*byte) {
                        let action_set = dispatch_csi(csi);
                        actions.extend(action_set);
                        self.state = ParserState::Ground;
                    }
                }
                ParserState::Osc {
                    escape_seen,
                    content,
                } => match (*byte, *escape_seen) {
                    (0x07, _) => {
                        actions.extend(dispatch_osc(content));
                        self.state = ParserState::Ground;
                    }
                    (b'\\', true) => {
                        if content.last() == Some(&0x1b) {
                            content.pop();
                        }
                        actions.extend(dispatch_osc(content));
                        self.state = ParserState::Ground;
                    }
                    (0x1b, _) => {
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
                ParserState::IgnoringCsi => {
                    if (0x40..=0x7e).contains(byte) {
                        self.state = ParserState::Ground;
                    }
                }
                ParserState::CharacterSetDesignation(slot) => {
                    let charset = if *byte == b'0' {
                        CharacterSet::DecSpecial
                    } else {
                        CharacterSet::Ascii
                    };
                    match slot {
                        CharacterSetSlot::G0 => self.g0_charset = charset,
                        CharacterSetSlot::G1 => self.g1_charset = charset,
                    }
                    self.state = ParserState::Ground;
                }
                ParserState::IgnoringOsc { escape_seen } => match (*byte, *escape_seen) {
                    (0x07, _) | (b'\\', true) => self.state = ParserState::Ground,
                    (0x1b, _) => *escape_seen = true,
                    (_, _) => *escape_seen = false,
                },
                ParserState::StringControl {
                    kind,
                    escape_seen,
                    content,
                } => match (*byte, *escape_seen) {
                    (0x18, _) | (0x1a, _) => self.state = ParserState::Ground,
                    (b'\\', true) => {
                        if content.last() == Some(&0x1b) {
                            content.pop();
                        }
                        if *kind == StringControlKind::Dcs {
                            actions.extend(dispatch_dcs(content));
                        }
                        self.state = ParserState::Ground;
                    }
                    (0x1b, _) => {
                        if content.len() >= MAX_STRING_PAYLOAD_BYTES {
                            self.state = ParserState::IgnoringStringControl { escape_seen: true };
                        } else {
                            content.push(*byte);
                            *escape_seen = true;
                        }
                    }
                    (_, _) => {
                        if content.len() >= MAX_STRING_PAYLOAD_BYTES {
                            self.state = ParserState::IgnoringStringControl { escape_seen: false };
                        } else {
                            content.push(*byte);
                            *escape_seen = false;
                        }
                    }
                },
                ParserState::IgnoringStringControl { escape_seen } => match (*byte, *escape_seen) {
                    (0x18, _) | (0x1a, _) | (b'\\', true) => {
                        self.state = ParserState::Ground;
                    }
                    (0x1b, _) => *escape_seen = true,
                    (_, _) => *escape_seen = false,
                },
            }
        }

        if matches!(self.state, ParserState::Ground) {
            self.flush_print_buffer(&mut actions, true);
        }

        actions
    }

    fn flush_print_buffer(&mut self, actions: &mut Vec<TerminalAction>, preserve_incomplete: bool) {
        while !self.print_buffer.is_empty() {
            match std::str::from_utf8(&self.print_buffer) {
                Ok(text) => {
                    actions.extend(text.chars().map(TerminalAction::Print));
                    self.print_buffer.clear();
                    return;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    if valid_up_to > 0 {
                        let valid = std::str::from_utf8(&self.print_buffer[..valid_up_to])
                            .expect("valid_up_to always names valid UTF-8");
                        actions.extend(valid.chars().map(TerminalAction::Print));
                        self.print_buffer.drain(..valid_up_to);
                    }

                    match error.error_len() {
                        Some(invalid_len) => {
                            self.print_buffer.drain(..invalid_len);
                        }
                        None if preserve_incomplete => return,
                        None => {
                            self.print_buffer.clear();
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParserState {
    Ground,
    Escape,
    Csi(CsiState),
    Osc {
        escape_seen: bool,
        content: Vec<u8>,
    },
    CharacterSetDesignation(CharacterSetSlot),
    IgnoringCsi,
    IgnoringOsc {
        escape_seen: bool,
    },
    StringControl {
        kind: StringControlKind,
        escape_seen: bool,
        content: Vec<u8>,
    },
    IgnoringStringControl {
        escape_seen: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringControlKind {
    Dcs,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharacterSetSlot {
    G0,
    G1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharacterSet {
    Ascii,
    DecSpecial,
}

fn dec_special_graphic(byte: u8) -> char {
    match byte {
        b'_' => ' ',
        b'`' => '◆',
        b'a' => '▒',
        b'b' => '␉',
        b'c' => '␌',
        b'd' => '␍',
        b'e' => '␊',
        b'f' => '°',
        b'g' => '±',
        b'h' => '␤',
        b'i' => '␋',
        b'j' => '┘',
        b'k' => '┐',
        b'l' => '┌',
        b'm' => '└',
        b'n' => '┼',
        b'o' => '⎺',
        b'p' => '⎻',
        b'q' => '─',
        b'r' => '⎼',
        b's' => '⎽',
        b't' => '├',
        b'u' => '┤',
        b'v' => '┴',
        b'w' => '┬',
        b'x' => '│',
        b'y' => '≤',
        b'z' => '≥',
        b'{' => 'π',
        b'|' => '≠',
        b'}' => '£',
        b'~' => '·',
        _ => char::from(byte),
    }
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
        b'@' => vec![TerminalAction::InsertChars(csi.param_or(0, 1))],
        b'A' => vec![move_cursor(csi, CursorDirection::Up)],
        b'B' => vec![move_cursor(csi, CursorDirection::Down)],
        b'C' => vec![move_cursor(csi, CursorDirection::Forward)],
        b'a' => vec![move_cursor(csi, CursorDirection::Forward)],
        b'D' => vec![move_cursor(csi, CursorDirection::Back)],
        b'E' => vec![move_cursor(csi, CursorDirection::NextLine)],
        b'F' => vec![move_cursor(csi, CursorDirection::PreviousLine)],
        b'G' => vec![TerminalAction::SetCursorColumn(csi.param_or(0, 1))],
        b'`' => vec![TerminalAction::SetCursorColumn(csi.param_or(0, 1))],
        b'd' => vec![TerminalAction::SetCursorRow(csi.param_or(0, 1))],
        b'e' => vec![move_cursor(csi, CursorDirection::Down)],
        b'H' | b'f' => vec![TerminalAction::SetCursorPosition {
            row: csi.param_or(0, 1),
            col: csi.param_or(1, 1),
        }],
        b'J' => vec![TerminalAction::ClearScreen(clear_mode(csi.param_or(0, 0)))],
        b'K' => vec![TerminalAction::ClearLine(clear_mode(csi.param_or(0, 0)))],
        b'L' => vec![TerminalAction::InsertLines(csi.param_or(0, 1))],
        b'M' => vec![TerminalAction::DeleteLines(csi.param_or(0, 1))],
        b'P' => vec![TerminalAction::DeleteChars(csi.param_or(0, 1))],
        b'S' => vec![TerminalAction::ScrollUp(csi.param_or(0, 1))],
        b'T' => vec![TerminalAction::ScrollDown(csi.param_or(0, 1))],
        b'X' => vec![TerminalAction::EraseChars(csi.param_or(0, 1))],
        b'Z' => vec![TerminalAction::BackTab(csi.param_or(0, 1))],
        b'I' => (0..csi.param_or(0, 1))
            .map(|_| TerminalAction::Tab)
            .collect(),
        b'b' => vec![TerminalAction::RepeatLastPrinted(csi.param_or(0, 1))],
        b'c' if !csi.private => vec![TerminalAction::PrimaryDeviceAttributes],
        b'g' => tab_clear_action(csi),
        b'm' => vec![TerminalAction::SetGraphicRendition(parse_sgr(
            &csi.params(),
        ))],
        b'n' if csi.private => vec![TerminalAction::PrivateDeviceStatusReport(
            csi.param_or(0, 0),
        )],
        b'n' => vec![TerminalAction::DeviceStatusReport(csi.param_or(0, 0))],
        b's' => vec![TerminalAction::SaveCursor],
        b'u' => vec![TerminalAction::RestoreCursor],
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
            2 => out.push(GraphicRendition::Dim),
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
        return csi
            .params()
            .into_iter()
            .filter_map(|mode| match mode {
                4 => Some(TerminalAction::SetMode {
                    mode: TerminalMode::Insert,
                    enabled,
                }),
                _ => None,
            })
            .collect();
    }

    let mut actions = Vec::new();
    for mode in csi.params() {
        match mode {
            1 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::ApplicationCursorKeys,
                enabled,
            }),
            6 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::Origin,
                enabled,
            }),
            7 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::AutoWrap,
                enabled,
            }),
            25 => actions.push(TerminalAction::SetCursorVisible(enabled)),
            66 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::ApplicationKeypad,
                enabled,
            }),
            1000 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::MouseReporting,
                enabled,
            }),
            1002 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::MouseCellMotion,
                enabled,
            }),
            1003 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::MouseAllMotion,
                enabled,
            }),
            1004 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::FocusEvents,
                enabled,
            }),
            1006 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::SgrMouse,
                enabled,
            }),
            1048 => {
                if enabled {
                    actions.push(TerminalAction::SaveCursor);
                } else {
                    actions.push(TerminalAction::RestoreCursor);
                }
            }
            1047 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::AlternateScreen,
                enabled,
            }),
            1049 => {
                if enabled {
                    actions.push(TerminalAction::SaveCursor);
                    actions.push(TerminalAction::SetMode {
                        mode: TerminalMode::AlternateScreen,
                        enabled: true,
                    });
                } else {
                    actions.push(TerminalAction::SetMode {
                        mode: TerminalMode::AlternateScreen,
                        enabled: false,
                    });
                    actions.push(TerminalAction::RestoreCursor);
                }
            }
            2004 => actions.push(TerminalAction::SetMode {
                mode: TerminalMode::BracketedPaste,
                enabled,
            }),
            _ => {}
        }
    }

    actions
}

fn tab_clear_action(csi: &CsiState) -> Vec<TerminalAction> {
    match csi.param_or(0, 0) {
        3 => vec![TerminalAction::ClearAllTabStops],
        _ => vec![TerminalAction::ClearTabStop],
    }
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

fn dispatch_osc(content: &[u8]) -> Vec<TerminalAction> {
    let text = String::from_utf8_lossy(content);
    let Some((command, payload)) = text.split_once(';') else {
        return Vec::new();
    };

    match command {
        "0" | "2" => vec![TerminalAction::SetTitle(payload.to_owned())],
        "52" => osc52_action(payload).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn osc52_action(payload: &str) -> Option<TerminalAction> {
    let (selector, payload_base64) = payload.split_once(';')?;
    let target = selector
        .chars()
        .next()
        .map(ClipboardTarget::from_osc52_selector)
        .unwrap_or(ClipboardTarget::Clipboard);

    Some(TerminalAction::Osc52Clipboard(Osc52ClipboardRequest {
        target,
        payload_base64: payload_base64.to_owned(),
    }))
}

fn dispatch_dcs(content: &[u8]) -> Vec<TerminalAction> {
    let Some(payload) = content.strip_prefix(b"tmux;") else {
        return Vec::new();
    };
    let mut unescaped = Vec::with_capacity(payload.len());
    let mut index = 0;
    while index < payload.len() {
        if payload[index] == 0x1b && payload.get(index + 1) == Some(&0x1b) {
            unescaped.push(0x1b);
            index += 2;
        } else {
            unescaped.push(payload[index]);
            index += 1;
        }
    }
    Parser::default().parse(&unescaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use term_core::{CellAttributes, GridPosition, Selection};

    fn terminal(size: TerminalSize, input: &[u8]) -> TerminalEmulator {
        let mut terminal = TerminalEmulator::new(size);
        terminal.apply_bytes(input).unwrap();
        terminal
    }

    fn line_text(terminal: &TerminalEmulator, row: u16) -> String {
        terminal.state().line(row).unwrap().raw_text()
    }

    fn assert_terminal_invariants(terminal: &TerminalEmulator) {
        let grid = terminal.state().grid();
        let cols = usize::from(grid.size.cols.max(1));
        let rows = usize::from(grid.size.rows.max(1));

        assert_eq!(grid.lines.len(), rows);
        for line in &grid.lines {
            assert_eq!(line.cells.len(), cols);
            assert_line_invariants(line);
        }
        for line in terminal.scrollback().lines {
            assert_eq!(line.cells.len(), cols);
            assert_line_invariants(&line);
        }

        let visible = terminal.visible_grid();
        assert_eq!(visible.cells.len(), rows * cols);

        let cursor = terminal.cursor_state().position;
        assert!(cursor.row >= 0);
        assert!(usize::try_from(cursor.row).is_ok_and(|row| row < rows));
        assert!(usize::from(cursor.col) < cols);

        if let Some(text) = terminal.state().selected_text() {
            assert!(!text.contains('\u{fffd}'));
        }
    }

    fn assert_line_invariants(line: &term_core::Line) {
        for (index, cell) in line.cells.iter().enumerate() {
            assert!(cell.width <= 2);
            if cell.wide_continuation {
                assert_eq!(cell.width, 0);
                assert!(index > 0);
                assert_eq!(line.cells[index - 1].width, 2);
            } else {
                assert!(cell.width >= 1);
                assert!(!cell.text.is_empty());
                if cell.width == 2 && index + 1 < line.cells.len() {
                    assert!(line.cells[index + 1].wide_continuation);
                }
            }
        }
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
        assert_eq!(terminal.cursor_state().position, GridPosition::new(1, 2));
    }

    #[test]
    fn autowrap_is_deferred_and_controls_cancel_wrap_pending() {
        let mut emulator = terminal(TerminalSize::new(3, 2), b"abc");
        assert_eq!(emulator.cursor_state().position, GridPosition::new(0, 2));
        emulator.apply_bytes(b"d").unwrap();
        assert_eq!(line_text(&emulator, 0), "abc");
        assert_eq!(line_text(&emulator, 1), "d");
        assert!(emulator.state().line(0).unwrap().hard_wrapped);

        let terminal = terminal(TerminalSize::new(3, 2), b"abc\rX");
        assert_eq!(line_text(&terminal, 0), "Xbc");
        assert_eq!(line_text(&terminal, 1), "");
    }

    #[test]
    fn scrolled_blank_lines_preserve_current_background_attributes() {
        let terminal = terminal(TerminalSize::new(3, 2), b"\x1b[41mabc\r\ndef\r\n");
        let bottom = terminal.state().line(1).unwrap();
        assert!(
            bottom
                .cells
                .iter()
                .all(|cell| cell.attributes.background == Some(Color::Indexed(1)))
        );
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
    fn dec_application_keypad_escape_sequences_toggle_mode() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(10, 2));
        terminal.apply_bytes(b"\x1b=").unwrap();
        assert!(terminal.modes().contains(&TerminalMode::ApplicationKeypad));

        terminal.apply_bytes(b"\x1b>").unwrap();
        assert!(!terminal.modes().contains(&TerminalMode::ApplicationKeypad));
    }

    #[test]
    fn golden_extended_sgr_truecolor_and_dim() {
        let terminal = terminal(
            TerminalSize::new(10, 2),
            b"\x1b[2;3;4;9;38;2;1;2;3;48;5;42mA",
        );

        let attrs = terminal.state().cell(0, 0).unwrap().attributes;
        assert!(attrs.dim);
        assert!(attrs.italic);
        assert!(attrs.underline);
        assert!(attrs.strikethrough);
        assert_eq!(
            attrs.foreground,
            Some(Color::Rgb {
                red: 1,
                green: 2,
                blue: 3
            })
        );
        assert_eq!(attrs.background, Some(Color::Indexed(42)));
    }

    #[test]
    fn golden_title_save_restore_and_status_response() {
        let mut terminal = terminal(
            TerminalSize::new(20, 3),
            b"abc\x1b[s\r\nnext\x1b[u!\x1b]2;Panea title\x07\x1b[6n",
        );

        assert_eq!(terminal.state().title(), Some("Panea title"));
        assert_eq!(line_text(&terminal, 0), "abc!");
        assert_eq!(
            String::from_utf8(terminal.state_mut().take_pending_output()).unwrap(),
            "\x1b[1;5R"
        );
    }

    #[test]
    fn incremental_apply_returns_split_terminal_query_responses() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(80, 24));

        assert_eq!(
            terminal
                .apply_bytes_and_take_pending_output(b"\x1b[")
                .unwrap(),
            Vec::<u8>::new()
        );
        assert_eq!(
            terminal.apply_bytes_and_take_pending_output(b"6n").unwrap(),
            b"\x1b[1;1R"
        );
    }

    #[test]
    fn golden_osc52_is_reported_as_pending_clipboard_request() {
        let mut terminal = terminal(TerminalSize::new(20, 3), b"\x1b]52;c;cGFuZWE=\x07");

        let requests = terminal.state_mut().take_pending_clipboard_requests();

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].target, ClipboardTarget::Clipboard);
        assert_eq!(requests[0].payload_base64, "cGFuZWE=");
        assert_eq!(line_text(&terminal, 0), "");
    }

    #[test]
    fn golden_insert_delete_line_and_char_operations() {
        let terminal = terminal(
            TerminalSize::new(8, 4),
            b"one\r\ntwo\r\nthree\x1b[2;1H\x1b[1Lnew\x1b[4;2H\x1b[2P",
        );

        assert_eq!(line_text(&terminal, 0), "one");
        assert_eq!(line_text(&terminal, 1), "new");
        assert_eq!(line_text(&terminal, 3), "tee");
    }

    #[test]
    fn vt_index_reverse_index_and_next_line_respect_scroll_region() {
        let terminal = terminal(
            TerminalSize::new(4, 3),
            b"one\r\ntwo\r\ntri\x1b[2;3r\x1b[2;1H\x1bMtop\x1bEend",
        );

        assert_eq!(line_text(&terminal, 0), "one");
        assert_eq!(line_text(&terminal, 1), "top");
        assert!(line_text(&terminal, 2).starts_with("end"));
    }

    #[test]
    fn origin_autowrap_scroll_and_repeat_controls_are_applied() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(5, 4));
        terminal
            .apply_bytes(b"\x1b[2;4r\x1b[?6h\x1b[2;2HX\x1b[?7l\x1b[4;5HYZ\x1b[?7h\x1b[?6l")
            .unwrap();
        assert_eq!(terminal.state().line(2).unwrap().cells[1].text, "X");
        assert_eq!(terminal.state().line(3).unwrap().cells[4].text, "Z");

        terminal
            .apply_bytes(b"\x1b[1;1HA\x1b[4b\x1b[1S\x1b[1T")
            .unwrap();
        assert_eq!(line_text(&terminal, 0), "AAAAA");
        assert!(!terminal.modes().contains(&TerminalMode::Origin));
        assert!(terminal.modes().contains(&TerminalMode::AutoWrap));
    }

    #[test]
    fn horizontal_vertical_tab_and_charset_controls_do_not_leak_bytes() {
        let terminal = terminal(
            TerminalSize::new(24, 3),
            b"A\x1b(B\x1b[2IB\x1b[ZC\x1b[2dD\x1b[3`E",
        );
        assert_eq!(terminal.state().line(0).unwrap().cells[16].text, "C");
        assert_eq!(terminal.state().line(1).unwrap().cells[2].text, "E");
        assert!(!line_text(&terminal, 0).contains('B'));
    }

    #[test]
    fn primary_attributes_and_private_cursor_report_are_bounded_responses() {
        let mut terminal = terminal(TerminalSize::new(10, 4), b"\x1b[3;4H\x1b[c\x1b[?6n");
        assert_eq!(
            terminal.state_mut().take_pending_output(),
            b"\x1b[?1;2c\x1b[?3;4R"
        );
    }

    #[test]
    fn string_controls_are_bounded_and_never_leak_into_terminal_text() {
        let mut parser = Parser::default();
        let mut input = b"before\x1bPignored".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_STRING_PAYLOAD_BYTES + 32));
        input.extend_from_slice(b"\x1b\\after\x1b_hidden\x1b\\done");
        let mut state = TerminalState::new(TerminalSize::new(32, 2));
        state.apply_actions(parser.parse(&input)).unwrap();
        assert_eq!(state.line(0).unwrap().raw_text(), "beforeafterdone");
    }

    #[test]
    fn tmux_dcs_passthrough_applies_nested_terminal_sequences() {
        let terminal = terminal(
            TerminalSize::new(16, 2),
            b"\x1bPtmux;\x1b\x1b[31mred\x1b\x1b[0m\x1b\\",
        );
        let line = terminal.state().line(0).unwrap();
        assert_eq!(line.raw_text(), "red");
        assert_eq!(line.cells[0].attributes.foreground, Some(Color::Indexed(1)));
        assert_eq!(line.cells[2].attributes.foreground, Some(Color::Indexed(1)));
    }

    #[test]
    fn dec_special_graphics_support_tui_line_drawing_in_g0_and_g1() {
        let terminal = terminal(
            TerminalSize::new(16, 2),
            b"\x1b(0lqk\x1b(B \x1b)0\x0emqx\x0f ascii",
        );
        assert_eq!(line_text(&terminal, 0), "┌─┐ └─│ ascii");
    }

    #[test]
    fn golden_tab_stops_can_be_set_and_cleared() {
        let terminal = terminal(TerminalSize::new(20, 2), b"\x1b[9G\x1b[g\x1b[1GA\tB");

        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 17));
        assert_eq!(line_text(&terminal, 0), "A               B");
    }

    #[test]
    fn golden_mouse_modes_are_tracked() {
        let terminal = terminal(TerminalSize::new(10, 2), b"\x1b[?1000;1006;1004h");

        assert!(terminal.modes().contains(&TerminalMode::MouseReporting));
        assert!(terminal.modes().contains(&TerminalMode::SgrMouse));
        assert!(terminal.modes().contains(&TerminalMode::FocusEvents));
    }

    #[test]
    fn golden_unicode_wide_and_combining_cells() {
        let terminal = terminal(TerminalSize::new(6, 2), "a界e\u{301}b".as_bytes());

        assert_eq!(line_text(&terminal, 0), "a界e\u{301}b");
        assert!(terminal.state().cell(0, 2).unwrap().wide_continuation);
        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 5));
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
    fn split_utf8_scalar_across_reads_is_preserved() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(8, 2));
        let text = "👍🏽";
        let bytes = text.as_bytes();

        terminal.apply_bytes(&bytes[..2]).unwrap();
        assert_eq!(line_text(&terminal, 0), "");

        terminal.apply_bytes(&bytes[2..5]).unwrap();
        terminal.apply_bytes(&bytes[5..]).unwrap();

        assert_eq!(line_text(&terminal, 0), text);
        assert_eq!(terminal.state().cell(0, 0).unwrap().text, text);
        assert!(terminal.state().cell(0, 1).unwrap().wide_continuation);
    }

    #[test]
    fn invalid_utf8_is_dropped_without_corrupting_later_text() {
        let terminal = terminal(TerminalSize::new(8, 2), &[b'a', 0xff, b'b']);

        assert_eq!(line_text(&terminal, 0), "ab");
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

    #[test]
    fn unterminated_osc_payload_is_bounded_and_dropped() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(12, 3));
        let mut payload = vec![0x1b, b']'];
        payload.extend(std::iter::repeat_n(b'a', MAX_OSC_PAYLOAD_BYTES + 512));
        payload.extend_from_slice(b"\x07visible");

        terminal.apply_bytes(&payload).unwrap();

        assert_terminal_invariants(&terminal);
        assert_eq!(line_text(&terminal, 0), "visible");
    }

    #[test]
    fn oversized_csi_parameters_are_dropped_without_panicking() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(12, 3));
        let mut payload = vec![0x1b, b'['];
        payload.extend(std::iter::repeat_n(b'1', MAX_CSI_PARAM_BYTES + 128));
        payload.extend_from_slice(b"mplain");

        terminal.apply_bytes(&payload).unwrap();

        assert_terminal_invariants(&terminal);
        assert_eq!(line_text(&terminal, 0), "plain");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        #[test]
        fn fuzz_parser_byte_streams_do_not_corrupt_terminal(
            input in prop::collection::vec(any::<u8>(), 0..2048)
        ) {
            let mut terminal = TerminalEmulator::new(TerminalSize::new(24, 8));
            terminal.apply_bytes(&input).unwrap();
            assert_terminal_invariants(&terminal);
        }

        #[test]
        fn fuzz_parser_chunk_boundaries_preserve_invariants(
            input in prop::collection::vec(any::<u8>(), 0..1024),
            chunk_size in 1_usize..32
        ) {
            let mut terminal = TerminalEmulator::new(TerminalSize::new(24, 8));
            for chunk in input.chunks(chunk_size) {
                terminal.apply_bytes(chunk).unwrap();
                assert_terminal_invariants(&terminal);
            }
        }

        #[test]
        fn fuzz_parser_resize_and_selection_stay_valid(
            input in prop::collection::vec(any::<u8>(), 0..512),
            sizes in prop::collection::vec((1_u16..80, 1_u16..24), 0..64)
        ) {
            let mut terminal = TerminalEmulator::new(TerminalSize::new(20, 6));
            terminal.apply_bytes(&input).unwrap();
            for (index, (cols, rows)) in sizes.into_iter().enumerate() {
                terminal.resize(TerminalSize::new(cols, rows)).unwrap();
                terminal.state_mut().set_selection(Selection::normal(
                    GridPosition::new(0, 0),
                    GridPosition::new(
                        i64::from(rows.saturating_sub(1)),
                        cols.saturating_sub(1),
                    ),
                ));
                if index % 3 == 0 {
                    let _ = terminal.state().selected_text();
                }
                assert_terminal_invariants(&terminal);
            }
        }
    }
}
