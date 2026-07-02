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
- Completed Phase 4 platform/window foundation: desktop app entrypoint
  placeholders for config/session/diagnostics, Winit-backed window creation,
  monitor and DPI snapshots, platform-neutral keyboard/mouse/IME/resize/close
  event translation, clipboard copy/paste bridge with diagnostics, portable
  window-mode config, Linux backend and decoration strategy config/diagnostics,
  and emergency restore window actions.
- Implemented the Phase 5 GPU renderer foundation: WGPU surface/device/queue
  initialization, surface resize/present lifecycle, font discovery and fallback
  chain resolution, glyph rasterization and cache policy, glyph atlas allocation,
  renderer-independent style fields, damage tracking, frame scheduling, CPU
  visual snapshot tests, and a desktop window path that feeds local PTY output
  through the terminal parser/core into the renderer.
- Implemented the Phase 6 baseline compatibility foundation: expanded ANSI/VT
  parser coverage for truecolor/indexed/style SGR, title OSC, save/restore
  cursor, line/character insert/delete/erase, tab stop operations, DSR/CPR
  pending responses, mouse/focus/bracketed-paste modes, TERM/COLORTERM local PTY
  defaults, desktop forwarding for bracketed paste, focus, and SGR/legacy mouse
  reports, URL hint detection with basic semantic overlays, and Unicode
  wide/combining-cell groundwork.
- Implemented the Phase 7 static configuration foundation: completed baseline
  `AppConfig` sections for window/font/colors/cursor/scrollback/input/shell
  profiles/renderer/performance/diagnostics/platform overrides, safe defaults,
  TOML discovery and explicit path loading, parse locations, unknown and
  deprecated setting diagnostics, validation diagnostics, default config
  generation, schema JSON export, platform override resolution, reload-impact
  classification, xtask helpers, and desktop startup wiring through the config
  model.
- Implemented the Phase 8 performance harness foundation: added a repeatable
  `panea-bench` runner and `cargo xtask bench` wrapper for render-grid,
  cat-large-file, color-heavy, scrollback, resize, input-latency, Unicode,
  alternate-screen, and cursor-animation cost cases; added deterministic
  benchmark fixture seeds; defined portable performance profiles including
  `battery_saver`; added renderer instrumentation for frame time, CPU render
  preparation, GPU submission timing where available, glyph cache hits/misses,
  atlas uploads, damage region count, draw-call count, animated region count,
  and idle wakeups; added diagnostics-side performance overlay text and gate
  evaluation; and wired optional desktop performance reporting through the
  diagnostics config.
- Implemented the Phase 9 native multiplexer model foundation: added
  workspace/window/tab/pane/session IDs and state, a proportional split layout
  tree, tab lifecycle operations, pane split/focus/resize/zoom/swap/close
  operations, session restore snapshots for workspace names, tab names, layout,
  profile identities and working directories, portable mux action names, default
  mux keybindings, and an explicit compatibility policy that tmux/screen/zellij
  remain ordinary terminal applications inside panes.
- Implemented the Phase 10 shell integration and semantic layer foundation:
  added semantic event storage for prompt/input/output/command boundaries,
  command blocks with exit status and duration, current directory and
  shell/remote metadata, command navigation and output copy actions,
  shell-integration parsers for OSC 133, OSC 633, OSC 7, and Panea-private OSC
  777 events, baseline hook scripts for bash/zsh/fish/PowerShell, portable
  shell integration config and keybindings, and diagnostics reports for active
  or inactive shell integration state.
- Implemented the Phase 11 visual enhancement foundation: added a portable
  visual theme model, cursor customization and bounded animation config,
  prompt decoration and command block styles, input/output grouping modes,
  renderer-independent overlay kinds for prompts, command blocks, grouping, and
  badges, semantic OSC feeding in the desktop app, basic semantic overlay
  generation from `SemanticTimelineStore`, visual budget diagnostics, and
  shipped example visual configs.
- Implemented the Phase 12 SSH transport foundation: expanded portable SSH
  profile config, added a security-layer host-key and secret contract,
  implemented an `ssh2`-backed `TerminalTransport` with host-key verification,
  agent/public-key/password authentication boundaries, remote PTY allocation,
  read/write/resize/shutdown support, bounded best-effort drop behavior, SSH
  mux session specs, and desktop config-to-transport profile mapping.

## Deferred By Design

- Full Unicode width/grapheme handling is deferred to the terminal compatibility
  and font phases.
- Mouse/focus/application keypad behavior is represented as modes in Phase 2;
  event semantics are implemented in later platform and parser compatibility
  phases.
- Phase 4 translates platform mouse/focus/IME events into platform-neutral
  events, but terminal mouse reporting, mouse-driven selection UI, focus escape
  reports, and application keypad/cursor encoding remain later compatibility
  work.
- Phase 4 models Linux X11/Wayland backend and decoration preferences and emits
  diagnostics, but compositor-specific fullscreen/frameless behavior still
  needs real Linux X11/Wayland host verification before it can be called
  complete.
- Clipboard copy/paste has a platform bridge and diagnostics; OSC clipboard,
  primary selection, and full selection-driven copy behavior remain later
  platform/compatibility work.
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
- Phase 6 establishes compatibility mechanics and lower-level golden coverage,
  but app-level smoke testing for bash/zsh/fish/PowerShell/cmd/vim/less/TUI
  apps/tmux/screen/zellij/SSH remains unverified until run on the relevant
  host platforms.
- Full grapheme cluster editing, emoji ZWJ behavior, primary selection, OSC 52
  clipboard, application keypad output mapping, a custom terminfo entry, and a
  configurable hint engine remain deferred compatibility work.
- Phase 7 classifies safe live-reload changes, but the file watcher and runtime
  applier are deferred until the desktop lifecycle can apply changes without
  destabilizing sessions or the renderer.
- Programmable config remains deferred until the static TOML model has more
  runtime mileage. It must compile into the same `AppConfig` and stay out of the
  render hot path.
- Phase 8 uses CPU-side timing plus WGPU submission wall-clock timing. Hardware
  GPU timestamp queries and richer in-window overlay rendering remain deferred
  until the renderer has the later batched glyph path and stable overlay
  composition.
- Phase 9 establishes the native mux state model and action contract. Full
  split-pane desktop rendering, tab chrome, per-pane transport orchestration,
  and cross-OS native mux runtime smoke tests remain follow-up work on top of
  the model.
- Phase 10 establishes semantic storage, parsers, scripts, config, and
  diagnostics. Desktop runtime injection/loading of shell integration scripts,
  remote installation flows, heuristic fallback, and cross-shell real-session
  verification remain follow-up work before command blocks can be considered
  product-complete.
- Phase 11 establishes visual overlay contracts and basic semantic overlay
  generation, but animated image cursors, off-render-thread decoding, GPU frame
  caching for animated assets, polished badge text rendering, collapse/expand
  long output UI, and real shell-driven command-block verification remain
  deferred until the renderer and desktop semantic runtime are hardened.
- Phase 12 establishes secure SSH transport contracts and a real backend, but
  interactive host-key approval UI, OS keychain-backed secret providers, proxy
  jump, remote shell-integration install flows, reconnect UI/actions, and real
  SSH server smoke tests across Windows/macOS/Linux remain unverified follow-up
  work.
