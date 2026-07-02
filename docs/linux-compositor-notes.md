# Linux Compositor Notes

Linux support means both X11 and Wayland.

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
