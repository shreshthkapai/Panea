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

OS keychain-backed secret providers and interactive trust UI are not complete
yet as product UI, but the security layer now has explicit provider contracts:

- `HostTrustProvider` for unknown-host and changed-host decisions
- `SecretProvider` for password/passphrase requests
- `KeychainProvider` for platform secret storage capability and persistence

Until native providers and prompts are wired into the desktop app, unavailable
secret storage must be reported as a fallback and must not become plaintext
config.
