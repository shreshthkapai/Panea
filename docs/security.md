# Security

Security and user trust outrank convenience.

## SSH Host Keys

SSH transport must never silently skip host verification.

Supported policies:

- `ask`: default. Existing known hosts are trusted; unknown hosts fail with a
  fingerprint that the UI/session manager must present to the user.
- `require_known`: only pre-existing known-host entries are accepted.
- `trust_on_first_use`: explicitly opts into storing the first observed key.
- `pin_fingerprint`: trusts only the configured `SHA256:<base64>` fingerprint.

Changed host keys are blocking mismatches. The known-hosts store is JSON so it
is inspectable and easy to audit during early development.

Interactive trust decisions flow through `security::HostTrustProvider`.
The default provider rejects unknown and changed host keys. App UI may expose
explicit actions such as trust once, trust and store, or replace stored key,
but those decisions must stay visible to the user.

## Secrets

Passphrases and passwords flow through the `security::SecretProvider`
interface. The default provider returns no secrets. A
`KeychainBackedSecretProvider` can read from a `KeychainProvider`, then delegate
to app UI for a prompt, and write back only when the user explicitly chooses to
save the secret. Secret debug output is redacted, and in-memory strings are
zeroized on drop where practical.

Desktop secure storage uses Windows Credential Manager, macOS Keychain, or
Linux Secret Service through `PlatformKeychainProvider`. The app displays
masked credential prompts and persists only when the user explicitly enables
the save option. iOS still requires its native Keychain bridge.

## Clipboard

System clipboard copy/paste exists through the platform bridge. Paste handling
can normalize newlines and strip control characters when
`clipboard.paste_protection` is enabled.

OSC 52 clipboard writes are parsed into pending terminal requests, decoded only
after policy checks, bounded by `clipboard.osc52.max_bytes`, and denied for
remote sessions by default. Local OSC 52 writes are allowed by default because
they are common terminal behavior, but users can disable them with:

```toml
[clipboard.osc52]
enabled = false
```

Remote sessions require explicit opt-in:

```toml
[clipboard.osc52]
allow_remote = true
confirm_remote_writes = true
```

When confirmation is enabled, Panea displays a renderer overlay with the remote
session identity, clipboard target, and bounded payload size. It never displays
the clipboard contents. `Y` permits one write after the full policy is checked
again; `N` or Escape rejects it. Only one request can wait per pane.

## Diagnostics and Logs

Bug-report snapshots must not include terminal contents, command output,
environment variables, secrets, SSH keys, or clipboard contents by default.

Run:

```powershell
cargo xtask security-review
```

Run real SSH transport smoke tests against a controlled server with:

```powershell
cargo xtask ssh-smoke run --host 127.0.0.1 --user panea --auth agent
```

Current blockers for release security posture:

- real remote OSC clipboard application smoke is still required on every OS
- shell integration installer trust and update policy still need review
- real SSH trust/auth reports must be collected on every target OS
