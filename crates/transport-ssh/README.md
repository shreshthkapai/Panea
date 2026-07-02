# transport-ssh

- Owns: SSH session transport, host key policy integration, authentication flow boundaries, and remote PTY allocation.
- Must not import: renderers, platform window APIs, semantic overlay drawing, mux layout internals, config frontends executing scripts.
- Layer: session transport and security.
- Tests required: host key decisions, auth failure states, remote PTY resize, byte I/O, shutdown, and security diagnostics.

Current implementation provides a `TerminalTransport` backend using `ssh2`,
secure host-key verification through the `security` crate, agent/public-key/
password auth boundaries, remote PTY allocation, read/write/resize/shutdown,
and bounded best-effort drop behavior.

Real remote smoke tests require a configured SSH server and credentials and are
not part of the default workspace test suite yet.
