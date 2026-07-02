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

## Secrets

Passphrases and passwords flow through the `security::SecretProvider`
interface. The default provider returns no secrets; UI or OS keychain-backed
providers must be added at the app boundary. Secret debug output is redacted,
and in-memory strings are zeroized on drop where practical.

Deferred intentionally: OS secure storage integration, interactive host-key
decision UI, and remote credential prompts are follow-up app lifecycle work.

## Clipboard

System clipboard copy/paste exists through the platform bridge. Paste handling
can normalize newlines and strip control characters.

OSC 52 clipboard access is intentionally not implemented yet. It needs a clear
permission policy, diagnostics, and safe defaults before release.

## Diagnostics and Logs

Bug-report snapshots must not include terminal contents, command output,
environment variables, secrets, SSH keys, or clipboard contents by default.

Run:

```powershell
cargo xtask security-review
```

Current blockers for release security posture:

- OS keychain-backed secret providers are not wired
- OSC clipboard policy is not implemented
- interactive SSH host-key approval UI is not complete
- shell integration installer trust and update policy still need review
