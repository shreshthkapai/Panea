# Compatibility Fixtures

This directory stores small, deterministic fixtures for app compatibility
checks. They are intentionally boring: short output, explicit markers, and no
network access by default.

Primary runner:

```text
cargo xtask compat plan
cargo xtask compat run --required-only --timeout-ms 5000
cargo xtask compat run --timeout-ms 5000
```

The runner writes generated reports under `target/compatibility/`; those reports
are not committed.

## Fixture Scripts

- `ansi-fixture.ps1` emits a truecolor SGR marker and OSC title marker for
  Windows/PowerShell hosts.
- `ansi-fixture.sh` emits the same markers for POSIX shells.

These scripts mirror the protocol fixture embedded in `cargo xtask compat`.
They are useful for manual reproduction inside Panea when a runner report
points to a protocol path problem.

## Manual Compatibility Areas

Some applications cannot be meaningfully verified by a non-interactive version
probe. Full compatibility still requires manual or future automated PTY-driving
checks for editors, pagers, TUIs, external multiplexers, WSL, and SSH sessions.
