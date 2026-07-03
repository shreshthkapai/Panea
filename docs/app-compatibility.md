# App Compatibility Test Suite

Feature name: App compatibility test suite
Layer: diagnostics / conformance tooling
User-facing behavior: developers can run bounded real app smoke checks and get
a report that says pass, fail, skip, or manual-required.
Config keys: none.
macOS behavior: same runner shape; real host execution still required.
Windows behavior: PowerShell, cmd, and ANSI/OSC required smoke checks passed on
the current Windows host.
Linux X11 behavior: same runner shape; real X11 host execution still required.
Linux Wayland behavior: same runner shape; real Wayland host execution still
required.
Fallback behavior: missing optional apps are marked skipped, not passed.
Diagnostics: report includes command, category, duration, bytes, preview,
lifecycle events, and PTY teardown diagnostics.
Performance cost when disabled: zero; this is an explicit developer command.
Performance cost when enabled: bounded by per-case timeout and short one-shot
commands.
Tests: `cargo test -p xtask`; current-host required smoke through
`cargo xtask compat run --required-only --timeout-ms 5000`.

## Commands

List the planned suite:

```text
cargo xtask compat plan
```

Run the current host's required subset:

```text
cargo xtask compat run --required-only --timeout-ms 5000
```

Run all current-host automated probes, with unavailable optional apps recorded
as skipped:

```text
cargo xtask compat run --timeout-ms 5000
```

Filter by category or case:

```text
cargo xtask compat run --category shells
cargo xtask compat run --case shell-powershell
```

Reports are written to:

```text
target/compatibility/<platform>.md
```

## Categories

The runner tracks:

- shells: PowerShell, cmd, pwsh, sh, bash, zsh, fish, WSL
- editors: vim, neovim, nano, helix
- TUIs/tools: htop, btop, lazygit, fzf, git, cargo, npm/pnpm/yarn, Python, Node
- multiplexers: tmux, screen, zellij
- SSH: local client probe plus manual local-server verification
- protocol: ANSI/VT marker fixtures for truecolor and OSC title bytes

## Acceptance Status

The runner is now implemented. Product compatibility is not complete until the
reports exist and are reviewed on Windows, macOS, Linux X11, and Linux Wayland,
and until manual interactive checks cover full-screen editors, pagers, TUIs,
external multiplexers, WSL, SSH remote PTY resize, mouse mode, bracketed paste,
focus events, keyboard modifiers, OSC 52 policy, and Unicode behavior.

## Manual Checklist

Run each item inside Panea and record the platform, command, expected behavior,
and failure reproduction steps:

- `vim` or `nvim`: alternate screen, resize, cursor shape, keyboard modifiers
- `nano` or `helix`: text entry, selection/copy, resize
- `less` and `man`: alternate screen, search, scroll, quit cleanup
- `htop` or `btop`: full-screen TUI redraw, mouse mode if enabled
- `fzf`: interactive input, resize, keyboard navigation
- `git log` and `git diff`: color, pager behavior, Unicode
- `tmux`, `screen`, `zellij`: nested resize, mouse mode, alternate screen
- PowerShell, cmd, bash, zsh, fish: prompt, paste, Unicode, title changes
- WSL shell: local PTY through WSL, Unicode, resize, clipboard policy
- SSH local server: remote PTY, resize, Unicode, OSC 52 policy, disconnect

## Failure Policy

Failures should be recorded with:

- command and arguments
- platform and display backend
- terminal protocol feature involved
- expected output or behavior
- observed output or behavior
- generated compatibility report path
- reproduction steps

Do not mark an app compatible because its binary exists. Version probes are
only availability checks; interactive terminal behavior still requires PTY or
manual validation.
