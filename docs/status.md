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
| Terminal core baseline | tested | Grid, cells, cursor, scrollback, alternate screen, resize, modes, selection extraction, and baseline golden coverage exist. |
| Unicode/grapheme cell model | tested | UTF-8 scalar buffering, grapheme clustering, combining marks, wide CJK cells, emoji modifiers, ZWJ emoji, variation selectors, selection, cursor movement, overwrite/delete/erase, resize, and scrollback tests exist in `term-core` and `term-parser`. |
| Parser baseline | tested | ANSI/VT parser adapter handles printable text, common controls, SGR colors/styles, alternate screen, clears, insert/delete groundwork, tab stops, title OSC, mouse/focus/bracketed-paste mode state, and pending responses. |
| Fuzzing harness | tested | `fuzz/` contains cargo-fuzz targets for parser, grid, resize, Unicode, selection, OSC/DCS, and shell markers; property smoke tests run through `cargo xtask fuzz-smoke` and scheduled CI. |
| App compatibility runner | tested | `cargo xtask compat` lists and runs bounded process/PTY compatibility probes, writes reports under `target/compatibility`, and the required Windows PowerShell/cmd/protocol subset passed on the current host. |
| Screenshot verification runner | tested | Deterministic renderer fixtures, PPM capture, tolerance-based diffing, Windows baselines, and `cargo xtask screenshot verify --platform windows` exist. |
| Linux compositor verification matrix | tested | Target matrix, fallback checklist, runtime environment snapshot, and `cargo xtask linux-compositor` exist; real Linux host verification remains open. |
| Config model and TOML | tested | `AppConfig` defaults, TOML parsing, unknown/deprecated diagnostics, validation, platform overrides, default generation, schema export, reload impact classification, debounced file watching, safe live apply, and previous-config retention exist. |
| Windows local transport | tested | Portable PTY/ConPTY lifecycle is bounded; Windows smoke tests were made non-hanging and observed output. |
| Diagnostics foundations | tested | `cargo xtask doctor ...`, bug-report snapshots, release/security/hardening/package readiness reports, and iOS readiness reports exist through shared diagnostics models. |
| Performance harness foundation | tested | `cargo xtask bench ...` and `tools/bench` fixtures exist for repeatable local measurements. |
| Mux model | tested | Workspace, window, tab, pane, session, split tree, restore snapshot, and mux action models exist with unit coverage. |
| Desktop mux runtime foundation | tested | The desktop app owns one terminal/semantic/transport runtime per native pane, routes focus/input/paste/mouse to the active pane, composes pane viewports into one render scene, draws basic tab chrome/borders, and resizes active pane PTYs from split layout. |
| Semantic model | tested | Semantic regions, command blocks, OSC semantic parser support, navigation, copy actions, shell metadata, and diagnostics models exist. |
| Shell integration activation foundation | tested | Portable activation plans, config modes, desktop startup hook injection for bash/zsh/fish/PowerShell, disabled/manual/heuristic/off behavior, and ignored real-shell verification tests exist; PowerShell semantic smoke passed on the current Windows host. |
| Command block visual overlay foundation | tested | Desktop scene projection now creates command-block backgrounds, input/output grouping overlays, status/duration/cwd/shell/host badges, conservative alternate-screen suppression, and renderer-batched overlay label glyphs without mutating terminal cells. |
| Cursor animation foundation | tested | Opt-in cursor animation config, cursor-neighborhood animation damage, batched renderer animation quads, nonblocking image cursor asset metadata cache, and budget validation exist; full image frame decode/upload and cross-OS visual smoke remain open. |
| Clipboard/OSC 52 policy | tested | Portable `clipboard` config, paste protection, bracketed paste forwarding, middle-click paste suppression during mouse reporting, parser pending OSC 52 requests, bounded security policy, local allow/default remote deny behavior, and TOML/security/parser/app tests exist. |
| SSH transport foundation | tested | SSH profile mapping, host-key policy contracts, secret-provider boundaries, SSH2 transport, remote PTY request, resize, and shutdown foundations exist. |
| iOS shared-engine foundation | tested | `apps/ios` reuses shared parser/core/config/transport/semantic/render contracts and models lifecycle, input, safe-area sizing, and mobile SSH session specs. |

