# Engineering Rules

This document is the execution contract for feature work. It complements
`architecture.md`: architecture defines what the product is; this file defines
how accepted features are routed, designed, tested, diagnosed, and rolled out.

## 0. Execution Rules

Every implementation task must obey these rules:

1. Read `architecture.md` before making changes.
2. Implement only the current assigned slice.
3. Do not jump ahead to future phases.
4. Do not redesign the architecture unless explicitly asked.
5. Do not make user-facing features OS-specific.
6. Do not mutate the terminal buffer for visual effects.
7. Do not put config scripting in the hot render path.
8. Do not block PTY input/output on rendering, animations, config, or UI.
9. Every feature must have a test, smoke test, benchmark, or manual
   verification path.
10. Every cross-platform feature must degrade clearly if the platform blocks
    exact behavior.

Panea's core promise is:

```text
Same config.
Same behavior.
Same visual system.
Same performance discipline.
Different OS internals hidden underneath.
```

## 1. Remaining Work Overview

The remaining high-level work is:

- Unicode/font/render conformance beyond the hardened core/parser cell model.
- Stronger architecture/layer-boundary checks.
- Long-running fuzz history and crash-regression intake from the fuzzing
  harness.
- macOS, Linux X11, and Linux Wayland screenshot baseline capture plus
  accumulated runtime evidence for the batched GPU glyph renderer.
- Real Linux X11/Wayland compositor host verification using the committed
  target matrix.
- Mouse-driven selection UX, Linux primary selection provider, remote OSC 52
  confirmation UI, and real cross-OS clipboard smoke coverage.
- App compatibility tests now have a bounded runner; full interactive shells,
  editors, TUIs, tmux/screen/zellij, WSL, SSH, and cross-OS reports remain.
- Runtime config watching/live reload cross-OS validation and richer error UI.
- Advanced programmable config later.
- GPU timestamp queries and in-window performance overlay.
- Product-complete desktop mux runtime polish: startup layouts, SSH panes,
  polished tab chrome, pane move/swap UI, and cross-OS GUI smoke.
- Runtime shell integration activation exists for local desktop sessions; real
  bash/zsh/fish/macOS/Linux/WSL/remote verification and heuristic fallback
  hardening remain.
- Command-block collapse/copy UI, real shell-driven verification, and
  cross-OS visual overlay smoke coverage.
- Full animated image cursor pixel-frame decode/upload/draw path and cross-OS
  cursor animation visual smoke coverage.
- Desktop SSH trust/secret UI, native OS keychain backend wiring, and real
  provider verification.
- Collected real SSH server smoke reports across Windows, macOS, Linux X11,
  and Linux Wayland.
- Real cross-OS verification runners.
- Installed terminal doctor binary.
- Packaging artifacts.
- Real GPU device-loss validation across sleep/wake, display changes, DPI
  changes, and backend failure scenarios.
- Native iOS shell, iOS GPU surface, Keychain provider, device validation.

This list must not be implemented randomly. It must be implemented in
dependency order.

## 2. Dependency-Ordered Phase Plan

The correct order for the next implementation pass is:

```text
Phase 0  - Current-state freeze and status matrix
Phase 1  - Architecture and layer-boundary hardening
Phase 2  - Unicode, grapheme, emoji, and width correctness
Phase 3  - Real fuzzing harness
Phase 4  - GPU renderer batching and glyph pipeline
Phase 5  - GPU device-loss recovery
Phase 6  - Cross-OS screenshot verification
Phase 7  - Linux X11/Wayland compositor verification
Phase 8  - Clipboard, selection, and OSC clipboard policy
Phase 9  - Runtime config watching and live reload
Phase 10 - Desktop multiplexer runtime wiring
Phase 11 - Shell integration activation and verification
Phase 12 - Command blocks and visual overlays
Phase 13 - Cursor animation and animated image cursor pipeline
Phase 14 - App compatibility test suite
Phase 15 - SSH trust, secrets, and keychain providers
Phase 16 - Real SSH server smoke tests
Phase 17 - Performance instrumentation and in-window overlay
Phase 18 - Terminal doctor binary
Phase 19 - Cross-OS verification runners
Phase 20 - Packaging artifacts
Phase 21 - Advanced programmable config
Phase 22 - Native iOS SSH companion path
```

Do not skip ahead because a later phase is more visible. Each phase exists to
make the following phases possible without weakening terminal correctness,
platform parity, performance discipline, or security.

