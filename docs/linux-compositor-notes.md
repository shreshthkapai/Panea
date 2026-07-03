# Linux Compositor Notes

Linux support means both X11 and Wayland.

The operational target matrix and verification checklist live in
[Linux compositor matrix](linux-compositor-matrix.md). That document is the
source of truth for which compositor/window-manager classes must be tested and
which evidence must be recorded.

Window behavior must be validated over time on:

- GNOME/Mutter
- KDE/KWin
- wlroots/Sway class compositors
- Hyprland class compositors
- common tiling window managers on X11

Frameless and fullscreen behavior must degrade through reported capabilities,
not silent assumptions. Decoration strategy config supports:

- `auto`
- `native`
- `client_side`
- `custom`
- `none`
- `fallback_decorated`

Every compositor-specific limitation should appear in diagnostics and platform
support docs.

Current diagnostic entry points:

```text
cargo xtask linux-compositor
cargo xtask doctor platform
cargo xtask doctor window
```

These commands are useful on every host, but Linux compositor behavior is only
verified when they are run on real Linux X11 or Wayland sessions.
