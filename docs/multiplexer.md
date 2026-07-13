# Multiplexer Usage

Panea has a native mux model for workspaces, windows, tabs, panes, split trees,
and sessions. The desktop app wires that model to independent pane runtimes:
each pane owns a terminal emulator, semantic timeline, mouse protocol state,
and either a local PTY or first-class SSH transport.

## Phase 10 Design Note

```text
Feature name: Desktop multiplexer runtime wiring
Layer: multiplexer structure, session transport, render performance
User-facing behavior: keybindings can manage workspaces, tabs, nested splits,
  focus, resize, close, zoom, move, and swap panes. Tabs are mouse-selectable and
  middle-click closeable. Each pane may start a local or SSH profile.
Config keys: mux.enabled, mux.restore_sessions, mux.default_workspace,
  mux.startup_workspaces, mux.show_tab_bar, mux.tab_title_format,
  mux.pane_resize_step, mux.appearance, keyboard.keybindings
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
Performance cost when enabled: proportional to the number of pane transports
  polled and visible panes rendered; each pane is bounded to 64 output batches
  per event-loop tick. SSH connection setup runs off the UI thread.
Tests: mux model, snapshot validation, startup layout, local/SSH spec, desktop
  keybinding, tab hit-testing, and layout tests. Real cross-OS GUI smoke remains
  unverified.
```

Native mux responsibilities:

- create, close, rename, move, and switch tabs
- split panes horizontally and vertically
- focus, resize, zoom, move, swap, and close panes
- persist layout metadata where safe

Current desktop runtime behavior:

- one `PaneRuntime` per native pane
- one terminal emulator per pane
- one local PTY or SSH transport per pane
- per-pane terminal resize when split ratios, window size, tab switching, or
  zoom changes
- active-pane keyboard, IME, paste, focus, mouse, selection-copy, and OSC 52
  policy handling
- configurable top-row tab chrome with mouse switching and middle-click close
- renderer scene composition offsets each pane into its own viewport and draws
  configurable pane borders as renderer decorations
- declarative startup workspaces with nested local/SSH layouts
- optional layout/session-profile restoration using fresh processes
- directional pane movement and swapping
- workspace creation, switching, renaming in the model, and closure

Profile actions use `action:profile`, for example `new_ssh_tab:prod`,
`split_ssh_horizontal:prod`, `new_local_tab:dev`, or
`split_local_vertical:dev`.

External multiplexers such as tmux, screen, and zellij remain normal terminal
applications inside a pane. Panea must not parse or special-case their internals.

Process resurrection is not promised. Restored layouts may reopen shell or SSH
profiles, but long-running process persistence belongs to a future session
mechanism or external tools.

## Still Unverified Or Deferred

- drag-to-reorder UI; keyboard move/swap actions are implemented
- interactive SSH trust/secret prompts and reconnect presentation
- cross-OS native mux GUI smoke tests
- automated tmux/screen/zellij compatibility runs inside native panes