## 3. Review Checklist for Every Agent Diff

Before accepting any agent output, review this checklist.

Architecture:

- Did it preserve layer boundaries?
- Did it avoid OS-specific leakage?
- Did it obey `architecture.md`?

Scope:

- Did it only implement the current task?
- Did it avoid jumping ahead?
- Did it avoid unrelated refactors?

Correctness:

- Are edge cases tested?
- Are failures handled clearly?
- Does it avoid panics?

Performance:

- Does disabled functionality have zero or near-zero cost?
- Does it avoid unnecessary redraws?
- Does it avoid blocking input/output?

Cross-platform:

- Does the feature have a path for Windows?
- Does the feature have a path for macOS?
- Does the feature have a path for Linux X11?
- Does the feature have a path for Linux Wayland?
- Are unavoidable platform differences documented?

Testing:

- Are unit tests included where appropriate?
- Are smoke tests included where appropriate?
- Are manual verification steps documented where automation is not possible?

User impact:

- Does it fail clearly?
- Does it avoid silent breakage?
- Does it preserve config compatibility?

## 16. Feature Implementation Map

This table tells workers where each accepted feature belongs.

| Feature | Primary Layer | Secondary Layers | Notes |
|---|---|---|---|
| GPU acceleration | render-wgpu | render-core, font-system, diagnostics | Built from day one |
| macOS/Linux/Windows support | platform-core/platform-winit | transport, renderer, config | Feature parity required |
| Linux compositor support | platform-winit | diagnostics, config | X11 + Wayland + fallbacks |
| Portable config | config-core | config-toml, config-lua, diagnostics | One internal model |
| Fonts/themes/colors | config-core | font-system, renderer | Must be portable with fallback |
| Custom cursor | render-core/render-wgpu | config-core | Overlay/renderer only |
| Cursor animations | render-wgpu | config-core, performance | Bounded; disabled cost near zero |
| Animated image cursor | render-wgpu | assets, performance, diagnostics | Opt-in and warned |
| Prompt beautification | semantics | renderer, shell-integration, config | Requires semantic regions for reliability |
| Command blocks | semantics | renderer, config, shell-integration | Visual overlay only |
| Input/output grouping | semantics | renderer, config | Same as command blocks |
| Fullscreen | platform layer | config, diagnostics | Cross-OS fallback required |
| Frameless/titlebarless | platform layer | config, diagnostics | Linux compositor caveats handled |
| Tabs/panes/workspaces | mux | transport, renderer, config | Pane contains session |
| tmux/screen/zellij compatibility | term-core/parser | transport, platform input | Correctness before native mux tricks |
| SSH sessions | transport-ssh | security, mux, semantics | First-class transport |
| iOS SSH client | apps/ios | shared core, SSH, renderer | Future phase, not desktop blocker |
| Performance profiles | config-core | render, diagnostics, bench | User-controlled tradeoffs |
| Diagnostics/doctor | diagnostics | all layers | Must explain fallbacks |

## 17. Cross-OS Implementation Checklist for Every Feature

Before implementing any feature, create a short design note answering:

```text
Feature name:
Layer:
User-facing behavior:
Config keys:
macOS behavior:
Windows behavior:
Linux X11 behavior:
Linux Wayland behavior:
Fallback behavior:
Diagnostics:
Performance cost when disabled:
Performance cost when enabled:
Tests:
```

A feature is not accepted until all of those fields are answered.

Required cross-OS gates for every feature:

1. Same config key works across OSes.
2. Same default works across OSes.
3. Same visual output is attempted across OSes.
4. Platform limitations are expressed through capabilities.
5. Fallbacks are explicit.
6. Diagnostics explain fallback.
7. Tests cover at least one happy path and one fallback/error path.

## 18. Performance Checklist for Every Feature

Every feature must include a performance note.

```text
Does this run every frame?
Does this run every input event?
Does this run every PTY output batch?
Does this allocate in the hot path?
Does this force full redraw?
Does this require GPU uploads?
Does this run script/user code?
Can it be cached?
Can it be disabled to near-zero cost?
Can the user budget it?
Can diagnostics show its cost?
```

Hot path rules prohibit:

- config parsing during rendering
- Lua/script execution during rendering
- image decoding during rendering
- full scrollback scanning during rendering
- full-screen redraw for tiny cursor animations
- shell integration script logic in the renderer
- terminal buffer rewrites for decorations
- blocking PTY I/O on animation state

