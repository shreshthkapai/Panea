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

## Deferred By Design

- Full Unicode width/grapheme handling is deferred to the terminal compatibility
  and font phases.
- Mouse/focus/application keypad behavior is represented as modes in Phase 2;
  event semantics are implemented in later platform and parser compatibility
  phases.
- Real local transport smoke tests are present but remain ignored until the
  harness has reliable timeout and process-tree cleanup across operating
  systems. On the current Windows host, manually running the ignored smoke test
  timed out, so it is not claimed as a passing gate yet.
