# Auto-Hidden Fullscreen Titlebar

Feature name: Auto-hidden fullscreen titlebar

Layer: platform parity, render performance, config portability

User-facing behavior: In borderless or frameless fullscreen, moving the pointer
into a configurable strip at the top edge reveals Panea-rendered window chrome.
Moving below the bar hides it. The optional controls minimize, return to a
decorated window, or close Panea. Terminal content remains full-screen because
the bar is an overlay and never changes the terminal grid or PTY size.

Config keys: `window.fullscreen_titlebar.enabled`, `height`, `reveal_height`,
and `show_window_controls`.

macOS behavior: Uses the same renderer overlay and app event routing over winit
borderless fullscreen. Native visual verification remains required.

Windows behavior: Uses the shared renderer overlay over borderless fullscreen;
window actions route through winit. Automated behavior and renderer tests pass
on Windows. Packaged visual verification is required after installation.

Linux X11 behavior: Uses the shared overlay over the WM fullscreen request.
The active WM may govern fullscreen placement; compositor-matrix verification
remains required.

Linux Wayland behavior: Uses the shared overlay over compositor-negotiated
fullscreen. The compositor remains authoritative for fullscreen placement;
Mutter, KWin, Sway/wlroots, and Hyprland verification remains required.

Fallback behavior: The feature is dormant in windowed, maximized, and exclusive
fullscreen modes. If borderless fullscreen itself is unavailable or altered by
the platform, existing requested/effective window-mode diagnostics apply. The
emergency `restore_window_decorations` binding remains available independently.

Diagnostics: `panea doctor window` reports whether the bar is enabled, its
logical dimensions, whether controls are enabled, and whether the configured
window mode can activate it.

Performance cost when disabled: A single predictable configuration/mode branch
during mouse and scene projection; no overlay allocation, timer, wakeup, or
redraw.

Performance cost when enabled: No continuous animation. A frame is requested
only when visibility or hovered control changes. Damage is bounded to the old
or new titlebar surface region.

Tests: Config defaults, validation, TOML, programmable config, platform
overrides, live reload classification, hover reveal/hide, input consumption,
control actions, surface-relative projection, and overlay damage tests. Manual
visual verification remains required on every target window system.

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

`height` and `reveal_height` are logical pixels and scale with the active
monitor. The feature is opt-in and defaults to disabled.
