# Current Status

This document is the Phase 0 current-state freeze. It records what is real,
partial, stubbed, and unimplemented so future work starts from facts rather
than phase-name assumptions.

Read this with [architecture.md](../architecture.md),
[implementation.md](../implementation.md), and
[Engineering rules](engineering-rules.md). The implementation phase list shows
foundation work that exists; this file states whether that work is product
complete.

## Status Labels

| Label | Meaning |
| --- | --- |
| planned | Accepted by architecture or roadmap, but not implemented. |
| stubbed | A crate, type, placeholder, or doc exists, but no meaningful runtime behavior exists. |
| partial | Meaningful foundation exists, but product behavior, runtime wiring, coverage, or platform verification is incomplete. |
| implemented | The feature has usable behavior in the codebase, but is not fully verified across all target platforms. |
| tested | Automated tests, smoke tests, or local manual verification have run for the stated scope. |
| cross-os verified | Verified on Windows, macOS, Linux X11, and Linux Wayland, with documented fallbacks where needed. |

No current feature should be treated as `cross-os verified`.

## Verification Baseline

- Current active host: Windows.
- Workspace build/test/lint has been verified on the current Windows host during
  earlier implementation phases.
- Windows local PTY/ConPTY one-shot, interactive, and event-loop smoke tests
  were verified during lifecycle hardening.
- macOS, Linux X11, and Linux Wayland runtime behavior is unverified unless a
  future entry explicitly says otherwise.
- iOS simulator and device behavior is unverified.
- `INTERNAL_TODO.md` tracks uncommitted internal deferred work and is ignored by
  git.

## What Currently Works

These areas have concrete implementation and local verification for their
stated scope:

