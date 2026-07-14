# SSH Profiles

SSH profiles describe first-class remote terminal sessions.

Config fields:

- `name`
- `host`
- `port`
- `username`
- `auth_method`
- `identity_file`
- `known_hosts_policy`
- `remote_command`
- `remote_working_directory`
- `shell_integration`
- `agent_forwarding`
- `proxy_jump`

Host key verification is mandatory. Panea must never silently skip it.

Recommended release posture:

- use `require_known` or `pin_fingerprint` for high-trust profiles
- treat `trust_on_first_use` as an explicit user decision
- block changed host keys until resolved
- never include passwords, passphrases, private keys, terminal contents, or
  command output in diagnostics

Panea's desktop SSH panes use these provider contracts:

- `HostTrustProvider` for unknown-host and changed-host decisions
- `SecretProvider` for password/passphrase requests
- `KeychainProvider` for Windows Credential Manager, macOS Keychain, or Linux
  Secret Service persistence

Unknown and changed hosts are presented in a renderer overlay with explicit
actions. Password/passphrase entry is masked; Tab toggles opt-in native
keychain persistence. A disconnected pane preserves scrollback and can run the
`reconnect_session` action (default `Ctrl+Alt+R`). Unavailable secure storage is
reported and never replaced with plaintext config.