## What Is Partial

These areas are real foundations but must not be called complete:

| Area | Status | Missing before completion |
| --- | --- | --- |
| Desktop app runtime | partial | Full app lifecycle, polished UI chrome, complete mux integration, installed doctor command, packaging, and cross-OS manual validation. |
| Platform windowing | partial | Real macOS lifecycle, real Linux X11/Wayland compositor behavior, decoration negotiation, IME validation, native notifications, and platform-specific fallback verification. |
| GPU renderer | tested | WGPU surface/device setup, glyph atlas/cache policy, damage-aware batch preparation, indexed background/glyph/decoration/selection/cursor batches, row-scoped atlas uploads, renderer benchmarks, recovery status/event contracts, WGPU device-lost callback detection, disposable WGPU backend recreation, GPU atlas invalidation after recovery, and screenshot verification infrastructure exist; hardware timestamp queries, real sleep/wake/monitor-loss validation, macOS/Linux screenshot baselines, and cross-OS render validation remain. |
| Font system | partial | Deeper shaping, full fallback validation across installed font sets, emoji fallback, and grapheme-aware metrics. |
| Unicode support | tested | Core/parser Unicode hardening is covered by automated tests; renderer font fallback, shaping, screenshot parity, and real app conformance remain later phases. |
| Clipboard and selection | partial | Raw selection extraction, keyboard copy/paste, paste protection, bracketed paste, middle-click paste guard, and OSC 52 policy exist; mouse-driven selection UX, Linux primary selection provider, remote confirmation UI, and full copy/paste app compatibility coverage remain. |
| Baseline compatibility | partial | App compatibility runner and required Windows smoke exist; full interactive verification for shells, editors, pagers, TUIs, tmux, screen, zellij, SSH, WSL, and command-line tools remains incomplete. |
| Shell integration | partial | Local runtime activation planning and desktop injection exist for supported shells, with Windows PowerShell semantic smoke verified. Remote install flows, heuristic command detection, WSL-specific coverage, and real bash/zsh/fish/macOS/Linux session verification remain. |
| Visual overlays | partial | Semantic command-block overlay projection, badge glyph batching, cursor animation quads, and image cursor metadata caching exist; collapse/expand behavior, polished interactive UI, full image cursor frame upload/draw, real shell-driven verification, and cross-OS visual verification remain. |
| Native mux runtime | partial | Local tab/split runtime wiring exists, but startup workspaces, SSH panes, polished tab chrome, pane drag/move UI, and cross-OS GUI/runtime smoke tests remain. |
| SSH UX and security | partial | Interactive host-key approval UI, changed-host-key resolution UI, password/passphrase prompts, OS keychain providers, reconnect UI, proxy jump, and real SSH server smoke tests. |
| Performance reporting | partial | Hardware GPU timings, installed in-window overlay, CI regression gates, and reproducible cross-machine benchmark reporting. |
| Hardening/release readiness | partial | GPU recovery and crash-safe config reload foundations exist, but real device-loss platform validation, packaging artifacts, validation suite automation, and platform lab coverage remain. |
| iOS companion | partial | Native UIKit/SwiftUI shell, iOS GPU surface, Keychain provider, host-key approval UI, key import UX, simulator/device validation, and packaging. |

## What Is Stubbed

These areas exist mostly as placeholders, contracts, or documentation:

| Area | Status | Current shape |
| --- | --- | --- |
| `config-lua` | stubbed | Programmable config crate exists, but advanced scripting is intentionally deferred until static config is stable. |
| `tools/conformance` | stubbed | Directory and README exist; full terminal conformance fixture suite is not built out. |
| Packaging artifacts | stubbed | Packaging plans and diagnostics exist; macOS app bundle, Windows installer/portable build, and Linux AppImage generation are not implemented. |
| Installed `terminal doctor` | stubbed | Diagnostics are exposed through `cargo xtask doctor`; the installed product binary command is not implemented. |
| Native notifications | stubbed | Tracked in the platform matrix as not implemented. |
| iOS app shell | stubbed | Rust shared-engine crate exists; no native mobile app host exists yet. |
| Advanced config import/helpers | stubbed | Accepted by rollout rules, but no product implementation exists. |

