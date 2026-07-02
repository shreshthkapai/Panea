# transport-core

- Owns: terminal session I/O traits, output/input byte contracts, resize contracts, lifecycle events, metadata, and transport errors.
- Must not import: windows, renderers, platform event loops, terminal parser state, config frontends, mux, semantic visuals.
- Layer: session transport.
- Tests required: metadata contracts, lifecycle state modeling, resize payload invariants, error-state behavior, and dependency-boundary tests.
