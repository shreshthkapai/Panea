# Clipboard, Selection, and OSC 52 Policy

## Design Note

Feature name: Clipboard, selection, and OSC 52 policy
Layer: security, config portability, platform parity, core correctness
User-facing behavior: keyboard copy/paste works through the system clipboard, paste is protected by default, middle-click paste is allowed only when terminal mouse reporting is inactive, and OSC 52 writes are controlled by explicit policy. Remote writes can require an explicit one-time overlay decision.
Config keys: `clipboard.*`, `clipboard.osc52.*`, and legacy `paste.*` sanitization keys.
macOS behavior: system clipboard path is used through the platform provider; OSC 52 follows the same policy.
Windows behavior: system clipboard path is used through the platform provider; OSC 52 follows the same policy.
Linux X11 behavior: system and Primary selections use the platform provider; real window-manager verification remains.
Linux Wayland behavior: system and Primary selections use the platform provider; compositor-specific clipboard failures need real host verification.
Fallback behavior: unavailable clipboard providers report diagnostics; denied OSC 52 requests never write silently, and only one bounded remote confirmation can be pending per pane.
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
- Linux copy-on-selection and middle-click paste use the Primary selection when configured; unavailable Wayland compositor support reports a diagnostic and paste falls back to the system clipboard.
- Keyboard copy/paste is wired through the desktop clipboard bridge.
- Paste protection normalizes newlines and strips control characters when enabled.
- Bracketed paste is emitted when the terminal has bracketed paste mode enabled.
- OSC 52 is parsed into pending terminal requests and evaluated by the security policy before any clipboard write.
- Malformed, unknown-target, read, and oversized remote requests are rejected before prompting.
- Remote prompts show session identity, target, and byte count without showing clipboard contents. Approval is one-time and re-runs the full policy.

## Still Open

- Real clipboard smoke tests on macOS, Linux X11, and Linux Wayland.
- Real OSC 52 application smoke inside local and SSH panes on each target OS.
