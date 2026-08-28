//! Data-driven VT conformance and application fixtures.
//!
//! Each fixture in `tools/conformance/vt` is a readable text file pairing an
//! escape-sequence input with the terminal state it must produce. Keeping them
//! as data rather than Rust lets a protocol gap be described in the terms the
//! spec uses, and lets a real application trace be dropped in as a fixture.
//!
//! Expectations here are derived from the VT500 series and xterm behaviour, not
//! transcribed from this implementation's output: a fixture that disagrees with
//! the code is a finding, not a fixture to adjust.

use std::{fmt::Write as _, fs, path::Path, path::PathBuf};

use term_core::{TerminalCore, TerminalSize};
use term_parser::TerminalEmulator;

/// One parsed fixture file.
#[derive(Debug)]
struct Fixture {
    name: String,
    path: PathBuf,
    cols: u16,
    rows: u16,
    /// Bytes fed to the terminal, already unescaped.
    input: Vec<u8>,
    /// Size of the chunks the input is delivered in, to exercise sequences that
    /// straddle a read boundary. `None` feeds it all at once.
    chunk: Option<usize>,
    expect_grid: Option<Vec<String>>,
    expect_cursor: Option<(i64, u16)>,
    /// Substrings the terminal's reply to the host must contain.
    expect_reply: Vec<String>,
    /// Substrings the reply must not contain.
    reject_reply: Vec<String>,
    /// Per-cell attribute assertions: (row, col, attribute, expected).
    expect_attrs: Vec<(usize, usize, String, bool)>,
    /// Cells whose text must be exactly this.
    expect_cells: Vec<(usize, usize, String)>,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/conformance/vt")
        .canonicalize()
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/conformance/vt")
        })
}

/// Decodes the escape forms a fixture may use in its input section.
fn unescape(source: &str) -> Result<Vec<u8>, String> {
    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        let Some(kind) = bytes.get(index + 1) else {
            return Err("input ends with a dangling backslash".to_owned());
        };
        match kind {
            b'e' => {
                out.push(0x1b);
                index += 2;
            }
            b'r' => {
                out.push(b'\r');
                index += 2;
            }
            b'n' => {
                out.push(b'\n');
                index += 2;
            }
            b't' => {
                out.push(b'\t');
                index += 2;
            }
            b'a' => {
                out.push(0x07);
                index += 2;
            }
            b'0' => {
                out.push(0x00);
                index += 2;
            }
            b'\\' => {
                out.push(b'\\');
                index += 2;
            }
            // String Terminator, spelled out because `\e\\` is easy to mangle.
            b's' if bytes.get(index + 2) == Some(&b't') => {
                out.extend_from_slice(b"\x1b\\");
                index += 3;
            }
            b'x' => {
                let hex = bytes
                    .get(index + 2..index + 4)
                    .and_then(|pair| std::str::from_utf8(pair).ok())
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .ok_or_else(|| format!("bad \\x escape at byte {index}"))?;
                out.push(hex);
                index += 4;
            }
            other => {
                return Err(format!(
                    "unknown escape \\{} at byte {index}",
                    char::from(*other)
                ));
            }
        }
    }
    Ok(out)
}

