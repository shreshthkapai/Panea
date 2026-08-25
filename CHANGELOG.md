# Changelog

All notable changes to Panea are documented here. Panea follows Semantic
Versioning and publishes immutable release tags.

## [0.1.1] - 2026-08-25

### Fixed

- Fill transparent GPU surfaces edge-to-edge with the configured background
  color and opacity, including startup, padding, fractional-cell remainder,
  resize, and retained-damage clears.
- Replace terminal background cells instead of blending them over the surface
  clear, preventing compounded opacity.
- Keep renderer draw-call diagnostics accurate for full and retained frames.

## [0.1.0] - 2026-08-24

- Initial public Panea release.

[0.1.1]: https://github.com/shreshthkapai/Panea/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/shreshthkapai/Panea/releases/tag/v0.1.0
