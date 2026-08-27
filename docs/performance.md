# Performance

Performance claims require measurements. Optional systems must have bounded
costs and near-zero cost when disabled.

## Local Benchmarks

Run the repeatable harness through xtask. The wrapper uses the release profile
so benchmark results are not dominated by debug-mode overhead:

```powershell
cargo xtask bench all
cargo xtask bench gui-startup --samples 5 --config default
cargo xtask bench gui-prompt --samples 5 --config default
cargo xtask bench gui-input --samples 5 --config default
cargo xtask bench render-grid
cargo xtask bench render-full-ascii
cargo xtask bench render-mixed-unicode
cargo xtask bench render-emoji-heavy
cargo xtask bench render-fast-scrolling
cargo xtask bench render-large-scrollback-viewport
cargo xtask bench render-many-panes
cargo xtask bench render-cursor-animation
cargo xtask bench render-command-blocks
cargo xtask bench cat-large-file
cargo xtask bench color-heavy
cargo xtask bench scrollback
cargo xtask bench resize
cargo xtask bench parser-input
cargo xtask bench unicode
cargo xtask bench alternate-screen
cargo xtask bench cursor-animation
cargo xtask bench fullscreen-chrome
```

The three `gui-*` commands run the release desktop binary against a real
window, GPU surface, PTY, and shell. `--config default` uses an empty temporary
TOML file so discovered user configuration cannot change the result;
`--config user` (the default) deliberately measures the user's actual config,
shell profile, transparency, and prompt setup. The temporary default config is
removed after the run. Use `--backend auto|vulkan|metal|dx12|gl` only when
measuring backend differences; normal product measurements should use `auto`.

The GUI metrics are distributions, not single stopwatch readings:

- `gui-startup`: process start to the first successfully presented terminal frame
- `gui-prompt`: process start to the first recognized shell prompt
- `gui-input`: accepted input burst to the frame containing its PTY echo

`gui-input` waits 100 ms after recognizing the prompt before taking the sample.
That wait is excluded from the metric and prevents shell startup from being
misreported as steady-state input latency. Its JSON output separates
input-to-output (transport, shell echo, and parser) from output-to-present
(scene preparation, GPU submission, and presentation). `parser-input` is a
parser/grid microbenchmark only; the old `input-latency` spelling remains an
explicitly labeled compatibility alias.

The cursor-animation benchmark also measures default and heavy decoded image
cursor frame sets. Disabled mode allocates no image asset, creates no image
quad, and schedules no animation frame.

The fullscreen-chrome benchmark uses a fixed `1920x1080` surface and a
`120 ms` transition. It reports disabled, instant, and smooth cases with CPU
preparation time, frame count, dirty pixels, and draw calls. Disabled mode
performs no render preparation; animated damage is clipped to the configured
chrome height. Runtime chrome metrics are allocated only while the feature is
enabled in a supported fullscreen mode.

The harness reports elapsed time, byte throughput where applicable, frame
timing, CPU render preparation time, glyph cache hits/misses, atlas uploads,
damage region count, draw-call count, animated region count, idle wakeups, and
performance-gate warnings.
Renderer commands report cold cache population separately from timed warm
iterations, so glyph discovery/rasterization cost is not confused with steady
state batch preparation.

## Local Engineering Baseline

This is a dated development baseline, not a cross-product or cross-platform
claim. It exists so later changes can detect regressions on the same machine.

```text
Date: 2026-08-27
Panea: 0.1.10 working tree, release profile
OS: Windows 11 build 26200
CPU: AMD Ryzen 7 7840HS
GPU: NVIDIA GeForce RTX 4060 Laptop GPU
Power: battery
Samples: 5 warm process launches per GUI metric
```

| Config | Effective backend | Metric | p50 | p95 |
|---|---|---:|---:|---:|
| portable defaults | DX12 | process to first terminal frame | 478 ms | 489 ms |
| portable defaults | DX12 | process to prompt observed | 663 ms | 677 ms |
| portable defaults | DX12 | input burst to presented echo | 25.2 ms | 26.0 ms |
| personal transparent config + Oh My Posh | Vulkan | process to first terminal frame | 522 ms | 527 ms |
| personal transparent config + Oh My Posh | Vulkan | process to prompt observed | 1,077 ms | 1,087 ms |
| personal transparent config + Oh My Posh | Vulkan | input burst to presented echo | 23.6 ms | 24.6 ms |

In the default input trace, the median parsed-output-to-present portion was
about 2.9 ms; most of the remaining time was before Panea observed echoed PTY
bytes. The clear-only startup pass removed a redundant full-surface quad upload
and reduced the measured startup-background pass from tens of milliseconds to
roughly 0.6-12 ms depending on the WGPU backend. Driver initialization remains
the dominant startup phase on this host.

Renderer batching details and the Phase 4 design note live in
[renderer-batching.md](renderer-batching.md).
Renderer device-loss recovery details and the Phase 5 design note live in
[renderer-device-recovery.md](renderer-device-recovery.md).
Performance instrumentation and the Phase 17 overlay design note live in
[performance-instrumentation.md](performance-instrumentation.md).
Cursor animation details and the Phase 13 design note live in
[cursor-customization.md](cursor-customization.md).

## Profiles

The portable performance profiles are:

- `maximum_performance`
- `balanced`
- `visual`
- `battery_saver`

Profiles describe budget posture. They are not public performance claims.
When `disable_expensive_effects_on_battery = true`, Panea samples the platform
power provider outside hot paths and temporarily applies battery-saver caps to
optional animation work. Returning to AC restores the configured profile.

## Gates

Internal gates:

- idle terminals must not redraw constantly
- static cursors should be negligible
- default mode should stay smooth under normal shell output
- disabled features should not appear in hot-path profiles
- cursor animations should redraw only cursor-neighborhood regions where possible
- heavy visual settings should warn when budgets are exceeded
- `diagnostics.performance_overlay = false` should retain near-zero runtime
  cost; when enabled, it must report backend, frame pacing, GPU timing status,
  glyph atlas/cache stats, damage/draw counts, PTY/parser throughput, and
  memory estimates

The installed overlay supports compact/detailed views, all four window
corners, click controls, `Ctrl+Shift+F12`, and an optional persisted runtime
preference. Persistence writes only on a user setting change; disabled mode
does not collect mux throughput/memory samples or project overlay primitives.

Do not claim the terminal is faster than another terminal until benchmarks are
reproducible, fair, and documented.

For comparisons with Alacritty, WezTerm, or Kitty, record the exact commit/version,
release build, GPU/backend, window grid, font/fallback chain, shell/PTY fixture,
warm-up count, and optional-feature state. Report latency/throughput
distributions and include cases Panea loses; a single FPS number is not a
terminal-performance result. Compare only on platforms each product supports;
Kitty is not a native Windows comparison target. GitHub-hosted runners are
useful regression gates but are not controlled machines for public competitor
claims.
