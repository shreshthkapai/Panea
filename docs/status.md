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
| Parser baseline | tested | ANSI/VT parser adapter handles printable text, common controls, SGR colors/styles, alternate screen, clears, insert/delete groundwork, tab stops, title OSC, mouse/focus/bracketed-paste mode state, and pending responses. |
| Config model and TOML | tested | `AppConfig` defaults, TOML parsing, unknown/deprecated diagnostics, validation, platform overrides, default generation, schema export, and reload impact classification exist. |
| Windows local transport | tested | Portable PTY/ConPTY lifecycle is bounded; Windows smoke tests were made non-hanging and observed output. |
| Diagnostics foundations | tested | `cargo xtask doctor ...`, bug-report snapshots, release/security/hardening/package readiness reports, and iOS readiness reports exist through shared diagnostics models. |
| Performance harness foundation | tested | `cargo xtask bench ...` and `tools/bench` fixtures exist for repeatable local measurements. |
| Mux model | tested | Workspace, window, tab, pane, session, split tree, restore snapshot, and mux action models exist with unit coverage. |
| Semantic model | tested | Semantic regions, command blocks, OSC semantic parser support, navigation, copy actions, shell metadata, and diagnostics models exist. |
| SSH transport foundation | tested | SSH profile mapping, host-key policy contracts, secret-provider boundaries, SSH2 transport, remote PTY request, resize, and shutdown foundations exist. |
| iOS shared-engine foundation | tested | `apps/ios` reuses shared parser/core/config/transport/semantic/render contracts and models lifecycle, input, safe-area sizing, and mobile SSH session specs. |

## What Is Partial

These areas are real foundations but must not be called complete:

| Area | Status | Missing before completion |
| --- | --- | --- |
| Desktop app runtime | partial | Full app lifecycle, polished UI chrome, complete mux integration, runtime config reload, installed doctor command, packaging, and cross-OS manual validation. |
| Platform windowing | partial | Real macOS lifecycle, real Linux X11/Wayland compositor behavior, decoration negotiation, IME validation, native notifications, and platform-specific fallback verification. |
| GPU renderer | partial | Fully batched GPU glyph quads, partial atlas/texture updates by damage, hardware timestamp queries, screenshot verification, full device-loss recovery, and cross-OS render validation. |
| Font system | partial | Deeper shaping, full fallback validation across installed font sets, emoji fallback, and grapheme-aware metrics. |
| Unicode support | partial | Full grapheme clusters, emoji ZWJ behavior, combining correctness, and grapheme-aware cursor/editing behavior. |
| Clipboard and selection | partial | Mouse-driven selection UX, primary selection on Linux, OSC 52 policy, permission/security prompts, and full copy/paste app compatibility coverage. |
| Baseline compatibility | partial | Real app smoke matrix for shells, editors, pagers, TUIs, tmux, screen, zellij, SSH, WSL, and command-line tools. |
| Shell integration | partial | Runtime activation/injection, remote install flows, heuristic fallback, and real bash/zsh/fish/PowerShell session verification. |
| Visual overlays | partial | Product-complete prompt decorations, command blocks, badge text composition, collapse/expand behavior, animated image cursor pipeline, and cross-OS visual verification. |
| Native mux runtime | partial | Tab chrome, split rendering, per-pane transports, pane resize-to-PTY propagation, startup workspaces, and runtime smoke tests. |
| SSH UX and security | partial | Interactive host-key approval UI, changed-host-key resolution UI, password/passphrase prompts, OS keychain providers, reconnect UI, proxy jump, and real SSH server smoke tests. |
| Performance reporting | partial | Hardware GPU timings, installed in-window overlay, CI regression gates, and reproducible cross-machine benchmark reporting. |
| Hardening/release readiness | partial | Device-loss recovery, crash-safe config reload, packaging artifacts, validation suite automation, and platform lab coverage. |
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

- Cross-OS verification runners for Windows, macOS, Linux X11, and Linux
  Wayland.
- Real Linux compositor verification for GNOME/Mutter, KDE/KWin, wlroots/Sway,
  Hyprland class, tiling window managers, and X11 window managers.
- Full Unicode/grapheme/emoji correctness.
- Real fuzzing harness such as proptest or cargo-fuzz style coverage.
- Fully batched GPU glyph rendering.
- GPU device-loss recreation.
- Cross-OS screenshot verification.
- OSC 52 clipboard policy and permission model.
- Runtime config file watching and safe live reload applier.
- Full desktop tabs/panes/sessions/workspaces runtime.
- Runtime shell integration activation and remote install flows.
- Product-complete command blocks and semantic visual overlays.
- Cursor animation polish and animated image cursor asset pipeline.
- Interactive SSH trust, secret prompts, keychain providers, and real SSH server
  smoke tests.
- Installed doctor binary.
- Release packaging artifacts.
- Native iOS SSH companion app.

## Layer Status Matrix

| Layer | Status | Notes |
| --- | --- | --- |
| core correctness | partial | Strong baseline exists, but Unicode, app compatibility, fuzzing, and conformance hardening remain. |
| platform parity | partial | Capabilities and desktop window foundations exist; real macOS/Linux X11/Linux Wayland verification remains open. |
| render performance | partial | WGPU foundation, glyph cache, damage, scheduling, and benchmarks exist; batched GPU text and device-loss recovery remain. |
| config portability | partial | Static config model is useful; runtime reload and advanced config are deferred. |
| semantic meaning | partial | Semantic events and timeline exist; runtime shell activation and real-shell verification remain. |
| visual overlay | partial | Overlay contracts and basic generation exist; polished command blocks/cursor assets remain. |
| session transport | partial | Local and SSH transport foundations exist; non-Windows local smoke, SSH real-server tests, and app UX remain. |
| multiplexer structure | partial | Model exists; runtime desktop wiring remains. |
| diagnostics | partial | Xtask diagnostics exist; installed doctor and live platform reports remain. |
| security | partial | SSH/security contracts exist; keychain/secret UI/OSC clipboard policy remain. |

## Immediate Next Slice

After architecture and layer-boundary hardening, the next dependency-ordered
phase is Unicode, grapheme, emoji, and width correctness.
