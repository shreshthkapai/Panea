# Bench

Repeatable benchmarks and benchmark fixtures live here.

Performance claims must be backed by measurements from this layer.

Run locally:

```text
cargo xtask bench all
cargo xtask bench render-grid
cargo xtask bench cat-large-file
cargo xtask bench scrollback
cargo xtask bench resize
cargo xtask bench input-latency
cargo xtask bench unicode
cargo xtask bench alternate-screen
cargo xtask bench cursor-animation
```

The first benchmark fixtures are deterministic generators in `panea-bench`.
Large binary or captured fixtures should be added under `fixtures/` only when
they are stable, reviewable, and safe to commit.
