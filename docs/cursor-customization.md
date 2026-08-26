# Cursor Customization

Cursor config controls shape, color, blink behavior, thickness, corner radius,
inactive style, and animation budgets.

Supported shape model:

- `block`
- `beam`
- `underline`
- `hollow_block`
- `custom_static_shape`

The static geometry renderer implements `block`, `beam`, `underline`, and
`hollow_block`. `custom` and `custom_static_shape` use block geometry unless a
validated `[cursor.vector]` asset is enabled. Static PNG cursors remain
available through `[cursor.image]`.

## Static Cursor Design Note

Feature name: static cursor customization
Layer: config-core, render-core, render-wgpu, desktop app
User-facing behavior: block, beam, underline, and hollow block shapes with
thickness, cell-relative corner radius, color, blink interval, inactive style,
and terminal-mode styles.
Config keys: `cursor.shape`, `cursor.blink`, `cursor.blink_interval_ms`,
`cursor.thickness`, `cursor.corner_radius`, `cursor.color`,
`cursor.inactive_shape`, `cursor.inactive_color`, and
`cursor.mode_specific_styles`.
macOS behavior: same config and WGPU cursor batches; runtime visual verification
is pending on a macOS host.
Windows behavior: config, renderer, damage, screenshot, and desktop unit tests
pass on the current Windows host.
Linux X11 behavior: same config and renderer batches; real X11 verification is
pending.
Linux Wayland behavior: same config and renderer batches; real Wayland
verification is pending.
Fallback behavior: custom shapes without a valid vector asset use static block geometry;
an unfocused window uses the configured inactive style; terminal DECSCUSR shape
requests are honored unless a matching mode-specific style overrides them.
Diagnostics: invalid thickness, radius, blink intervals, and mode names fail
config validation.
Performance cost when disabled: no animation state or wakeups; a visible static
cursor is one persistent batch and one cursor-local damage region.
Performance cost when enabled: blinking wakes only at the configured interval
and damages only the old/new cursor cell; rounded geometry adds scanline quads
inside the existing cursor batch without adding draw calls.
Tests: shape/mode/focus resolution, deterministic blink, rounded batching,
cursor-local damage, config validation, and cursor screenshots.

Mode style keys are `normal`, `insert`, `alternate_screen`,
`application_cursor`, and `application_keypad`.

## Phase 13 Design Note

Feature name: cursor animation and animated image cursor pipeline
Layer: render-wgpu, render-core, config-core
User-facing behavior: users can enable smooth movement, typing pulse/stretch,
trail, blink easing, glow, and an optional image cursor asset with FPS and size
budgets.
Config keys: `cursor.animation`, `cursor.animations_enabled`, `cursor.smooth_movement`,
`cursor.typing_pulse`, `cursor.typing_stretch`, `cursor.trail`,
`cursor.blink_easing`, `cursor.short_lived_glow`, `cursor.shadow`,
`cursor.image.*`, and
`performance.max_animation_fps`.

`cursor.animation = "panea"` selects Panea's built-in directional tilt with a
short elastic extension. The cursor leans with horizontal movement while the
extension collapses from the previous cell, then both settle at the destination.
The normal product default is static. `cursor.animation = "custom"` exposes the
individual effect controls; configs without the profile key retain their
existing `animations_enabled` behavior.
macOS behavior: same config and render scene contract; runtime verification is
still pending on a macOS host.
Windows behavior: config, pixel decode, batching, damage, screenshot, and GPU
shader validation tests pass on the current Windows host. Interactive visual
smoke remains a manual release check.
Linux X11 behavior: same config and render scene contract; compositor/runtime
verification remains pending.
Linux Wayland behavior: same config and render scene contract; compositor/runtime
verification remains pending.
Fallback behavior: disabled animations add no render-scene animation handles or
damage regions; invalid image cursor config is rejected; failed image loads are
reported and the normal cursor remains active.
Diagnostics: config validation reports bad paths/FPS and budget warnings; the
desktop runtime logs image decode failures and expensive asset warnings.
Performance cost when disabled: zero scene animation handles, no image decode
thread, no image texture, no animation wakeups, and no animation damage.
Performance cost when enabled: bounded cursor-neighborhood damage regions and
batched overlay quads; image cursor assets are decoded off the render thread,
shared as immutable RGBA frames, and uploaded once into a GPU texture array.
Tests: config validation, animation budget and region tests, animation batching,
real GIF/PNG pixel decoding, image-frame damage, CPU compositing, GPU shader
validation, and screenshot fixtures.

