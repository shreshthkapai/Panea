# Cursor Customization

Cursor config controls shape, color, blink behavior, thickness, corner radius,
inactive style, and animation budgets.

Supported shape model:

- `block`
- `beam`
- `underline`
- `hollow_block`
- `custom_static_shape`

Animated cursor behavior is opt-in and budgeted. Input must never wait for
animation work, and disabled animation paths must not appear in hot-path
profiles.

Animated image cursor assets are deferred until decode, upload, cache, size, and
FPS limits are enforced.
