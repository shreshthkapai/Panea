# SSH Product Integration

## Feature Design Note

```text
Feature name: SSH product integration
Layer: desktop app/mux, transport-ssh, security, semantics, diagnostics
User-facing behavior: SSH profiles open in tabs or panes, request explicit host trust and credentials in a modal, preserve scrollback on disconnect, and reconnect on demand
Config keys: ssh_profiles.*, mux startup layouts, keyboard.keybindings action reconnect_session
macOS behavior: shared UI and transport with secrets persisted in macOS Keychain
Windows behavior: shared UI and transport with secrets persisted in Windows Credential Manager
Linux X11 behavior: shared UI and transport with Secret Service persistence when available
Linux Wayland behavior: shared UI and transport with Secret Service persistence when available
Fallback behavior: unavailable keychain keeps the secret transient; rejected trust or auth leaves a disconnected pane with a readable error and reconnect action
Diagnostics: pane status overlay plus panea doctor ssh/keychain provider reporting
Performance cost when disabled: zero; no SSH worker, prompt polling, or overlay exists without an SSH pane
Performance cost when enabled: connection/auth runs on one worker; prompt polling is nonblocking and overlays damage only their bounds
Tests: trust action, secret masking/persistence intent, transport security, mux SSH layout, semantic remote metadata, and real-server smoke harness
```

## Runtime Contract

- A pane owns one SSH transport and its terminal/semantic state.
- TCP, handshake, host verification, keychain access, and authentication run off
  the event/render thread.
- Unknown hosts require `trust once` or `trust and store`. Escape rejects.
- Changed keys expose only `replace stored key` or reject. A pinned mismatch
  remains blocked until config is corrected.
- Passwords and passphrases are masked and never enter terminal cells, logs, or
  render diagnostics.
- Saving a secret is opt-in. The native provider is Windows Credential Manager,
  macOS Keychain, or Linux Secret Service.
- Disconnect keeps terminal content and semantic history visible. The
  `reconnect_session` action starts a fresh transport without rewriting old
  output.
- Remote OSC 133/7 markers drive the same semantic timeline and command-block
  overlays when integration is installed remotely.

## Performance Note

```text
Does this run every frame? Only while a small prompt/status overlay is visible.
Does this run every input event? Prompt routing only while a prompt is active.
Does this run every PTY output batch? Existing SSH transport/parser path only.
Does this allocate in the hot path? No new steady-state allocation beyond transport output.
Does this force full redraw? No; prompt/status/IME overlays have bounded damage.
Does this require GPU uploads? Overlay labels use the existing glyph cache.
Does this run script/user code? No.
Can it be cached? Keychain entries, glyphs, terminal state, and semantic state are retained.
Can it be disabled to near-zero cost? Yes; no SSH pane means no worker or polling.
Can the user budget it? Connection timeout and renderer budgets remain configured independently.
Can diagnostics show its cost? Transport and renderer metrics remain visible in the performance overlay.
```
