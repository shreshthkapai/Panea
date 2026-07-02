# Platform Support

macOS, Windows, Linux X11, and Linux Wayland are first-class desktop targets.

Platform differences must be exposed through capabilities and diagnostics.

## Initial Matrix

| Platform | Initial support target |
| --- | --- |
| macOS | Current supported version range to be defined |
| Windows | Current supported version range to be defined |
| Linux X11 | Required |
| Linux Wayland | Required |

Linux must not mean one developer's compositor. The compositor test set will grow
over time and must include:

- GNOME/Mutter
- KDE/KWin
- wlroots/Sway/Hyprland class compositors

Any unsupported or degraded behavior must be represented as a capability,
fallback, and diagnostic.
