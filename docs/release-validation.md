# Release Validation

Before any daily-driver release candidate, run:

- unit tests
- integration tests
- parser tests
- conformance fixtures
- renderer smoke tests
- benchmark suite
- config compatibility tests
- shell integration tests
- SSH tests
- platform parity matrix
- manual smoke tests on macOS, Windows, Linux X11, and Linux Wayland

Local commands:

```powershell
cargo xtask ci
cargo xtask bench all
cargo xtask doctor
cargo xtask hardening
cargo xtask security-review
cargo xtask release-check
```

Performance comparisons against other terminals must wait until benchmark
fixtures, machines, settings, and competing terminal configs are public and
fair. Results must include scenarios where Panea loses.
