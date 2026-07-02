# Crate Layers

Crates are organized by architectural layer. Lower layers must not import
higher-layer concepts.

Examples:

- `term-core` must not depend on rendering, platforms, config, SSH, panes, or
  shell integration.
- `render-wgpu` must not parse shell output.
- `transport-pty` must not know how semantic command blocks are drawn.
- `config-lua` must not execute inside the render hot path.

