# Themes

Themes are portable config data compiled into renderer-friendly runtime values.
They must not mutate terminal text.

Current theme-related config lives under:

- `colors`
- `visual_theme`
- `prompt_decorations`
- `command_blocks`
- `cursor`
- `performance`

Example configs are in `crates/assets/config-examples`.

Built-in profile names are:

- `balanced`
- `plain-fast`
- `minimal-aesthetic`
- `command-blocks`

TOML expands a recognized profile before applying explicit values. An explicit
`colors`, `cursor`, or visual setting therefore wins over the profile default.
Unrecognized names remain valid labels for fully explicit custom themes.

The complete example is
`crates/assets/config-examples/foundational-customization.toml`. Runtime and
cross-OS behavior are documented in
[foundational-customization.md](foundational-customization.md).

Rules:

- disabled visuals must have near-zero runtime cost
- semantic visuals render as overlays
- heavy visuals must stay behind performance budgets
- platform-specific theme survival must use portable fallbacks
