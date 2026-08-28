//! ANSI/VT parsing boundary.

pub const LAYER: &str = "core correctness";

use term_core::{
    ClearMode, ClipboardTarget, Color, CursorDirection, CursorShape, CursorState, GraphicRendition,
    KittyKeyboardMode, Osc52ClipboardRequest, Scrollback, SelectionRange, TerminalAction,
    TerminalCore, TerminalMode, TerminalResult, TerminalSize, TerminalState, VisibleGrid,
};

const MAX_CSI_PARAMS: usize = 32;
const MAX_CSI_SUBPARAMS: usize = 8;
const MAX_CSI_PRIVATE_MARKERS: usize = 4;
const MAX_CSI_INTERMEDIATES: usize = 4;
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

    /// Builds an emulator that retains at most `scrollback_limit` scrolled-off
    /// lines, so a host can honour its configured `scrollback.lines`.
    #[must_use]
    pub fn with_scrollback_limit(size: TerminalSize, scrollback_limit: usize) -> Self {
        Self {
            parser: Parser::default(),
            state: TerminalState::with_scrollback_limit(size, scrollback_limit),
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
    pub fn modes_ref(&self) -> &std::collections::BTreeSet<TerminalMode> {
        self.state.modes_ref()
    }

    #[must_use]
    pub fn scrollback_lines(&self) -> std::collections::VecDeque<term_core::Line> {
        self.state.scrollback_lines()
    }

    #[must_use]
    pub fn scrollback_line_count(&self) -> usize {
        self.state.scrollback_line_count()
    }

    #[must_use]
    pub fn scrollback_memory_bytes(&self) -> u64 {
        self.state.scrollback_memory_bytes()
    }

    #[must_use]
    pub fn history_stats(&self) -> term_core::HistoryStats {
        self.state.history_stats()
    }

    pub fn resize_with_positions(
        &mut self,
        size: TerminalSize,
        positions: &mut [term_core::GridPosition],
    ) -> TerminalResult<()> {
        self.state.resize_with_positions(size, positions)
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
        let parser = &mut self.parser;
        let state = &mut self.state;
        let mut result = Ok(());
        parser.parse_outputs(bytes, |output| {
            if result.is_ok() {
                match output {
                    ParserOutput::Text(text) => state.apply_printable_text(text),
                    ParserOutput::Action(action) => result = state.apply_action(action),
                }
            }
        });
        result
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

enum ParserOutput<'a> {
    Text(&'a str),
    Action(TerminalAction),
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
        self.parse_with(bytes, |action| actions.push(action));
        actions
    }

    fn parse_with(&mut self, bytes: &[u8], mut emit: impl FnMut(TerminalAction)) {
        self.parse_with_dyn(bytes, &mut emit);
    }

    fn parse_with_dyn(&mut self, bytes: &[u8], emit: &mut dyn FnMut(TerminalAction)) {
        self.parse_outputs(bytes, |output| match output {
            ParserOutput::Text(text) => {
                for ch in text.chars() {
                    emit(TerminalAction::Print(ch));
                }
            }
            ParserOutput::Action(action) => emit(action),
        });
    }

    fn parse_outputs(&mut self, bytes: &[u8], mut emit: impl FnMut(ParserOutput<'_>)) {
        let mut index = 0;
        while index < bytes.len() {
            let byte = &bytes[index];
            let mut advance = true;
            match &mut self.state {
                ParserState::Ground => match *byte {
                    0x1b => {
                        self.flush_print_buffer(&mut emit, false);
                        self.state = ParserState::Escape;
                    }
                    b'\r' => {
                        self.flush_print_buffer(&mut emit, false);
                        emit(ParserOutput::Action(TerminalAction::CarriageReturn));
                    }
                    b'\n' => {
                        self.flush_print_buffer(&mut emit, false);
                        emit(ParserOutput::Action(TerminalAction::LineFeed));
                    }
                    0x08 => {
                        self.flush_print_buffer(&mut emit, false);
                        emit(ParserOutput::Action(TerminalAction::Backspace));
                    }
                    b'\t' => {
                        self.flush_print_buffer(&mut emit, false);
                        emit(ParserOutput::Action(TerminalAction::Tab));
                    }
                    0x0e => {
                        self.flush_print_buffer(&mut emit, false);
                        self.active_charset = CharacterSetSlot::G1;
                    }
                    0x0f => {
                        self.flush_print_buffer(&mut emit, false);
                        self.active_charset = CharacterSetSlot::G0;
                    }
                    0x00..=0x1f | 0x7f => {}
                    byte @ 0x20..=0x7e
                        if match self.active_charset {
                            CharacterSetSlot::G0 => self.g0_charset,
                            CharacterSetSlot::G1 => self.g1_charset,
                        } == CharacterSet::DecSpecial =>
                    {
                        self.flush_print_buffer(&mut emit, false);
                        emit(ParserOutput::Action(TerminalAction::Print(
                            dec_special_graphic(byte),
                        )));
                    }
                    _ => self.print_buffer.push(*byte),
                },
                ParserState::Escape => match *byte {
                    0x1b => self.state = ParserState::Escape,
                    0x18 | 0x1a => self.state = ParserState::Ground,
                    byte if execute_c0(byte, &mut emit) => self.state = ParserState::Escape,
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
                        emit(ParserOutput::Action(TerminalAction::SaveCursor));
                        self.state = ParserState::Ground;
                    }
                    b'8' => {
                        emit(ParserOutput::Action(TerminalAction::RestoreCursor));
                        self.state = ParserState::Ground;
                    }
                    b'H' => {
                        emit(ParserOutput::Action(TerminalAction::SetTabStop));
                        self.state = ParserState::Ground;
                    }
                    b'D' => {
                        emit(ParserOutput::Action(TerminalAction::LineFeed));
                        self.state = ParserState::Ground;
                    }
                    b'E' => {
                        emit(ParserOutput::Action(TerminalAction::NextLine));
                        self.state = ParserState::Ground;
                    }
                    b'M' => {
                        emit(ParserOutput::Action(TerminalAction::ReverseIndex));
                        self.state = ParserState::Ground;
                    }
                    b'(' | b')' | b'*' | b'+' => {
                        self.state = ParserState::CharacterSetDesignation(match *byte {
                            b')' | b'+' => CharacterSetSlot::G1,
                            _ => CharacterSetSlot::G0,
                        });
                    }
                    b'=' => {
                        emit(ParserOutput::Action(TerminalAction::SetMode {
                            mode: TerminalMode::ApplicationKeypad,
                            enabled: true,
                        }));
                        self.state = ParserState::Ground;
                    }
                    b'>' => {
                        emit(ParserOutput::Action(TerminalAction::SetMode {
                            mode: TerminalMode::ApplicationKeypad,
                            enabled: false,
                        }));
                        self.state = ParserState::Ground;
                    }
                    b'c' => {
                        emit(ParserOutput::Action(TerminalAction::Reset));
                        self.g0_charset = CharacterSet::Ascii;
                        self.g1_charset = CharacterSet::Ascii;
                        self.active_charset = CharacterSetSlot::G0;
                        self.state = ParserState::Ground;
                    }
                    b'Z' => {
                        emit(ParserOutput::Action(
                            TerminalAction::PrimaryDeviceAttributes,
                        ));
                        self.state = ParserState::Ground;
                    }
                    byte @ 0x20..=0x2f => {
                        let mut intermediates = EscapeIntermediates::default();
                        if intermediates.push(byte) {
                            self.state = ParserState::EscapeIntermediate(intermediates);
                        } else {
                            self.state = ParserState::Ground;
                        }
                    }
                    _ => self.state = ParserState::Ground,
                },
                ParserState::EscapeIntermediate(intermediates) => match *byte {
                    0x1b => self.state = ParserState::Escape,
                    0x18 | 0x1a => self.state = ParserState::Ground,
                    byte if execute_c0(byte, &mut emit) => {}
                    byte @ 0x20..=0x2f => {
                        if !intermediates.push(byte) {
                            self.state = ParserState::Ground;
                        }
                    }
                    final_byte @ 0x30..=0x7e => {
                        dispatch_escape_intermediate(intermediates, final_byte, &mut |action| {
                            emit(ParserOutput::Action(action))
                        });
                        self.state = ParserState::Ground;
                    }
                    _ => self.state = ParserState::Ground,
                },
                ParserState::Csi(csi) => match *byte {
                    0x1b => self.state = ParserState::Escape,
                    0x18 | 0x1a => self.state = ParserState::Ground,
                    byte if execute_c0(byte, &mut emit) => {}
                    byte => match csi.consume(byte) {
                        CsiConsume::Continue => {}
                        CsiConsume::Dispatch => {
                            dispatch_csi(csi, &mut |action| {
                                emit(ParserOutput::Action(action));
                            });
                            self.state = ParserState::Ground;
                        }
                        CsiConsume::Ignore => self.state = ParserState::IgnoringCsi,
                    },
                },
                ParserState::Osc {
                    escape_seen,
                    content,
                } => match (*byte, *escape_seen) {
                    (0x18 | 0x1a, _) => self.state = ParserState::Ground,
                    (0x07, _) => {
                        dispatch_osc(content, &mut |action| emit(ParserOutput::Action(action)));
                        self.state = ParserState::Ground;
                    }
                    (b'\\', true) => {
                        dispatch_osc(content, &mut |action| emit(ParserOutput::Action(action)));
                        self.state = ParserState::Ground;
                    }
                    (_, true) => {
                        dispatch_osc(content, &mut |action| emit(ParserOutput::Action(action)));
                        self.state = ParserState::Escape;
                        advance = false;
                    }
                    (0x1b, false) => {
                        if content.len() >= MAX_OSC_PAYLOAD_BYTES {
                            self.state = ParserState::IgnoringOsc { escape_seen: true };
                        } else {
                            *escape_seen = true;
                        }
                    }
                    (_, false) => {
                        if content.len() >= MAX_OSC_PAYLOAD_BYTES {
                            self.state = ParserState::IgnoringOsc { escape_seen: false };
                        } else {
                            content.push(*byte);
                            *escape_seen = false;
                        }
                    }
                },
                ParserState::IgnoringCsi => match *byte {
                    0x1b => self.state = ParserState::Escape,
                    0x18 | 0x1a => self.state = ParserState::Ground,
                    byte if execute_c0(byte, &mut emit) => {}
                    0x40..=0x7e => self.state = ParserState::Ground,
                    _ => {}
                },
                ParserState::CharacterSetDesignation(slot) => match *byte {
                    0x1b => self.state = ParserState::Escape,
                    0x18 | 0x1a => self.state = ParserState::Ground,
                    byte if execute_c0(byte, &mut emit) => {}
                    byte => {
                        let charset = if byte == b'0' {
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
                },
                ParserState::IgnoringOsc { escape_seen } => match (*byte, *escape_seen) {
                    (0x18 | 0x1a | 0x07, _) | (b'\\', true) => {
                        self.state = ParserState::Ground;
                    }
                    (_, true) => {
                        self.state = ParserState::Escape;
                        advance = false;
                    }
                    (0x1b, false) => *escape_seen = true,
                    (_, false) => *escape_seen = false,
                },
                ParserState::StringControl {
                    kind,
                    escape_seen,
                    content,
                } => match (*byte, *escape_seen) {
                    (0x18, _) | (0x1a, _) => self.state = ParserState::Ground,
                    (b'\\', true) => {
                        if *kind == StringControlKind::Dcs {
                            dispatch_dcs(content, &mut |action| emit(ParserOutput::Action(action)));
                        }
                        self.state = ParserState::Ground;
                    }
                    (_, true) => {
                        if *kind == StringControlKind::Dcs {
                            dispatch_dcs(content, &mut |action| emit(ParserOutput::Action(action)));
                        }
                        self.state = ParserState::Escape;
                        advance = false;
                    }
                    (0x1b, false) => {
                        if content.len() >= MAX_STRING_PAYLOAD_BYTES {
                            self.state = ParserState::IgnoringStringControl { escape_seen: true };
                        } else {
                            *escape_seen = true;
                        }
                    }
                    (_, false) => {
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
                    (_, true) => {
                        self.state = ParserState::Escape;
                        advance = false;
                    }
                    (0x1b, false) => *escape_seen = true,
                    (_, false) => *escape_seen = false,
                },
            }
            if advance {
                index += 1;
            }
        }

        if matches!(self.state, ParserState::Ground) {
            self.flush_print_buffer(&mut emit, true);
        }
    }

    fn flush_print_buffer(
        &mut self,
        emit: &mut impl FnMut(ParserOutput<'_>),
        preserve_incomplete: bool,
    ) {
        let mut offset = 0;
        while offset < self.print_buffer.len() {
            match std::str::from_utf8(&self.print_buffer[offset..]) {
                Ok(text) => {
                    emit(ParserOutput::Text(text));
                    offset = self.print_buffer.len();
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    if valid_up_to > 0 {
                        let valid =
                            std::str::from_utf8(&self.print_buffer[offset..offset + valid_up_to])
                                .expect("valid_up_to always names valid UTF-8");
                        emit(ParserOutput::Text(valid));
                        offset += valid_up_to;
                    }

                    match error.error_len() {
                        Some(invalid_len) => {
                            emit(ParserOutput::Text("\u{fffd}"));
                            offset += invalid_len;
                        }
                        None if preserve_incomplete => break,
                        None => {
                            emit(ParserOutput::Text("\u{fffd}"));
                            offset = self.print_buffer.len();
                        }
                    }
                }
            }
        }
        if offset > 0 {
            self.print_buffer.drain(..offset);
        }
    }
}

fn execute_c0(byte: u8, emit: &mut impl FnMut(ParserOutput<'_>)) -> bool {
    let action = match byte {
        b'\r' => Some(TerminalAction::CarriageReturn),
        b'\n' | 0x0b | 0x0c => Some(TerminalAction::LineFeed),
        0x08 => Some(TerminalAction::Backspace),
        b'\t' => Some(TerminalAction::Tab),
        0x00..=0x1f | 0x7f => None,
        _ => return false,
    };
    if let Some(action) = action {
        emit(ParserOutput::Action(action));
    }
    true
}

fn dispatch_escape_intermediate(
    intermediates: &EscapeIntermediates,
    final_byte: u8,
    emit: &mut impl FnMut(TerminalAction),
) {
    if intermediates.as_slice() == b"#" && final_byte == b'8' {
        emit(TerminalAction::ScreenAlignmentTest);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
// Keep CSI storage inline: boxing this variant would allocate for every sequence.
#[allow(clippy::large_enum_variant)]
enum ParserState {
    Ground,
    Escape,
    EscapeIntermediate(EscapeIntermediates),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct EscapeIntermediates {
    bytes: [u8; MAX_CSI_INTERMEDIATES],
    len: u8,
}

impl EscapeIntermediates {
    fn push(&mut self, byte: u8) -> bool {
        let index = usize::from(self.len);
        if index >= self.bytes.len() {
            return false;
        }
        self.bytes[index] = byte;
        self.len += 1;
        true
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Param {
    values: [u16; MAX_CSI_SUBPARAMS],
    len: u8,
}

impl Default for Param {
    fn default() -> Self {
        Self {
            values: [0; MAX_CSI_SUBPARAMS],
            len: 1,
        }
    }
}

impl Param {
    fn push_digit(&mut self, digit: u8) {
        let index = usize::from(self.len.saturating_sub(1));
        self.values[index] = self.values[index]
            .saturating_mul(10)
            .saturating_add(u16::from(digit - b'0'));
    }

    fn push_subparam(&mut self) -> bool {
        let index = usize::from(self.len);
        if index >= self.values.len() {
            return false;
        }
        self.len += 1;
        true
    }

    fn value(&self) -> u16 {
        self.values[0]
    }

    fn as_slice(&self) -> &[u16] {
        &self.values[..usize::from(self.len)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CsiState {
    private_markers: [u8; MAX_CSI_PRIVATE_MARKERS],
    private_len: u8,
    params: [Param; MAX_CSI_PARAMS],
    param_count: u8,
    intermediates: [u8; MAX_CSI_INTERMEDIATES],
    intermediate_len: u8,
    final_byte: Option<u8>,
}

impl Default for CsiState {
    fn default() -> Self {
        Self {
            private_markers: [0; MAX_CSI_PRIVATE_MARKERS],
            private_len: 0,
            params: [Param::default(); MAX_CSI_PARAMS],
            param_count: 0,
            intermediates: [0; MAX_CSI_INTERMEDIATES],
            intermediate_len: 0,
            final_byte: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CsiConsume {
    Continue,
    Dispatch,
    Ignore,
}

impl CsiState {
    fn consume(&mut self, byte: u8) -> CsiConsume {
        match byte {
            0x3c..=0x3f if self.param_count == 0 && self.intermediate_len == 0 => {
                let index = usize::from(self.private_len);
                if index >= self.private_markers.len() {
                    return CsiConsume::Ignore;
                }
                self.private_markers[index] = byte;
                self.private_len += 1;
            }
            b'0'..=b'9' if self.intermediate_len == 0 => {
                if !self.ensure_param() {
                    return CsiConsume::Ignore;
                }
                self.params[usize::from(self.param_count - 1)].push_digit(byte);
            }
            b':' if self.intermediate_len == 0 => {
                if !self.ensure_param()
                    || !self.params[usize::from(self.param_count - 1)].push_subparam()
                {
                    return CsiConsume::Ignore;
                }
            }
            b';' if self.intermediate_len == 0 => {
                if !self.ensure_param() || !self.push_param() {
                    return CsiConsume::Ignore;
                }
            }
            0x20..=0x2f => {
                let index = usize::from(self.intermediate_len);
                if index >= self.intermediates.len() {
                    return CsiConsume::Ignore;
                }
                self.intermediates[index] = byte;
                self.intermediate_len += 1;
            }
            0x40..=0x7e => {
                self.final_byte = Some(byte);
                return CsiConsume::Dispatch;
            }
            _ => return CsiConsume::Ignore,
        }
        CsiConsume::Continue
    }

    fn ensure_param(&mut self) -> bool {
        if self.param_count == 0 {
            self.push_param()
        } else {
            true
        }
    }

    fn push_param(&mut self) -> bool {
        let index = usize::from(self.param_count);
        if index >= self.params.len() {
            return false;
        }
        self.params[index] = Param::default();
        self.param_count += 1;
        true
    }

    fn params(&self) -> &[Param] {
        &self.params[..usize::from(self.param_count)]
    }

    fn param_or(&self, index: usize, default: u16) -> u16 {
        self.params()
            .get(index)
            .map(Param::value)
            .filter(|value| *value != 0)
            .unwrap_or(default)
    }

    fn has_private(&self, marker: u8) -> bool {
        self.private_markers[..usize::from(self.private_len)] == [marker]
    }

    fn has_intermediates(&self, expected: &[u8]) -> bool {
        self.intermediates[..usize::from(self.intermediate_len)] == *expected
    }

    fn plain(&self) -> bool {
        self.private_len == 0 && self.intermediate_len == 0
    }
}

fn dispatch_csi(csi: &CsiState, emit: &mut impl FnMut(TerminalAction)) {
    let Some(final_byte) = csi.final_byte else {
        return;
    };

    if csi.has_private(b'>') && csi.has_intermediates(b"") {
        match final_byte {
            b'c' => emit(TerminalAction::SecondaryDeviceAttributes),
            b'q' => emit(TerminalAction::TerminalVersion),
            b'u' => emit(TerminalAction::PushKittyKeyboardFlags(csi.param_or(0, 0))),
            _ => {}
        }
        return;
    }
    if csi.has_private(b'<') && csi.has_intermediates(b"") && final_byte == b'u' {
        emit(TerminalAction::PopKittyKeyboardFlags(csi.param_or(0, 1)));
        return;
    }
    if csi.has_private(b'=') && csi.has_intermediates(b"") && final_byte == b'u' {
        let mode = match csi.param_or(1, 1) {
            1 => KittyKeyboardMode::Set,
            2 => KittyKeyboardMode::Add,
            3 => KittyKeyboardMode::Remove,
            _ => return,
        };
        emit(TerminalAction::SetKittyKeyboardFlags {
            flags: csi.param_or(0, 0),
            mode,
        });
        return;
    }
    if csi.has_private(b'?') && csi.has_intermediates(b"$") && final_byte == b'p' {
        for param in csi.params() {
            emit(TerminalAction::RequestMode {
                private: true,
                mode: param.value(),
            });
        }
        return;
    }
    if csi.private_len == 0 && csi.has_intermediates(b"$") && final_byte == b'p' {
        for param in csi.params() {
            emit(TerminalAction::RequestMode {
                private: false,
                mode: param.value(),
            });
        }
        return;
    }
    if csi.private_len == 0 && csi.has_intermediates(b"!") && final_byte == b'p' {
        emit(TerminalAction::SoftReset);
        return;
    }
    if csi.has_private(b'?') && csi.has_intermediates(b"") && final_byte == b'u' {
        emit(TerminalAction::KittyKeyboardStatus);
        return;
    }
    if csi.private_len == 0 && csi.has_intermediates(b" ") && final_byte == b'q' {
        cursor_shape_action(csi, emit);
        return;
    }
    if !csi.plain() && !csi.has_private(b'?') {
        return;
    }

    let action = match final_byte {
        b'@' => Some(TerminalAction::InsertChars(csi.param_or(0, 1))),
        b'A' => Some(move_cursor(csi, CursorDirection::Up)),
        b'B' => Some(move_cursor(csi, CursorDirection::Down)),
        b'C' | b'a' => Some(move_cursor(csi, CursorDirection::Forward)),
        b'D' => Some(move_cursor(csi, CursorDirection::Back)),
        b'E' => Some(move_cursor(csi, CursorDirection::NextLine)),
        b'F' => Some(move_cursor(csi, CursorDirection::PreviousLine)),
        b'G' | b'`' => Some(TerminalAction::SetCursorColumn(csi.param_or(0, 1))),
        b'd' => Some(TerminalAction::SetCursorRow(csi.param_or(0, 1))),
        b'e' => Some(move_cursor(csi, CursorDirection::Down)),
        b'H' | b'f' => Some(TerminalAction::SetCursorPosition {
            row: csi.param_or(0, 1),
            col: csi.param_or(1, 1),
        }),
        b'J' => Some(TerminalAction::ClearScreen(clear_mode(csi.param_or(0, 0)))),
        b'K' => Some(TerminalAction::ClearLine(clear_mode(csi.param_or(0, 0)))),
        b'L' => Some(TerminalAction::InsertLines(csi.param_or(0, 1))),
        b'M' => Some(TerminalAction::DeleteLines(csi.param_or(0, 1))),
        b'P' => Some(TerminalAction::DeleteChars(csi.param_or(0, 1))),
        b'S' => Some(TerminalAction::ScrollUp(csi.param_or(0, 1))),
        b'T' => Some(TerminalAction::ScrollDown(csi.param_or(0, 1))),
        b'X' => Some(TerminalAction::EraseChars(csi.param_or(0, 1))),
        b'Z' => Some(TerminalAction::BackTab(csi.param_or(0, 1))),
        b'b' => Some(TerminalAction::RepeatLastPrinted(csi.param_or(0, 1))),
        b'c' if csi.plain() => Some(TerminalAction::PrimaryDeviceAttributes),
        b'n' if csi.has_private(b'?') => Some(TerminalAction::PrivateDeviceStatusReport(
            csi.param_or(0, 0),
        )),
        b'n' => Some(TerminalAction::DeviceStatusReport(csi.param_or(0, 0))),
        b's' if csi.plain() => Some(TerminalAction::SaveCursor),
        b'u' if csi.plain() => Some(TerminalAction::RestoreCursor),
        _ => None,
    };
    if let Some(action) = action {
        emit(action);
        return;
    }

    match final_byte {
        b'I' => {
            for _ in 0..csi.param_or(0, 1) {
                emit(TerminalAction::Tab);
            }
        }
        b'g' => tab_clear_action(csi, emit),
        b'm' => parse_sgr(csi, emit),
        b'h' | b'l' => mode_actions(csi, final_byte == b'h', emit),
        b'r' => scroll_region_action(csi, emit),
        _ => {}
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

fn parse_sgr(csi: &CsiState, emit: &mut impl FnMut(TerminalAction)) {
    let params = csi.params();
    if params.is_empty() {
        emit(TerminalAction::SetGraphicRendition(GraphicRendition::Reset));
        return;
    }
    let mut index = 0;

    while index < params.len() {
        let param = &params[index];
        let value = param.value();
        let rendition = match value {
            0 => Some(GraphicRendition::Reset),
            1 => Some(GraphicRendition::Bold),
            2 => Some(GraphicRendition::Dim),
            3 => Some(GraphicRendition::Italic),
            4 if param.as_slice().len() > 1 => Some(GraphicRendition::UnderlineStyle(
                underline_style(param.as_slice()[1]),
            )),
            4 => Some(GraphicRendition::Underline),
            5 | 6 => Some(GraphicRendition::Blink),
            7 => Some(GraphicRendition::Inverse),
            8 => Some(GraphicRendition::Hidden),
            9 => Some(GraphicRendition::Strikethrough),
            21 => Some(GraphicRendition::UnderlineStyle(
                term_core::UnderlineStyle::Double,
            )),
            22 => Some(GraphicRendition::NormalIntensity),
            23 => Some(GraphicRendition::NoItalic),
            24 => Some(GraphicRendition::NoUnderline),
            25 => Some(GraphicRendition::NoBlink),
            27 => Some(GraphicRendition::NoInverse),
            28 => Some(GraphicRendition::NoHidden),
            29 => Some(GraphicRendition::NoStrikethrough),
            30..=37 => Some(GraphicRendition::Foreground(Color::Indexed(
                (value - 30) as u8,
            ))),
            40..=47 => Some(GraphicRendition::Background(Color::Indexed(
                (value - 40) as u8,
            ))),
            90..=97 => Some(GraphicRendition::Foreground(Color::Indexed(
                (value - 90 + 8) as u8,
            ))),
            100..=107 => Some(GraphicRendition::Background(Color::Indexed(
                (value - 100 + 8) as u8,
            ))),
            38 | 48 | 58 => {
                if param.as_slice().len() > 1 {
                    if let Some(color) = parse_colon_color(param.as_slice()) {
                        emit(TerminalAction::SetGraphicRendition(color_rendition(
                            value, color,
                        )));
                    }
                } else {
                    let (color, consumed) = parse_legacy_color(params, index);
                    if let Some(color) = color {
                        emit(TerminalAction::SetGraphicRendition(color_rendition(
                            value, color,
                        )));
                    }
                    index += consumed;
                }
                None
            }
            39 => Some(GraphicRendition::DefaultForeground),
            49 => Some(GraphicRendition::DefaultBackground),
            53 => Some(GraphicRendition::Overline),
            55 => Some(GraphicRendition::NoOverline),
            59 => Some(GraphicRendition::DefaultUnderlineColor),
            _ => None,
        };
        if let Some(rendition) = rendition {
            emit(TerminalAction::SetGraphicRendition(rendition));
        }
        index += 1;
    }
}

fn underline_style(value: u16) -> term_core::UnderlineStyle {
    match value {
        0 => term_core::UnderlineStyle::None,
        2 => term_core::UnderlineStyle::Double,
        3 => term_core::UnderlineStyle::Curly,
        4 => term_core::UnderlineStyle::Dotted,
        5 => term_core::UnderlineStyle::Dashed,
        _ => term_core::UnderlineStyle::Single,
    }
}

fn parse_colon_color(values: &[u16]) -> Option<Color> {
    match values.get(1).copied()? {
        5 => u8::try_from(*values.get(2)?).ok().map(Color::Indexed),
        2 if values.len() >= 5 => {
            let rgb = &values[values.len() - 3..];
            Some(Color::Rgb {
                red: u8::try_from(rgb[0]).ok()?,
                green: u8::try_from(rgb[1]).ok()?,
                blue: u8::try_from(rgb[2]).ok()?,
            })
        }
        _ => None,
    }
}

fn parse_legacy_color(params: &[Param], index: usize) -> (Option<Color>, usize) {
    match params.get(index + 1).map(Param::value) {
        Some(5) => {
            let color = params
                .get(index + 2)
                .and_then(|param| u8::try_from(param.value()).ok())
                .map(Color::Indexed);
            (color, 2.min(params.len().saturating_sub(index + 1)))
        }
        Some(2) => {
            let color = (|| {
                Some(Color::Rgb {
                    red: u8::try_from(params.get(index + 2)?.value()).ok()?,
                    green: u8::try_from(params.get(index + 3)?.value()).ok()?,
                    blue: u8::try_from(params.get(index + 4)?.value()).ok()?,
                })
            })();
            (color, 4.min(params.len().saturating_sub(index + 1)))
        }
        Some(_) => (None, 1),
        None => (None, 0),
    }
}

fn color_rendition(target: u16, color: Color) -> GraphicRendition {
    match target {
        38 => GraphicRendition::Foreground(color),
        48 => GraphicRendition::Background(color),
        _ => GraphicRendition::UnderlineColor(color),
    }
}

fn mode_actions(csi: &CsiState, enabled: bool, emit: &mut impl FnMut(TerminalAction)) {
    if csi.private_len == 0 {
        for param in csi.params() {
            match param.value() {
                4 => emit(TerminalAction::SetMode {
                    mode: TerminalMode::Insert,
                    enabled,
                }),
                20 => emit(TerminalAction::SetMode {
                    mode: TerminalMode::LineFeedNewLine,
                    enabled,
                }),
                _ => {}
            }
        }
        return;
    }
    if !csi.has_private(b'?') {
        return;
    }

    for param in csi.params() {
        let mode = param.value();
        match mode {
            1 => emit(TerminalAction::SetMode {
                mode: TerminalMode::ApplicationCursorKeys,
                enabled,
            }),
            6 => emit(TerminalAction::SetMode {
                mode: TerminalMode::Origin,
                enabled,
            }),
            7 => emit(TerminalAction::SetMode {
                mode: TerminalMode::AutoWrap,
                enabled,
            }),
            12 => emit(TerminalAction::SetMode {
                mode: TerminalMode::CursorBlinking,
                enabled,
            }),
            25 => emit(TerminalAction::SetCursorVisible(enabled)),
            66 => emit(TerminalAction::SetMode {
                mode: TerminalMode::ApplicationKeypad,
                enabled,
            }),
            1000 => emit(TerminalAction::SetMode {
                mode: TerminalMode::MouseReporting,
                enabled,
            }),
            1002 => emit(TerminalAction::SetMode {
                mode: TerminalMode::MouseCellMotion,
                enabled,
            }),
            1003 => emit(TerminalAction::SetMode {
                mode: TerminalMode::MouseAllMotion,
                enabled,
            }),
            1004 => emit(TerminalAction::SetMode {
                mode: TerminalMode::FocusEvents,
                enabled,
            }),
            1005 => emit(TerminalAction::SetMode {
                mode: TerminalMode::Utf8Mouse,
                enabled,
            }),
            1006 => emit(TerminalAction::SetMode {
                mode: TerminalMode::SgrMouse,
                enabled,
            }),
            1015 => emit(TerminalAction::SetMode {
                mode: TerminalMode::UrxvtMouse,
                enabled,
            }),
            // Windows win32-input-mode. Tracked so key encoding can defer to
            // the Windows-native input contract while an application has it on.
            9001 => emit(TerminalAction::SetMode {
                mode: TerminalMode::Win32InputMode,
                enabled,
            }),
            1048 => {
                if enabled {
                    emit(TerminalAction::SaveCursor);
                } else {
                    emit(TerminalAction::RestoreCursor);
                }
            }
            47 | 1047 => emit(TerminalAction::SetMode {
                mode: TerminalMode::AlternateScreen,
                enabled,
            }),
            1049 => {
                if enabled {
                    emit(TerminalAction::SaveCursor);
                    emit(TerminalAction::SetMode {
                        mode: TerminalMode::AlternateScreen,
                        enabled: true,
                    });
                } else {
                    emit(TerminalAction::SetMode {
                        mode: TerminalMode::AlternateScreen,
                        enabled: false,
                    });
                    emit(TerminalAction::RestoreCursor);
                }
            }
            2004 => emit(TerminalAction::SetMode {
                mode: TerminalMode::BracketedPaste,
                enabled,
            }),
            2026 => emit(TerminalAction::SetMode {
                mode: TerminalMode::SynchronizedOutput,
                enabled,
            }),
            _ => {}
        }
    }
}

fn tab_clear_action(csi: &CsiState, emit: &mut impl FnMut(TerminalAction)) {
    emit(match csi.param_or(0, 0) {
        3 => TerminalAction::ClearAllTabStops,
        _ => TerminalAction::ClearTabStop,
    });
}

fn scroll_region_action(csi: &CsiState, emit: &mut impl FnMut(TerminalAction)) {
    if csi.params().is_empty() {
        emit(TerminalAction::ResetScrollRegion);
        return;
    }

    emit(TerminalAction::SetScrollRegion {
        top: csi.param_or(0, 1),
        bottom: csi.param_or(1, u16::MAX),
    });
}

fn cursor_shape_action(csi: &CsiState, emit: &mut impl FnMut(TerminalAction)) {
    let shape = match csi.param_or(0, 1) {
        3 | 4 => CursorShape::Underline,
        5 | 6 => CursorShape::Beam,
        _ => CursorShape::Block,
    };

    emit(TerminalAction::SetCursorShape(shape));
}

fn dispatch_osc(content: &[u8], emit: &mut impl FnMut(TerminalAction)) {
    let text = String::from_utf8_lossy(content);
    let Some((command, payload)) = text.split_once(';') else {
        return;
    };

    match command {
        "0" | "2" => emit(TerminalAction::SetTitle(payload.to_owned())),
        "8" => emit(osc8_action(payload)),
        "10" | "11" if payload == "?" => {
            emit(TerminalAction::RequestDynamicColor(
                command.parse().expect("matched numeric OSC command"),
            ));
        }
        "52" => {
            if let Some(action) = osc52_action(payload) {
                emit(action);
            }
        }
        _ => {}
    }
}

fn osc8_action(payload: &str) -> TerminalAction {
    let (params, uri) = payload.split_once(';').unwrap_or(("", ""));
    let id = params
        .split(':')
        .find_map(|part| part.strip_prefix("id="))
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    TerminalAction::SetHyperlink {
        id,
        uri: (!uri.is_empty()).then(|| uri.to_owned()),
    }
}

fn osc52_action(payload: &str) -> Option<TerminalAction> {
    let (selector, payload_base64) = payload.split_once(';')?;
    if payload_base64 == "?" {
        return None;
    }
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

fn dispatch_dcs(content: &[u8], emit: &mut impl FnMut(TerminalAction)) {
    if let Some(request) = content.strip_prefix(b"$q") {
        emit(TerminalAction::RequestStatusString(
            String::from_utf8_lossy(request).into_owned(),
        ));
        return;
    }
    if let Some(payload) = content.strip_prefix(b"tmux;") {
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
        Parser::default().parse_with_dyn(&unescaped, emit);
    }
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
            assert!(std::str::from_utf8(text.as_bytes()).is_ok());
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
    fn streaming_parser_matches_collected_actions() {
        let input = b"plain \xe7\x95\x8c\x1b[31mred\x1b[0m\r\n\x1b]2;Panea\x07\x1bP$qm\x1b\\";
        let expected = Parser::default().parse(input);
        let mut actual = Vec::new();
        Parser::default().parse_with(input, |action| actual.push(action));

        assert_eq!(actual, expected);
    }

    #[test]
    fn streaming_parser_keeps_printable_ascii_as_one_span() {
        let mut output = Vec::new();

        Parser::default().parse_outputs(b"panea\r", |item| match item {
            ParserOutput::Text(text) => output.push(format!("text:{text}")),
            ParserOutput::Action(TerminalAction::CarriageReturn) => {
                output.push("action:cr".to_owned());
            }
            ParserOutput::Action(action) => output.push(format!("action:{action:?}")),
        });

        assert_eq!(output, ["text:panea", "action:cr"]);
    }

    #[test]
    fn borrowed_mode_view_tracks_parser_updates() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(80, 24));
        assert!(
            !terminal
                .modes_ref()
                .contains(&TerminalMode::ApplicationCursorKeys)
        );

        terminal.apply_bytes(b"\x1b[?1h").expect("set mode");

        assert!(
            terminal
                .modes_ref()
                .contains(&TerminalMode::ApplicationCursorKeys)
        );
    }

    #[test]
    fn borrowed_scrollback_view_tracks_terminal_output() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(4, 1));
        terminal.apply_bytes(b"one\r\ntwo").unwrap();

        let scrollback = terminal.scrollback_lines();
        assert_eq!(scrollback.len(), 1);
        assert_eq!(scrollback[0].raw_text(), "one");
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
            b"\x1b[?62;22c\x1b[?3;4R"
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
    fn csi_private_markers_and_intermediates_never_leak_final_bytes() {
        let mut terminal = terminal(
            TerminalSize::new(80, 24),
            b"\x1b[>c\x1b[>q\x1b[?25$p\x1b[!p\x1b[\"q\x1b['wplain",
        );

        assert_eq!(line_text(&terminal, 0), "plain");
        assert_eq!(
            terminal.state_mut().take_pending_output(),
            b"\x1b[>1;10;0c\x1bP>|Panea 0.1\x1b\\\x1b[?25;1$y"
        );
    }

    #[test]
    fn kitty_keyboard_query_does_not_restore_cursor() {
        let mut terminal = terminal(TerminalSize::new(20, 4), b"A\x1b7\x1b[3;5H\x1b[?uX");

        assert_eq!(terminal.state().cell(2, 4).unwrap().text, "X");
        assert_eq!(terminal.cursor_state().position, GridPosition::new(2, 5));
        assert_eq!(terminal.state_mut().take_pending_output(), b"\x1b[?0u");
    }

    #[test]
    fn kitty_keyboard_flags_support_set_push_pop_and_query() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(20, 4));

        assert_eq!(
            terminal
                .apply_bytes_and_take_pending_output(b"\x1b[=1u\x1b[?u")
                .unwrap(),
            b"\x1b[?1u"
        );
        assert_eq!(
            terminal
                .apply_bytes_and_take_pending_output(b"\x1b[>3u\x1b[?u")
                .unwrap(),
            b"\x1b[?3u"
        );
        assert_eq!(
            terminal
                .apply_bytes_and_take_pending_output(b"\x1b[<u\x1b[?u")
                .unwrap(),
            b"\x1b[?1u"
        );
        assert_eq!(
            terminal
                .apply_bytes_and_take_pending_output(b"\x1b[=2;2u\x1b[?u")
                .unwrap(),
            b"\x1b[?3u"
        );
        assert_eq!(
            terminal
                .apply_bytes_and_take_pending_output(b"\x1b[=1;3u\x1b[?u")
                .unwrap(),
            b"\x1b[?2u"
        );
    }

    #[test]
    fn csi_state_is_fixed_storage_without_drop_or_heap_ownership() {
        assert!(!std::mem::needs_drop::<CsiState>());
        assert!(std::mem::size_of::<CsiState>() <= 640);
    }

    #[test]
    fn decstr_soft_reset_preserves_text_and_restores_protocol_defaults() {
        let terminal = terminal(
            TerminalSize::new(12, 3),
            b"text\x1b[31;1m\x1b[?1;6;25l\x1b[2;3r\x1b[!pX",
        );

        assert_eq!(line_text(&terminal, 0), "Xext");
        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 1));
        assert!(terminal.cursor_state().visible);
        assert_eq!(
            terminal.state().cell(0, 0).unwrap().attributes,
            CellAttributes::default()
        );
        assert_eq!(
            terminal.modes(),
            std::collections::BTreeSet::from([TerminalMode::AutoWrap])
        );
    }

    #[test]
    fn colon_sgr_subparameters_preserve_styles_and_extended_colors() {
        let terminal = terminal(
            TerminalSize::new(12, 2),
            b"\x1b[4:3;58:2::255:0:0;38:2::1:2:3;48:5:250mA",
        );
        let attrs = terminal.state().cell(0, 0).unwrap().attributes;

        assert_eq!(attrs.underline_style, term_core::UnderlineStyle::Curly);
        assert_eq!(
            attrs.underline_color,
            Some(Color::Rgb {
                red: 255,
                green: 0,
                blue: 0,
            })
        );
        assert_eq!(
            attrs.foreground,
            Some(Color::Rgb {
                red: 1,
                green: 2,
                blue: 3,
            })
        );
        assert_eq!(attrs.background, Some(Color::Indexed(250)));
        assert!(!attrs.dim);
    }

    #[test]
    fn controls_inside_escape_and_csi_follow_vt_cancellation_rules() {
        let terminal = terminal(
            TerminalSize::new(20, 3),
            b"abc\x1b[1;\x1b[2JX\x1b\x1b[31mR\x1b[12\x18mY\x1b[2\rCZ",
        );

        assert_eq!(line_text(&terminal, 0), "  ZXRmY");
        assert_eq!(
            terminal.state().cell(0, 4).unwrap().attributes.foreground,
            Some(Color::Indexed(1))
        );
    }

    #[test]
    fn escape_intermediates_are_consumed_and_decaln_fills_the_screen() {
        let terminal = terminal(TerminalSize::new(4, 2), b"\x1b#8\x1b%G\x1b F");

        assert_eq!(line_text(&terminal, 0), "EEEE");
        assert_eq!(line_text(&terminal, 1), "EEEE");
        assert_eq!(terminal.cursor_state().position, GridPosition::new(0, 0));
    }

    #[test]
    fn unterminated_control_strings_abort_at_a_new_escape_sequence() {
        let terminal = terminal(
            TerminalSize::new(20, 2),
            b"\x1b]0;broken\x1b[31mred\x1bPbroken\x1b[0mplain",
        );

        assert_eq!(line_text(&terminal, 0), "redplain");
        assert_eq!(
            terminal.state().cell(0, 0).unwrap().attributes.foreground,
            Some(Color::Indexed(1))
        );
        assert_eq!(
            terminal.state().cell(0, 3).unwrap().attributes,
            CellAttributes::default()
        );
    }

    #[test]
    fn csi_parameters_saturate_and_invalid_extended_colors_are_consumed() {
        let terminal = terminal(
            TerminalSize::new(12, 3),
            b"\x1b[99999BX\x1b[38;5;300mA\x1b[38;2;1;2mB",
        );

        assert_eq!(terminal.cursor_state().position.row, 2);
        assert_eq!(terminal.state().cell(2, 0).unwrap().text, "X");
        assert_eq!(
            terminal.state().cell(2, 1).unwrap().attributes.foreground,
            None
        );
        assert_eq!(
            terminal.state().cell(2, 2).unwrap().attributes,
            CellAttributes::default()
        );
    }

    #[test]
    fn modern_modes_sgr_queries_and_hyperlinks_are_supported() {
        let mut terminal = terminal(
            TerminalSize::new(20, 3),
            b"\x1b[?47;1005;1015;2026;12h\x1b[20h\x1b[5;8;21;53mA\
              \x1b]8;id=docs;https://example.test\x07L\x1b]8;;\x07\
              \x1b]52;c;?\x07\x1b]10;?\x07\x1b]11;?\x07\x1b[c\x1bZ\
              \x1bP$qm\x1b\\",
        );

        let modes = terminal.modes();
        assert!(modes.contains(&TerminalMode::AlternateScreen));
        assert!(modes.contains(&TerminalMode::Utf8Mouse));
        assert!(modes.contains(&TerminalMode::UrxvtMouse));
        assert!(modes.contains(&TerminalMode::SynchronizedOutput));
        assert!(modes.contains(&TerminalMode::CursorBlinking));
        assert!(modes.contains(&TerminalMode::LineFeedNewLine));
        let attrs = terminal.state().cell(0, 0).unwrap().attributes;
        assert!(attrs.blink);
        assert!(attrs.hidden);
        assert!(attrs.overline);
        assert_eq!(attrs.underline_style, term_core::UnderlineStyle::Double);
        assert!(terminal.state().cell(0, 1).unwrap().hyperlink_id.is_some());
        assert!(terminal.state().pending_clipboard_requests().is_empty());
        assert_eq!(
            terminal.state_mut().take_pending_output(),
            b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\\x1b]11;rgb:0000/0000/0000\x1b\\\x1b[?62;22c\x1b[?62;22c\x1bP1$r0m\x1b\\"
        );
    }

    #[test]
    fn cursor_blink_mode_updates_the_exposed_cursor_state() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(8, 1));
        terminal.apply_bytes(b"\x1b[?12l").unwrap();
        assert!(!terminal.cursor_state().blinking);

        terminal.apply_bytes(b"\x1b[?12h").unwrap();
        assert!(terminal.cursor_state().blinking);
    }

    #[test]
    fn dynamic_color_queries_and_reused_hyperlink_ids_track_current_state() {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(8, 1));
        terminal
            .state_mut()
            .set_dynamic_colors([0x12, 0x34, 0x56], [0xab, 0xcd, 0xef]);
        terminal
            .apply_bytes(
                b"\x1b]8;id=link;https://old.test\x07A\x1b]8;;\x07\
                  \x1b]8;id=link;https://new.test\x07B\x1b]8;;\x07\
                  \x1b]10;?\x07\x1b]11;?\x07",
            )
            .unwrap();

        let first = terminal.state().cell(0, 0).unwrap().hyperlink_id.unwrap();
        let second = terminal.state().cell(0, 1).unwrap().hyperlink_id.unwrap();
        assert_eq!(first, second);
        assert_eq!(
            terminal.state().hyperlink_uri(second),
            Some("https://new.test")
        );
        assert_eq!(
            terminal.state_mut().take_pending_output(),
            b"\x1b]10;rgb:1212/3434/5656\x1b\\\x1b]11;rgb:abab/cdcd/efef\x1b\\"
        );
    }

    #[test]
    fn invalid_utf8_emits_one_replacement_per_invalid_sequence() {
        let terminal = terminal(
            TerminalSize::new(12, 2),
            &[b'a', 0xf0, 0x28, 0x8c, 0x28, b'b'],
        );

        assert_eq!(line_text(&terminal, 0), "a\u{fffd}(\u{fffd}(b");
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
    fn invalid_utf8_is_replaced_without_corrupting_later_text() {
        let terminal = terminal(TerminalSize::new(8, 2), &[b'a', 0xff, b'b']);

        assert_eq!(line_text(&terminal, 0), "a\u{fffd}b");
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
        for _ in 0..(MAX_CSI_PARAMS + 16) {
            payload.extend_from_slice(b"1;");
        }
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
