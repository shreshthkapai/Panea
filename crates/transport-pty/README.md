# transport-pty

- Owns: Unix PTY and Windows pseudoconsole integration behind the transport-core contract.
- Must not import: renderer crates, semantic overlay drawing, config frontends, mux layout, platform window code.
- Layer: session transport.
- Tests required: PTY spawn/shutdown, resize propagation, pseudoconsole behavior, byte I/O, lifecycle errors, and cross-platform fallbacks.
