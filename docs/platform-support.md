# Platform Support

macOS, Windows, Linux X11, and Linux Wayland are first-class desktop targets.

Platform differences must be represented as capabilities, fallbacks, and
diagnostics. A feature is not considered fully accepted until it has been tested
on every desktop target or has an explicit platform limitation.

Linux compositor coverage is tracked in
[Linux compositor matrix](linux-compositor-matrix.md). Linux support is not
considered verified until that matrix has real X11 and Wayland host evidence.

## Support Status Terms

| Status | Meaning |
| --- | --- |
| full | Implemented and verified on that platform class. |
| partial | Implemented or modeled, but runtime coverage is incomplete. |
| fallback | Implemented with a documented fallback or degraded behavior. |
| unsupported by platform | The platform blocks the feature or equivalent behavior. |
| not implemented yet | No product behavior exists yet beyond possible placeholders. |

## Feature Parity Matrix

| Feature | macOS | Windows | Linux X11 | Linux Wayland | Notes |
| --- | --- | --- | --- | --- | --- |
| window modes | partial | partial | partial | partial | Windowed, maximized, borderless fullscreen, and frameless states are modeled; real compositor validation remains open. |
| frameless modes | partial | partial | fallback | fallback | Implemented through winit decorations with Linux decoration negotiation still requiring compositor tests. |
| fullscreen modes | partial | partial | partial | fallback | Exclusive fullscreen currently falls back to borderless fullscreen. |
| clipboard | partial | partial | partial | partial | System clipboard bridge exists; primary selection and OSC clipboard remain later compatibility work. |
| IME | partial | partial | partial | partial | Platform-neutral IME events are represented; real composed-input validation is still required. |
| DPI/fractional scaling | partial | partial | partial | partial | Monitor scale snapshots exist; fractional behavior needs real host verification. |
| font fallback | partial | partial | partial | partial | Fallback chains are configurable; per-OS font availability validation is not automated yet. |
| GPU backend | partial | partial | partial | partial | WGPU surface/device path exists; GPU backend inventory and screenshot verification remain open. |
| local PTY | partial | full | partial | partial | Windows real-shell smoke passed on the current host; macOS/Linux real PTY smoke remains unverified. |
| PowerShell/cmd/WSL | not implemented yet | partial | not implemented yet | not implemented yet | Windows shell profile groundwork exists; WSL runtime smoke is not verified. |
| shell integration | partial | partial | partial | partial | Semantic parsers and scripts exist; desktop startup activation and real shell validation remain open. |
| tabs/panes | partial | partial | partial | partial | Mux state model exists; full desktop multi-pane runtime is deferred. |
| command blocks | partial | partial | partial | partial | Semantic storage and basic overlays exist; real shell-driven UI verification remains open. |
| cursor animations | partial | partial | partial | partial | Config and budget contracts exist; polished animation runtime and asset pipeline are deferred. |
| SSH | partial | partial | partial | partial | Secure transport backend exists; interactive trust UI and real server smoke tests remain open. |
| config reload | partial | partial | partial | partial | Reload impact is classified; runtime file watching/application is deferred. |
| notifications | not implemented yet | not implemented yet | not implemented yet | not implemented yet | Native notification surface has not been implemented. |
| OSC clipboard | not implemented yet | not implemented yet | not implemented yet | not implemented yet | OSC 52 policy and security prompts remain later compatibility/security work. |

## Current Verification Status

| Area | Windows | macOS | Linux X11 | Linux Wayland |
| --- | --- | --- | --- | --- |
| Workspace build/test/lint | Verified on current host | Unverified | Unverified | Unverified |
| Local PTY real-shell smoke | Verified on current host | Unverified | Unverified | Unverified |
| Window creation/input translation | Build-verified on current host | Unverified | Unverified | Unverified |
| GPU renderer window path | Build-verified on current host | Unverified | Unverified | Unverified |
| Linux compositor matrix | Not applicable | Not applicable | Matrix exists, unverified | Matrix exists, unverified |
| SSH real-server smoke | Unverified | Unverified | Unverified | Unverified |
| Shell integration real-session smoke | Unverified | Unverified | Unverified | Unverified |
| Native mux runtime smoke | Unverified | Unverified | Unverified | Unverified |

