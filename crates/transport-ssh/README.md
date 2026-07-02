# transport-ssh

- Owns: SSH session transport, host key policy integration, authentication flow boundaries, and remote PTY allocation.
- Must not import: renderers, platform window APIs, semantic overlay drawing, mux layout internals, config frontends executing scripts.
- Layer: session transport and security.
- Tests required: host key decisions, auth failure states, remote PTY resize, byte I/O, shutdown, and security diagnostics.
