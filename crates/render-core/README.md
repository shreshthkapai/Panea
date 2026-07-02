# render-core

- Owns: renderer-independent draw model, damage regions, cursor visuals, selection visuals, overlays, animations, and frame requests.
- Must not import: wgpu or any GPU API, platform/window crates, parser crates, PTY/SSH transports, shell integration.
- Layer: render performance.
- Tests required: damage-region modeling, draw-scene construction, visual primitive invariants, frame-request behavior, and dependency-boundary tests.
