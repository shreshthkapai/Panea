# Implementation

This file tracks implementation steps as they are accepted and completed.
Feature routing, rollout order, test expectations, diagnostics requirements,
and release readiness gates are defined in
[docs/engineering-rules.md](docs/engineering-rules.md).

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

## Next Hardening Roadmap

The first fifteen phases established foundations across the core, transport,
platform, renderer, config, mux, semantics, visuals, SSH, diagnostics,
hardening, and iOS companion layers. The next pass is not a random feature
backlog. It follows the dependency-ordered plan in
[docs/engineering-rules.md](docs/engineering-rules.md):

1. Phase 0 - Current-state freeze and status matrix
2. Phase 1 - Architecture and layer-boundary hardening
3. Phase 2 - Unicode, grapheme, emoji, and width correctness
4. Phase 3 - Real fuzzing harness
5. Phase 4 - GPU renderer batching and glyph pipeline
6. Phase 5 - GPU device-loss recovery
7. Phase 6 - Cross-OS screenshot verification
8. Phase 7 - Linux X11/Wayland compositor verification
9. Phase 8 - Clipboard, selection, and OSC clipboard policy
10. Phase 9 - Runtime config watching and live reload
11. Phase 10 - Desktop multiplexer runtime wiring
12. Phase 11 - Shell integration activation and verification
13. Phase 12 - Command blocks and visual overlays
14. Phase 13 - Cursor animation and animated image cursor pipeline
15. Phase 14 - App compatibility test suite
16. Phase 15 - SSH trust, secrets, and keychain providers
17. Phase 16 - Real SSH server smoke tests
18. Phase 17 - Performance instrumentation and in-window overlay
19. Phase 18 - Terminal doctor binary
20. Phase 19 - Cross-OS verification runners
21. Phase 20 - Packaging artifacts
22. Phase 21 - Advanced programmable config
23. Phase 22 - Native iOS SSH companion path

Each phase should be implemented as its own assigned slice. Do not jump ahead
to more visible features before the dependencies they rely on are hardened.

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
- Completed next-pass Phase 0 current-state freeze: `docs/status.md` and
  `docs/capability-matrix.md` record what is tested, partial, stubbed, planned,
  and not cross-OS verified.
- Completed next-pass Phase 1 architecture hardening: enforceable workspace
  dependency checks, provider interfaces, architecture-boundary CI, independent
  `term-core` checks, and fake renderer tests.
- Completed next-pass Phase 2 Unicode, grapheme, emoji, and width correctness:
  parser UTF-8 buffering across reads, grapheme-aware cell storage, combining
  mark handling, wide CJK cells, emoji modifiers, ZWJ emoji, variation
  selectors, cursor movement, selection expansion, overwrite/delete/erase
  invariants, and resize/scrollback tests.
- Completed next-pass Phase 3 real fuzzing harness: added cargo-fuzz targets
  for parser input, grid actions, resize, Unicode, selection ranges, OSC/DCS
  escape handling, and shell markers; added corpus/artifact structure,
  proptest property smoke coverage, parser and shell OSC/CSI payload bounds,
  `cargo xtask fuzz-smoke`, `cargo xtask fuzz <target>`, and scheduled
  cross-OS fuzz-smoke CI.
- Completed next-pass Phase 4 GPU renderer batching and glyph pipeline:
  added a damage-aware batch planner for background, glyph, decoration,
  selection, and cursor quads; added GPU-facing vertex/index batches, glyph run
  caching, glyph bitmap cache reuse, atlas entry reuse, row-scoped atlas upload
  records, WGPU colored-quad and glyph-atlas pipelines, indexed batch
  submission in `GpuTerminalRenderer::render_scene`, renderer-focused
  benchmark commands for ASCII, Unicode, emoji, scrolling, large viewports,
  many panes, cursor animation, and command-block overlays, plus renderer
  batching documentation and unit coverage for batch grouping, cache reuse, and
  cursor-only damage.
