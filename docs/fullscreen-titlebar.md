# Auto-Hidden Fullscreen Titlebar

Feature name: Auto-hidden fullscreen titlebar

Layer: platform parity, config portability

User-facing behavior: In borderless or frameless fullscreen, moving the pointer
into a configurable strip at the top edge temporarily reveals the platform's
native decorated, maximized window. The operating system owns the caption,
application icon, system menu, and window controls. Moving back into the client
area returns Panea to borderless fullscreen. Panea does not draw a titlebar or
imitate native controls in the terminal renderer.

Config keys: `window.fullscreen_titlebar.enabled`, `height`, `reveal_height`,
and `show_window_controls`.

macOS behavior: Uses winit's native decorated/maximized window and returns to
borderless fullscreen. Native visual verification remains required.

Windows behavior: Uses the real Windows non-client titlebar and caption buttons
while revealed. The installed release has been visually verified for startup,
reveal, and return to borderless fullscreen. Resize events are coalesced before
terminal and PTY resize so intermediate Windows transition geometry cannot
reflow terminal content repeatedly.

Linux X11 behavior: Requests the WM's native decorated/maximized window, then
returns to fullscreen. The active WM remains authoritative and compositor-matrix
verification is required.

Linux Wayland behavior: Uses compositor-negotiated native decorations and
fullscreen transitions. Mutter, KWin, Sway/wlroots, and Hyprland verification
remains required.

Fallback behavior: The feature is dormant in windowed, maximized, and exclusive
fullscreen modes. If borderless fullscreen itself is unavailable or altered by
the platform, existing requested/effective window-mode diagnostics apply. The
emergency `restore_window_decorations` binding remains available independently.

Diagnostics: `panea doctor window` reports whether the bar is enabled, its
logical dimensions, whether controls are enabled, and whether the configured
window mode can activate it.

Performance cost when disabled: A single predictable branch for pointer events;
no render work, allocation, timer, wakeup, or redraw.

Performance cost when enabled: No per-frame work or animation. Only entering or
leaving the top-edge state requests a native window-mode transition. Terminal
and PTY resize is applied once after the geometry settles.

Tests: Config defaults, validation, TOML, programmable config, platform
overrides, live reload classification, and native reveal/hide state transitions.
Manual visual verification remains required on every target window system.

## Configuration

```toml
[window]
mode = "borderless_fullscreen"

[window.fullscreen_titlebar]
enabled = true
height = 36
reveal_height = 3
show_window_controls = true
```

`reveal_height` is expressed in logical pixels and scales with the active
monitor. `height` and `show_window_controls` remain accepted for config
compatibility; native titlebar dimensions and controls are owned by the OS.
The feature is opt-in and defaults to disabled.
