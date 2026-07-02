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
cargo xtask config-default
cargo xtask config-schema
cargo xtask bench all
cargo xtask doctor
cargo xtask bug-report
cargo xtask hardening
cargo xtask security-review
cargo xtask package-plan
cargo xtask release-check
cargo xtask ios-readiness
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

## Native Mux Rule

Panea's native mux owns workspaces, tabs, panes, sessions, and layout state.
External multiplexers such as tmux, screen, and zellij must continue to run as
normal terminal applications inside a pane. Mux features may resize and focus
Panea panes, but must not special-case or parse external mux internals.

## Release Readiness Rule

A release candidate is not daily-driver ready until `cargo xtask release-check`
has no blockers and the platform matrix has been validated on macOS, Windows,
Linux X11, and Linux Wayland. Local Windows success is not enough.