fn parse_fixture(path: &Path) -> Result<Fixture, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut fixture = Fixture {
        name: path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: path.to_path_buf(),
        cols: 20,
        rows: 4,
        input: Vec::new(),
        chunk: None,
        expect_grid: None,
        expect_cursor: None,
        expect_reply: Vec::new(),
        reject_reply: Vec::new(),
        expect_attrs: Vec::new(),
        expect_cells: Vec::new(),
    };

    let mut section = String::new();
    let mut input = String::new();
    let mut grid: Vec<String> = Vec::new();

    for (number, line) in text.lines().enumerate() {
        let number = number + 1;
        if let Some(rest) = line.strip_prefix("--- ") {
            section = rest.trim().to_owned();
            continue;
        }
        match section.as_str() {
            "" => {
                if line.trim().is_empty() || line.trim_start().starts_with('#') {
                    continue;
                }
                let Some((key, value)) = line.split_once(':') else {
                    return Err(format!(
                        "{}:{number}: expected `key: value`",
                        path.display()
                    ));
                };
                let value = value.trim();
                match key.trim() {
                    "size" => {
                        let (cols, rows) = value.split_once('x').ok_or_else(|| {
                            format!("{}:{number}: size must look like 20x4", path.display())
                        })?;
                        fixture.cols = cols
                            .trim()
                            .parse()
                            .map_err(|_| format!("{}:{number}: bad cols", path.display()))?;
                        fixture.rows = rows
                            .trim()
                            .parse()
                            .map_err(|_| format!("{}:{number}: bad rows", path.display()))?;
                    }
                    "chunk" => {
                        fixture.chunk = Some(
                            value
                                .parse()
                                .map_err(|_| format!("{}:{number}: bad chunk", path.display()))?,
                        );
                    }
                    "description" => {}
                    other => {
                        return Err(format!(
                            "{}:{number}: unknown key `{other}`",
                            path.display()
                        ));
                    }
                }
            }
            "input" => {
                // Line breaks in the file are formatting: long inputs may be
                // wrapped freely. Data newlines are written as `\r` / `\n`
                // escapes so a fixture never depends on invisible whitespace.
                input.push_str(line.trim_end());
            }
            "expect-grid" => {
                if line.trim().is_empty() {
                    continue;
                }
                // Rows are written between pipes so trailing blanks are visible.
                let row = line
                    .strip_prefix('|')
                    .and_then(|row| row.strip_suffix('|'))
                    .ok_or_else(|| {
                        format!("{}:{number}: grid rows must be |wrapped|", path.display())
                    })?;
                grid.push(row.to_owned());
            }
            "expect-cursor" => {
                if line.trim().is_empty() {
                    continue;
                }
                let mut row = None;
                let mut col = None;
                for part in line.split_whitespace() {
                    let Some((key, value)) = part.split_once('=') else {
                        continue;
                    };
                    let parsed = value.parse::<i64>().map_err(|_| {
                        format!("{}:{number}: bad cursor value `{value}`", path.display())
                    })?;
                    match key {
                        "row" => row = Some(parsed),
                        "col" => col = Some(parsed),
                        _ => {}
                    }
                }
                let (Some(row), Some(col)) = (row, col) else {
                    return Err(format!(
                        "{}:{number}: expect-cursor needs row= and col=",
                        path.display()
                    ));
                };
                fixture.expect_cursor = Some((row, col.max(0) as u16));
            }
            "expect-reply" => {
                if !line.trim().is_empty() {
                    fixture.expect_reply.push(line.to_owned());
                }
            }
            "reject-reply" => {
                if !line.trim().is_empty() {
                    fixture.reject_reply.push(line.to_owned());
                }
            }
            "expect-cell" => {
                if line.trim().is_empty() {
                    continue;
                }
                // `row=0 col=3 text=x` or `row=0 col=3 inverse=false`
                let mut row = 0usize;
                let mut col = 0usize;
                for part in line.split_whitespace() {
                    let Some((key, value)) = part.split_once('=') else {
                        continue;
                    };
                    match key {
                        "row" => row = value.parse().unwrap_or(0),
                        "col" => col = value.parse().unwrap_or(0),
                        "text" => fixture
                            .expect_cells
                            .push((row, col, unescape_text_value(value))),
                        attribute => {
                            let expected = value == "true";
                            fixture
                                .expect_attrs
                                .push((row, col, attribute.to_owned(), expected));
                        }
                    }
                }
            }
            other => {
                return Err(format!(
                    "{}:{number}: unknown section `{other}`",
                    path.display()
                ));
            }
        }
    }

    fixture.input = unescape(&input)?;
    if !grid.is_empty() {
        fixture.expect_grid = Some(grid);
    }
    Ok(fixture)
}

/// `_` stands for a space so trailing expectations stay visible.
fn unescape_text_value(value: &str) -> String {
    value.replace('_', " ")
}

fn render_grid(terminal: &TerminalEmulator, cols: usize) -> Vec<String> {
    terminal
        .state()
        .grid()
        .lines
        .iter()
        .map(|line| {
            let mut row = String::with_capacity(cols);
            for cell in &line.cells {
                if cell.wide_continuation {
                    continue;
                }
                row.push_str(&cell.text);
            }
            // Trailing blanks matter, but keep rows comparable in width.
            while row.chars().count() > cols {
                row.pop();
            }
            row
        })
        .collect()
}

fn attribute_value(cell: &term_core::Cell, attribute: &str) -> Option<bool> {
    let attributes = &cell.attributes;
    match attribute {
        "bold" => Some(attributes.bold),
        "dim" => Some(attributes.dim),
        "italic" => Some(attributes.italic),
        "underline" => Some(attributes.underline),
        "inverse" => Some(attributes.inverse),
        "strikethrough" => Some(attributes.strikethrough),
        "has_background" => Some(attributes.background.is_some()),
        "has_foreground" => Some(attributes.foreground.is_some()),
        "wide_continuation" => Some(cell.wide_continuation),
        _ => None,
    }
}

