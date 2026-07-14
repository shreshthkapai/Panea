# SSH Trust and Secrets

## Feature Design Note

```text
Feature name: SSH trust, secrets, and keychain providers
Layer: security, session transport, diagnostics
User-facing behavior: unknown SSH hosts require explicit trust, changed host keys block, and passwords/passphrases never live in config
Config keys: ssh_profiles.* plus existing known_hosts_policy/auth_method/identity_file/agent_forwarding fields
macOS behavior: same SSH trust model; native provider is macOS Keychain
Windows behavior: same SSH trust model; native provider is Windows Credential Manager
Linux X11 behavior: same SSH trust model; native provider is Secret Service where available
Linux Wayland behavior: same SSH trust model; native provider is Secret Service where available
Fallback behavior: if no provider is available, secrets are not persisted and the session must prompt or fail clearly
Diagnostics: doctor/security-review reports host-key policy, secret-provider boundaries, and native-provider gaps
Performance cost when disabled: zero render/input/PTY cost
Performance cost when enabled: one host-key check per SSH connection and one secret lookup/prompt per auth request; not in render or PTY hot paths
Tests: security unit tests for host-key decisions, redaction, keychain-backed prompt flow, provider fallback, and desktop prompt actions/masking
```

## Host Trust Model

Panea never disables host-key verification by default.

- `ask` is the default. Known hosts are accepted. Unknown hosts require an
  app-provided `HostTrustProvider` decision before connecting.
- `require_known` accepts only an existing known-host entry.
- `trust_on_first_use` stores the first observed key only because the user
  explicitly chose that policy.
- `pin_fingerprint` accepts only the configured SHA256 fingerprint.

Changed host keys are blocking. An app UI may expose an explicit
`replace stored key` action, but the default provider rejects the connection.
Pinned fingerprint mismatches require config or known-host resolution; they are
not silently overridden.

## Secrets

Passwords and passphrases flow through `security::SecretProvider`.

The config file stores intent only:

- auth method
- identity file reference
- known-host policy
- agent forwarding preference

It must not store passwords, passphrases, private key bytes, or recovery
tokens.

`security::KeychainBackedSecretProvider` first asks a `KeychainProvider`.
If no stored secret exists, it can delegate to an app-level prompt provider.
Only a prompt response that explicitly requests persistence is written back to
the keychain provider.

## Platform Providers

The platform targets are:

- Windows: Credential Manager or equivalent Windows secret storage.
- macOS: Keychain.
- Linux X11 and Wayland: Secret Service/libsecret-compatible provider.
- iOS later: Keychain.

The desktop provider uses the native credential store through a maintained safe
Rust adapter. Product code treats an unavailable provider as a clear fallback:
the user may continue with a transient secret or cancel. It never writes
plaintext secrets to config or logs.

Run the opt-in native provider smoke on each target host with:

```text
cargo test -p security native_platform_keychain_round_trip -- --ignored
```

The smoke writes and deletes one process-scoped test credential. Normal unit
tests never write to the user's credential store.

## Diagnostics and Logging

Diagnostics may include:

- profile name
- host and port
- auth method
- host-key policy
- host-key fingerprint
- provider capability/fallback message

Diagnostics must not include:

- passwords
- passphrases
- private keys
- raw terminal contents
- command output
- clipboard contents
