# Renderer Device Recovery

This document records the Phase 5 GPU device-loss recovery slice. It should be
read with `architecture.md`, `docs/engineering-rules.md`, and
`docs/renderer-batching.md`.

## Feature Design Note

```text
Feature name: GPU device-loss recovery
Layer: render performance, diagnostics
User-facing behavior: if the GPU surface/device is lost, Panea attempts to rebuild renderer resources while preserving terminal, session, pane, and semantic state outside the renderer.
Config keys: no new user-facing config keys in this slice; existing renderer backend/present/damage settings are reused when resources are recreated.
macOS behavior: same recovery contract and WGPU backend recreation path; sleep/wake and display-change validation still requires a macOS host.
Windows behavior: same recovery contract and WGPU backend recreation path; compile and automated unit checks were run on the current Windows host.
Linux X11 behavior: same recovery contract and WGPU backend recreation path; X11 runtime validation remains unverified until run on Linux X11.
Linux Wayland behavior: same recovery contract and WGPU backend recreation path; Wayland runtime validation remains unverified until run on Linux Wayland.
Fallback behavior: recoverable surface lost/outdated events reconfigure the surface; WGPU device-lost callback signals and out-of-memory surface failures drop WGPU handles, invalidate GPU glyph residency, and attempt backend recreation. If recreation fails, the renderer reports a failed recovery status instead of corrupting terminal state.
Diagnostics: recovery status and recovery events record reason, attempts, rebuilt resources, terminal-state preservation, and failure messages.
Performance cost when disabled: no polling loop or background work is added; recovery bookkeeping is inert during normal frames.
Performance cost when enabled: recovery is exceptional work; the next frame may re-upload glyph atlas entries and rebuild pipelines once after resource loss.
Tests: render-core recovery contract tests; render-wgpu atlas invalidation/re-upload test; desktop render error path attempts bounded recovery. Manual verification remains required for sleep/wake, monitor attach/detach, DPI changes, and backend failure simulation on each OS.
```

## Performance Note

```text
Does this run every frame? Only a cheap ready-state check and normal render error match run during rendered frames.
Does this run every input event? No.
Does this run every PTY output batch? No.
Does this allocate in the hot path? No new steady-state allocations beyond existing batch preparation.
Does this force full redraw? Recovery requests a redraw after backend recreation; normal surface reconfigure does not rewrite terminal state.
Does this require GPU uploads? Only after recovery, when the glyph atlas texture is rebuilt and cached glyphs are uploaded again as needed.
Does this run script/user code? No.
Can it be cached? CPU glyph bitmaps and terminal state remain cached; only GPU-resident atlas state is invalidated.
Can it be disabled to near-zero cost? It is part of renderer hardening rather than an optional visual feature; steady-state cost is near-zero.
Can the user budget it? It uses existing renderer/performance diagnostics; recovery is exceptional and not a visual budget feature.
Can diagnostics show its cost? Recovery events and render instrumentation expose recovery attempts and post-recovery upload work.
```

## Implementation Shape

`GpuTerminalRenderer` now separates persistent app-facing state from disposable
WGPU resources:

- terminal, transport, pane/session, and semantic state live outside the
  renderer and are not rebuilt during GPU recovery
- `GpuBackend` owns the WGPU surface, device, queue, pipelines, glyph texture,
  sampler, and bind groups
- surface `Lost` and `Outdated` events reconfigure the current surface
- WGPU device-lost callbacks and out-of-memory surface failures drop the
  backend and move the renderer to a lost or failed recovery status
- recovery recreates the surface, device, queue, pipelines, and glyph atlas
  texture
- CPU glyph cache entries are preserved, but GPU atlas residency is reset so
  glyph uploads happen again after recovery

## Manual Verification Still Required

Automated unit tests can cover the recovery contract and atlas invalidation
logic, but real device loss is platform and driver dependent. Before calling the
desktop product ready, run and record:

- window minimized/restored
- display sleep/wake
- external monitor attach/detach
- DPI/fractional-scale change
- GPU backend failure simulation where tooling is available
- Windows, macOS, Linux X11, and Linux Wayland runtime validation

## Remaining Work

- Screenshot verification infrastructure exists; macOS, Linux X11, and Linux
  Wayland baselines remain to be captured on their hosts.
- Linux X11/Wayland compositor runtime verification is Phase 7.
- Hardware GPU timestamp queries and installed in-window overlay remain later
  performance instrumentation work.
- Backend-specific device-loss simulation hooks remain future test tooling.