| Area | Status | What is real |
| --- | --- | --- |
| Rust workspace | tested | Workspace members compile under the existing standard gates on the Windows host. |
| Layer skeleton | tested | Crates exist for core, parser, renderer, transport, platform, config, mux, semantics, diagnostics, security, assets, desktop, iOS, xtask, and bench. |
| Architecture boundary enforcement | tested | `cargo xtask layer-check` validates allowed workspace dependencies, `cargo xtask ci` runs it, and GitHub Actions runs the architecture boundary subset on Windows, macOS, and Ubuntu. |
| Terminal core baseline | tested | Grid, cells, cursor, anchored scrollback viewport, alternate screen, resize, modes, absolute normal/rectangular selection extraction, terminal key encoding, and baseline golden coverage exist. |
| Unicode/grapheme cell model | tested | UTF-8 scalar buffering, grapheme clustering, combining marks, wide CJK cells, emoji modifiers, ZWJ emoji, variation selectors, selection, cursor movement, overwrite/delete/erase, resize, and scrollback tests exist in `term-core` and `term-parser`. |
| Parser compatibility | tested | ANSI/VT parser handles common controls, SGR colors/styles, origin/autowrap/insert modes, alternate screen, scroll regions, index/reverse-index, insert/delete/erase/repeat, tab controls, DEC line graphics, title/clipboard OSC, bounded string controls, tmux DCS passthrough, mouse/focus/bracketed-paste modes, and DA/DSR responses. |
| Fuzzing harness | tested | `fuzz/` contains cargo-fuzz targets for parser, grid, resize, Unicode, selection, OSC/DCS, and shell markers; property smoke tests run through `cargo xtask fuzz-smoke` and scheduled CI. |
| App compatibility runner | tested | `cargo xtask compat` lists and runs bounded process/PTY compatibility probes, writes reports under `target/compatibility`, and the required Windows PowerShell/cmd/protocol subset passed on the current host. |
| Screenshot verification runner | tested | Deterministic renderer fixtures, PPM capture, tolerance-based diffing, Windows baselines, and `cargo xtask screenshot verify --platform windows` exist. |
| Linux compositor verification matrix | tested | Target matrix, fallback checklist, runtime environment snapshot, and `cargo xtask linux-compositor` exist; real Linux host verification remains open. |
| Config model and TOML | tested | `AppConfig` defaults, schema-v2 migration/rejection policy, TOML parsing, unknown/deprecated diagnostics, validation, platform overrides, deterministic visual/performance profile expansion, complete baseline color/font/window/input customization, generation/schema export, debounced file watching, safe live apply, and previous-config retention exist. |
| Programmable config | tested | `config-lua` provides a controlled deterministic `panea.*` frontend that compiles into `AppConfig`, supports generated themes, platform conditionals/overrides, keybindings, profiles and formatting, and now participates in debounced desktop live reload without entering hot paths. |
| Windows local transport | tested | Portable PTY/ConPTY lifecycle is bounded; Windows smoke tests were made non-hanging and observed output. |
| Diagnostics foundations | tested | Installed `panea doctor ...`, `cargo xtask doctor ...`, JSON doctor output, bug-report snapshots, release/security/hardening/package readiness reports, and iOS readiness reports exist through shared diagnostics models. |
| Performance harness foundation | tested | `cargo xtask bench ...` and `tools/bench` fixtures exist for repeatable local measurements. |
| Performance instrumentation overlay | tested | Shared instrumentation now reports frame/CPU/GPU timing status, glyph cache and atlas occupancy, damage/draw counts, active animations, idle wakeups, PTY/parser throughput, and memory estimates; the desktop app can draw a developer overlay through renderer overlay primitives. |
| Mux model | tested | Workspace, window, tab, pane, session, split tree, restore snapshot, and mux action models exist with unit coverage. |
| Desktop mux runtime | tested | Independent local/SSH pane runtimes, workspaces, tabs, nested splits, focus/resize/zoom/move/swap/close, clickable configurable tab chrome, pane borders, startup layouts, fresh-process session restoration, PTY resize, and active-pane input routing are wired. Cross-OS GUI smoke remains separate. |
| Semantic model and runtime actions | tested | Incrementally positioned OSC regions, command blocks, navigation, raw output selection/copy, exit status, measured duration, cwd, shell and remote metadata are wired per pane without buffer mutation. |
| Shell integration activation foundation | tested | Portable activation plans, config modes, desktop startup hook injection for bash/zsh/fish/PowerShell, disabled/manual/heuristic/off behavior, and ignored real-shell verification tests exist; PowerShell semantic smoke passed on the current Windows host. |
| Command block visual overlay foundation | tested | Desktop scene projection now creates command-block backgrounds, input/output grouping overlays, status/duration/cwd/shell/host badges, conservative alternate-screen suppression, and renderer-batched overlay label glyphs without mutating terminal cells. |
| Cursor animation foundation | tested | Opt-in cursor animation config, cursor-neighborhood animation damage, batched renderer animation quads, nonblocking image cursor asset metadata cache, and budget validation exist; full image frame decode/upload and cross-OS visual smoke remain open. |
| Clipboard/OSC 52 policy | tested | Portable `clipboard` config, paste protection, bracketed paste forwarding, middle-click paste suppression during mouse reporting, parser pending OSC 52 requests, bounded security policy, local allow/default remote deny behavior, and TOML/security/parser/app tests exist. |
| SSH transport foundation | tested | SSH profile mapping, explicit host-trust contracts, host-key policy enforcement, secret/keychain-provider boundaries, SSH2 transport, remote PTY request, resize, and shutdown foundations exist. |
| SSH trust and secret contracts | tested | Unknown-host decisions, changed-host replacement actions, redacted secret prompts, keychain-backed secret lookup/prompt/store flow, and platform keychain capability reporting exist. |
| SSH real-server smoke harness | tested | `cargo xtask ssh-smoke` uses the real `transport-ssh` backend, explicit trust providers, smoke-owned known-hosts storage, remote PTY output polling, resize, reconnect, changed-host detection, and remote OSC 52 policy checks. Real server reports still need to be collected per OS. |
| Cross-OS verification runner | tested | `cargo xtask verify-os` composes architecture, unit, parser, Unicode, fuzz-smoke, renderer, config, clipboard, shell, PTY, screenshot, compatibility, doctor, Linux compositor, SSH, and package-smoke checks into platform-stamped markdown/JSON reports. GitHub Actions defines Windows, macOS, Linux X11, and Linux Wayland jobs. |
| Packaging artifact runner | tested | `cargo xtask package` plans, builds, and smokes portable/staged desktop packages. The Windows dev portable package was staged on the current host; packaged `panea.exe doctor --json` and packaged `panea shell-smoke --json` both passed. macOS/Linux package reports remain uncollected. |
| iOS shared-engine foundation | tested | `apps/ios` reuses shared parser/core/config/transport/semantic/render contracts and models lifecycle, input, safe-area sizing, mobile SSH session specs, native bridge contracts, SSH profile forms, trust prompt models, Keychain capability handoff, renderer surface specs, and device checklist cases. |

## What Is Partial

These areas are real foundations but must not be called complete:

