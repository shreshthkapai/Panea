# Benchmark Fixtures

The Phase 8 harness uses deterministic generated fixtures for:

- large logs
- color-heavy output
- Unicode-heavy output
- emoji-heavy output
- many small updates
- full-screen TUI-style redraws
- resize storms
- scrollback stress

Keep committed fixtures small and deterministic. Do not commit private shell
history, host paths, credentials, or captured terminal output that may contain
secrets.
