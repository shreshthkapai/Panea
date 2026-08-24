# render-wgpu

- Owns: WGPU renderer implementation, glyph atlas use, batching, frame scheduling, and GPU resource lifecycle.
- May import: renderer/font contracts and immutable, bounded built-in visual assets.
- Must not import: shell output parsers, PTY/SSH transports, config frontends running code, mux control logic, or user-controlled asset discovery.
- Layer: render performance.
- Tests required: renderer initialization fallbacks, batch generation, damage-aware frame scheduling, glyph atlas policy, and benchmark-backed hot-path changes.
