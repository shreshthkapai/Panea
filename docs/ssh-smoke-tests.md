# SSH Server Smoke Tests

## Feature Design Note

```text
Feature name: Real SSH server smoke tests
Layer: session transport, security, diagnostics
User-facing behavior: developers can verify Panea's SSH transport against a real server with bounded output, trust, resize, and cleanup checks
Config keys: none; the harness uses explicit CLI flags or PANEA_SSH_SMOKE_* environment variables
macOS behavior: same client harness; requires a reachable test SSH server and a macOS report before verification is claimed
Windows behavior: same client harness; requires a reachable test SSH server and a Windows report before verification is claimed
Linux X11 behavior: same client harness; requires a reachable test SSH server and a Linux X11 report before verification is claimed
Linux Wayland behavior: same client harness; requires a reachable test SSH server and a Linux Wayland report before verification is claimed
Fallback behavior: missing server configuration fails fast; no fake pass is recorded
Diagnostics: markdown report includes target, auth method, known-hosts path, trust behavior, lifecycle events, bytes received, and output previews
Performance cost when disabled: zero; this is an explicit developer command
Performance cost when enabled: bounded by connection and polling timeouts; no render/input hot-path work
Tests: `cargo test -p xtask`, `cargo test -p transport-ssh`, and real-server `cargo xtask ssh-smoke run ...`
```

## Command

Plan the suite:

```text
cargo xtask ssh-smoke plan
```

Run against a configured server:

```text
cargo xtask ssh-smoke run --host 127.0.0.1 --port 22 --user panea --auth agent
```

Public key auth:

```text
cargo xtask ssh-smoke run --host 127.0.0.1 --user panea --auth public_key --identity-file C:\path\to\id_ed25519
```

Password auth reads the password from a named environment variable:

```text
$env:PANEA_SSH_SMOKE_PASSWORD = "<password>"
cargo xtask ssh-smoke run --host 127.0.0.1 --user panea --auth password
```

The report is written to:

```text
target/ssh-smoke/<platform>.md
```

## Environment Variables

The runner accepts these fallback variables:

```text
PANEA_SSH_SMOKE_HOST
PANEA_SSH_SMOKE_PORT
PANEA_SSH_SMOKE_USER
PANEA_SSH_SMOKE_AUTH
PANEA_SSH_SMOKE_IDENTITY_FILE
PANEA_SSH_SMOKE_PASSWORD
PANEA_SSH_SMOKE_PASSPHRASE
```

Secret values are read only by the `SecretProvider` used for the smoke run.
They are not written to reports.

## What The Harness Verifies

- Unknown hosts are rejected by the default trust provider.
- Unknown hosts are accepted and persisted only by an explicit scripted trust
  action in the smoke harness.
- The persisted known-host entry supports a later `require_known` reconnect.
- A changed host-key fixture is detected and blocked.
- The real `transport-ssh` backend opens a remote PTY.
- Remote output includes a plain marker, a Unicode marker, and large-output
  markers.
- Remote PTY resize is sent through `TerminalTransport::resize`.
- Shutdown is bounded.
- Remote OSC 52 clipboard writes are denied by the default policy.

## Local Test Server

The harness intentionally does not install or start a privileged `sshd`.
Use a controlled local server, VM, container, or lab host with a disposable
test account.

Minimum server expectations:

- reachable host and port
- key, agent, or password auth for a disposable user
- remote PTY allocation allowed
- a POSIX shell for the default smoke command, or `--remote-kind powershell`
  for a PowerShell-capable server command path

Do not run this against production hosts unless the host-key trust behavior and
test account are intentionally set up for smoke testing.

## Verification Status

The harness is implemented. Panea is not cross-OS SSH verified until reports
exist for Windows, macOS, Linux X11, and Linux Wayland.
