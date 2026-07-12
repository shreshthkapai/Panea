# Terminal Compatibility

Panea's default compatibility target is a modern xterm-compatible terminal.

## TERM Policy

The default local PTY environment sets:

```text
TERM=xterm-256color
COLORTERM=truecolor
TERM_PROGRAM=Panea
TERM_PROGRAM_VERSION=<Panea version>
```

Shell profiles may override these variables explicitly. First-class SSH PTYs
request `xterm-256color` as the portable remote terminal identity. A custom
Panea terminfo entry is intentionally deferred until the emulator has enough
compatibility surface to justify a stable public terminal identity. Panea does
not advertise a private `TERM` value for which users lack a terminfo entry.
Inside tmux, use tmux's normal `screen-256color` or `tmux-256color` policy; the
outer Panea session remains `xterm-256color`.

## Current Baseline

Implemented in the baseline compatibility layer:

- SGR indexed colors and truecolor
- bold, dim, italic, underline, inverse, and strikethrough attributes
- alternate screen mode
- scroll regions
- save and restore cursor
- cursor visibility and shape metadata
- title-setting OSC 0/2
- insert/delete lines
- insert/delete/erase characters
- tab stop set/clear/reset behavior
- origin, autowrap, insert, application cursor, and application keypad modes
- deferred wrap-pending semantics, resize cursor mapping, and background-color erase on scroll
- index, next-line, reverse-index, explicit scrolling, back-tab, and repeat
- DSR 5 and CPR/DSR 6 responses via terminal pending output
- primary device attributes and private cursor-position responses
- DEC Special Graphics G0/G1 line drawing
- bounded DCS/APC/PM/SOS handling and tmux DCS passthrough
- bracketed paste forwarding
- OSC 52 clipboard requests as bounded pending events with security policy
- focus in/out reporting
- normal, drag, wheel, and SGR mouse report encoding
- URL detection and basic semantic highlight overlays
- Unicode cell storage for split UTF-8 input, combining marks, CJK width, emoji
  modifiers, ZWJ emoji, variation selectors, selection, cursor movement,
  resize, and scrollback
- App compatibility smoke runner through `cargo xtask compat`, with bounded
  PTY/process probes and generated reports under `target/compatibility`

## App Compatibility Runner

Run:

```text
cargo xtask compat plan
cargo xtask compat run --required-only --timeout-ms 5000
cargo xtask compat run --timeout-ms 5000
```

The required Windows subset currently covers PowerShell PTY output, cmd PTY
output, bounded teardown, and an ANSI/OSC marker fixture. Optional app probes
for editors, TUIs, multiplexers, WSL, SSH client availability, and other shells
are recorded as pass, skip, fail, or manual-required.

Details and manual compatibility checklists live in
[App compatibility](app-compatibility.md).

## Deferred Compatibility Work

- Remote OSC 52 confirmation UI
- Real app-level Unicode conformance across shells, editors, TUIs, and SSH
- Full configurable hint pattern engine
- Optional Panea-specific terminfo identity only if future capabilities require it
- Full automated interactive app driving for editors, TUIs, tmux/screen/zellij,
  WSL, and SSH
- Cross-OS app compatibility reports across Windows, macOS, Linux X11, and
  Linux Wayland
