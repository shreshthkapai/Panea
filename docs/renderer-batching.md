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
Windows behavior: same render-core scene and render-wgpu batch planner; retained cross-frame GPU readback passed on the current Windows host using the available WGPU Vulkan adapter. Interactive DX12 verification remains required.
Linux X11 behavior: same render-core scene and render-wgpu batch planner; X11 runtime validation remains unverified until run on Linux X11.
Linux Wayland behavior: same render-core scene and render-wgpu batch planner; Wayland runtime validation remains unverified until run on Linux Wayland.
Fallback behavior: retained damage activates only when requested and the active surface supports receiving the retained frame texture. Unsupported surfaces report an explicit status and use event-driven full-frame GPU batches. CPU snapshot rasterization remains available for renderer tests and GPU surface errors report through RendererError.
Diagnostics: RenderInstrumentation reports frame time, CPU preparation time, GPU timing status, glyph cache hits/misses, atlas uploads/occupancy, damage region count, draw-call count, animated regions, idle wakeups, and runtime throughput fields.
Performance cost when disabled: disabled visual features do not create animation batches or extra overlay batches.
Performance cost when enabled: enabled overlays and cursor animation add bounded batches for their affected damage regions.
Tests: render-wgpu unit tests for batch grouping, glyph cache/atlas reuse, cursor-only damage, atlas policy, damage tracking, frame scheduling, and CPU snapshots; panea-bench renderer commands for repeatable local measurement.
```

## Performance Note

```text
Does this run every frame? Yes, scene-to-batch preparation runs for frames that the scheduler requests.
Does this run every input event? No, input only requests rendering through normal frame scheduling.
Does this run every PTY output batch? Only after terminal state changes request a render frame.
Does this allocate in the hot path? CPU damage batches are bounded per requested frame. GPU vertex/index buffers are persistent, grow geometrically, and are updated with `Queue::write_buffer`; they are not recreated every frame.
Does this force full redraw? No when retained damage is requested and supported. Incremental frames clear and redraw only reported damage; startup, resize, resource invalidation, and unsupported surfaces use a full frame. Rendering remains event-driven, so an idle terminal does not redraw.
Does this require GPU uploads? Only newly cached glyphs produce RGBA atlas uploads; unchanged monochrome and color glyphs reuse atlas entries.
Does this run script/user code? No.
Can it be cached? Glyph bitmaps, atlas entries, and text-to-glyph run keys are cached.
Can it be disabled to near-zero cost? Optional visuals can produce no batches when disabled.
Can the user budget it? Existing performance profiles and diagnostics carry the budget posture.
Can diagnostics show its cost? Yes, through RenderInstrumentation, benchmark output, and the developer performance overlay.
```

## Implementation Shape

`render-wgpu` now prepares scenes into these GPU-facing batches:

- background quads
- glyph quads sampled from a glyph atlas
- decoration and semantic overlay quads
- selection quads
- cursor quads

Text preparation groups compatible adjacent terminal cells into OpenType-shaped
runs, selects fallback faces per grapheme, caches shaped output, and uploads
monochrome or color glyphs into one RGBA atlas. Real bold/italic faces and color
emoji use the same bounded batched draw path.

Terminal palette values and user-authored image assets are sRGB. The WGPU
pipelines linearize configured solid/glyph colors before writing to an sRGB
surface, while sRGB atlas textures decode color emoji and cursor images during
sampling. Explicit non-sRGB fragment variants preserve the encoded output on a
linear swapchain fallback. This prevents colors such as `#0c0c0c` from being
double-encoded and displayed as gray.

`GpuTerminalRenderer::render_scene` uses those batches directly:

- prepares batches through `TerminalRasterizer::prepare_batches`
- uploads only new glyph atlas rows
- submits indexed WGPU draws for non-empty batches
- records batch draw counts and glyph atlas/cache stats
- submits complete GPU batches for required full frames and bounded batches for
  verified retained-damage frames
- clears the presentation target for every full frame
- overwrites each incremental damage region through a non-blending clear batch
  before drawing current cells, glyphs, overlays, decorations, and cursors;
  removed content therefore cannot survive in retained pixels
- does not allocate, load, or copy a retained texture when damage tracking is
  disabled or unsupported
- creates the animated-image cursor shader, pipeline, sampler, and bind-group
  layout only when an image cursor is actually uploaded; disabled image cursors
  add no GPU pipeline work to normal startup
- forces a full redraw after startup, resize, or device recovery

The desktop runtime feeds `DamageTracker` output into every scene. Damage
includes changed and removed cells, old/new cursor positions, selections,
semantic/search overlays, decorations, and animations. Unchanged text uses the
glyph-run cache, resident glyph atlas, and retained frame. `renderer.damage_tracking`
remains conservative and opt-in. When requested, status is capability-resolved:
supported surfaces retain unchanged pixels; unsupported or unavailable backends
report why they use event-driven full-frame batches. The offscreen GPU sequence
test renders four regions, replaces one damaged region through the same
production compositor, reads the texture back, and verifies that unchanged
pixels survive while damaged pixels are replaced.

Glyph bitmap and atlas hits are constant-time. Shaped runs use shared immutable
storage with deterministic bounded eviction, and the GPU renderer recycles CPU
vertex/index allocations between frames. Incremental text preparation shapes a
complete style run for stable ligature and glyph positioning, then emits only
glyphs intersecting the damaged span. Adjacent damage cells coalesce before GPU
clear geometry is built.

Cursor text color is applied by the cursor overlay while terminal-cell colors
remain unchanged. Cursor blink and movement therefore reuse identical shaping
geometry instead of splitting and reshaping application-owned text runs.

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

- Screenshot verification infrastructure is documented in
  [screenshot-verification.md](screenshot-verification.md); macOS, Linux X11,
  and Linux Wayland baselines remain to be captured on their hosts.
- Linux X11/Wayland compositor runtime verification is Phase 7.
- GPU device-loss recovery foundation is documented in
  [renderer-device-recovery.md](renderer-device-recovery.md); real platform
  event validation remains.
- Performance instrumentation and the developer in-window overlay are
  documented in [performance-instrumentation.md](performance-instrumentation.md);
  real GPU timing validation and polished installed UX remain open.
- Batch vector reuse/pooling remains future renderer hot-path hardening.
- Retained damage still needs interactive startup, typing, output, erase,
  cursor, scroll, resize, and recovery verification on DX12, Metal, Vulkan X11,
  and Vulkan Wayland before it is cross-OS verified.
