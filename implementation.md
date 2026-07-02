# Implementation

This file tracks implementation steps as they are accepted and completed.

## Execution Categories

Every task must clearly fit one or more of these categories:

- core correctness
- platform parity
- render performance
- config portability
- semantic meaning
- visual overlay
- session transport
- multiplexer structure
- diagnostics
- security

If a task does not fit, redesign it before implementation.

## Completed

- Created the initial Rust workspace and repository layer skeleton.
- Defined early core interfaces for session transport, terminal core state,
  semantic timelines, renderer-independent scenes, platform capabilities, and
  portable application configuration.
- Completed Phase 0 bootstrap: repository hygiene, standard build/test/lint
  commands, xtask wrappers, platform matrix, contribution gates, and issue
  label categories.
- Completed Phase 1 architectural contracts: crate boundary READMEs, terminal
  data primitives, transport/platform/render/semantic contracts, serializable
  config skeleton, and dependency-boundary tests.
- Completed Phase 2 terminal core baseline: platform-neutral grid/cell storage,
  scrollback, line wrapping, scroll regions, alternate screen storage, resize
  reflow, cursor/mode metadata, raw selection extraction, ANSI/VT parser adapter,
  golden tests, and deterministic invalid-input fuzz coverage.
- Completed Phase 3 local transport baseline: portable PTY/ConPTY-backed local
  shell transport, default Unix shell profile, Windows PowerShell/cmd/WSL
  profile groundwork, byte write/read polling, resize propagation, lifecycle
  metadata, bounded transport event loop, and ignored real-shell smoke fixtures.
- Completed Phase 3.5 deterministic PTY lifecycle hardening for the local
  transport: explicit Running/ClosingInput/TerminatingChild/DrainingOutput/Closed
  states, non-blocking Drop cleanup, bounded shutdown, reader diagnostics,
  failure diagnostics for real PTY smoke tests, and Windows verification of
  one-shot, interactive, and event-loop smoke cases.
- Implemented the Phase 5 GPU renderer foundation: WGPU surface/device/queue
  initialization, surface resize/present lifecycle, font discovery and fallback
  chain resolution, glyph rasterization and cache policy, glyph atlas allocation,
  renderer-independent style fields, damage tracking, frame scheduling, CPU
  visual snapshot tests, and a desktop window path that feeds local PTY output
  through the terminal parser/core into the renderer.

## Deferred By Design

- Full Unicode width/grapheme handling is deferred to the terminal compatibility
  and font phases.
- Mouse/focus/application keypad behavior is represented as modes in Phase 2;
  event semantics are implemented in later platform and parser compatibility
  phases.
- Real local transport smoke tests remain ignored by default because they spawn
  host shells. On the current Windows host, the one-shot, interactive, and
  event-loop smoke tests pass quickly. macOS and Linux smoke status is still
  unverified until those platforms are actually run.
- The first GPU renderer presents a rasterized terminal frame through WGPU and
  has a glyph atlas/cache foundation. Fully batched GPU glyph rendering,
  partial texture updates by damage region, and cross-OS screenshot automation
  are deferred render-performance work.
- Phase 5 has build/test verification on the current Windows host. macOS,
  Linux X11, and Linux Wayland rendering remain unverified until run on those
  platforms.