## What Is Unimplemented

The following major accepted features have no complete product behavior yet:

- Cross-OS verification runners for macOS, Linux X11, and Linux Wayland, plus
  product-level Windows GUI/runtime verification beyond current host smoke.
- Real Linux compositor verification runs for GNOME/Mutter, KDE/KWin,
  wlroots/Sway, Hyprland class, tiling window managers, and X11 window managers.
- Long-running coverage-guided fuzz history and crash-regression backlog from
  real-world fuzz findings.
- Unicode/font/render conformance beyond the core parser model, including
  cross-OS font fallback and real app screenshot parity.
- macOS, Linux X11, and Linux Wayland screenshot baseline capture and
  verification.
- Cross-OS runtime verification of the batched GPU glyph renderer.
- Real GPU device-loss validation for sleep/wake, monitor attach/detach, DPI
  changes, and backend failure simulation across desktop OSes.
- Linux primary selection provider and remote OSC 52 confirmation UI.
- Native OS config watcher backends and real macOS/Linux runtime reload validation.
- Product-complete desktop tabs/panes/sessions/workspaces runtime, including
  startup layouts, SSH panes, polished chrome, and cross-OS smoke tests.
- Remote shell integration install flows and real bash/zsh/fish/macOS/Linux
  shell verification.
- Full animated image cursor pixel-frame decode/upload/draw path and cross-OS
  cursor animation visual smoke coverage.
- Full interactive app compatibility automation for editors, pagers, TUIs,
  tmux/screen/zellij, WSL, and SSH sessions.
- Interactive SSH trust, secret prompts, keychain providers, and real SSH server
  smoke tests.
- Installed doctor binary.
- Release packaging artifacts.
- Native iOS SSH companion app.

## Layer Status Matrix

| Layer | Status | Notes |
| --- | --- | --- |
| core correctness | partial | Strong baseline, Unicode cell hardening, fuzz harness, and app compatibility runner exist, but interactive app compatibility and conformance hardening remain. |
| platform parity | partial | Capabilities, desktop window foundations, and Linux compositor verification matrix exist; real macOS/Linux X11/Linux Wayland verification remains open. |
| render performance | partial | WGPU foundation, glyph cache, damage, scheduling, batched glyph/quad rendering, atlas uploads, benchmarks, renderer recovery foundation, and screenshot verification infrastructure exist; macOS/Linux screenshot baselines, real device-loss validation, and cross-OS runtime validation remain. |
| config portability | partial | Static config model, TOML loading, validation, platform overrides, schema export, and runtime live-reload foundation exist; advanced config and cross-OS reload validation remain. |
| semantic meaning | partial | Semantic events, timeline, runtime activation planning, desktop local hook injection, and Windows PowerShell semantic smoke exist; remote flows and non-Windows/bash/zsh/fish real verification remain. |
| visual overlay | partial | Prompt and command block overlay projection, input/output grouping, metadata badges, alternate-screen suppression, renderer overlay glyph batching, and cursor animation quads exist; collapse/expand UI, full cursor image drawing, and cross-OS visual smoke remain. |
| session transport | partial | Local and SSH transport foundations exist; non-Windows local smoke, SSH real-server tests, and app UX remain. |
| multiplexer structure | partial | Model and local desktop runtime wiring exist; startup layouts, SSH panes, polished chrome, and cross-OS smoke remain. |
| diagnostics | partial | Xtask diagnostics exist; installed doctor and live platform reports remain. |
| security | partial | SSH/security contracts and OSC 52 policy exist; keychain/secret UI and remote OSC 52 confirmation UI remain. |

## Immediate Next Slice

After the app compatibility test suite, the next dependency-ordered phase is
SSH trust, secrets, and keychain providers.
