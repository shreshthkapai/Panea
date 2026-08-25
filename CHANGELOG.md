# Changelog

All notable changes to Panea are documented here. Panea follows Semantic
Versioning and publishes immutable release tags.

## [0.1.2] - 2026-08-25

### Fixed

- Drain packaged-command output concurrently while supervising release smokes,
  preventing Windows pipe backpressure from deadlocking `doctor`, shell, GUI,
  or installer validation.
- Preserve bounded process termination and captured failure diagnostics across
  every packaged smoke command.

## [0.1.1] - 2026-08-25

The source tag was created, but production artifacts were not published after
the Windows package smoke exposed the output-supervisor deadlock fixed in
0.1.2.

### Fixed

- Fill transparent GPU surfaces edge-to-edge with the configured background
  color and opacity, including startup, padding, fractional-cell remainder,
  resize, and retained-damage clears.
- Replace terminal background cells instead of blending them over the surface
  clear, preventing compounded opacity.
- Keep renderer draw-call diagnostics accurate for full and retained frames.

## [0.1.0] - 2026-08-24

- Initial public Panea release.

[0.1.2]: https://github.com/shreshthkapai/Panea/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/shreshthkapai/Panea/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/shreshthkapai/Panea/releases/tag/v0.1.0