Hot path rules allow only:

- precompiled config structs
- cached glyphs/assets
- dirty region iteration
- bounded animation state updates
- renderer-local overlay drawing
- incremental semantic updates

## 19. Testing Strategy

Unit tests are required for:

- terminal grid
- parser actions
- scrollback
- resize
- selection
- config validation
- semantic region tracking
- mux layout tree
- transport state machines

Golden tests are required for:

- terminal escape streams
- grid output
- style attributes
- cursor state
- semantic command regions
- config parsing results

Integration tests are required for:

- local shell spawn
- PTY resize
- shell integration events
- tabs/panes/session creation
- SSH session where test infrastructure exists

Renderer tests are required for:

- glyph atlas correctness
- damage tracking
- cursor rendering
- selection rendering
- command block overlay rendering
- animation bounds

Snapshot tests may be used, but they must account for font differences
carefully.

Platform manual tests are required before release:

```text
macOS native desktop
Windows desktop
Linux X11
Linux Wayland GNOME/Mutter
Linux Wayland KDE/KWin
Linux wlroots/Sway or equivalent
Linux Hyprland or equivalent
```

Compatibility manual tests are required for:

```text
vim/neovim
less/man
fzf
git diff/log
htop/btop style TUI
tmux
screen
zellij
ssh
PowerShell
cmd
WSL
bash/zsh/fish
```

## 20. Diagnostics Requirements

Diagnostics are not optional. They are how the terminal keeps cross-OS behavior
understandable.

Implement diagnostics for:

```text
config load path
config parse errors
config validation warnings
unknown/deprecated settings
platform backend
Linux compositor/window manager
window decoration fallback
fullscreen fallback
GPU backend
font fallback
shell integration status
semantic event status
SSH host verification
performance budget warnings
active expensive effects
renderer device errors
transport errors
```

The user should never be forced to guess why something changed across OSes.

## 21. Config Rollout Order

Implement config in this order:

1. Internal `AppConfig` defaults
2. Static TOML config
3. Config validation
4. Platform overrides
5. Hot reload for safe settings
6. Generated config reference/schema
7. Theme files
8. Cursor profiles
9. Command block profiles
10. Performance profiles
11. Advanced programmable config
12. Config import helpers where practical

Do not add advanced scripting until the normal config model is stable.

## 22. Visual Feature Rollout Order

Implement visuals in this order:

1. Static cursor styles
2. Cursor blink and basic cursor color
3. Damage-aware cursor redraw
4. Smooth cursor movement
5. Typing pulse/stretch
6. Cursor trail/glow
7. Prompt separators
8. Prompt boxes/pills
9. Command separators
10. Full command blocks
11. Per-command success/error styles
12. Command metadata badges
13. Collapsible command output
14. Animated image cursor
15. Heavy visual demo themes

Do not implement animated image cursors before the basic animation system and
performance budgets are proven.

## 23. Shell Integration Rollout Order

Implement shell integration in this order:

1. Internal semantic events
2. Parse semantic escape sequences
3. zsh integration
4. bash integration
5. fish integration
6. PowerShell/pwsh integration
7. Current working directory reporting
8. Exit status reporting
9. Command duration reporting
10. Command navigation
11. Output selection/copy
12. Remote shell integration instructions
13. nushell support
14. cmd limited mode if feasible
15. Heuristic fallback mode

Heuristic mode must be labeled as less reliable.

## 24. Windowing Rollout Order

Implement window behavior in this order:

1. Normal decorated window
2. Resize/DPI handling
3. Maximized mode
4. Fullscreen mode
5. Borderless fullscreen
6. Frameless windowed mode
7. Frameless fullscreen mode
8. Custom drag regions
9. Emergency restore shortcuts
10. Linux X11 fallback diagnostics
11. Linux Wayland decoration negotiation diagnostics
12. Compositor compatibility table
13. User-facing doctor output

Do not ship titlebarless modes without emergency restore actions.

## 25. Multiplexer Rollout Order

Implement mux in this order:

1. Session model
2. One pane per window
3. Tabs
4. Split tree model
5. Horizontal split
6. Vertical split
7. Focus movement
8. Pane resize
9. Pane close
10. Pane zoom
11. Pane move/swap
12. Workspace model
13. Layout save/restore
14. Startup workspaces
15. Remote/SSH sessions in panes
16. Future remote domains

