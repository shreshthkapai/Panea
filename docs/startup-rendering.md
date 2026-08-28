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

## Font Discovery Cost

`fontdb::Database::load_system_fonts` parses the name tables of every installed
font. Measured on a Windows host with 376 installed faces: **2.47s** on a cold
file cache, and the terminal needs three or four of those faces. Querying is not
the cost - fifteen family queries, nine of which resolve to nothing, take a
combined **774us**.

So the catalog is built from the font files that satisfied the previous launch,
recorded in `font-cache.txt` beside the other desktop state. It promotes itself
to a full system scan the first time a query misses, which makes a stale or
absent cache a performance question rather than a correctness one.

Two rules keep a partial catalog honest:

- `fontdb` returns its closest match rather than nothing, so while the catalog
  is still partial a match must genuinely satisfy the request: the face has to
  carry the requested family name, and a bold or italic request has to be
  answered by a bold or italic face. Otherwise a cache holding only a regular
  file would answer every bold request with it and never look for the real one.
- Generic families are never answered from a partial catalog. `monospace` has no
  name to verify against, so a single cached file must not stand in for the
  platform's real monospace choice. A config naming a concrete family gets the
  fast path; one asking for `monospace` scans as before.

The cache is also invalidated when the font directories change: installing or
removing a font moves the containing directory's modified time and entry count,
which the stored signature covers. Listing those directories is milliseconds;
parsing what is in them is seconds.

Measured effect on this host, isolating the font step between the
`window-created` and `fonts-ready` startup milestones, with a warm file cache:

| | font step |
| --- | --- |
| full scan every launch | 23-58ms |
| cached font files | 3-5ms |

The cold-cache case is the one that motivated this, and it is the 2.47s above.
