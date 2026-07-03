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
| Terminal core grid/state | tested | implemented | implemented | implemented | Platform-neutral code exists; runtime app verification still needed off Windows. |
| Parser baseline | tested | implemented | implemented | implemented | Lower-level tests exist; app-level compatibility suite is not complete. |
| Fuzz/property harness | tested | implemented | implemented | implemented | cargo-fuzz targets and proptest smoke tests exist; scheduled CI runs smoke properties, but long-running fuzz history has not accumulated yet. |
| Scrollback/alternate screen/resize | tested | implemented | implemented | implemented | Core behavior exists; full app smoke remains open per OS. |
| Selection extraction | tested | implemented | implemented | implemented | Raw extraction exists; mouse-driven selection UX and platform clipboards remain partial. |
| Unicode/grapheme cell model | tested | implemented | implemented | implemented | Platform-neutral parser/core tests cover split UTF-8, combining marks, CJK width, emoji modifiers, ZWJ emoji, variation selectors, selection, cursor movement, resize, and scrollback; cross-OS renderer/font screenshots remain open. |
| Local PTY transport | tested | partial | partial | partial | Windows ConPTY smoke passed; macOS/Linux real PTY smoke is unverified. |
| Local PTY lifecycle contract | tested | implemented | implemented | implemented | Shared bounded lifecycle exists; non-Windows real shell validation remains open. |
| Windows PowerShell/cmd/WSL profiles | partial | planned | planned | planned | Windows profile groundwork exists; WSL smoke is unverified. |
| Desktop window creation | partial | partial | partial | partial | Winit path exists; real OS/window manager behavior needs validation. |
| Window modes | partial | partial | partial | partial | Windowed/maximized/fullscreen/frameless states are modeled; cross-OS behavior not verified. |
| Linux backend selection | planned | planned | partial | partial | X11/Wayland preferences and diagnostics are modeled; real compositor verification is open. |
| Decoration strategy | partial | partial | partial | partial | Requested/effective diagnostics exist; Linux negotiation needs real tests. |
| Emergency restore shortcuts | partial | partial | partial | partial | Actions/keybinding concepts exist; full titlebarless UX validation remains open. |
| Keyboard input translation | partial | partial | partial | partial | Platform-neutral events exist; layout, AltGr, Command/Option, and IME testing remain. |
| Mouse input translation | partial | partial | partial | partial | Events and terminal reporting groundwork exist; selection UX and full protocol coverage remain. |
| IME/composed text | partial | partial | partial | partial | Event contract exists; real composed-input verification is not complete. |
| System clipboard | partial | partial | partial | partial | Clipboard bridge exists; primary selection, OSC 52, and security policy remain. |
| OSC 52 clipboard | planned | planned | planned | planned | Not implemented. |
| GPU surface/device path | partial | partial | partial | partial | WGPU initialization and device-loss recovery foundation exist; sleep/wake, monitor-change, and backend validation remain. |
| GPU glyph rendering | partial | partial | partial | partial | Damage-aware batched glyph/quad submission, atlas rebuild after recovery, and screenshot tooling exist; macOS/Linux baselines and cross-OS GPU validation remain open. |
| Screenshot verification | tested | partial | partial | partial | Deterministic fixtures, PPM baselines, tolerance diffing, and reports exist. Windows baselines verify on the current host; macOS/Linux X11/Linux Wayland baselines remain uncaptured. |
| Damage tracking | tested | implemented | implemented | implemented | Renderer-independent tracking exists; real GPU partial-update behavior needs hardening. |
| Frame scheduler | tested | implemented | implemented | implemented | Scheduler distinctions exist; idle behavior still needs platform profiling. |
| Font discovery/fallback | partial | partial | partial | partial | Font fallback chain exists; installed-font variance and emoji fallback remain. |
| Static TOML config | tested | implemented | implemented | implemented | Portable model and parser exist; non-Windows file-location behavior needs runtime validation. |
| Platform config overrides | tested | implemented | implemented | implemented | Model exists for macOS/Windows/Linux/X11/Wayland refinement. |
| Config validation diagnostics | tested | implemented | implemented | implemented | Validation exists; runtime UX still needs product integration. |
| Config live reload | partial | partial | partial | partial | Reload impact classification exists; file watcher/applier is unimplemented. |
| Config schema export | tested | implemented | implemented | implemented | Xtask helper exists. |
| Advanced programmable config | stubbed | stubbed | stubbed | stubbed | `config-lua` placeholder exists; implementation is deferred. |
| Benchmark CLI | tested | implemented | implemented | implemented | Repeatable local command exists; CI/platform runners remain open. |
| Renderer instrumentation | tested | implemented | implemented | implemented | CPU and submission metrics exist; GPU timestamp queries remain. |
| In-window performance overlay | partial | partial | partial | partial | Text/diagnostic overlay model exists; polished installed overlay remains. |
| Native mux model | tested | implemented | implemented | implemented | Workspace/tab/pane/session/layout model exists. |
| Native tabs runtime | partial | partial | partial | partial | Model/actions exist; desktop tab chrome and runtime switching need completion. |
| Native panes/splits runtime | partial | partial | partial | partial | Split tree exists; full rendering and per-pane transport orchestration are deferred. |
| External tmux/screen/zellij compatibility | partial | partial | partial | partial | Architecture preserves compatibility; real app suite is not automated. |
| Semantic timeline | tested | implemented | implemented | implemented | Storage and command-region model exist. |
| Semantic escape parser | tested | implemented | implemented | implemented | OSC 133, OSC 633, OSC 7, and private OSC 777 foundations exist. |
| Shell integration scripts | partial | partial | partial | partial | bash/zsh/fish/PowerShell scripts exist; runtime activation and real-shell verification remain. |
| Command navigation/copy actions | tested | implemented | implemented | implemented | Semantic actions exist; desktop UX integration remains partial. |
| Prompt decorations | partial | partial | partial | partial | Overlay contracts and basic generation exist; polished UI remains. |
| Command blocks | partial | partial | partial | partial | Semantic model and overlays exist; product-complete UI and real shell verification remain. |
| Static cursor styles | partial | partial | partial | partial | Config/render contracts exist; visual polish needs renderer hardening. |
| Cursor animations | partial | partial | partial | partial | Budget/config contracts exist; polished animation runtime remains. |
| Animated image cursor | planned | planned | planned | planned | Not implemented; intentionally deferred. |
| SSH profile config | tested | implemented | implemented | implemented | Portable config model exists. |
| SSH host-key policy | tested | implemented | implemented | implemented | Security contract exists; interactive UI remains. |
| SSH transport | partial | partial | partial | partial | Backend exists; real server smoke tests are unverified. |
| SSH secret handling | partial | partial | partial | partial | Secret-provider interface exists; OS keychain providers and prompts remain. |
| SSH in mux | partial | partial | partial | partial | Session specs exist; runtime tab/pane actions remain. |
| Doctor diagnostics | tested | implemented | implemented | implemented | Shared diagnostics and xtask commands exist; installed product command remains. |
| Bug-report snapshot | tested | implemented | implemented | implemented | Privacy-aware model exists; product export UX remains. |
| Native notifications | planned | planned | planned | planned | Not implemented. |
| Packaging artifacts | stubbed | stubbed | stubbed | stubbed | Plans/docs exist; real installers/packages do not. |
| Release validation suite | partial | partial | partial | partial | Reports/gates exist; full cross-OS suite is unimplemented. |