- Completed next-pass Phase 5 GPU device-loss recovery foundation: added
  renderer-independent recovery reason/status/event contracts, split disposable
  WGPU resources into a rebuildable backend, preserved terminal/session state
  outside renderer recovery, added surface lost/outdated reconfiguration,
  device-level loss detection through WGPU callbacks and out-of-memory surface
  failures, async backend recreation, GPU atlas residency invalidation with CPU
  glyph cache retention, desktop recovery handling, renderer recovery
  documentation, and tests proving recovery events preserve terminal state and
  cached glyphs are re-uploaded after atlas reset.
- Completed next-pass Phase 6 cross-OS screenshot verification infrastructure:
  added deterministic renderer fixtures for ASCII, truecolor, styles, CJK,
  emoji, cursor states, selection, prompt decorations, command blocks, multiple
  panes, and opacity overlays; added CPU screenshot capture to binary PPM,
  PPM decode, tolerance-based image comparison, diff classes that distinguish
  antialiasing drift from likely text/layout failures, markdown reports,
  `cargo xtask screenshot <capture|verify|report>`, committed Windows
  baselines, and verified the Windows baseline set on the current host.
- Completed next-pass Phase 7 Linux X11/Wayland compositor verification
  infrastructure: added the Linux compositor target matrix for GNOME Xorg, KDE
  X11, XFCE, i3, Openbox-class WMs, GNOME/Mutter, KDE/KWin, Sway/wlroots,
  Hyprland, and COSMIC when available; added feature/fallback checklists,
  runtime environment diagnostics, `cargo xtask linux-compositor`,
  documentation for required manual evidence, and unit coverage for the target
  matrix and Linux environment detection. Real Linux host runs remain required
  before any compositor target can be marked verified.
- Completed next-pass Phase 8 clipboard, selection, and OSC clipboard policy:
  added portable `[clipboard]` and `[clipboard.osc52]` config with validation,
  schema export, TOML discovery, and platform overrides; added parser/core
  pending OSC 52 clipboard requests without platform dependencies; added a
  security-layer OSC 52 evaluator that bounds payloads, denies remote writes by
  default, and refuses unsupported clipboard reads; wired desktop keyboard
  copy/paste, paste protection, bracketed paste, middle-click paste suppression
  during mouse reporting, OSC 52 policy application, and optional clipboard
  operation logging; documented current policy and remaining primary-selection
  and remote-confirmation gaps.
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
- Completed next-pass Phase 12 command blocks and visual overlays: added
  conservative alternate-screen policy config for prompt and command overlays,
  desktop semantic projection for command-block backgrounds, input/output
  grouping overlays, and status/duration/current-directory/shell/host badges,
  renderer batch ordering that keeps command overlays behind terminal text,
  overlay-label glyph batching for badges, and tests for disabled cost,
  alternate-screen suppression, metadata badge generation, and renderer draw
  ordering.
- Implemented the Phase 12 SSH transport foundation: expanded portable SSH
  profile config, added a security-layer host-key and secret contract,
  implemented an `ssh2`-backed `TerminalTransport` with host-key verification,
  agent/public-key/password authentication boundaries, remote PTY allocation,
  read/write/resize/shutdown support, bounded best-effort drop behavior, SSH
  mux session specs, and desktop config-to-transport profile mapping.
- Implemented the Phase 13 platform parity diagnostics foundation: replaced the
  initial platform-support note with a feature-level macOS/Windows/Linux X11/
  Linux Wayland parity matrix, added explicit polish checklists and verification
  status, introduced shared support-level terminology, added reusable
  `doctor`/bug-report diagnostics models for platform, window, renderer,
  config, shell integration, performance, and SSH checks, and exposed them
  through `cargo xtask doctor ...` and `cargo xtask bug-report`.
- Implemented the Phase 14 hardening and release-readiness foundation: added
  desktop panic boundaries around platform event translation, renderer
  frame/resize work, transport input/output/resize/shutdown, and parser
  application; added diagnostics models for stability hardening, security
  review, packaging planning, and release validation gates; exposed
  `cargo xtask hardening`, `cargo xtask security-review`,
  `cargo xtask package-plan`, and `cargo xtask release-check`; and added
  operational docs for getting started, themes, cursor customization, SSH
  profiles, mux usage, troubleshooting, packaging, release validation, and
  Linux compositor notes.
