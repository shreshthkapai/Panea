#![allow(dead_code)]

use term_core::{
    CellAttributes, ClearMode, Color, CursorDirection, GraphicRendition, GridPosition, Line,
    Selection, TerminalAction, TerminalCore, TerminalMode, TerminalSize, TerminalState,
};
use term_parser::TerminalEmulator;

const MAX_CELL_GRAPHEME_SCALARS: usize = 16;
const MAX_PENDING_OUTPUT_PER_STEP: usize = 64 * 1024;

pub fn assert_terminal_invariants(terminal: &TerminalState) {
    let grid = terminal.grid();
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
    assert!(terminal.scrollback_lines().len() <= terminal.scrollback_limit());

    assert_eq!(terminal.visible_grid().cells.len(), rows * cols);

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
        assert!(cell.text.chars().count() <= MAX_CELL_GRAPHEME_SCALARS);
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

pub fn assert_pending_output_invariants(terminal: &mut TerminalState) {
    let output = terminal.take_pending_output();
    assert!(output.len() <= MAX_PENDING_OUTPUT_PER_STEP);
    assert!(std::str::from_utf8(&output).is_ok());
}

pub fn assert_alt_screen_saved_cursor_round_trip(data: &[u8]) {
    let row = u16::from(data.first().copied().unwrap_or(2) % 6) + 1;
    let col = u16::from(data.get(1).copied().unwrap_or(3) % 20) + 1;
    let mut terminal = TerminalEmulator::new(TerminalSize::new(24, 8));
    let sequence = format!(
        "\x1b[{row};{col}H\x1b7\x1b[?1049h\x1b[8;24H\x1b[?1049l\x1b8"
    );
    terminal.apply_bytes(sequence.as_bytes()).unwrap();
    let cursor = terminal.cursor_state().position;
    assert_eq!(cursor.row, i64::from(row - 1));
    assert_eq!(cursor.col, col - 1);
    assert_pending_output_invariants(terminal.state_mut());
}

pub fn fuzz_char(byte: u8) -> char {
    match byte % 20 {
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
        17 => '\u{1f1fa}',
        18 => '\u{1f1f8}',
        _ => 'x',
    }
}

pub fn apply_grid_ops(data: &[u8]) {
    let mut terminal = TerminalState::new(TerminalSize::new(16, 6));
    let mut scroll_region = None;

    for chunk in data.chunks(9) {
        let tag = byte(chunk, 0);
        let a = u16_from(chunk, 1);
        let b = u16_from(chunk, 3);
        let c = u16_from(chunk, 5);
        let d = u16_from(chunk, 7);
        apply_grid_op(&mut terminal, tag, a, b, c, d);
        update_scroll_region(&terminal, tag, a, c, &mut scroll_region);
        assert_terminal_invariants(&terminal);
        assert_cursor_within_origin_margins(&terminal, scroll_region);
        assert_pending_output_invariants(&mut terminal);
    }

    assert_alt_screen_saved_cursor_round_trip(data);
}

pub fn apply_grid_op(terminal: &mut TerminalState, tag: u8, a: u16, b: u16, c: u16, d: u16) {
    let size = terminal.grid().size;
    let row = (a % size.rows.max(1)) + 1;
    let col = (b % size.cols.max(1)) + 1;
    let count = (c % 16) + 1;

    match tag % 39 {
        0 => terminal
            .apply_action(TerminalAction::Print(fuzz_char((a ^ b ^ c ^ d) as u8)))
            .unwrap(),
        1 => terminal
            .apply_action(TerminalAction::CarriageReturn)
            .unwrap(),
        2 => terminal.apply_action(TerminalAction::LineFeed).unwrap(),
        3 => terminal.apply_action(TerminalAction::Backspace).unwrap(),
        4 => terminal.apply_action(TerminalAction::Tab).unwrap(),
        5 => terminal
            .apply_action(TerminalAction::MoveCursor {
                direction: CursorDirection::Up,
                count,
            })
            .unwrap(),
        6 => terminal
            .apply_action(TerminalAction::MoveCursor {
                direction: CursorDirection::Down,
                count,
            })
            .unwrap(),
        7 => terminal
            .apply_action(TerminalAction::MoveCursor {
                direction: CursorDirection::Forward,
                count,
            })
            .unwrap(),
        8 => terminal
            .apply_action(TerminalAction::MoveCursor {
                direction: CursorDirection::Back,
                count,
            })
            .unwrap(),
        9 => terminal
            .apply_action(TerminalAction::SetCursorPosition { row, col })
            .unwrap(),
        10 => terminal
            .apply_action(TerminalAction::SetCursorColumn(col))
            .unwrap(),
        11 => terminal
            .apply_action(TerminalAction::ClearScreen(clear_mode(d)))
            .unwrap(),
        12 => terminal
            .apply_action(TerminalAction::ClearLine(clear_mode(d)))
            .unwrap(),
        13 => terminal
            .apply_action(TerminalAction::InsertLines(count))
            .unwrap(),
        14 => terminal
            .apply_action(TerminalAction::DeleteLines(count))
            .unwrap(),
        15 => terminal
            .apply_action(TerminalAction::InsertChars(count))
            .unwrap(),
        16 => terminal
            .apply_action(TerminalAction::DeleteChars(count))
            .unwrap(),
        17 => terminal
            .apply_action(TerminalAction::EraseChars(count))
            .unwrap(),
        18 => terminal
            .apply_action(TerminalAction::SetMode {
                mode: TerminalMode::AlternateScreen,
                enabled: d % 2 == 0,
            })
            .unwrap(),
        19 => terminal
            .apply_action(TerminalAction::SetMode {
                mode: TerminalMode::Insert,
                enabled: d % 2 == 0,
            })
            .unwrap(),
        20 => terminal
            .apply_action(TerminalAction::SetGraphicRendition(sgr(a, b, c, d)))
            .unwrap(),
        21 => terminal
            .apply_action(TerminalAction::SetScrollRegion {
                top: row.min(size.rows),
                bottom: (row + count).min(size.rows),
            })
            .unwrap(),
        22 => terminal
            .apply_action(TerminalAction::ResetScrollRegion)
            .unwrap(),
        23 => terminal.apply_action(TerminalAction::Reset).unwrap(),
        24 => terminal
            .resize(TerminalSize::new((a % 96).max(1), (b % 32).max(1)))
            .unwrap(),
        25 => set_fuzz_selection(terminal, a, b, c, d, false),
        26 => set_fuzz_selection(terminal, a, b, c, d, true),
        27 => terminal
            .apply_action(TerminalAction::SetMode {
                mode: TerminalMode::Origin,
                enabled: d % 2 == 0,
            })
            .unwrap(),
        28 => terminal.apply_action(TerminalAction::ReverseIndex).unwrap(),
        29 => terminal
            .apply_action(TerminalAction::ScrollUp(count))
            .unwrap(),
        30 => terminal
            .apply_action(TerminalAction::ScrollDown(count))
            .unwrap(),
        31 => terminal
            .apply_action(TerminalAction::RepeatLastPrinted(count))
            .unwrap(),
        32 => terminal.apply_action(TerminalAction::SetTabStop).unwrap(),
        33 => terminal.apply_action(TerminalAction::ClearTabStop).unwrap(),
        34 => terminal
            .apply_action(TerminalAction::ClearAllTabStops)
            .unwrap(),
        35 => terminal.apply_action(TerminalAction::SaveCursor).unwrap(),
        36 => terminal.apply_action(TerminalAction::RestoreCursor).unwrap(),
        37 => terminal
            .apply_action(TerminalAction::BackTab(count))
            .unwrap(),
        _ => terminal.apply_action(TerminalAction::NextLine).unwrap(),
    }
}

fn update_scroll_region(
    terminal: &TerminalState,
    tag: u8,
    row_seed: u16,
    count_seed: u16,
    scroll_region: &mut Option<(i64, i64)>,
) {
    match tag % 39 {
        21 => {
            let rows = terminal.grid().size.rows.max(1);
            let top = (row_seed % rows) + 1;
            let count = (count_seed % 16) + 1;
            let bottom = top.saturating_add(count).min(rows);
            if top < bottom {
                *scroll_region = Some((i64::from(top - 1), i64::from(bottom - 1)));
            }
        }
        22 | 23 | 24 => *scroll_region = None,
        _ => {}
    }
}

fn assert_cursor_within_origin_margins(
    terminal: &TerminalState,
    scroll_region: Option<(i64, i64)>,
) {
    if terminal.modes_ref().contains(&TerminalMode::Origin)
        && let Some((top, bottom)) = scroll_region
    {
        let row = terminal.cursor_state().position.row;
        assert!(row >= top && row <= bottom);
    }
}

pub fn set_fuzz_selection(
    terminal: &mut TerminalState,
    a: u16,
    b: u16,
    c: u16,
    d: u16,
    rect: bool,
) {
    let size = terminal.grid().size;
    let start = GridPosition::new(i64::from(a % size.rows.max(1)), b % size.cols.max(1));
    let end = GridPosition::new(i64::from(c % size.rows.max(1)), d % size.cols.max(1));
    let selection = if rect {
        Selection::rectangular(start, end)
    } else {
        Selection::normal(start, end)
    };
    terminal.set_selection(selection);
    let _ = terminal.selected_text();
}

fn clear_mode(value: u16) -> ClearMode {
    match value % 4 {
        0 => ClearMode::FromCursor,
        1 => ClearMode::ToCursor,
        2 => ClearMode::All,
        _ => ClearMode::Saved,
    }
}

fn sgr(a: u16, b: u16, c: u16, d: u16) -> GraphicRendition {
    match d % 8 {
        0 => GraphicRendition::Reset,
        1 => GraphicRendition::Bold,
        2 => GraphicRendition::Dim,
        3 => GraphicRendition::Italic,
        4 => GraphicRendition::Underline,
        5 => GraphicRendition::Foreground(Color::Indexed((a % 256) as u8)),
        6 => GraphicRendition::Background(Color::Indexed((b % 256) as u8)),
        _ => GraphicRendition::Foreground(Color::Rgb {
            red: a as u8,
            green: b as u8,
            blue: c as u8,
        }),
    }
}

pub fn fill_unicode_grid(terminal: &mut TerminalState, data: &[u8]) {
    let attributes = CellAttributes::default();
    terminal
        .apply_action(TerminalAction::SetGraphicRendition(
            GraphicRendition::Foreground(Color::DefaultForeground),
        ))
        .unwrap();
    for byte in data {
        terminal
            .apply_action(TerminalAction::Print(fuzz_char(*byte)))
            .unwrap();
    }
    terminal
        .apply_action(TerminalAction::SetGraphicRendition(
            GraphicRendition::Background(Color::DefaultBackground),
        ))
        .unwrap();
    let _ = attributes;
}

fn byte(data: &[u8], index: usize) -> u8 {
    data.get(index).copied().unwrap_or_default()
}

fn u16_from(data: &[u8], index: usize) -> u16 {
    u16::from_le_bytes([byte(data, index), byte(data, index + 1)])
}
