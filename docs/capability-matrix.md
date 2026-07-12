# Capability Matrix

This matrix uses the Phase 0 status labels from [Current Status](status.md):

```text
planned
stubbed
partial
implemented
tested
cross-os verified
```

`tested` always means the stated platform/scope was actually tested. If a
feature compiles on Windows but was not exercised on macOS or Linux, the macOS
and Linux cells stay `partial`, `stubbed`, or `planned`.

## Desktop Platform Matrix

| Capability | Windows | macOS | Linux X11 | Linux Wayland | Notes |
| --- | --- | --- | --- | --- | --- |
| Workspace build/test/lint | tested | partial | partial | partial | Verified on the current Windows host; other desktop OSes require runners. |
| Cross-OS verification runner | tested | implemented | implemented | implemented | `cargo xtask verify-os` and GitHub Actions jobs exist for Windows, macOS, Linux X11, and Linux Wayland. Current host verification has run on Windows only; CI/platform reports must be collected before any platform becomes cross-OS verified. |
| Terminal core grid/state | tested | implemented | implemented | implemented | Platform-neutral code exists; runtime app verification still needed off Windows. |
| Parser baseline | tested | implemented | implemented | implemented | Lower-level tests and a compatibility smoke runner exist; full interactive app verification remains incomplete. |
| Fuzz/property harness | tested | implemented | implemented | implemented | cargo-fuzz targets and proptest smoke tests exist; scheduled CI runs smoke properties, but long-running fuzz history has not accumulated yet. |
| Scrollback/alternate screen/resize | tested | implemented | implemented | implemented | Core behavior exists; full app smoke remains open per OS. |
| Selection extraction | tested | implemented | implemented | implemented | Absolute-buffer normal and rectangular extraction, pane-aware mouse and keyboard selection, and renderer selection overlays exist; real off-Windows UX verification remains. |
| Unicode/grapheme cell model | tested | implemented | implemented | implemented | Platform-neutral parser/core tests cover split UTF-8, combining marks, CJK width, emoji modifiers, ZWJ emoji, variation selectors, selection, cursor movement, resize, and scrollback; cross-OS renderer/font screenshots remain open. |
| Local PTY transport | tested | partial | partial | partial | Windows ConPTY smoke passed; macOS/Linux real PTY smoke is unverified. |
| Local PTY lifecycle contract | tested | implemented | implemented | implemented | Shared bounded lifecycle exists; non-Windows real shell validation remains open. |
| Windows PowerShell/cmd/WSL profiles | partial | planned | planned | planned | Windows profile groundwork exists; WSL smoke is unverified. |
| Desktop window creation | partial | partial | partial | partial | Winit path exists; real OS/window manager behavior needs validation. |
| Window modes | partial | partial | partial | partial | Windowed/maximized/fullscreen/frameless states are modeled; cross-OS behavior not verified. |
| Linux backend selection | planned | planned | partial | partial | X11/Wayland preferences and diagnostics are modeled; real compositor verification is open. |
| Linux compositor verification matrix | planned | planned | tested | tested | Target matrix, runtime environment snapshot, fallback checklist, and `cargo xtask linux-compositor` exist; actual Linux host runs remain unverified. |
| Decoration strategy | partial | partial | partial | partial | Requested/effective diagnostics exist; Linux negotiation needs real tests. |
| Emergency restore shortcuts | partial | partial | partial | partial | Actions/keybinding concepts exist; full titlebarless UX validation remains open. |
| Keyboard input translation | tested | implemented | implemented | implemented | Shared terminal encoding covers text/control input, navigation/editing/function/keypad keys, xterm modifiers, AltGr preservation, and application cursor/keypad modes; real layout, Command/Option, and IME testing remains off Windows. |
| Mouse input translation | tested | implemented | implemented | implemented | Normal, button-motion, all-motion, SGR encoding, focus reports, pane-aware selection, and scrollback wheel navigation exist; real cross-OS protocol and selection UX verification remains. |
| IME/composed text | partial | partial | partial | partial | Event contract exists; real composed-input verification is not complete. |
| System clipboard | tested | implemented | implemented | implemented | Clipboard bridge, portable Ctrl/Super copy/paste bindings, paste protection, and middle-click behavior exist; real OS clipboard smoke remains incomplete off Windows. |
| Linux primary selection | planned | planned | implemented | implemented | Arboard Primary selection get/set and explicit system-clipboard fallback diagnostics are wired for X11/Wayland; real compositor verification remains open. |
| OSC 52 clipboard policy | tested | implemented | implemented | implemented | Parser pending requests and bounded security policy exist; remote writes are denied by default. Remote confirmation UI and cross-OS app smoke remain open. |
| GPU surface/device path | partial | partial | partial | partial | WGPU initialization and device-loss recovery foundation exist; sleep/wake, monitor-change, and backend validation remain. |
| GPU glyph rendering | partial | partial | partial | partial | Damage-aware batched glyph/quad submission, atlas rebuild after recovery, and screenshot tooling exist; macOS/Linux baselines and cross-OS GPU validation remain open. |
| Screenshot verification | tested | partial | partial | partial | Deterministic fixtures, PPM baselines, tolerance diffing, and reports exist. Windows baselines verify on the current host; macOS/Linux X11/Linux Wayland baselines remain uncaptured. |
| Damage tracking | tested | implemented | implemented | implemented | Renderer-independent tracking exists; real GPU partial-update behavior needs hardening. |
| Frame scheduler | tested | implemented | implemented | implemented | Scheduler distinctions exist; idle behavior still needs platform profiling. |
| Font discovery/fallback | partial | partial | partial | partial | Font fallback chain exists; installed-font variance and emoji fallback remain. |
| Static TOML config | tested | implemented | implemented | implemented | Portable model and parser exist; non-Windows file-location behavior needs runtime validation. |
| Platform config overrides | tested | implemented | implemented | implemented | Model exists for macOS/Windows/Linux/X11/Wayland refinement. |
| Config validation diagnostics | tested | implemented | implemented | implemented | Validation exists; runtime UX still needs product integration. |
| Config live reload | tested | partial | partial | partial | Debounced TOML watcher, validation, live apply, and previous-config retention exist. Windows unit/desktop tests pass; macOS/Linux runtime validation remains open. |
| Config schema export | tested | implemented | implemented | implemented | Xtask helper exists. |
| Advanced programmable config | tested | implemented | implemented | implemented | Controlled `config-lua` frontend compiles deterministic `panea.*` programs into `AppConfig`, with platform conditionals, platform overrides, generated themes, keybindings, shell/SSH profiles, mux formatting, validation, and reload-plan tests. Automatic `.panea` runtime watching and non-Windows host reports remain open. |
| Benchmark CLI | tested | implemented | implemented | implemented | Repeatable local command exists; CI/platform runners remain open. |
| Renderer instrumentation | tested | implemented | implemented | implemented | CPU, submission, glyph/cache, atlas, PTY/parser throughput, memory estimate, and GPU timestamp status metrics exist; real timestamp samples need cross-OS backend validation. |
| In-window performance overlay | tested | partial | partial | partial | Developer overlay projection exists through renderer overlay primitives; polished installed toggle/UX and cross-OS visual verification remain. |
| Native mux model | tested | implemented | implemented | implemented | Workspace/tab/pane/session/layout model exists. |
| Native tabs runtime | partial | partial | partial | partial | Desktop runtime switching and basic tab chrome exist; real GUI smoke and polished tab UI remain. |
| Native panes/splits runtime | partial | partial | partial | partial | Desktop split rendering, per-pane local transports, focus, resize, zoom, and close are wired; cross-OS smoke, SSH panes, and startup layouts remain. |
| External tmux/screen/zellij compatibility | partial | partial | partial | partial | Compatibility runner records binary availability; nested PTY behavior still needs manual or future automated checks. |
| App compatibility runner | tested | partial | partial | partial | `cargo xtask compat` exists with required Windows PowerShell/cmd/protocol smoke passing on the current host. macOS, Linux X11, and Linux Wayland reports remain unverified. |
| Semantic timeline | tested | implemented | implemented | implemented | Storage and command-region model exist. |
| Semantic escape parser | tested | implemented | implemented | implemented | OSC 133, OSC 633, OSC 7, and private OSC 777 foundations exist. |
| Shell integration activation | partial | partial | partial | partial | Activation plans, desktop hook injection, config modes, and ignored real-shell tests exist; Windows PowerShell smoke passed, while bash/zsh/fish, WSL, remote, macOS, and Linux verification remain. |
| Command navigation/copy actions | tested | implemented | implemented | implemented | Semantic actions exist; desktop UX integration remains partial. |
| Prompt decorations | partial | partial | partial | partial | Overlay projection, alternate-screen suppression, and config policy exist; cross-OS visual smoke and polished UI remain. |
| Command blocks | tested | partial | partial | partial | Windows-host tests cover command-block backgrounds, input/output grouping, metadata badges, alternate-screen suppression, and renderer overlay glyph batching. Real shell-driven and cross-OS visual verification remain. |
| Static cursor styles | partial | partial | partial | partial | Config/render contracts exist; visual polish needs renderer hardening. |
| Cursor animations | tested | partial | partial | partial | Windows-host tests cover opt-in config, bounded cursor-neighborhood damage, desktop runtime wiring, and batched animation quads; cross-OS visual verification remains. |
| Animated image cursor | partial | partial | partial | partial | Opt-in config, nonblocking asset read/header decode, metadata cache, and budget warnings exist; pixel-frame decode/upload/draw and cross-OS verification remain. |
| SSH profile config | tested | implemented | implemented | implemented | Portable config model exists. |
| SSH host-key policy | tested | implemented | implemented | implemented | Security contract exists with explicit unknown/changed-host trust decisions; desktop UI remains. |
| SSH transport | partial | partial | partial | partial | Backend and `cargo xtask ssh-smoke` real-server harness exist; collected server reports remain unverified. |
| SSH secret handling | tested | partial | partial | partial | Secret/keychain provider contracts, redaction, prompt persistence flow, and platform capability reporting exist; native OS backend wiring and prompts remain. |
| SSH in mux | partial | partial | partial | partial | Session specs exist; direct SSH tab/pane runtime actions remain deferred until SSH trust/secret UI is ready. |
| Doctor diagnostics | tested | implemented | implemented | implemented | Installed `panea doctor` and `cargo xtask doctor` share one diagnostics model with human-readable and JSON output; macOS/Linux runtime output still needs host verification. |
| Bug-report snapshot | tested | implemented | implemented | implemented | Privacy-aware model exists; product export UX remains. |
| Native notifications | planned | planned | planned | planned | Not implemented. |
| Packaging artifacts | tested | implemented | implemented | implemented | `cargo xtask package` builds portable/staged packages with binary, config template/schema/examples, shell integration scripts, docs, license, manifest, packaged doctor smoke, and packaged headless shell-session smoke. Windows dev portable smoke passed on the current host; macOS app bundle and Linux portable package reports still need to be collected on those OSes. MSI/DMG/AppImage/deb/rpm remain later. |
| Release validation suite | partial | partial | partial | partial | Reports/gates exist; full cross-OS suite is unimplemented. |