- Implemented the Phase 15 iOS SSH companion foundation: added `apps/ios` as a
  shared-engine Rust crate, modeled mobile lifecycle/touch/keyboard/safe-area
  sizing contracts, mapped portable SSH profiles into mobile SSH session specs
  while preserving host-key and auth policy, proved terminal bytes flow through
  the shared parser/core, added mobile render-frame sizing helpers, documented
  iOS lifecycle/security limits, and exposed `cargo xtask ios-readiness`.

## Deferred By Design

- Unicode cell storage, grapheme boundaries, emoji modifiers, ZWJ sequences,
  variation selectors, selection, cursor movement, and resize/scrollback
  behavior are covered in the core/parser model. Font fallback, renderer
  shaping, screenshot parity, and real app conformance remain later phases.
- The fuzzing harness exists and property smoke tests are wired into CI, but
  long-running coverage-guided fuzz history and cross-OS scheduled results
  must accumulate over time before fuzzing can be treated as exhaustive.
- Mouse/focus/application keypad behavior is represented as modes in Phase 2;
  event semantics are implemented in later platform and parser compatibility
  phases.
- Phase 4 translates platform mouse/focus/IME events into platform-neutral
  events, but terminal mouse reporting, mouse-driven selection UI, focus escape
  reports, and application keypad/cursor encoding remain later compatibility
  work.
- Phase 4 models Linux X11/Wayland backend and decoration preferences, and
  next-pass Phase 7 adds a concrete compositor verification matrix and xtask
  report. Compositor-specific fullscreen/frameless behavior still needs real
  Linux X11/Wayland host runs before it can be called complete.
- Clipboard copy/paste has a platform bridge and diagnostics; OSC 52 has a
  bounded policy with safe remote defaults. Linux primary selection provider
  support, remote OSC 52 confirmation UI, mouse-driven selection UX, and real
  cross-OS clipboard smoke coverage remain later platform/compatibility work.
- Real local transport smoke tests remain ignored by default because they spawn
  host shells. On the current Windows host, the one-shot, interactive, and
  event-loop smoke tests pass quickly. macOS and Linux smoke status is still
  unverified until those platforms are actually run.
- The renderer now submits GPU-facing background/glyph/decoration/selection/
  cursor batches, uploads only new glyph atlas rows, and has a GPU recovery
  path that rebuilds disposable WGPU resources while preserving terminal state.
  Cross-OS screenshot automation now exists with a verified Windows baseline;
  macOS, Linux X11, and Linux Wayland baseline capture/verification, hardware
  GPU timestamp queries, real sleep/wake and monitor-change device-loss
  validation, batch vector
  reuse/pooling, and deeper font fallback/shaping validation remain deferred
  render-performance work.
- Phase 5 has build/test verification on the current Windows host. macOS,
  Linux X11, and Linux Wayland rendering remain unverified until run on those
  platforms.
- Phase 6 establishes compatibility mechanics and lower-level golden coverage,
  but app-level smoke testing for bash/zsh/fish/PowerShell/cmd/vim/less/TUI
  apps/tmux/screen/zellij/SSH remains unverified until run on the relevant
  host platforms.
- Primary selection provider support, remote OSC 52 confirmation UI,
  application keypad output mapping, a custom terminfo entry, configurable hint
  patterns, and real app-level Unicode conformance remain deferred
  compatibility work.
- Next-pass Phase 9 adds a debounced TOML file watcher and desktop live-reload
  applier. It applies safe sections, reports restart-required settings, and
  keeps the previous active config after parse, validation, or runtime apply
  failures. Native OS watcher backends, richer error UI, and macOS/Linux
  runtime validation remain deferred.
- Programmable config remains deferred until the static TOML model has more
  runtime mileage. It must compile into the same `AppConfig` and stay out of the
  render hot path.
- Phase 8 uses CPU-side timing plus WGPU submission wall-clock timing. Hardware
  GPU timestamp queries and richer in-window overlay rendering remain deferred
  until screenshot verification and stable overlay composition are in place.
