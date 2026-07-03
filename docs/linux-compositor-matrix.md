# Linux Compositor Matrix

This document is the Phase 7 Linux X11/Wayland compositor verification plan.
It records the exact Linux environments Panea must validate before Linux
window behavior can be called real support rather than architecture intent.

Read this with [architecture.md](../architecture.md),
[Engineering rules](engineering-rules.md), and
[Platform support](platform-support.md).

## Design Note

```text
Feature name: Linux X11/Wayland compositor verification
Layer: platform parity, diagnostics
User-facing behavior: Panea reports the active Linux backend, compositor/desktop, requested window behavior, effective behavior, and fallback reason.
Config keys: window.linux_backend, window.decoration_strategy, window.mode, platform.linux, platform.linux.x11, platform.linux.wayland
macOS behavior: not a Linux compositor target; remains covered by platform support docs and macOS verification.
Windows behavior: not a Linux compositor target; `cargo xtask linux-compositor` reports that Linux behavior cannot be verified on Windows.
Linux X11 behavior: verify GNOME Xorg, KDE X11, XFCE, i3, and Openbox or similar lightweight WM.
Linux Wayland behavior: verify GNOME/Mutter, KDE/KWin, Sway/wlroots, Hyprland, and COSMIC when available.
Fallback behavior: blocked decorations/fullscreen behavior must fall back explicitly and appear in diagnostics.
Diagnostics: `cargo xtask linux-compositor`, `cargo xtask doctor platform`, and `cargo xtask doctor window`.
Performance cost when disabled: none; diagnostics run only when requested.
Performance cost when enabled: bounded environment/config inspection and manual smoke reporting; not a render hot-path feature.
Tests: diagnostics unit tests cover target completeness, non-Linux honesty, Wayland environment detection, and fallback feature reporting.
```

## Status Terms

| Status | Meaning |
| --- | --- |
| planned | Target is required by architecture but no verification exists. |
| partial | Backend capability or diagnostic model exists, but target was not run. |
| tested | Target was run and evidence was recorded for the current feature set. |
| fallback | Target was run and required a documented fallback. |
| blocked | Target was run and a required behavior failed without an acceptable fallback. |

No Linux target is currently cross-OS verified from the Windows development
host. The matrix below starts as `partial` because the backend and diagnostic
contracts exist, but real compositor runs are still required.

## Target Matrix

| Target | Backend | Current status | Required evidence |
| --- | --- | --- | --- |
| GNOME Xorg | X11 | partial | Window creation, resize, DPI, clipboard, fullscreen, decorations, keyboard, mouse, IME/dead keys. |
| KDE X11 | X11 | partial | KWin X11 decorations, fullscreen, scaling, clipboard, keyboard, mouse, IME/dead keys. |
| XFCE | X11 | partial | Lightweight desktop window behavior, resize, clipboard, fullscreen, keyboard, mouse. |
| i3 | X11 | partial | Tiling resize, fullscreen, decoration fallback, keyboard modifiers, mouse wheel/buttons. |
| Openbox or similar | X11 | partial | Floating WM window behavior, fullscreen, decoration fallback, DPI, clipboard. |
| GNOME/Mutter | Wayland | partial | Decoration negotiation, fullscreen, fractional scaling, clipboard, keyboard, mouse, IME. |
| KDE/KWin | Wayland | partial | Server/client decoration behavior, fullscreen, fractional scaling, clipboard, input. |
| Sway/wlroots | Wayland | partial | wlroots tiling behavior, decoration fallback, clipboard, input, resize. |
| Hyprland | Wayland | partial | Compositor-specific fullscreen, decorations, scaling, clipboard, input. |
| COSMIC | Wayland | planned | Verify when available; absence must be recorded rather than treated as a pass. |

## Feature Checklist

Each target must record the result for every feature below.

| Feature | Required behavior | Fallback rule |
| --- | --- | --- |
| Window creation | A normal decorated window opens and becomes interactive. | Failure is blocking; report backend, compositor, and creation error. |
| Resize | Resize events update logical and physical dimensions. | No silent ignore; diagnostics must show stale or missing resize events. |
| DPI/fractional scaling | Scale factor is reported and renderer sizing remains coherent. | Report effective scale and compositor when scaling differs from request. |
| Clipboard | Copy and paste either work or report unavailable provider. | Do not silently drop copy/paste. |
| Fullscreen | Requested fullscreen produces an effective fullscreen mode. | Report requested/effective mode when exclusive fullscreen falls back. |
| Borderless fullscreen | Borderless fullscreen uses the intended monitor where possible. | Report monitor/compositor behavior when placement differs. |
| Frameless window mode | Decorations can be hidden when the compositor allows it. | Fall back to decorated mode if decoration removal is blocked. |
| Custom titlebar mode | Custom titlebar behavior does not trap the user. | Fall back to native/fallback decorated mode until custom drag regions are verified. |
| Decorations fallback | Requested and effective decoration strategies are visible. | Always report `requested` and `effective`; no silent decoration changes. |
| Keyboard input | Text, modifiers, shortcuts, AltGr, and compositor shortcuts are recorded. | Document compositor-reserved shortcuts or layout limitations. |
| Mouse input | Move, click, drag, wheel, focus, and selection behavior are recorded. | Document compositor-specific focus or wheel behavior. |
| IME/dead keys | Composed input is either verified or explicitly marked partial. | Mark unsupported/partial when composed input cannot be verified. |

## Commands

Run these on each Linux target:

```text
cargo xtask linux-compositor
cargo xtask doctor platform
cargo xtask doctor window
cargo xtask screenshot verify --platform linux-x11
cargo xtask screenshot verify --platform linux-wayland
```

Use the screenshot command that matches the current backend. If baselines are
missing, capture them on that target first:

```text
cargo xtask screenshot capture --platform linux-x11
cargo xtask screenshot capture --platform linux-wayland
```

The screenshot command verifies deterministic render fixtures; it does not by
itself prove live compositor behavior. Record both screenshot output and manual
window behavior.

## Manual Verification Record

For every target, save a short record with:

```text
Target:
Date:
Commit:
Distribution:
Kernel:
Session type:
XDG_CURRENT_DESKTOP:
DESKTOP_SESSION:
WAYLAND_DISPLAY:
DISPLAY:
WINIT_UNIX_BACKEND:
GPU:
Scale factor:
Commands run:
Window creation:
Resize:
DPI/fractional scaling:
Clipboard:
Fullscreen:
Borderless fullscreen:
Frameless window:
Custom titlebar:
Decorations fallback:
Keyboard:
Mouse:
IME/dead keys:
Fallbacks observed:
Open bugs:
```

## Acceptance Rule

A Linux target can move from `partial` to `tested` only after the feature
checklist is run on a real host and the record is committed or linked from an
issue. A target can move to `fallback` only when the fallback is intentional,
documented, and visible through diagnostics.

Linux support is not complete until both X11 and Wayland have tested coverage
across the required target classes, with compositor quirks documented here and
in `cargo xtask doctor`.
