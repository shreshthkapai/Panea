# term-core

- Owns: platform-neutral terminal state primitives: cells, lines, grid, cursor, modes, scrollback, selection, viewport, resize state.
- Must not import: parser adapters, renderers, GPU APIs, platform/window APIs, transports, config frontends, mux, shell integration, diagnostics UI.
- Layer: core correctness.
- Tests required: grid/cursor/mode/selection behavior, resize invariants, Unicode cell storage, scrollback invariants, and dependency-boundary tests.
