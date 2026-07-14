# Panea Doctor

`panea doctor` is the installed diagnostic command. It runs before the desktop
event loop starts, so it can be used from a shell, CI job, or bug-report flow
without opening a terminal window.

## Commands

```text
panea doctor
panea doctor window
panea doctor renderer
panea doctor config
panea doctor shell
panea doctor ssh
panea doctor fonts
panea doctor clipboard
panea doctor notifications
panea doctor --json
```

`cargo xtask doctor` uses the same diagnostics model for development workflows.

## Design Note

Feature name: installed terminal doctor binary

Layer: diagnostics, with bounded app-level probes for platform, renderer,
font, clipboard, notifications, keychain, PTY, and SSH provider status.

User-facing behavior: users can run `panea doctor` or a topic-specific command
to get human-readable output. `--json` emits machine-readable output for bug
reports and automation.

Config keys: no new config keys. Doctor reports the active config source,
validation diagnostics, renderer/font/window/clipboard/SSH settings, and
runtime provider status where available.

macOS behavior: reports macOS platform snapshot, winit/macOS backend label,
Metal-capable WGPU adapter when detected, configured fonts, system clipboard
provider status, macOS Keychain capability status, and portable PTY/SSH status.

Windows behavior: reports Windows platform snapshot, winit/windows backend
label, WGPU adapter/backend/features when detected, configured fonts, system
clipboard provider status, Windows Credential Manager capability status,
ConPTY/portable-pty backend label, and SSH provider status.

Linux X11 behavior: reports Linux backend environment, `DISPLAY`,
`WINIT_UNIX_BACKEND` if set, WGPU adapter/backend/features when detected,
configured fonts, clipboard provider status, Linux Secret Service capability
status, Unix PTY backend label, and SSH provider status.

Linux Wayland behavior: reports Linux backend environment, `WAYLAND_DISPLAY`,
`XDG_SESSION_TYPE`, `WINIT_UNIX_BACKEND` if set, WGPU adapter/backend/features
when detected, configured fonts, clipboard provider status, Linux Secret
Service capability status, Unix PTY backend label, and SSH provider status.

Fallback behavior: failed config loads are reported using default config plus a
recent error. Failed GPU adapter probes report `not detected` rather than
blocking the command. Runtime details that require an active window/session are
labeled as not initialized during doctor.

Diagnostics: output includes OS, app version, renderer/GPU status, window
backend, Linux display environment, config path and parse status, shell
integration status, clipboard provider, notification provider, keychain provider, PTY backend, SSH
provider status, and findings.

Performance cost when disabled: none. Doctor runs only when explicitly invoked.

Performance cost when enabled: bounded startup-only work: config load, no-window
GPU adapter probe, font database resolution, clipboard bridge initialization,
and keychain capability query.

Tests: diagnostics unit tests cover topic aliases, JSON escaping, fonts and
clipboard reporting. Desktop command verification is covered by `cargo run -p
panea-desktop -- doctor ...` smoke commands.
