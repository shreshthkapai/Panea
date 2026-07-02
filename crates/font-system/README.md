# font-system

- Owns: font discovery abstraction, shaping, fallback selection, glyph cache policy, and font metrics.
- Must not import: terminal transports, platform window event loops, semantic command regions, config frontends, mux.
- Layer: render performance.
- Tests required: font fallback selection, metrics stability, shaping fixtures, glyph cache policy, and platform capability fallbacks.
