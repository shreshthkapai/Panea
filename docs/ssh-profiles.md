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

Remote semantic hooks can be prepared without weakening SSH trust:

```powershell
panea shell-integration remote-plan --shell zsh --profile production
panea shell-integration export --shell zsh --output panea.zsh
```

The helper only emits a reviewable plan and local hook file. It never connects
to or modifies the remote account. Panea reports remote integration as active
only after semantic markers are observed in that SSH session.

`proxy_jump` is reserved for a later transport extension. Profiles that set it
currently receive a validation warning and the connection is rejected
explicitly; Panea does not silently ignore it or invoke a platform-specific
OpenSSH process behind the shared SSH transport contract.
