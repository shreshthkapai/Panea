# Bench

Repeatable benchmarks and benchmark fixtures live here.

Performance claims must be backed by measurements from this layer.

Run locally:

```text
cargo xtask bench all
cargo xtask bench gui-startup --samples 5 --config default
cargo xtask bench gui-prompt --samples 5 --config default
cargo xtask bench gui-input --samples 5 --config default
cargo xtask bench render-grid
cargo xtask bench render-coding-agent
cargo xtask bench render-partial-update
cargo xtask bench cat-large-file
cargo xtask bench scrollback
cargo xtask bench resize
cargo xtask bench parser-input
cargo xtask bench unicode
cargo xtask bench alternate-screen
cargo xtask bench cursor-animation
```

The first benchmark fixtures are deterministic generators in `panea-bench`.
`render-coding-agent` covers dense agent output and long URLs.
`render-partial-update` reports p50 and p95 CPU preparation time while changing
one cell and recycling the production batch storage between frames.
Large binary or captured fixtures should be added under `fixtures/` only when
they are stable, reviewable, and safe to commit.

The GUI commands launch the release desktop binary and report distributions.
Use `--config default` for a portable baseline and `--config user` to include
the discovered user config, shell profile, prompt, opacity, and visual settings.
`parser-input` intentionally excludes platform input, PTY, shell, GPU, and
presentation costs.
