# Performance

Performance claims require measurements. Optional systems must have bounded
costs and near-zero cost when disabled.

## Local Benchmarks

Run the repeatable harness through xtask. The wrapper uses the release profile
so benchmark results are not dominated by debug-mode overhead:

```powershell
cargo xtask bench all
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
cargo xtask bench input-latency
cargo xtask bench unicode
cargo xtask bench alternate-screen
cargo xtask bench cursor-animation
```

The harness reports elapsed time, byte throughput where applicable, frame
timing, CPU render preparation time, glyph cache hits/misses, atlas uploads,
damage region count, draw-call count, animated region count, idle wakeups, and
performance-gate warnings.

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

Do not claim the terminal is faster than another terminal until benchmarks are
reproducible, fair, and documented.
