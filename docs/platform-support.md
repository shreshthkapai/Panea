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

## Current Verification Status

| Area | Windows | macOS | Linux X11 | Linux Wayland |
| --- | --- | --- | --- | --- |
| Workspace build/test/lint | Verified on current host | Unverified | Unverified | Unverified |
| Local PTY real-shell smoke | Verified on current host | Unverified | Unverified | Unverified |
| Window creation/input translation | Build-verified on current host | Unverified | Unverified | Unverified |
| GPU renderer window path | Build-verified on current host | Unverified | Unverified | Unverified |

Linux backend and decoration preferences are represented in config and
diagnostics. Exact compositor behavior is not considered verified until tested
on real X11 and Wayland sessions.
