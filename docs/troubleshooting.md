# Troubleshooting

Start with doctor commands:

```powershell
cargo xtask doctor
cargo xtask doctor renderer
cargo xtask doctor config
cargo xtask doctor platform
cargo xtask doctor shell-integration
cargo xtask doctor performance
cargo xtask doctor ssh
cargo xtask doctor window
```

Create a privacy-aware report:

```powershell
cargo xtask bug-report
```

The bug-report snapshot excludes terminal contents, command output, environment
variables, secrets, SSH keys, and clipboard contents.

For release readiness:

```powershell
cargo xtask hardening
cargo xtask security-review
cargo xtask package-plan
cargo xtask release-check
```

Common current blockers:

- macOS and Linux X11/Wayland smoke tests are not verified from this Windows host
- packaged installers are not produced yet
- OS keychain-backed secret storage is not wired
- OSC 52 clipboard policy is intentionally deferred
