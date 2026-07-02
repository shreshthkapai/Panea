# Fuzzing

Panea fuzzing belongs to `core correctness` and `diagnostics`. It protects the
parser, terminal grid, resize, scrollback, Unicode, selection, OSC, DCS-like
escape handling, and shell integration marker parsing before higher-level
features rely on them.

## Feature Design Note

```text
Feature name: Real fuzzing harness
Layer: core correctness, diagnostics
User-facing behavior: none directly; malformed terminal input should not panic, corrupt terminal state, or grow memory without bound
Config keys: none
macOS behavior: same cargo-fuzz targets and property tests
Windows behavior: same cargo-fuzz targets and property tests; cargo-fuzz availability depends on local Rust/libFuzzer tooling
Linux X11 behavior: same cargo-fuzz targets and property tests
Linux Wayland behavior: same cargo-fuzz targets and property tests
Fallback behavior: if cargo-fuzz is unavailable, `cargo xtask fuzz-smoke` still runs bounded property fuzz tests in normal CI/toolchains
Diagnostics: failing property tests print the minimized proptest case; cargo-fuzz writes crash artifacts under `fuzz/artifacts/<target>/`
Performance cost when disabled: zero runtime cost
Performance cost when enabled: developer/CI fuzz time only; no renderer, PTY, platform, or config hot-path cost
Tests: cargo-fuzz targets, proptest properties, parser OSC/CSI payload cap regression tests, shell marker payload cap regression tests
```

## Targets

| Target | Surface |
| --- | --- |
| `parser_input` | Arbitrary byte streams through `term-parser` into `term-core`. |
| `grid_actions` | Direct terminal grid mutation actions, modes, scroll regions, and selection. |
| `resize` | Repeated resize with Unicode content and selection extraction. |
| `unicode` | Grapheme-heavy terminal cell input. |
| `selection_ranges` | Normal and rectangular selection over mixed Unicode cells. |
| `osc_dcs` | OSC payloads, ST/BEL termination, DCS-like escape streams, and parser caps. |
| `shell_markers` | Shell-integration OSC marker parser and direct payload parser. |

## Commands

Fast CI/property smoke:

```text
cargo xtask fuzz-smoke
```

Coverage-guided local fuzzing with `cargo-fuzz` installed:

```text
rustup toolchain install nightly
cargo install cargo-fuzz
```

```text
cargo xtask fuzz parser_input -- -runs=100000
cargo xtask fuzz grid_actions -- -runs=100000
cargo xtask fuzz resize -- -runs=100000
cargo xtask fuzz unicode -- -runs=100000
cargo xtask fuzz selection_ranges -- -runs=100000
cargo xtask fuzz osc_dcs -- -runs=100000
cargo xtask fuzz shell_markers -- -runs=100000
```

`cargo xtask fuzz ...` invokes `cargo +nightly fuzz run ...` because
`cargo-fuzz` uses sanitizer flags that are not available on stable Rust. The
workspace's normal build, test, lint, and smoke commands still use the
repository toolchain.

Long-running scheduled jobs should run each target with a time budget, for
example:

```text
cargo xtask fuzz parser_input -- -max_total_time=300
```

## Invariants

Every terminal fuzz target checks:

- grid row and column counts match `TerminalSize`
- visible grid cell count is stable
- cursor row and column stay inside the grid
- wide continuation cells have a valid wide base
- wide base cells have a continuation when there is room
- selection extraction does not emit replacement characters
- malformed OSC/CSI streams are bounded and do not panic

## Crash Handling

Crash artifacts are written under:

```text
fuzz/artifacts/<target>/
```

When a crash is found:

1. Minimize it with cargo-fuzz.
2. Add the minimized input to `fuzz/corpus/<target>/`.
3. Add a normal Rust regression test in the owning crate when the bug is fixed.
4. Keep the regression test focused on the invariant that failed.

Do not leave a fix protected only by a fuzz artifact. Found crashes must become
permanent tests.

## Current Limits

This phase adds the harness and property coverage. It does not claim that any
target has been exhaustively fuzzed on all operating systems. Cross-OS fuzz
execution and scheduled runner history belong to later verification phases.
