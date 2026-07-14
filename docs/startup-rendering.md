# Theme-Aware Startup Rendering

Feature name: Theme-aware startup background

Layer: platform window lifecycle and GPU renderer

User-facing behavior: Panea does not expose an unpainted native client area. It
creates the window hidden, initializes WGPU, presents the configured terminal
background, and then reveals the window. The first visible pixels therefore
match `colors.background` instead of the operating system's default white.

Config keys: `colors.background` and `window.opacity`

macOS behavior: the winit window remains hidden until the first Metal-backed
WGPU surface presentation completes.

Windows behavior: the winit window remains hidden until the first DX12-backed
WGPU surface presentation completes.

Linux X11 behavior: the winit window remains unmapped until the first WGPU
surface presentation completes.

Linux Wayland behavior: the winit surface remains hidden until the first WGPU
surface presentation completes. Compositor-managed transparency remains
subject to the reported alpha-mode capability.

Fallback behavior: a startup presentation failure is reported explicitly and
Panea reveals the window for normal first-frame rendering rather than failing
to launch. Transparent windows retain a transparent surface clear so
compositor opacity behavior is not replaced by an opaque theme color.

Diagnostics: existing renderer creation, surface recovery, transparency
fallback, and GUI smoke milestones report failures in this path.

Performance cost when disabled: not applicable; this replaces the first blank
native frame.

Performance cost when enabled: one clear-only GPU submission during startup.
There is no per-frame work beyond using the precompiled background color for
normal surface clears.

Tests: renderer unit tests verify sRGB and linear clear conversion. Desktop GUI
smoke exercises hidden-window initialization, startup presentation, reveal,
and normal terminal rendering. This path passes on Windows; real visual
confirmation remains part of the macOS, Linux X11, and Linux Wayland packaged
GUI smoke checklists.
