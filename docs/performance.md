# Performance

Performance claims require measurements. Optional systems must have bounded
costs and near-zero cost when disabled.

## Local Benchmarks

Run the repeatable harness through xtask:

```powershell
cargo xtask bench all
cargo xtask bench render-grid
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
- animations should redraw only affected regions where possible
- heavy visual settings should warn when budgets are exceeded

Do not claim the terminal is faster than another terminal until benchmarks are
reproducible, fair, and documented.
