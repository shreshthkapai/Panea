# Command Blocks

Command blocks are semantic metadata and visual overlays. They must not mutate
the raw terminal buffer.

## Contract

The semantic layer owns command regions, prompt/input/output boundaries, exit
status, duration, shell metadata, and remote metadata. Renderers receive overlay
primitives derived from those regions.

Phase 12 establishes:

- portable config for prompt decoration and command block styles
- renderer-independent overlay kinds for prompt decorations, command blocks,
  grouping, and badges
- desktop overlay generation from `SemanticTimelineStore`
- command-block backgrounds, input/output grouping overlays, and status,
  duration, current-directory, shell, and host badge primitives
- conservative alternate-screen suppression by default, with explicit config
  overrides for users who accept the TUI compatibility risk
- renderer batch ordering that draws command-block/group overlays behind
  terminal text and badge labels as a bounded overlay glyph batch
- copy/jump action flags in config, backed by Phase 10 semantic actions
- visual performance budgets for animations and animated regions

The raw terminal grid remains authoritative. If shell integration is inactive,
command block overlays should degrade to absent or clearly diagnostic behavior,
not heuristic rewriting of visible output.

## Config

```toml
[prompt_decorations]
enabled = true
style = "pill_header"
allow_in_alternate_screen = false

[command_blocks]
enabled = true
style = "card"
separate_prompt_input_output = true
show_duration = true
show_exit_status = true
show_current_directory = true
show_shell_host = true
allow_in_alternate_screen = false
```

`allow_in_alternate_screen` defaults to `false` for both prompt decorations and
command blocks. Enabling it is valid, but config validation warns because visual
overlays can obscure full-screen TUIs.

## Phase 12 Design Note

Feature name: Command blocks and semantic visual overlays

Layer: semantic meaning, visual overlay, render performance, config portability

User-facing behavior: shell-integrated command regions can render as visual
groups with input/output subdivisions and compact metadata badges. Traditional
terminal rendering remains available by disabling prompt decorations and
command blocks.

Config keys: `prompt_decorations.*`, `command_blocks.*`,
`visual_theme.grouping_style`, `visual_theme.spacing.*`,
`visual_theme.borders.*`, `visual_theme.badges.*`,
`visual_theme.success_color`, and `visual_theme.error_color`.

macOS behavior: same semantic/render/config path; runtime behavior is not yet
verified on macOS.

Windows behavior: implementation and automated tests pass on the current
Windows host; real PowerShell semantic smoke already exists from Phase 11.

Linux X11 behavior: same semantic/render/config path; runtime compositor
verification remains open.

Linux Wayland behavior: same semantic/render/config path; runtime compositor
verification remains open.

Fallback behavior: if semantic regions are absent or semantic visuals are
disabled, no command overlays are projected. Alternate-screen applications hide
these overlays by default unless explicitly allowed.

Diagnostics: config validation warns when semantic visuals are allowed in the
alternate screen. Shell integration diagnostics continue to report whether
semantic command regions are trusted, heuristic, disabled, or absent.

Performance cost when disabled: near-zero in scene generation; no semantic
visual overlays are projected.

Performance cost when enabled: proportional to visible semantic regions and a
bounded set of badges per command block. Overlay glyphs are batched separately
instead of drawn through per-badge calls.

Tests: desktop scene-builder tests cover badges, input/output grouping,
alternate-screen suppression, and disabled projection. Renderer tests cover
command-block draw ordering and overlay glyph batching.
