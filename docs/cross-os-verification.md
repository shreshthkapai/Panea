# Cross-OS Verification

Panea's platform promise is not satisfied by code that only works on one
developer machine. The cross-OS runner is the shared verification contract for
Windows, macOS, Linux X11, and Linux Wayland.

## Command

```text
cargo xtask verify-os run --target-platform <platform> --suite ci
```

Supported platform keys:

```text
windows
macos
linux-x11
linux-wayland
```

Reports are written to:

```text
target/cross-os/<platform>/report.md
target/cross-os/<platform>/report.json
target/cross-os/<platform>/logs/
```

Each step has a bounded timeout, separate stdout/stderr logs, a markdown
summary, and a machine-readable JSON result. Required step failures fail the
runner. Skipped and blocked steps are reported explicitly rather than silently
counted as success.

## Suites

```text
smoke
ci
full
```

`smoke` runs the platform verification contract without the formatting gate.
`ci` adds the formatting gate and is what GitHub Actions uses. `full` adds
optional application compatibility probes for tools that may or may not be
installed on a runner.

## Coverage

The runner composes existing Panea verification tools:

- architecture layer checks
- workspace unit tests
- parser tests
- Unicode/grapheme terminal-core tests
- fuzz regression/property smoke tests
- renderer tests
- config tests
- clipboard/OSC 52 policy tests
- shell integration tests
- PTY tests
- screenshot verification
- required app compatibility smoke tests
- doctor JSON diagnostics
- Linux compositor diagnostics on Linux targets
- SSH real-server smoke tests when explicitly configured
- packaging smoke status through the packaging plan

SSH smoke tests run only when `--with-ssh` is passed or
`PANEA_SSH_SMOKE_HOST` is set. The runner does not invent a fake SSH pass; it
records the test as skipped unless a real server is configured.

## CI Runners

`.github/workflows/cross-os-verification.yml` defines four jobs:

```text
windows       -> windows-latest
macos         -> macos-latest
linux-x11     -> ubuntu-latest with XDG_SESSION_TYPE=x11
linux-wayland -> ubuntu-latest with XDG_SESSION_TYPE=wayland
```

The Linux CI jobs exercise the Linux X11/Wayland code paths and diagnostics
contract, but hosted Ubuntu runners are not a substitute for real compositor
verification. GNOME, KDE, wlroots/Sway, Hyprland, XFCE, i3, Openbox-class WMs,
and COSMIC evidence still belongs in the Linux compositor matrix.

## Screenshot Baselines

CI runs with:

```text
--allow-missing-screenshot-baseline
```

This keeps platform runners usable before all baselines exist while still
recording missing baselines as `blocked`. Screenshot parity is not verified for
a platform until its baseline is captured and the screenshot step passes
without that allowance.

## Design Note

Feature name: real cross-OS verification runners

Layer: diagnostics, platform parity

User-facing behavior: developers and CI can run one command per supported
platform and receive a comparable pass/fail/blocked report.

Config keys: none.

macOS behavior: runs the same verification contract on a macOS runner and
records macOS-specific failures in `target/cross-os/macos`.

Windows behavior: runs the same verification contract on a native Windows
runner and records Windows-specific failures in `target/cross-os/windows`.

Linux X11 behavior: runs the same verification contract with
`XDG_SESSION_TYPE=x11`, includes Linux compositor diagnostics, and writes
`target/cross-os/linux-x11`.

Linux Wayland behavior: runs the same verification contract with
`XDG_SESSION_TYPE=wayland`, includes Linux compositor diagnostics, and writes
`target/cross-os/linux-wayland`.

Fallback behavior: tests that require unavailable external infrastructure, such
as a configured SSH server or missing screenshot baselines, are marked skipped
or blocked with an explanation.

Diagnostics: markdown and JSON reports include target platform, detected host,
suite, step status, exit code, duration, notes, and log paths.

Performance cost when disabled: zero; this is an explicit developer/CI command.

Performance cost when enabled: bounded by per-step timeout and normal cargo
test/runtime cost.

Tests: `xtask` unit tests cover option parsing, Linux target environment, step
coverage, and report serialization; the current host can run a short
`verify-os` smoke.
