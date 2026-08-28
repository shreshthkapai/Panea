# VT conformance fixtures

Data-driven fixtures for the terminal parser and grid. Each `.vt` file pairs an
escape-sequence input with the state it must produce, and is run by
`crates/term-parser/tests/conformance.rs`:

```
cargo test -p term-parser --test conformance
```

A failure prints the fixture name, the expected row and the actual row, so a
protocol regression names itself.

## Writing a fixture

Expectations describe **what the spec requires**, not what the code currently
does. A fixture that disagrees with the implementation is a finding to
investigate, not a fixture to adjust. Where the correct behaviour is genuinely
ambiguous, say so in a comment and assert the unambiguous case instead — see
`dcs-unrecognised-is-consumed.vt`.

### Format

Leading `key: value` lines configure the run:

| key | meaning |
| --- | --- |
| `size` | grid as `COLSxROWS` (default `20x4`) |
| `chunk` | feed the input in chunks of N bytes, to exercise sequences that straddle a read boundary |
| `description` | one line, for humans |

Then any of these sections:

```
--- input
A\e[1mB
```

Line breaks in the file are formatting — long inputs may be wrapped freely.
Data newlines must be written as escapes, so a fixture never depends on
invisible whitespace. Supported escapes: `\e` `\r` `\n` `\t` `\a` `\0` `\`
`\xNN`, and `\st` for the String Terminator (`ESC \`).

```
--- expect-grid
|AB|
||
```

Rows are pipe-wrapped so trailing blanks stay visible; trailing blanks are
ignored when comparing, so a row may be written short.

```
--- expect-cursor
row=0 col=2
```

```
--- expect-reply
\e[1;1R
```

Substrings the terminal's reply to the host must contain. `--- reject-reply`
asserts the opposite — useful for "this query must not be answered".

```
--- expect-cell
row=0 col=0 text=A bold=true underline=false
```

`text=` compares the cell's exact contents, with `_` standing for a space.
Boolean attributes: `bold` `dim` `italic` `underline` `inverse`
`strikethrough` `has_background` `has_foreground` `wide_continuation`.

## Coverage

Parser: autowrap, DECALN, private markers and intermediates, colon SGR
sub-parameters, the kitty keyboard query, unrecognised versus tmux DCS.
Grid: back-colour erase, ICH shifting, scroll-region clamping, per-screen saved
cursor, wide-glyph margins, combining-mark bounds.
Queries: DA1, DSR 6 including origin mode, DECRQM.
Applications: an editor's alternate-screen session, alternate-screen scrollback
isolation, powerline prompt segments, carriage-return progress output.

Screenshot fixtures are separate: see `../screenshots` and
`crates/render-wgpu/src/conformance.rs`.
