# Command Blocks

Command blocks are semantic metadata and visual overlays. They must not mutate
the raw terminal buffer.

## Contract

The semantic layer owns command regions, prompt/input/output boundaries, exit
status, duration, shell metadata, and remote metadata. Renderers receive overlay
primitives derived from those regions.

Phase 11 establishes:

- portable config for prompt decoration and command block styles
- renderer-independent overlay kinds for prompt decorations, command blocks,
  grouping, and badges
- basic desktop overlay generation from `SemanticTimelineStore`
- copy/jump action flags in config, backed by Phase 10 semantic actions
- visual performance budgets for animations and animated regions

The raw terminal grid remains authoritative. If shell integration is inactive,
command block overlays should degrade to absent or clearly diagnostic behavior,
not heuristic rewriting of visible output.