## Animation Runtime

Animated cursor behavior is opt-in and budgeted. Input never waits for animation
work. The desktop runtime records typing as a lightweight state change, and the
renderer receives bounded `AnimationHandle` values that affect only cursor-local
damage regions. `performance.max_active_animations` and
`performance.max_animated_region_pixels` are enforced when effects are created,
and `performance.max_animation_fps` caps scheduling.

Cursor animation quads are renderer overlays. They do not mutate terminal cells,
scrollback, selections, semantic regions, or copied text.

Blink easing is represented as a bounded cursor-region animation on visibility
transitions. It must not create perpetual idle redraws.

## Animated Image Cursor

Animated image cursors are opt-in through `[cursor.image]`. GIF frames and PNG
pixels are decoded on a named worker thread. A GIF is animated at the configured
FPS; a PNG is a user-authored static image cursor. Relative paths resolve from
the config directory and `~/...` paths expand through the platform home
directory.

The decoder rejects files above `performance.max_cursor_asset_size_kb`, images
larger than 512x512, GIFs above 256 frames, and decoded frame sets above the
derived memory budget. Frames are cached in memory and uploaded to one GPU
texture array only when the asset changes. Rendering uses one textured quad and
damages only the old/new cursor bounds. Decode failure leaves the normal cursor
active and emits a diagnostic.

Example: `crates/assets/config-examples/custom-cursor.toml`.

## Static Vector Cursor

Feature name: portable user-authored static vector cursor
Layer: config-core, render-core, render-wgpu, desktop app, assets
User-facing behavior: a custom cursor composed from normalized rounded
rectangles, using either the configured cursor color or per-primitive RGBA
Config keys: `cursor.vector.enabled`, `cursor.vector.path`, and the shared
`performance.max_cursor_asset_size_kb`
macOS behavior: same parser, immutable scene asset, and WGPU cursor batch;
native visual verification remains pending
Windows behavior: parsing, validation, batching, damage, and direct GUI startup
tests pass on the current host
Linux X11 behavior: same config and renderer path; native X11 verification remains pending
Linux Wayland behavior: same config and renderer path; native Wayland verification remains pending
Fallback behavior: malformed, oversized, unsupported-version, or unavailable
assets emit a clear runtime error and leave the normal static cursor active
Diagnostics: config rejects missing paths and image/vector conflicts; the
loader reports file, JSON, version, count, size, and geometry failures
Performance cost when disabled: zero file reads, worker threads, scene assets,
batch geometry, and redraws
Performance cost when enabled: one bounded worker parse on asset change, one
immutable cached asset, cursor-local damage, and one existing GPU cursor draw call
Tests: portable TOML/Lua config, validation, strict format parsing, bounds and
unknown-field rejection, primitive limits, GPU quad batching, and local damage

`[cursor.vector]` enables a user-authored `.panea-cursor.json` asset. The format
is deliberately data-only and portable: version 1 contains up to 64 rounded
rectangle primitives on a normalized 1000x1000 canvas. Each primitive may use
the configured terminal cursor color or an explicit `[red, green, blue, alpha]`
color. Coordinates, dimensions, unknown fields, file size, and format version
are validated before the immutable asset enters the render path.

```json
{
  "version": 1,
  "primitives": [
    { "x": 80, "y": 80, "width": 220, "height": 840, "corner_radius": 80 },
    { "x": 300, "y": 390, "width": 620, "height": 220, "color": [80, 180, 255, 255] }
  ]
}
```

The file is read and compiled on a worker thread, cached by content, and drawn
in the existing GPU cursor batch. It is not SVG: scripts, fonts, external
resources, filters, and platform-specific parsing are intentionally excluded.
When disabled, no file polling, parsing, scene projection, or batch work runs.
Image and vector cursor assets cannot be enabled together.