| Area | Status | Missing before completion |
| --- | --- | --- |
| Desktop app runtime | partial | Full app lifecycle, polished UI chrome, complete mux integration, installer-grade packaging, and cross-OS manual validation. |
| Platform windowing | partial | Real macOS lifecycle, real Linux X11/Wayland compositor behavior, decoration negotiation, IME validation, native notifications, and platform-specific fallback verification. |
| GPU renderer | tested | WGPU setup, persistent growable GPU batches, retained-frame damage rendering with full-draw fallback, incremental desktop damage projection, shaped-run/glyph/RGBA emoji atlas caching, row-scoped uploads, low-idle scheduling, benchmarks, device-loss backend recreation, screenshot infrastructure, and GPU timing plumbing exist; real sleep/wake/monitor-loss validation, macOS/Linux baselines, and cross-OS render validation remain. |
| Font and text rendering | tested | OpenType shaping, per-grapheme configured/system fallback, real regular/bold/italic/bold-italic face resolution, CJK/combining/ligature/emoji shaping, COLR/bitmap color glyph rasterization, RGBA atlas batching, run caching, and portable font diagnostics pass automated tests on Windows. |
| Unicode support | tested | Core/parser grapheme correctness and renderer shaping/fallback/color-glyph paths are covered by automated tests; real installed-font and screenshot/app parity still require macOS/Linux host reports. |
| Input, clipboard, selection, and scrollback UX | tested | Shared key/application-mode encoding, mouse/focus protocols, absolute normal/rectangular mouse and keyboard selection, selection overlays, anchored wheel/page scrollback, interactive search, portable HTTP(S) URL activation, keyboard copy/paste, Linux primary-selection provider behavior, paste protection, bracketed paste, middle-click paste, and OSC 52 policy exist. Real macOS/Linux runtime verification, remote OSC 52 confirmation UI, and full app compatibility coverage remain. |
| Baseline compatibility | tested | The xterm-256color protocol implementation and Windows required compatibility smoke are tested; full interactive evidence for editors, pagers, TUIs, tmux, screen, zellij, SSH, WSL, and all target OSes remains tracked by app/cross-OS verification. |
| Shell integration | partial | Local runtime activation planning and desktop injection exist for supported shells, with Windows PowerShell semantic smoke verified. Remote install flows, heuristic command detection, WSL-specific coverage, and real bash/zsh/fish/macOS/Linux session verification remain. |
| Visual overlays | tested | Prompt separators/boxes/pills, real metadata badges and status accents, distinct command/grouping styles, viewport-correct command cards, presentation-only output collapse/expand, configured spacing/borders/colors, alternate-screen suppression, damage tracking, and batched overlay glyphs pass Windows-host automated tests. Full image cursor frame upload/draw, real non-Windows shell-driven verification, and cross-OS visual verification remain separate work. |
| Native mux runtime | tested | Runtime workspaces, tabs, nested local/SSH panes, startup/restored layouts and configurable appearance are implemented. Drag UI, reconnect UI, cross-OS GUI runs and automated nested external-mux runs remain unverified/deferred. |
| SSH UX and security | partial | Provider contracts and real-server smoke harness exist, but desktop host-key approval UI, changed-host-key resolution UI, password/passphrase prompt UI, native OS keychain backend wiring, reconnect UI, proxy jump, and collected real-server reports remain. |
| Performance reporting | partial | GPU timestamp query wiring and a developer in-window overlay exist, but real timestamp samples across hardware/backends, polished installed overlay UX, CI regression gates, and reproducible cross-machine benchmark reporting remain. |
| Hardening/release readiness | partial | GPU recovery, crash-safe config reload, and portable/staged packaging foundations exist, but real device-loss platform validation, installer/AppImage/DMG artifacts, validation suite automation, and platform lab coverage remain. |
| iOS companion | partial | Shared-engine contracts, native bridge boundaries, mobile connection planning, renderer surface specs, and device checklist exist. Native UIKit/SwiftUI shell, iOS GPU surface implementation, Keychain provider backend, host-key approval UI, key import UX, simulator/device validation, and packaging remain. |

## What Is Stubbed

These areas exist mostly as placeholders, contracts, or documentation:

| Area | Status | Current shape |
| --- | --- | --- |
| `tools/conformance` | stubbed | Directory and README exist; full terminal conformance fixture suite is not built out. |
| Packaging installers | stubbed | Portable/staged package directories exist; Windows installer, macOS DMG/zip/signing/notarization, Linux AppImage/deb/rpm, and terminfo installation remain unimplemented. |
| Native notifications | stubbed | Tracked in the platform matrix as not implemented. |
| iOS app shell | stubbed | Rust shared-engine crate and native bridge traits exist; no UIKit/SwiftUI mobile app host exists yet. |
| Advanced config import/helpers | stubbed | Accepted by rollout rules, but no product implementation exists. |

