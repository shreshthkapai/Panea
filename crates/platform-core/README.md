# platform-core

- Owns: platform capability contracts, input events, monitor/DPI information, clipboard and notification provider contracts, compositor metadata, and explicit fallback reporting.
- Must not import: winit, GPU renderer implementations, terminal parser state, PTY/SSH transports, config frontends.
- Layer: platform parity.
- Tests required: event model invariants, capability/fallback modeling, monitor/DPI payloads, and dependency-boundary tests.
