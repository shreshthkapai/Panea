# Alpha Scope

Panea is an alpha candidate, not yet a cross-OS verified daily-driver release.
The alpha exists to exercise the real terminal, renderer, configuration,
multiplexer, shell-integration, visual, and SSH paths with informed users.

## Included

- GPU-rendered desktop terminal window and local PTY sessions.
- Windows ConPTY lifecycle smoke coverage on the current development host.
- Portable terminal core, Unicode model, parser, selection, scrollback, and
  xterm-256color compatibility baseline.
- TOML and controlled programmable configuration compiled into one `AppConfig`.
- Themes, font fallback, static and animated cursors, prompt decorations, and
  command-block overlays.
- Native tabs, panes, workspaces, local/SSH transports, host trust, OS keychain
  integration, and reconnect presentation.
- Clipboard policy, paste protection, bounded OSC 52 handling, diagnostics,
  performance instrumentation, and renderer recovery contracts.
- Windows portable ZIP and per-user installer with packaged doctor and local
  shell smoke commands.

## Explicit Limits

- Windows is the only platform with current-host package and real local-shell
  evidence. macOS, Linux X11, and Linux Wayland implementations are not called
  verified until their native reports pass.
- Editors, TUIs, tmux/screen/zellij, WSL, SSH servers, IME, clipboard, GPU
  device-loss, and compositor behavior still require broader interactive and
  real-host evidence.
- Windows artifacts are not Authenticode-signed. macOS artifacts are not signed
  or notarized. Linux AppImage and RPM artifacts are not produced yet.
- The native iOS companion remains a shared-engine foundation, not an app.
- Performance is instrumented and benchmarkable, but Panea makes no claim of
  outperforming another terminal without a controlled reproducible comparison.

## Alpha Acceptance

An alpha build must compile, pass workspace unit tests and layer checks, retain
the previous valid configuration after reload failure, keep PTY shutdown
bounded, deny unsafe clipboard and SSH actions by default, and pass its packaged
doctor and local-shell smoke commands. A platform is listed as verified only
after those checks and the platform-specific manual checklist run on that OS.

Alpha bug reports should include `panea doctor --json` output. Diagnostic
exports exclude terminal contents, command output, environment variables,
clipboard contents, SSH keys, and secrets by default.
