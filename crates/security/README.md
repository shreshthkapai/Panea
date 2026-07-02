# security

- Owns: secret handling interfaces, key storage policy, SSH security helpers, redaction contracts, and trust decisions.
- Must not import: renderers, platform window event loops, config frontends executing untrusted code, semantic visual overlays.
- Layer: security.
- Tests required: redaction behavior, host key policy, secret lifetime boundaries, config safety checks, and audit-friendly error states.
