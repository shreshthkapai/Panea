# Renderer Batching

This document records the Phase 4 renderer batching and glyph pipeline hardening
slice. It should be read with `architecture.md`, `docs/performance.md`, and
`docs/layer-boundaries.md`.

## Feature Design Note

```text
Feature name: GPU renderer batching and glyph pipeline
Layer: render performance
User-facing behavior: terminal scenes are prepared as batched background, glyph, decoration, selection, and cursor draws instead of per-cell draw calls.
Config keys: existing renderer.damage_tracking and performance profile settings apply; no new user-facing keys in this slice.
macOS behavior: same render-core scene and render-wgpu batch planner; runtime GPU validation still requires a macOS host.
Windows behavior: same render-core scene and render-wgpu batch planner; compile, unit tests, and renderer benchmarks were run on the current Windows host.
Linux X11 behavior: same render-core scene and render-wgpu batch planner; X11 runtime validation remains unverified until run on Linux X11.
Linux Wayland behavior: same render-core scene and render-wgpu batch planner; Wayland runtime validation remains unverified until run on Linux Wayland.
Fallback behavior: CPU snapshot rasterization remains available for renderer tests and future screenshot fixtures; GPU surface errors still report through RendererError.
Diagnostics: RenderInstrumentation reports frame time, CPU preparation time, glyph cache hits/misses, atlas uploads, damage region count, draw-call count, animated regions, and idle wakeups.
Performance cost when disabled: disabled visual features do not create animation batches or extra overlay batches.
Performance cost when enabled: enabled overlays and cursor animation add bounded batches for their affected damage regions.
Tests: render-wgpu unit tests for batch grouping, glyph cache/atlas reuse, cursor-only damage, atlas policy, damage tracking, frame scheduling, and CPU snapshots; panea-bench renderer commands for repeatable local measurement.
```

## Performance Note

```text
Does this run every frame? Yes, scene-to-batch preparation runs for frames that the scheduler requests.
Does this run every input event? No, input only requests rendering through normal frame scheduling.
Does this run every PTY output batch? Only after terminal state changes request a render frame.
Does this allocate in the hot path? Batch vectors are currently rebuilt per prepared frame; reuse/pooling is a future optimization after the direct batching contract is stable.
Does this force full redraw? No; explicit damage regions filter generated cell, overlay, selection, and cursor batches.
Does this require GPU uploads? Only newly cached glyphs produce atlas uploads; unchanged glyphs reuse atlas entries.
Does this run script/user code? No.
Can it be cached? Glyph bitmaps, atlas entries, and text-to-glyph run keys are cached.
Can it be disabled to near-zero cost? Optional visuals can produce no batches when disabled.
Can the user budget it? Existing performance profiles and diagnostics carry the budget posture.
Can diagnostics show its cost? Yes, through RenderInstrumentation and benchmark/overlay text.
```

## Implementation Shape

`render-wgpu` now prepares scenes into these GPU-facing batches:

- background quads
- glyph quads sampled from a glyph atlas
- decoration and semantic overlay quads
- selection quads
- cursor quads

`GpuTerminalRenderer::render_scene` uses those batches directly:

- prepares batches through `TerminalRasterizer::prepare_batches`
- uploads only new glyph atlas rows
- submits indexed WGPU draws for non-empty batches
- records batch draw counts and glyph atlas/cache stats

The CPU rasterizer remains in place for deterministic snapshot-style tests. It
does not replace the normal WGPU batch submission path.

## Benchmarks

Run renderer-focused benchmarks with:

```powershell
cargo xtask bench render-full-ascii
cargo xtask bench render-mixed-unicode
cargo xtask bench render-emoji-heavy
cargo xtask bench render-fast-scrolling
cargo xtask bench render-large-scrollback-viewport
cargo xtask bench render-many-panes
cargo xtask bench render-cursor-animation
cargo xtask bench render-command-blocks
```

The benchmarks do not make public performance claims. They provide local,
repeatable measurements for regression detection and feature-cost review.

## Remaining Work

- Cross-OS screenshot verification is Phase 6.
- Linux X11/Wayland compositor runtime verification is Phase 7.
- Full GPU device-loss recovery is Phase 5.
- Hardware GPU timestamp queries and installed in-window overlay remain later
  performance instrumentation work.
- Batch vector reuse/pooling and deeper shaping/fallback behavior remain future
  renderer/font hardening.
