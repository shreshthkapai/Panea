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

Rules:

- disabled visuals must have near-zero runtime cost
- semantic visuals render as overlays
- heavy visuals must stay behind performance budgets
- platform-specific theme survival must use portable fallbacks