- Next-pass Phase 10 wires the native mux model into the desktop app for local
  sessions: tabs, horizontal/vertical splits, focus, resize, close, zoom,
  per-pane terminal/semantic/PTY ownership, active-pane input routing,
  split-aware render scene composition, basic tab chrome, and pane borders now
  exist. Declarative startup layouts, SSH panes, polished tab/move UI, and
  cross-OS native mux GUI smoke tests remain follow-up work.
- Next-pass Phase 11 wires local shell integration activation into desktop
  session startup through a shared activation plan, supports full/auto/manual/
  heuristic/off config modes, injects runtime hooks for bash/zsh/fish/
  PowerShell where safe, disables semantic parsing for off mode, and adds
  bounded real-shell verification tests. The PowerShell semantic smoke passed
  on the current Windows host. Real bash/zsh/fish, WSL, remote, macOS, Linux
  X11, and Linux Wayland verification, plus heuristic command detection and
  remote install flows, remain follow-up work before command blocks can be
  considered product-complete.
- Next-pass Phase 12 establishes command-block visual overlay projection and
  badge glyph batching, but collapse/expand long output UI, command-block copy
  mode UI, real shell-driven command-block verification beyond the Windows
  PowerShell semantic smoke, and cross-OS visual smoke coverage remain deferred
  until the app compatibility and cross-OS runner phases.
- Next-pass Phase 13 establishes opt-in cursor animation runtime wiring,
  cursor-neighborhood damage regions, batched renderer animation quads,
  `[cursor.image]` config, nonblocking GIF/PNG image metadata decode/cache, and
  budget warnings. Full animated image pixel-frame decode/upload/draw,
  polished visual tuning, and macOS/Linux X11/Linux Wayland visual smoke
  verification remain deferred.
- Next-pass Phase 14 adds `cargo xtask compat`, bounded real-process and
  real-PTY compatibility probes, fixture scripts, report generation under
  `target/compatibility`, and a manual app checklist. The required Windows
  PowerShell/cmd/protocol subset passed on the current host. Optional app
  probes, full interactive editor/TUI/multiplexer behavior, WSL, SSH server
  sessions, and macOS/Linux X11/Linux Wayland reports remain follow-up work.
- Next-pass Phase 15 hardens SSH trust and secret contracts: unknown-host and
  changed-host decisions now flow through explicit `HostTrustProvider`
  actions, the default trust provider rejects rather than silently accepting,
  passphrases/passwords can be backed by a `KeychainProvider` plus prompt flow,
  platform keychain capability reporting exists for Windows, macOS, Linux, and
  iOS targets, and docs/diagnostics explain that native provider wiring and
  desktop UI remain follow-up work.
- Phase 12 establishes secure SSH transport contracts and a real backend, but
  desktop host-key approval UI, native OS keychain backend wiring, proxy jump,
  remote shell-integration install flows, reconnect UI/actions, and real SSH
  server smoke tests across Windows/macOS/Linux remain unverified follow-up
  work.
- Phase 13 adds the parity matrix and diagnostics command foundation, but it
  does not magically verify macOS, Linux X11, or Linux Wayland from this Windows
  host. Real platform labs/CI runners, compositor coverage, GPU backend
  inventory, native notification support, remote OSC clipboard confirmation UI, and the final
  installed `terminal doctor` binary remain follow-up work before platform
  parity can be called product-complete.
- Phase 14 does not make Panea a daily-driver release by itself. Daily-driver
  release status remains blocked until packaged artifacts exist and
  manual/automated validation passes on macOS, Windows, Linux X11, and Linux
  Wayland. OS keychain-backed secret providers, interactive SSH host-key
  approval UI, remote OSC 52 clipboard confirmation UI, real GPU device-loss
  platform validation, cross-OS config reload validation, richer config error
  UI, and installer automation remain release-hardening follow-up work.
- Phase 15 establishes a compileable iOS companion foundation around the shared
  engine, but it is not a native iOS app yet. UIKit/SwiftUI app lifecycle,
  native touch/keyboard/settings UI, iOS GPU surface implementation, iOS
  Keychain-backed `SecretProvider`, host-key approval UI, key import UX, remote
  shell-integration activation, simulator/device validation, and App Store/Test
  Flight packaging remain follow-up work.
