# Development

Build in layers. Do not introduce a higher-layer dependency into a lower layer.

## Standard Commands

```powershell
cargo fmt --all
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo check --workspace
cargo test --workspace
cargo build --workspace
```

The optional xtask wrapper exposes the same gates:

```powershell
cargo xtask fmt
cargo xtask clippy
cargo xtask test
cargo xtask build
cargo xtask ci
```

## Contribution Gates

No pull request is mergeable unless it answers:

- Does this affect terminal correctness?
- Does this affect renderer hot path?
- Does this affect config schema?
- Does this affect cross-OS behavior?
- Does this require diagnostics?
- Does this require a benchmark?

## Layer Boundary Rule

Each crate owns one layer. Lower layers must not import higher layers casually.
If a lower layer needs a higher-layer concept, redesign the boundary.