External tmux/screen/zellij compatibility must be tested throughout, not after.

## 26. SSH Rollout Order

Implement SSH in this order:

1. SSH profile config
2. Host key storage model
3. Host key verification prompt/action
4. Password/passphrase handling
5. Key file authentication
6. Remote PTY session
7. Resize propagation
8. SSH session inside tab
9. SSH session inside pane
10. Remote shell integration support
11. Reconnect action
12. Proxy/jump support later
13. Agent forwarding, disabled by default unless explicitly configured
14. Port forwarding later if in scope

Security defaults must be conservative.

## 27. Release Readiness Checklist

Before calling the desktop product ready, the following must be true.

Core:

- terminal core golden tests pass
- parser fuzzing does not reveal crashes
- resize behavior is stable
- alternate screen works
- Unicode basics work

Platform:

- macOS works
- Windows works
- Linux X11 works
- Linux Wayland works
- major Linux compositor fallbacks are documented

Renderer:

- GPU renderer is stable
- glyph cache works
- damage tracking works
- idle redraw behavior is low
- cursor animations are bounded
- command blocks do not force whole-screen redraws unnecessarily

Config:

- config loads on all OSes
- same config works across OSes unless platform override is explicitly used
- invalid config is explained
- live reload is safe

Compatibility:

- normal shells work
- full-screen TUIs work
- tmux/screen/zellij work
- mouse reporting works
- bracketed paste works
- truecolor works

Enhanced features:

- custom cursors work
- cursor animations work
- prompt decorations work with shell integration
- command blocks work with shell integration
- visual features can be disabled cleanly

Performance:

- benchmark suite exists
- default mode is fast
- disabled visual features have near-zero cost
- expensive features warn when budgets are exceeded

Security:

- SSH host verification is correct
- secrets are handled safely
- logs/diagnostics do not leak sensitive session contents by default

Diagnostics:

- `terminal doctor` exists
- renderer/platform/config/shell/performance diagnostics are useful
- fallbacks are visible

Documentation:

- setup docs exist
- config docs exist
- platform docs exist
- shell integration docs exist
- SSH docs exist
- performance docs exist

## 28. Things Not to Do Early

Do not start with:

- arbitrary custom shaders
- plugin marketplace
- animated backgrounds
- web UI
- Electron wrapper
- dashboard-first experience
- mobile app before shared engine stability
- advanced scripting before static config stability
- command blocks before semantic shell integration
- prompt boxes by inserting fake terminal text
- Linux-only window behavior with Windows/macOS patched later
- performance claims without benchmark evidence

These can either wait or be rejected if they violate the architecture.

## 29. The First Concrete Work Session

The first real coding session should do exactly this:

1. Create the repository.
2. Add `architecture.md` and `implementation.md`.
3. Create the workspace crates as empty compiling crates.
4. Add root build/test/lint commands.
5. Add crate-level README files explaining ownership boundaries.
6. Define initial core data types in `term-core`.
7. Define `TerminalTransport` in `transport-core`.
8. Define platform capability types in `platform-core`.
9. Define `AppConfig` skeleton in `config-core`.
10. Add unit tests that compile and prove basic defaults exist.

Nothing flashy happens first.

The first win is not a beautiful terminal. The first win is a codebase that
cannot easily become the wrong product.

## 30. Final Definition of Done

Panea is product-complete only when all of the following are true:

- Core terminal behavior is correct.
- Unicode/grapheme/emoji handling is hardened.
- Renderer is GPU-batched and benchmarked.
- Visual effects are overlays, not buffer mutations.
- Cursor animation is performant and configurable.
- Command blocks work through shell integration.
- Multiplexer tabs/panes/sessions are wired at runtime.
- Clipboard and OSC 52 policy are safe.
- SSH trust and secrets are secure.
- Real app compatibility tests pass.
- Linux X11 and Wayland are verified.
- Windows and macOS are verified.
- Cross-OS screenshot verification exists.
- Runtime config reload works safely.
- Doctor diagnostics are installed.
- Packaging artifacts exist.
- GPU device-loss recovery exists.
- Performance overlay exists.
- Advanced config is powerful but not hot-path dangerous.
- iOS SSH companion has a validated shared-core path.

Panea must remain:

```text
Fast by default.
Cross-platform by design.
Config-compatible across OSes.
GPU-first.
Terminal-correct.
Visually expressive without corrupting terminal semantics.
Secure by default.
Tested against real applications.
```
