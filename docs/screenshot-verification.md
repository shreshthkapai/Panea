# Screenshot Verification

This document records the Phase 6 cross-OS screenshot verification slice.
It should be read with `architecture.md`, `docs/engineering-rules.md`, and
`docs/renderer-batching.md`.

## Feature Design Note

```text
Feature name: Cross-OS screenshot verification
Layer: render performance, platform parity, diagnostics
User-facing behavior: known terminal scenes can be rendered, captured, compared against per-platform baselines, and reported with clear diff classification.
Config keys: no user-facing config keys in this slice; fixtures use the renderer's deterministic default font configuration path.
macOS behavior: same fixture runner and tolerance model; macOS baselines must be captured on a macOS host before verification can be claimed.
Windows behavior: same fixture runner and tolerance model; Windows CPU-render baselines were captured and verified on the current Windows host.
Linux X11 behavior: same fixture runner and tolerance model; X11 baselines must be captured under an X11 session before verification can be claimed.
Linux Wayland behavior: same fixture runner and tolerance model; Wayland baselines must be captured under a Wayland session before verification can be claimed.
Fallback behavior: missing baselines fail fast with a command telling the operator how to capture them; minor antialiasing-level differences can pass within tolerance, while broad pixel movement is classified as likely text/layout drift.
Diagnostics: reports include fixture name, result class, dimensions, changed pixels, max channel delta, mean channel delta, and an explanation.
Performance cost when disabled: no runtime product cost; this is an offline verification tool.
Performance cost when enabled: fixture capture rasterizes deterministic scenes on demand and writes PPM files; it does not run in the app render hot path.
Tests: render-wgpu unit tests cover fixture categories, PPM round-trips, and diff classification; `cargo xtask screenshot verify --platform windows` passed on the current host.
```

## Commands

Capture baselines for the current platform:

```powershell
cargo xtask screenshot capture
```

Verify the current platform against committed baselines:

```powershell
cargo xtask screenshot verify
```

Force a specific platform key when running in controlled CI/manual jobs:

```powershell
cargo xtask screenshot capture --platform windows
cargo xtask screenshot verify --platform windows
```

Valid platform keys are:

```text
windows
macos
linux-x11
linux-wayland
```

## Fixtures

The current fixture set covers:

- ASCII grid
- truecolor grid
- bold, italic, underline, and strikethrough groundwork
- CJK wide-character samples
- emoji, modifiers, variation selectors, and ZWJ samples
- cursor states
- selection states
- prompt decoration overlays
- command block overlays
- multiple-pane composition
- transparency/opacity overlays

## Baselines

Baselines live under:

```text
tools/conformance/screenshots/baselines/<platform>/*.ppm
```

The committed Windows baseline was captured on the current Windows host. macOS,
Linux X11, and Linux Wayland baseline directories are intentionally present but
not yet verified.

## Diff Classification

The verifier separates failures into these classes:

- `Exact`: all pixels match.
- `AntialiasingWithinTolerance`: a bounded number of low-delta pixels changed.
- `MinorPixelDrift`: differences exceed tolerance but do not look like broad
  text movement.
- `TextLayoutFailure`: a large changed-pixel percentage suggests font fallback,
  glyph placement, dimensions, or overlay geometry changed.
- `DimensionMismatch`: the rendered frame size changed.

This distinction is important because font/GPU antialiasing can vary slightly
across platforms, while text layout drift is a correctness problem.

## Current Verification Status

| Platform | Status | Notes |
| --- | --- | --- |
| Windows | tested | `cargo xtask screenshot verify --platform windows` passed on the current host. |
| macOS | partial | Runner exists, but no macOS host capture/verify has been run. |
| Linux X11 | partial | Runner exists, but no X11 host capture/verify has been run. |
| Linux Wayland | partial | Runner exists, but no Wayland host capture/verify has been run. |

No screenshot feature is `cross-os verified` until Windows, macOS, Linux X11,
and Linux Wayland all have captured and verified baselines with documented
fallbacks.