## What Is Unimplemented

The following major accepted features have no complete product behavior yet:

- Cross-OS verification reports still need to be collected and reviewed for
  macOS, Linux X11, and Linux Wayland. The runners and CI jobs exist, but
  product-level platform validation is not complete until those reports pass
  on real target hosts and missing/blocked evidence is resolved.
- Real Linux compositor verification runs for GNOME/Mutter, KDE/KWin,
  wlroots/Sway, Hyprland class, tiling window managers, and X11 window managers.
- Long-running coverage-guided fuzz history and crash-regression backlog from
  real-world fuzz findings.
- Cross-OS installed-font, shaping, color-emoji, screenshot, and real-app
  evidence for the implemented text renderer.
- macOS, Linux X11, and Linux Wayland screenshot baseline capture and
  verification.
- Cross-OS runtime verification of the batched GPU glyph renderer.
- Real GPU device-loss validation for sleep/wake, monitor attach/detach, DPI
  changes, and backend failure simulation across desktop OSes.
- Remote OSC 52 confirmation UI and real Linux primary-selection compositor verification.
- Native OS watcher backends and real macOS/Linux runtime reload validation.
- Cross-OS GUI smoke and interaction polish for the implemented desktop
  tabs/panes/sessions/workspaces runtime.
- Remote shell integration install flows and real bash/zsh/fish/macOS/Linux
  shell verification.
- Full animated image cursor pixel-frame decode/upload/draw path and cross-OS
  cursor animation visual smoke coverage.
- Full interactive app compatibility automation for editors, pagers, TUIs,
  tmux/screen/zellij, WSL, and SSH sessions.
- Desktop SSH trust prompt UI, credential prompt UI, native OS keychain backend
  wiring, and collected real SSH server smoke reports on every target OS.
- Cross-OS verification of the installed doctor command output and packaged
  doctor smoke output.
- Release installers and compressed distribution artifacts beyond the current
  portable/staged package directories.
- Native iOS SSH companion app runtime and device-verified release path.

## Layer Status Matrix

| Layer | Status | Notes |
| --- | --- | --- |
| core correctness | partial | Strong baseline, Unicode cell hardening, fuzz harness, and app compatibility runner exist, but interactive app compatibility and conformance hardening remain. |
| platform parity | partial | Capabilities, desktop window foundations, Linux compositor verification matrix, cross-OS verification runners, and portable package layouts exist; real macOS/Linux X11/Linux Wayland verification reports and compositor lab evidence remain open. |
| render performance | partial | Persistent WGPU batches, retained-frame damage, shaping/glyph/emoji caches, low-idle scheduling, benchmarks, renderer recovery, screenshots, GPU timing, and a developer overlay exist; real device-loss/GPU timing and cross-OS runtime validation remain. |
| config portability | partial | Schema-v2 defaults/migrations, TOML discovery/validation, portable overrides, schema export, safe TOML/programmable live reload, and controlled programmable compilation exist; cross-OS runtime reload validation remains. |
| semantic meaning | tested | Semantic events, byte-positioned timeline updates, complete local hook marker sets, desktop activation, remote metadata context, command navigation and output copy are implemented; real remote and non-Windows shell verification remain. |
| visual overlay | tested | Prompt and command-block styles, real metadata/status badges, input/output grouping, renderer-only collapse masks, alternate-screen suppression, scrollback projection, configured rounded borders, damage tracking, and overlay glyph batching pass automated Windows-host tests; full cursor image drawing and cross-OS visual smoke remain. |
| session transport | partial | Local and SSH transport foundations plus the SSH real-server smoke harness exist; non-Windows local smoke, collected SSH server reports, and app UX remain. |
| multiplexer structure | partial | Model and local desktop runtime wiring exist; startup layouts, SSH panes, polished chrome, and cross-OS smoke remain. |
| diagnostics | partial | Installed and xtask doctor diagnostics plus packaged doctor smoke exist; richer live platform reports and cross-OS doctor/package output verification remain. |
| security | partial | SSH/security contracts, explicit host-trust decisions, keychain-backed secret-provider flow, platform keychain capability reporting, and OSC 52 policy exist; desktop trust/secret UI, native keychain backend wiring, and remote OSC 52 confirmation UI remain. |

## Immediate Next Slice

The dependency-ordered implementation pass through Phase 22 is now complete at
foundation level. Remaining work is release hardening and real platform/device
verification, not another roadmap phase.
