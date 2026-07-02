# mux

- Owns: tabs, panes, workspaces, sessions, layout tree, and multiplexer state transitions.
- Must not import: GPU renderer implementations, platform window backends, config frontends, shell integration installers.
- Layer: multiplexer structure.
- Tests required: layout tree invariants, pane/session lifecycle, tab/workspace routing, resize distribution, and persistence contracts.