## macOS Polish Checklist

- App lifecycle: unverified.
- Retina scaling: represented by DPI snapshots, unverified on real Retina hosts.
- Keyboard shortcuts and Command key conventions: not fully mapped yet.
- Option/Alt behavior config: not fully mapped yet.
- Fullscreen behavior: modeled through window modes, unverified.
- Frameless behavior: modeled, unverified.
- Clipboard: bridge exists, unverified.
- IME: event contract exists, unverified.
- Font fallback: configurable, unverified.
- Signing/notarization plan: later packaging phase.

## Windows Polish Checklist

- PowerShell: local shell profile exists; real smoke passed during local PTY
  lifecycle hardening on the current host.
- pwsh: profile groundwork exists; not separately verified.
- cmd: profile groundwork exists; not separately verified.
- WSL profiles: profile kind exists; runtime smoke unverified.
- ConPTY behavior: bounded lifecycle tests passed on current host.
- AltGr and modifier handling: translated through platform events; dedicated
  keyboard-layout tests still needed.
- DPI scaling: per-monitor behavior is represented; full manual matrix remains
  open.
- Window resize behavior: wired to terminal and transport resize; runtime GUI
  coverage remains shallow.
- Clipboard: bridge exists.
- IME: event contract exists; real composed-input coverage remains open.
- Font fallback: configurable; installed-font variance still needs coverage.
- Installer packaging: later packaging phase.

## Linux X11 Polish Checklist

- Window creation: represented through winit; real X11 host verification needed.
- Fullscreen: modeled; real WM behavior unverified.
- Frameless/custom decoration: modeled with fallback diagnostics; real WM
  behavior unverified.
- Clipboard: system bridge exists; primary selection remains future work.
- Selection clipboard: not implemented yet.
- DPI/scaling: represented; real X11 behavior unverified.
- Major window managers: unverified.
- Tiling WM behavior: unverified.
- Font fallback: configurable; distro font variance needs coverage.
- Verification checklist: see [Linux compositor matrix](linux-compositor-matrix.md).

## Linux Wayland Polish Checklist

- GNOME/Mutter: unverified.
- KDE/KWin: unverified.
- wlroots/Sway class: unverified.
- Hyprland class: unverified.
- Decoration negotiation: modeled as a requested/effective diagnostic, but
  compositor-specific behavior is unverified.
- Fullscreen behavior: modeled; compositor behavior unverified.
- Fractional scaling: represented; real compositor behavior unverified.
- Clipboard: system bridge exists; Wayland-specific failure modes need tests.
- IME: event contract exists; real composed-input coverage remains open.
- Fallback diagnostics: backend/decorations fields exist and need real host
  coverage.
- Verification checklist: see [Linux compositor matrix](linux-compositor-matrix.md).

## Doctor Commands

The shared diagnostics model supports these commands through the current xtask
wrapper:

```text
cargo xtask doctor
cargo xtask doctor renderer
cargo xtask doctor config
cargo xtask doctor platform
cargo xtask doctor shell-integration
cargo xtask doctor performance
cargo xtask doctor ssh
cargo xtask doctor window
cargo xtask linux-compositor
cargo xtask bug-report
```

The future installed `terminal doctor` command should call the same diagnostics
library rather than reimplementing these checks.

The privacy-aware bug-report snapshot intentionally excludes terminal contents,
command output, environment variables, secrets, SSH keys, and clipboard contents
by default.

## iOS Companion Status

The iOS SSH companion is tracked separately from desktop platform parity. Its
foundation reuses the shared terminal/parser/semantic/render/config/transport
contracts, but the native iOS app shell, iOS GPU surface, Keychain-backed secret
provider, and real device validation are not implemented yet.

Run:

```text
cargo xtask ios-readiness
```

Do not mark the iOS companion as complete until secure SSH, host-key approval,
mobile rendering, keyboard behavior, and lifecycle behavior are verified on
simulator and real iPhone/iPad hardware.
