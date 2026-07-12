# font-system

- Owns: system font discovery, OpenType shaping, per-grapheme fallback selection, real style-face resolution, monochrome/color glyph rasterization, glyph cache policy, and font metrics.
- Must not import: terminal transports, platform window event loops, semantic command regions, config frontends, mux.
- Layer: render performance.
- Tests required: font fallback selection, metrics stability, shaping fixtures, glyph cache policy, and platform capability fallbacks.
