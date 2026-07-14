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
| Parser/xterm baseline | tested | implemented | implemented | implemented | ANSI/VT controls, modes, DEC line graphics, bounded OSC/DCS strings, tmux passthrough, and DA/DSR tests exist; full interactive app verification remains separate. |
| Fuzz/property harness | tested | implemented | implemented | implemented | cargo-fuzz targets and proptest smoke tests exist; scheduled CI runs smoke properties, but long-running fuzz history has not accumulated yet. |
| Scrollback/alternate screen/resize | tested | implemented | implemented | implemented | Core behavior exists; full app smoke remains open per OS. |
| Selection extraction | tested | implemented | implemented | implemented | Absolute-buffer normal and rectangular extraction, pane-aware mouse and keyboard selection, and renderer selection overlays exist; real off-Windows UX verification remains. |
| Unicode/grapheme cell and render model | tested | implemented | implemented | implemented | Core grapheme/cell tests plus OpenType shaping, fallback, style-face, ligature, and color-emoji renderer tests exist; cross-OS installed-font screenshots remain open. |
| Local PTY transport | tested | partial | partial | partial | Windows ConPTY smoke passed; macOS/Linux real PTY smoke is unverified. |
| Local PTY lifecycle contract | tested | implemented | implemented | implemented | Shared bounded lifecycle exists; non-Windows real shell validation remains open. |
| Windows PowerShell/cmd/WSL profiles | partial | planned | planned | planned | Windows profile groundwork exists; WSL smoke is unverified. |
| Desktop window creation | partial | partial | partial | partial | Winit native path and explicit Linux X11/Wayland builders exist; real OS/window manager behavior needs validation. |
| Window modes | partial | partial | partial | partial | Windowed/maximized/exclusive/borderless/frameless runtime states and explicit fallback diagnostics exist; cross-OS behavior is not verified. |
| Window padding/margins/opacity | tested | implemented | implemented | implemented | Pixel insets affect grid sizing, PTY resize, rendering, mouse mapping, and live reload. Opacity requests transparent WGPU composition and diagnoses an opaque fallback; real macOS/Linux compositor verification remains open. |
| Linux backend selection | planned | planned | implemented | implemented | `auto`, `x11`, and `wayland` call winit's backend-specific event-loop builder; real compositor verification is open. |
| Linux compositor verification matrix | planned | planned | tested | tested | Target matrix, runtime environment snapshot, fallback checklist, and `cargo xtask linux-compositor` exist; actual Linux host runs remain unverified. |
| Decoration strategy | implemented | implemented | implemented | implemented | Requested/effective/fallback diagnostics and native/none behavior exist; custom/client/server requests degrade explicitly where exact control is unavailable. |
| Emergency restore shortcuts | partial | partial | partial | partial | Actions/keybinding concepts exist; full titlebarless UX validation remains open. |
| Keyboard input translation | tested | implemented | implemented | implemented | Shared terminal encoding covers text/control input, navigation/editing/function/keypad keys, xterm modifiers, AltGr preservation, and application cursor/keypad modes; real layout, Command/Option, and IME testing remains off Windows. |
| Keyboard and mouse bindings | tested | implemented | implemented | implemented | Portable modifier/gesture matching and validated actions drive keyboard, selection, copy/paste, URL, wheel, and primary-selection behavior while preserving application mouse protocol priority. Real non-Windows input verification remains open. |
| Themes and terminal colors | tested | implemented | implemented | implemented | Built-in profiles compile once into AppConfig; explicit values win. Foreground/background, cursor/text, selection, configurable ANSI-16, indexed-256, and truecolor paths are implemented. Cross-OS screenshot baselines remain open. |
| Mouse input translation | tested | implemented | implemented | implemented | Normal, button-motion, all-motion, SGR encoding, focus reports, pane-aware selection, and scrollback wheel navigation exist; real cross-OS protocol and selection UX verification remains. |
| IME/composed text | implemented | implemented | implemented | implemented | Winit preedit/commit events are enabled; preedit draws as an overlay and only committed text enters the PTY. Native-host verification remains incomplete. |
| System clipboard | tested | implemented | implemented | implemented | Clipboard bridge, portable Ctrl/Super copy/paste bindings, paste protection, and middle-click behavior exist; real OS clipboard smoke remains incomplete off Windows. |
| Linux primary selection | planned | planned | implemented | implemented | Arboard Primary selection get/set and explicit system-clipboard fallback diagnostics are wired for X11/Wayland; real compositor verification remains open. |
| OSC 52 clipboard policy | tested | implemented | implemented | implemented | Parser pending requests, bounded security policy, and one-time renderer confirmation overlay exist; remote writes remain denied by default and cross-OS app smoke remains open. |
| GPU surface/device path | partial | partial | partial | partial | WGPU initialization and device-loss recovery foundation exist; sleep/wake, monitor-change, and backend validation remain. |
| GPU glyph rendering | partial | tested | partial | partial | Persistent growable buffers, retained-frame damage, OpenType run caching, fallback, RGBA color/monochrome atlas reuse, recovery, and screenshot tooling exist; macOS/Linux baselines and cross-OS GPU validation remain open. |
| Screenshot verification | tested | partial | partial | partial | Deterministic fixtures, PPM baselines, tolerance diffing, and reports exist. Windows baselines verify on the current host; macOS/Linux X11/Linux Wayland baselines remain uncaptured. |
| Damage tracking | tested | implemented | implemented | implemented | Renderer-independent tracking exists; real GPU partial-update behavior needs hardening. |
| Frame scheduler | tested | implemented | implemented | implemented | Scheduler distinctions exist; idle behavior still needs platform profiling. |
| Font discovery/fallback | partial | tested | partial | partial | Configured/system per-grapheme fallback, size/line-height/ligature control, real style faces, CJK/emoji candidates, COLR/bitmap color rendering, and doctor source diagnostics exist; installed-font variance still needs non-Windows reports. |
| Static TOML config | tested | implemented | implemented | implemented | Portable model and parser exist; non-Windows file-location behavior needs runtime validation. |
| Platform config overrides | tested | implemented | implemented | implemented | Model exists for macOS/Windows/Linux/X11/Wayland refinement, including window, font, colors, cursor, shell, performance, visuals, clipboard, and diagnostics. |
| Config validation diagnostics | tested | implemented | implemented | implemented | Validation exists; runtime UX still needs product integration. |
| Config live reload | tested | partial | partial | partial | Debounced TOML and programmable watchers, validation, live apply, and previous-config retention exist. Windows unit/desktop tests pass; macOS/Linux runtime validation remains open. |
| Config schema export | tested | implemented | implemented | implemented | Xtask helper exists. |
| Advanced programmable config | tested | implemented | implemented | implemented | Controlled `config-lua` frontend compiles deterministic `panea.*` programs into `AppConfig`, with platform conditionals/overrides, generated themes, bindings, profiles, formatting, validation, and automatic safe reload. Non-Windows host reports remain open. |
| Benchmark CLI | tested | implemented | implemented | implemented | Repeatable local command exists; CI/platform runners remain open. |
| Renderer instrumentation | tested | implemented | implemented | implemented | CPU, submission, glyph/cache, atlas, PTY/parser throughput, memory estimate, and GPU timestamp status metrics exist; real timestamp samples need cross-OS backend validation. |
| In-window performance overlay | tested | partial | partial | partial | Renderer-only projection reports CPU/GPU/frame/cache/damage/throughput/memory/profile/power data with compact/detailed modes, four placements, click controls, live config and persisted runtime preferences; cross-OS visual verification remains. |
| Battery-aware performance policy | tested | implemented | implemented | implemented | Shared provider contract and native battery backends apply reversible optional-work caps outside hot paths; current-host Windows unit/runtime detection is verified, native macOS/Linux reports remain. |
| Native mux model | tested | implemented | implemented | implemented | Workspace/tab/pane/session/layout model exists. |
| Native tabs/workspaces runtime | tested | partial | partial | partial | Windows-host tests cover workspace/tab lifecycle, clickable configurable tab chrome, drag reorder, renderer target feedback, startup layouts and fresh-process restore; real GUI and non-Windows smoke remain. |
| Native panes/splits runtime | tested | partial | partial | partial | Windows-host tests cover nested layouts, per-pane local/SSH ownership, focus, resize, zoom, keyboard move/swap, modifier-drag swaps, close, transport resize and pane-aware rendering; real GUI/non-Windows smoke remains. |
| External tmux/screen/zellij compatibility | partial | partial | partial | partial | Compatibility runner records binary availability; nested PTY behavior still needs manual or future automated checks. |
| App compatibility runner | tested | partial | partial | partial | `cargo xtask compat` exists with required Windows PowerShell/cmd/protocol smoke passing on the current host. macOS, Linux X11, and Linux Wayland reports remain unverified. |
| Semantic timeline | tested | implemented | implemented | implemented | Storage and command-region model exist. |
| Semantic escape parser | tested | implemented | implemented | implemented | OSC 133, OSC 633, OSC 7, and private OSC 777 foundations exist. |
| Shell integration activation | tested | partial | partial | partial | Complete marker scripts, desktop injection, config modes and bounded real-shell tests exist; PowerShell is verified on Windows while bash/zsh/fish, WSL, remote, macOS and Linux verification remain. |
| Command navigation/copy actions | tested | implemented | implemented | implemented | Active-pane navigation, raw output selection, output copy, and command-plus-output copy are wired and unit tested. |
| Prompt decorations | tested | implemented | implemented | implemented | Shared overlay/config path supports separators, rounded boxes, pill headers, real shell/cwd/remote/elevated badges, and previous-status accents. Windows-host automated tests pass; non-Windows visual smoke remains. |
| Command blocks | tested | implemented | implemented | implemented | Shared overlay/config path supports traditional, subtle, card, split, minimal-header, and custom styles; grouping, configurable borders/spacing/badges, status styling, raw-copy actions, and presentation-only output collapse. Windows-host automated tests pass; real non-Windows shell-driven and cross-OS visual verification remain. |
| Static cursor styles | tested | implemented | implemented | implemented | Block, beam, underline, and hollow block rendering, thickness, rounded geometry, colors, deterministic blink, inactive and terminal-mode styles, retained-frame-safe cursor damage, and config validation are implemented. Real macOS/Linux visual verification and user-authored custom geometry remain open. |
| Cursor animations | tested | partial | partial | partial | Windows-host tests cover opt-in config, bounded cursor-neighborhood damage, desktop runtime wiring, and batched animation quads; cross-OS visual verification remains. |
| Animated image cursor | partial | tested | partial | partial | User GIF and static PNG assets use off-thread bounded pixel decode, immutable frame caching, one-time GPU texture-array upload, textured drawing, and local damage; macOS/Linux interactive visual verification remains. |
| Static vector cursor | partial | tested | partial | partial | Versioned data-only normalized JSON is strictly validated, size/count bounded, worker-compiled, immutable-cached, GPU-quad batched, and cursor-locally damaged; macOS/Linux interactive visual verification remains. |
| SSH profile config | tested | implemented | implemented | implemented | Portable config model exists. |
| SSH host-key policy | tested | implemented | implemented | implemented | Unknown/changed-host decisions use a masked desktop security modal; pinned mismatches remain blocking. Real-server reports remain. |
| SSH transport | partial | partial | partial | partial | Backend and `cargo xtask ssh-smoke` real-server harness exist; collected server reports remain unverified. |
| SSH secret handling | tested | implemented | implemented | implemented | Masked password/passphrase prompts and opt-in Windows Credential Manager/macOS Keychain/Linux Secret Service persistence are wired; real native-host auth reports remain. |
| SSH in mux | tested | implemented | implemented | implemented | Nonblocking SSH tabs/panes provide remote resize, trust/auth UI, semantic context, disconnect status, preserved scrollback and explicit reconnect. Real GUI/server reports remain. |
| Doctor diagnostics | tested | implemented | implemented | implemented | Installed `panea doctor` and `cargo xtask doctor` share one diagnostics model with human-readable and JSON output; macOS/Linux runtime output still needs host verification. |
| Bug-report snapshot | tested | implemented | implemented | implemented | Privacy-aware model exists; product export UX remains. |
| Native notifications | tested | implemented | implemented | implemented | Lazy bounded delivery uses Windows toast, macOS Notification Center, or freedesktop D-Bus behind one provider contract. Current-host contract/routing tests pass; real native permission and delivery reports remain open. |
| Packaging artifacts | tested | implemented | implemented | implemented | Windows ZIP/installer, macOS app/ZIP/DMG, Linux tarball/deb/AppImage/RPM, signing/notarization hooks, and bounded doctor/shell/GUI smokes exist. Windows staged and temporary-installed full smoke passes; credentialed signing and non-Windows artifact reports remain. |
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
| Windows | partial | Current-host build/test/lint, PTY, compatibility, doctor, and rebuilt staged/installed package lifecycle including terminal-I/O GPU GUI smoke are verified. Optional app matrix, real SSH report, credentialed signing, and alpha usage remain. |
| macOS | partial | Platform-neutral code and a macOS CI runner definition exist, but runtime behavior and verification reports are not yet collected. |
| Linux X11 | partial | Platform-neutral code, X11 strategy, and a Linux X11 runner definition exist, but real X11 window manager/compositor behavior and reports are unverified. |
| Linux Wayland | partial | Platform-neutral code, Wayland strategy, and a Linux Wayland runner definition exist, but real compositor behavior and reports are unverified. |

## Completion Rule

A row may move to `cross-os verified` only after it is tested on Windows,
macOS, Linux X11, and Linux Wayland. If exact parity is blocked, the row must
also document the capability fallback and doctor diagnostic that explains it.