## Mobile Matrix

| Capability | iOS | Notes |
| --- | --- | --- |
| Shared terminal engine reuse | tested | `apps/ios` proves shared parser/core flow in Rust tests. |
| Mobile lifecycle model | tested | Foreground, background, reconnect, and no-indefinite-background-session policy exists. |
| Touch/hardware/software keyboard contracts | partial | Rust-side model exists; native UIKit/SwiftUI integration is not implemented. |
| iOS rendering surface | planned | No native GPU surface exists yet. |
| iOS SSH profile mapping | tested | Portable SSH profile maps into a mobile session spec. |
| iOS Keychain provider | planned | Not implemented. |
| iOS host-key approval UI | planned | Not implemented. |
| iOS simulator/device validation | planned | Not run. |

## Current Cross-OS Verification Result

| Platform | Overall status | Why |
| --- | --- | --- |
| Windows | partial | Current host build/test/lint and local PTY smoke have been verified, but renderer GUI, app compatibility, SSH server, packaging, and full daily-driver workflows are incomplete. |
| macOS | partial | Platform-neutral code is implemented, but runtime behavior is unverified on macOS. |
| Linux X11 | partial | Platform-neutral code and X11 strategy exist, but real X11 window manager/compositor behavior is unverified. |
| Linux Wayland | partial | Platform-neutral code and Wayland strategy exist, but real compositor behavior is unverified. |

## Completion Rule

A row may move to `cross-os verified` only after it is tested on Windows,
macOS, Linux X11, and Linux Wayland. If exact parity is blocked, the row must
also document the capability fallback and doctor diagnostic that explains it.
