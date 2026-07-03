# Cursor Customization

Cursor config controls shape, color, blink behavior, thickness, corner radius,
inactive style, and animation budgets.

Supported shape model:

- `block`
- `beam`
- `underline`
- `hollow_block`
- `custom_static_shape`

## Phase 13 Design Note

Feature name: cursor animation and animated image cursor pipeline
Layer: render-wgpu, render-core, config-core
User-facing behavior: users can enable smooth movement, typing pulse/stretch,
trail, blink easing, glow, and an optional image cursor asset with FPS and size
budgets.
Config keys: `cursor.animations_enabled`, `cursor.smooth_movement`,
`cursor.typing_pulse`, `cursor.typing_stretch`, `cursor.trail`,
`cursor.blink_easing`, `cursor.short_lived_glow`, `cursor.image.*`, and
`performance.max_animation_fps`.
macOS behavior: same config and render scene contract; runtime verification is
still pending on a macOS host.
Windows behavior: config/render unit tests pass on the current Windows host.
Linux X11 behavior: same config and render scene contract; compositor/runtime
verification remains pending.
Linux Wayland behavior: same config and render scene contract; compositor/runtime
verification remains pending.
Fallback behavior: disabled animations add no render-scene animation handles or
damage regions; invalid image cursor config is rejected; failed image loads are
reported and the normal cursor remains active.
Diagnostics: config validation reports bad paths/FPS and budget warnings; the
desktop runtime logs image decode failures and expensive asset warnings.
Performance cost when disabled: zero scene animation handles and no animation
damage regions.
Performance cost when enabled: bounded cursor-neighborhood damage regions and
batched overlay quads; image cursor assets are loaded off the render thread.
Tests: config validation, render-wgpu cursor animation region tests, animation
batching tests, and image header decode tests.

## Animation Runtime

Animated cursor behavior is opt-in and budgeted. Input never waits for animation
work. The desktop runtime records typing as a lightweight state change, and the
renderer receives bounded `AnimationHandle` values that affect only cursor-local
damage regions.

Cursor animation quads are renderer overlays. They do not mutate terminal cells,
scrollback, selections, semantic regions, or copied text.

Blink easing is represented as a bounded cursor-region animation on visibility
transitions. It must not create perpetual idle redraws.

## Animated Image Cursor

Animated image cursors are opt-in through `[cursor.image]`. Asset reads and
header decoding run off the render thread, and decoded metadata is cached by the
renderer-side image cache. The current foundation validates GIF/PNG headers,
frame count metadata, configured FPS, asset size, and warning policy.

Full pixel-frame decoding/upload and image-cursor drawing remain follow-up work
after the bounded decode/cache path and performance warnings have more runtime
coverage.
