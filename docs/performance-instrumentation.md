# Performance Instrumentation and Overlay

This document records the Phase 17 performance instrumentation and in-window
overlay slice. It should be read with `architecture.md`, `docs/performance.md`,
and `docs/renderer-batching.md`.

## Feature Design Note

```text
Feature name: Performance instrumentation and in-window overlay
Layer: render performance, diagnostics, visual overlay
User-facing behavior: when diagnostics.performance_overlay is enabled, Panea draws a compact in-window overlay with recent frame timing, renderer stats, PTY/parser throughput, atlas usage, active performance profile, power source, and budget status. Battery adaptation temporarily applies conservative caps and restores the configured profile on AC power.
Config keys: diagnostics.performance_overlay, renderer.gpu_timestamps, performance.*, including disable_expensive_effects_on_battery.
macOS behavior: same renderer-independent instrumentation and overlay primitives; GPU timestamps are requested only when WGPU reports support.
Windows behavior: same instrumentation and overlay primitives; compile and unit tests passed on the current Windows host.
Linux X11 behavior: same instrumentation and overlay primitives; GPU timestamp and visual behavior remain unverified until run on Linux X11.
Linux Wayland behavior: same instrumentation and overlay primitives; GPU timestamp and visual behavior remain unverified until run on Linux Wayland.
Fallback behavior: renderer.gpu_timestamps defaults to false. If enabled on an unsupported backend, the overlay reports timestamps unsupported and rendering continues. Missing battery information reports unknown and leaves configured performance unchanged.
Diagnostics: overlay lines report frame/CPU/GPU timing status, backend, glyph hits/misses/uploads, atlas usage, damage regions, draw calls, active animations, idle wakeups, PTY read throughput, parser throughput, and memory estimates.
Performance cost when disabled: diagnostics.performance_overlay returns before retaining samples or projecting scene overlays; scrollback memory estimates are skipped. Disabling battery adaptation prevents provider polling and scheduling.
Performance cost when enabled: one compact overlay projection uses the previous sample and adds bounded overlay/glyph batches; PTY/parser counters are sampled from existing polling work.
Tests: diagnostics overlay formatting tests, desktop disabled/enabled overlay projection tests, reversible battery policy tests, disabled provider tests, render-wgpu instrumentation tests, benchmark contract jobs, and standard layer/build checks.
```

## Performance Note

```text
Does this run every frame? Only when the overlay is enabled or a frame is already being rendered.
Does this run every input event? No.
Does this run every PTY output batch? Existing PTY polling increments byte counters; it does not block on overlay rendering.
Does this allocate in the hot path? Disabled overlay allocates nothing. Enabled overlay creates a small bounded set of overlay labels.
Does this force full redraw? No extra redraw request is introduced; the overlay is drawn on frames that already render.
Does this require GPU uploads? Only overlay label glyphs may enter the existing glyph atlas when the overlay is enabled.
Does this run script/user code? No.
Can it be cached? Glyphs and atlas entries use the existing renderer caches; metrics are stored as compact samples.
Can it be disabled to near-zero cost? Yes, diagnostics.performance_overlay defaults to false.
Can the user budget it? Yes, performance.max_frame_time_ms feeds the diagnostics gate status.
Can diagnostics show its cost? Yes, the overlay and benchmark output use shared PerformanceOverlay formatting.
```

## Implementation Shape

- `render-core::RenderInstrumentation` now includes GPU timing status, GPU
  duration where available, glyph atlas occupancy, PTY/parser throughput, and
  memory estimate fields.
- `render-wgpu` requests `wgpu::Features::TIMESTAMP_QUERY` only when
  `renderer.gpu_timestamps = true` and the adapter reports support.
- Unsupported timestamp backends report `unsupported`; default config reports
  `disabled`; async readback reports `pending` until a sample is available.
- The desktop app projects the previous recorded sample into
  `OverlayKind::PerformanceOverlay` primitives. This keeps the overlay in the
  visual layer and avoids mutating terminal cells.
- PTY and parser throughput are counted from existing per-pane polling and
  parsing work. Scrollback memory estimates are computed only when the overlay
  is enabled.
- Power state is sampled through `platform-core::PowerStateProvider` at a
  30-second interval outside render/input/PTY paths. Battery mode caps optional
  animation/cache budgets; AC power restores the exact configured settings.

## Remaining Work

- Real GPU timestamp samples need validation on Windows, macOS, Linux X11, and
  Linux Wayland hardware/backends.
- A user-facing keybinding/command-palette toggle remains desktop UI polish;
  config reload can already enable or disable the overlay live.
- Absolute comparisons with Alacritty or WezTerm remain invalid until the same
  public fixtures, machine, fonts, backend, dimensions, and warm-up policy are
  used. Panea does not claim a win based on unlike workloads.
