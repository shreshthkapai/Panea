# Runtime Config Reload

Feature name: runtime config watching and live reload
Layer: config portability, diagnostics, platform parity
User-facing behavior: Panea watches the active TOML config path, debounces file changes, reloads valid edits, applies safe settings live, and keeps the previous active config when parsing, validation, or runtime apply fails.
Config keys: no new keys in this slice; existing `diagnostics.log_level` controls debug reload-pending logs and existing reloadable sections retain their normal keys.
macOS behavior: same polling watcher and reload policy; real filesystem/runtime validation remains unverified.
Windows behavior: same polling watcher and reload policy; unit and desktop binary tests pass on the current Windows host.
Linux X11 behavior: same polling watcher and reload policy; real filesystem/runtime validation remains unverified.
Linux Wayland behavior: same polling watcher and reload policy; real filesystem/runtime validation remains unverified.
Fallback behavior: invalid or unreadable config produces diagnostics and the previous valid runtime config remains active. Restart-required changes are reported and not silently applied.
Diagnostics: reload diagnostics include parse/validation messages, live sections applied, restart-required paths/reasons, and explicit failure messages when the previous config is retained.
Performance cost when disabled: none beyond constructing the watcher; no config parsing runs when no watched file fingerprint changes.
Performance cost when enabled: one debounced filesystem poll outside render/input/PTY hot paths, plus parse/validate/apply only after a file fingerprint changes.
Tests: config watcher unit tests cover valid reload, invalid reload failure, and no repeated failure spam; desktop binary tests compile the runtime integration.

## Contract

The watcher lives in `config-toml`, not `config-core`. `config-core` remains the portable internal model and reload-impact classifier.

The current implementation uses a conservative polling watcher based on file metadata plus content hash. This avoids OS-specific watcher behavior during the foundation phase and works for:

- explicit `PANEA_CONFIG` / explicit path loading
- default platform discovery paths
- config creation after startup
- deletion fallback to defaults for non-explicit discovered configs
- explicit-path deletion as a reload failure

Programmable `config.panea` files compile into the same `AppConfig` and can use
the same reload-impact classifier in tests or explicit reload planning.
Automatic runtime watching for programmable config is deferred until it can
preserve the same previous-valid-config behavior on every supported desktop OS.

## Live-Applied Sections

The desktop runtime applies these sections without restarting sessions:

- colors
- font, after runtime font metrics preflight succeeds
- cursor
- window title
- window padding
- keybindings
- mouse, clipboard, and paste policy
- visual theme, prompt decorations, command blocks, and shell integration settings
- diagnostics, including performance overlay enablement
- performance budgets
- mux model settings

## Restart-Required Sections

These are diagnosed but not silently applied to the running app in this phase:

- renderer backend, present mode, and damage policy
- window geometry, opacity, mode, decoration strategy, and Linux backend
- shell profile startup settings
- SSH profiles
- scrollback storage policy
- platform overrides

## Deferred

- Native OS filesystem watcher backends may be added later if polling is not good enough, but they must preserve the same config contract.
- Automatic runtime watching for programmable `config.panea` files is not wired
  into the desktop app yet.
- macOS, Linux X11, and Linux Wayland runtime validation has not been run.
- User-facing error UI is still stderr/diagnostic text in the desktop runtime.
- Existing sessions do not receive shell-profile or scrollback policy mutations live.
