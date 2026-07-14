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
`hollow_block`. `custom` and `custom_static_shape` remain reserved compatibility
values and currently use block geometry. A user-authored static image cursor is
supported by setting `[cursor.image]` to a PNG.

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
Fallback behavior: unsupported reserved custom shapes use static block geometry;
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
Config keys: `cursor.animations_enabled`, `cursor.smooth_movement`,
`cursor.typing_pulse`, `cursor.typing_stretch`, `cursor.trail`,
`cursor.blink_easing`, `cursor.short_lived_glow`, `cursor.shadow`,
`cursor.image.*`, and
`performance.max_animation_fps`.
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
