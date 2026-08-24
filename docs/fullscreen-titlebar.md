# Auto-Hidden Fullscreen Titlebar

Feature name: Auto-hidden fullscreen titlebar

Layer: platform parity, render performance, config portability, diagnostics

User-facing behavior: In `borderless_fullscreen` or `frameless_fullscreen`,
moving the pointer into the configured top-edge strip reveals Panea-owned
chrome over the terminal. The native window remains fullscreen throughout the
reveal and hide cycle. The overlay does not reserve terminal rows, resize the
PTY, or mutate terminal cells.

The titlebar provides minimize, restore-to-windowed, and close controls. Its
background consumes pointer presses but does not initiate a native drag or
change window mode. To move the window, restore it first and use the native
decorated titlebar. This explicit fallback avoids platform-specific fullscreen
drag behavior and unstable native frame reconstruction.

## Configuration

```toml
[window]
mode = "borderless_fullscreen"

[window.fullscreen_titlebar]
enabled = true
height = 36
reveal_height = 3
show_window_controls = true
animation = "smooth"
animation_duration_ms = 120
hide_delay_ms = 120
```

The feature is opt-in and defaults to disabled. `height` and `reveal_height`
are logical pixels and scale with the active monitor. `animation = "instant"`
provides the reduced-motion path. `panea doctor window` reports the configured
and effective animation, retained-damage status, metrics, and fallback reason.

## Platform Contract

- **Windows:** Panea chrome uses Windows-oriented right-side controls. Window
  actions are executed by `platform-winit`; the Windows non-client frame is
  never created or removed during hover.
- **macOS:** The same overlay and actions are implemented. Exact traffic-light
  fidelity is not claimed; native-host visual verification remains required.
- **Linux X11:** The same overlay is used. The window manager remains
  authoritative for minimize, restore, and close behavior; rejected actions
  must produce diagnostics.
- **Linux Wayland:** The same overlay is used. The compositor remains
  authoritative; unsupported operations use an explicit diagnostic fallback.

If retained presentation is unavailable, smooth motion resolves to an instant
overlay transition. Fullscreen remains stable if an action is rejected.
Emergency restore keybindings remain available independently of the overlay.

## Performance Contract

Disabled cost is one predictable event-loop branch with no timer, allocation,
overlay batch, GPU upload, or frame. Hidden steady state schedules no wakeups.
An active transition is bounded by `animation_duration_ms`, reuses cached logo
and glyph resources, and damages only the old and new chrome bounds. Input,
PTY output, terminal layout, and renderer recovery never wait on the animation.

## Automated Fixtures

The screenshot runner captures these deterministic states:

```text
fullscreen-chrome-hidden
fullscreen-chrome-half
fullscreen-chrome-visible
fullscreen-chrome-close-hover
fullscreen-chrome-no-controls
```

The hidden and visible fixtures use identical terminal cells and content
offsets. Automated tests also require every rendered pixel below the 36 px
chrome band to remain identical.

## Native Verification Checklist

Run this checklist on Windows, macOS, Linux X11, and Linux Wayland. Record the
OS version, window backend/compositor, DPI scale, monitor setup, GPU backend,
commit, package hash, and result in the platform verification report.

- Launch windowed by default; enter fullscreen only through the configured
  action.
- Reveal from the top-edge strip with no native fullscreen transition.
- Confirm prompt, cursor, selection, pane geometry, terminal rows/columns, and
  PTY size do not change during reveal or hide.
- Confirm re-entry cancels a pending hide and focus loss cancels interaction.
- Confirm minimize, restore-to-windowed, and close controls invoke one native
  action each; background press does not drag or leave fullscreen.
- Repeat after window resize, fullscreen/windowed cycles, DPI change, and
  monitor change.
- Verify `animation = "instant"`, `show_window_controls = false`, and live
  configuration reload.
- Leave the terminal idle while hidden and confirm no chrome animation wakeups.
- Verify `panea doctor window` explains any animation or platform fallback.

Deterministic screenshots verify renderer geometry, not compositor behavior.
A platform is not marked verified until its packaged native checklist passes.
