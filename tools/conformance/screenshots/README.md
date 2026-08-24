# Screenshot Conformance

This directory stores deterministic renderer screenshot baselines.

The fixture set includes terminal grids, text styles, Unicode, selections,
semantic overlays, panes, opacity, cursor assets, and these fullscreen chrome
states:

```text
fullscreen-chrome-hidden
fullscreen-chrome-half
fullscreen-chrome-visible
fullscreen-chrome-close-hover
fullscreen-chrome-no-controls
```

```text
baselines/windows
baselines/macos
baselines/linux-x11
baselines/linux-wayland
```

Generate or refresh baselines on the target host with:

```powershell
cargo xtask screenshot capture --platform windows
```

Verify baselines with:

```powershell
cargo xtask screenshot verify --platform windows
```

Only capture a platform baseline on that platform. Do not copy a Windows
baseline into macOS or Linux directories to make verification look complete.

Verification writes actual images and a Markdown report under
`target/screenshots/<platform>/`. A missing platform baseline is a failure, not
proof of parity.

Fullscreen chrome fixtures are renderer-only. Native reveal latency, controls,
DPI/monitor behavior, fullscreen stability, and idle wakeups must also pass the
manual checklist in `docs/fullscreen-titlebar.md` on each target platform.
