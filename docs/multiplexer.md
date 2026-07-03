# Multiplexer Usage

Panea has a native mux model for workspaces, windows, tabs, panes, split trees,
and sessions. The desktop app now wires that model to local pane runtimes: each
visible pane owns its own terminal emulator, semantic timeline, mouse protocol
state, and local PTY transport.

## Phase 10 Design Note

```text
Feature name: Desktop multiplexer runtime wiring
Layer: multiplexer structure, session transport, render performance
User-facing behavior: keybindings can create tabs, split panes, focus panes,
  resize panes, close panes, and zoom the active pane. The focused pane receives
  keyboard, mouse, paste, focus, and IME input.
Config keys: mux.enabled, mux.show_tab_bar, mux.pane_resize_step,
  keyboard.keybindings
macOS behavior: same config and runtime model; real GUI smoke unverified.
Windows behavior: build and unit tests pass on the current host; real multi-pane
  GUI smoke still needs a manual or automated run.
Linux X11 behavior: same config and runtime model; compositor/window-manager
  behavior unverified.
Linux Wayland behavior: same config and runtime model; compositor behavior
  unverified.
Fallback behavior: if mux.enabled is false, mux actions are ignored with a
  diagnostic; if a split/tab action fails, the current layout remains active.
Diagnostics: action failures are printed through the current desktop diagnostic
  surface; richer mux-specific doctor/UI reporting remains later work.
Performance cost when disabled: near zero beyond checking keybinding actions.
Performance cost when enabled: proportional to the number of panes polled and
  rendered; each pane is bounded to 64 output batches per event-loop tick.
Tests: mux model unit tests plus desktop keybinding/layout tests. Real
  cross-OS GUI smoke remains unverified.
```

Native mux responsibilities:

- create, close, rename, move, and switch tabs
- split panes horizontally and vertically
- focus, resize, zoom, move, swap, and close panes
- persist layout metadata where safe

Current desktop runtime behavior:

- one `PaneRuntime` per native pane
- one terminal emulator per pane
- one local PTY transport per pane
- per-pane terminal resize when split ratios, window size, tab switching, or
  zoom changes
- active-pane keyboard, IME, paste, focus, mouse, selection-copy, and OSC 52
  policy handling
- top-row tab chrome when more than one tab is open and `mux.show_tab_bar` is
  enabled
- renderer scene composition offsets each pane into its own viewport and draws
  basic pane borders as renderer decorations

External multiplexers such as tmux, screen, and zellij remain normal terminal
applications inside a pane. Panea must not parse or special-case their internals.

Process resurrection is not promised. Restored layouts may reopen shell or SSH
profiles, but long-running process persistence belongs to a future session
mechanism or external tools.

## Still Deferred

- declarative startup layout config
- SSH sessions opened directly as tabs or panes
- drag/move pane UI and pane swap commands with user-selectable targets
- polished tab chrome and tab mouse switching
- cross-OS native mux GUI smoke tests
- automated tmux/screen/zellij compatibility tests inside native panes
