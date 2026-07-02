# Multiplexer Usage

Panea has a native mux model for workspaces, windows, tabs, panes, split trees,
and sessions.

Native mux responsibilities:

- create, close, rename, move, and switch tabs
- split panes horizontally and vertically
- focus, resize, zoom, move, swap, and close panes
- persist layout metadata where safe

External multiplexers such as tmux, screen, and zellij remain normal terminal
applications inside a pane. Panea must not parse or special-case their internals.

Process resurrection is not promised. Restored layouts may reopen shell or SSH
profiles, but long-running process persistence belongs to a future session
mechanism or external tools.
