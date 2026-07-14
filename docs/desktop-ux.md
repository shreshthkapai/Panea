# Desktop UX

Panea's desktop controls operate on the shared mux and render contracts. They
do not create platform-specific session models or rewrite terminal cells.

## Tab And Pane Dragging

Tabs can be dragged across the tab bar to reorder them. A pane can be moved by
holding `Ctrl+Shift`, dragging it over another pane, and releasing to swap the
two split-tree leaves. The pane keeps its terminal state, scrollback,
selection, semantic timeline, and local/SSH transport.

Drag feedback is a transient `DragTarget` renderer overlay. It never enters the
terminal grid, selection, copy output, or PTY stream. Disable either behavior
with `mux.drag_tabs` or `mux.drag_panes`.

## Performance Overlay

`Ctrl+Shift+F12` toggles the metrics panel. Click its first row to open compact
controls for view detail, corner placement, and hide. Config defaults are:

```toml
[diagnostics]
performance_overlay = false
performance_overlay_position = "top_right"
performance_overlay_detail = "compact"
persist_performance_overlay = true
```

Persisted runtime choices live in the platform state directory as
`ui-state.json`; Panea does not edit the user's TOML or programmable config.
Invalid state falls back to config defaults with a diagnostic.

## Cross-OS Design Note

Feature name: Desktop mux drag controls and performance overlay UX

Layer: multiplexer structure, visual overlay, diagnostics, config portability

User-facing behavior: drag tabs to reorder, modifier-drag panes to swap, and
toggle/configure a persisted in-window metrics panel.

Config keys: `mux.drag_tabs`, `mux.drag_panes`,
`diagnostics.performance_overlay`,
`diagnostics.performance_overlay_position`,
`diagnostics.performance_overlay_detail`, and
`diagnostics.persist_performance_overlay`.

macOS behavior: same winit mouse events, mux operations, config keys, and state
model; real native GUI verification remains required.

Windows behavior: same shared implementation; unit tests pass on the current
Windows host, while a manual installed-GUI drag smoke remains required.

Linux X11 behavior: same shared implementation; window-manager mouse capture
and real GUI behavior remain to be verified.

Linux Wayland behavior: same shared implementation; compositor pointer event
and real GUI behavior remain to be verified.

Fallback behavior: drag can be disabled independently; keyboard mux actions
remain available. Invalid persisted UI state falls back to config. The overlay
remains off if rendering has not produced a metrics sample.

Diagnostics: `panea doctor performance` reports configured/runtime overlay
state and preference source. Mux/config failures are explicit stderr/doctor
diagnostics.

Performance cost when disabled: no overlay projection, no mux performance
sample population, and no scheduled frame work. Drag state is absent.

Performance cost when enabled: bounded metric sampling and at most four metric
rows plus three menu rows; drag redraws only while a drag target changes.

Tests: config defaults/schema/live reload, tab reorder, pane target overlay,
performance panel projection, menu hit-testing, and disabled fast paths are
covered. Cross-OS GUI verification remains tracked separately.