fn run_fixture(fixture: &Fixture) -> Result<(), String> {
    let mut terminal = TerminalEmulator::new(TerminalSize::new(fixture.cols, fixture.rows));
    let chunk = fixture.chunk.unwrap_or(fixture.input.len().max(1));
    for bytes in fixture.input.chunks(chunk.max(1)) {
        terminal
            .apply_bytes(bytes)
            .map_err(|error| format!("apply_bytes failed: {error}"))?;
    }

    let reply = String::from_utf8_lossy(&terminal.state_mut().take_pending_output()).into_owned();
    let mut failures = String::new();

    if let Some(expected) = &fixture.expect_grid {
        let actual = render_grid(&terminal, usize::from(fixture.cols));
        for (index, expected_row) in expected.iter().enumerate() {
            let actual_row = actual.get(index).map(String::as_str).unwrap_or("<missing>");
            if actual_row.trim_end() != expected_row.trim_end() {
                let _ = writeln!(
                    failures,
                    "  row {index}:\n    expected |{expected_row}|\n    actual   |{actual_row}|"
                );
            }
        }
    }

    if let Some((row, col)) = fixture.expect_cursor {
        let cursor = terminal.cursor_state().position;
        if cursor.row != row || cursor.col != col {
            let _ = writeln!(
                failures,
                "  cursor: expected row={row} col={col}, actual row={} col={}",
                cursor.row, cursor.col
            );
        }
    }

    for needle in &fixture.expect_reply {
        let decoded = String::from_utf8_lossy(&unescape(needle)?).into_owned();
        if !reply.contains(&decoded) {
            let _ = writeln!(
                failures,
                "  reply must contain {:?}, got {:?}",
                decoded, reply
            );
        }
    }
    for needle in &fixture.reject_reply {
        let decoded = String::from_utf8_lossy(&unescape(needle)?).into_owned();
        if reply.contains(&decoded) {
            let _ = writeln!(failures, "  reply must not contain {decoded:?}");
        }
    }

    let grid = terminal.state().grid();
    for (row, col, text) in &fixture.expect_cells {
        let actual = grid
            .lines
            .get(*row)
            .and_then(|line| line.cells.get(*col))
            .map(|cell| cell.text.to_string());
        if actual.as_deref() != Some(text.as_str()) {
            let _ = writeln!(
                failures,
                "  cell {row},{col}: expected text {text:?}, actual {actual:?}"
            );
        }
    }
    for (row, col, attribute, expected) in &fixture.expect_attrs {
        let cell = grid.lines.get(*row).and_then(|line| line.cells.get(*col));
        let Some(cell) = cell else {
            let _ = writeln!(failures, "  cell {row},{col}: out of range");
            continue;
        };
        match attribute_value(cell, attribute) {
            Some(actual) if actual == *expected => {}
            Some(actual) => {
                let _ = writeln!(
                    failures,
                    "  cell {row},{col}: {attribute} expected {expected}, actual {actual}"
                );
            }
            None => {
                let _ = writeln!(
                    failures,
                    "  cell {row},{col}: unknown attribute {attribute}"
                );
            }
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

#[test]
fn vt_conformance_fixtures_hold() {
    let dir = fixture_dir();
    let entries = fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read fixture dir {}: {error}", dir.display()));

    let mut fixtures = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "vt"))
        .collect::<Vec<_>>();
    fixtures.sort();

    assert!(
        !fixtures.is_empty(),
        "no .vt fixtures found in {}",
        dir.display()
    );

    let mut report = String::new();
    let mut failed = 0usize;
    for path in &fixtures {
        match parse_fixture(path).and_then(|fixture| {
            run_fixture(&fixture).map_err(|failures| {
                format!("{} ({})\n{failures}", fixture.name, fixture.path.display())
            })
        }) {
            Ok(()) => {}
            Err(failure) => {
                failed += 1;
                let _ = writeln!(report, "\n{failure}");
            }
        }
    }

    assert!(
        failed == 0,
        "{failed} of {} conformance fixtures failed:{report}",
        fixtures.len()
    );
}

#[test]
fn fixture_input_escapes_round_trip() {
    assert_eq!(unescape("\\e[2J").unwrap(), b"\x1b[2J");
    assert_eq!(unescape("a\\r\\nb").unwrap(), b"a\r\nb");
    assert_eq!(unescape("\\x41\\x7f").unwrap(), b"\x41\x7f");
    assert_eq!(unescape("\\\\").unwrap(), b"\\");
    assert!(unescape("\\q").is_err());
    assert!(unescape("trailing\\").is_err());
}

#[test]
fn a_multiplexer_startup_sequence_does_not_panic() {
    // Shapes a multiplexer emits on launch: alternate screen, scroll region,
    // mode sets, status line, wide chars, and a full redraw.
    let sequences: &[&[u8]] = &[
        b"\x1b[?1049h\x1b[?1h\x1b=\x1b[?2004h\x1b[?1000h\x1b[?1006h",
        b"\x1b[1;24r\x1b[H\x1b[2J",
        b"\x1b[24;1H\x1b[7m[0] 0:bash*                    host \x1b[0m",
        b"\x1b[1;1H\x1b[32m$\x1b[0m ",
        b"\x1b[?25l\x1b[H\x1b[K\x1b[2;1H\x1b[K\x1b[?25h",
        b"\x1b[38;5;240m\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\x1b[0m",
        b"\x1bPtmux;\x1b\x1b]0;title\x07\x1b\\",
        b"\x1b[?1049l\x1b[?1l\x1b>\x1b[?2004l",
    ];
    for size in [(80u16, 24u16), (20, 5), (200, 60)] {
        let mut terminal = TerminalEmulator::new(TerminalSize::new(size.0, size.1));
        for sequence in sequences {
            for chunk in sequence.chunks(7) {
                terminal
                    .apply_bytes(chunk)
                    .unwrap_or_else(|error| panic!("apply failed at {size:?}: {error}"));
            }
            let _ = terminal.state_mut().take_pending_output();
        }
    }
}
