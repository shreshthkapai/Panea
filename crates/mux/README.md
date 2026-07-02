# mux

- Owns: tabs, panes, workspaces, sessions, layout tree, and multiplexer state transitions.
- Must not import: GPU renderer implementations, platform window backends, config frontends, shell integration installers.
- Layer: multiplexer structure.
- Tests required: layout tree invariants, pane/session lifecycle, tab/workspace routing, resize distribution, and persistence contracts.

## Compatibility Policy

The native mux wraps terminal sessions. External multiplexers such as tmux,
screen, and zellij run inside panes as ordinary terminal applications. The mux
must not intercept their protocol behavior; it only resizes the pane/session and
routes user actions at the Panea UI layer.
