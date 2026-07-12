# Clipboard, Selection, and OSC 52 Policy

## Design Note

Feature name: Clipboard, selection, and OSC 52 policy
Layer: security, config portability, platform parity, core correctness
User-facing behavior: keyboard copy/paste works through the system clipboard, paste is protected by default, middle-click paste is allowed only when terminal mouse reporting is inactive, and OSC 52 writes are controlled by explicit policy.
Config keys: `clipboard.*`, `clipboard.osc52.*`, and legacy `paste.*` sanitization keys.
macOS behavior: system clipboard path is used through the platform provider; OSC 52 follows the same policy.
Windows behavior: system clipboard path is used through the platform provider; OSC 52 follows the same policy.
Linux X11 behavior: system clipboard path is used through the platform provider; primary selection is modeled but not fully backed yet.
Linux Wayland behavior: system clipboard path is used through the platform provider; primary selection and compositor-specific clipboard failures need real host verification.
Fallback behavior: unavailable clipboard providers report diagnostics; blocked OSC 52 writes are denied or held for future confirmation UI instead of silently writing.
Diagnostics: clipboard operations can be logged with `clipboard.log_operations = true`; unavailable providers return platform clipboard diagnostics.
Performance cost when disabled: pending OSC 52 requests are dropped with no decode or clipboard write, and copy/paste shortcuts are ignored.
Performance cost when enabled: user-initiated copy/paste is proportional to clipboard text size; OSC 52 decodes only bounded payloads and rejects oversized encoded payloads before decode.
Tests: config defaults and validation, TOML parsing, parser pending OSC 52 requests, security policy decisions, paste safety, and middle-click suppression when mouse reporting is active.

## Config

```toml
[clipboard]
enabled = true
copy_on_select = false
paste_protection = true
bracketed_paste = true
middle_click_paste = true
prefer_primary_selection_on_linux = true
log_operations = false

[clipboard.osc52]
enabled = true
allow_local = true
allow_remote = false
max_bytes = 1048576
confirm_remote_writes = true
```

`copy_on_select` is off by default because it can overwrite the user clipboard
frequently. `allow_remote` is off by default so SSH sessions cannot silently
write the local clipboard.

## Current State

- Normal and rectangular extraction uses absolute buffer positions and remains valid across scrollback.
- Pane-aware mouse drag selection is wired; hold Alt while beginning a drag for rectangular selection, or Shift to bypass application mouse reporting.
- Selection visuals are projected as renderer overlays and never mutate terminal cells.
- Keyboard copy/paste is wired through the desktop clipboard bridge.
- Paste protection normalizes newlines and strips control characters when enabled.
- Bracketed paste is emitted when the terminal has bracketed paste mode enabled.
- OSC 52 is parsed into pending terminal requests and evaluated by the security policy before any clipboard write.

## Still Open

- Keyboard-driven selection extension and search-result selection UX.
- Linux primary selection provider support.
- Remote OSC 52 confirmation UI.
- Real clipboard smoke tests on macOS, Linux X11, and Linux Wayland.
- OSC 52 behavior inside first-class SSH sessions once SSH tabs/panes are runtime-wired.