## Mobile Matrix

| Capability | iOS | Notes |
| --- | --- | --- |
| Shared terminal engine reuse | tested | `apps/ios` proves shared parser/core flow in Rust tests. |
| Mobile lifecycle model | tested | Foreground, background, reconnect, and no-indefinite-background-session policy exists. |
| Touch/hardware/software keyboard contracts | partial | Rust-side model exists; native UIKit/SwiftUI integration is not implemented. |
| iOS app shell bridge | partial | Rust-side native bridge traits exist for lifecycle, frame requests, diagnostics, host-key decisions, and secret prompts; UIKit/SwiftUI host is not implemented. |
| iOS rendering surface | partial | `IosGpuSurfaceSpec` records backend readiness, damage-driven redraw policy, and idle redraw prohibition; no native GPU surface exists yet. |
| iOS SSH profile mapping | tested | Portable SSH profile maps into a mobile session spec. |
| iOS SSH profile UI | partial | Profile form validation and connection planning exist; native editing UI and key import UX are not implemented. |
| iOS Keychain provider | partial | iOS Keychain capability handoff exists and reports unavailable until native provider wiring is added. |
| iOS host-key approval UI | partial | Trust prompt model defaults to reject and flags changed keys; native approval UI is not implemented. |
| iOS simulator/device validation | planned | Device checklist exists; no simulator or physical-device run has been collected. |

## Current Cross-OS Verification Result

| Platform | Overall status | Why |
| --- | --- | --- |
| Windows | partial | Current host build/test/lint, local PTY smoke, required compatibility runner smoke, SSH smoke harness compilation, installed doctor smoke, cross-OS runner implementation, and Windows portable package doctor smoke have been verified, but renderer GUI, optional app matrix, a real SSH server report, installer packaging, and full daily-driver workflows are incomplete. |
| macOS | partial | Platform-neutral code and a macOS CI runner definition exist, but runtime behavior and verification reports are not yet collected. |
| Linux X11 | partial | Platform-neutral code, X11 strategy, and a Linux X11 runner definition exist, but real X11 window manager/compositor behavior and reports are unverified. |
| Linux Wayland | partial | Platform-neutral code, Wayland strategy, and a Linux Wayland runner definition exist, but real compositor behavior and reports are unverified. |

## Completion Rule

A row may move to `cross-os verified` only after it is tested on Windows,
macOS, Linux X11, and Linux Wayland. If exact parity is blocked, the row must
also document the capability fallback and doctor diagnostic that explains it.
