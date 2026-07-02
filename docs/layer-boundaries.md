# Layer Boundaries

This document is the Phase 1 architecture hardening contract. It turns the
rules in `architecture.md` into enforceable crate dependency boundaries and
stable provider interfaces.

## Feature Design Note

```text
Feature name: Architecture and layer-boundary hardening
Layer: diagnostics, core correctness, platform parity, render performance, config portability, security
User-facing behavior: none directly; this prevents future changes from weakening the product architecture
Config keys: none
macOS behavior: same crate checks and provider contracts
Windows behavior: same crate checks and provider contracts
Linux X11 behavior: same crate checks and provider contracts
Linux Wayland behavior: same crate checks and provider contracts
Fallback behavior: violations fail `cargo xtask layer-check` with the crate, dependency, and boundary rule
Diagnostics: layer-check output names the invalid dependency and the owning rule
Performance cost when disabled: zero runtime cost
Performance cost when enabled: CI/developer-time only; no render/input/PTY hot-path cost
Tests: xtask boundary tests, terminal-core independent check, render-core fake renderer test
```

## Boundary Rules

Lower layers must not import higher or runtime-specific layers.

| Crate | Owns | Allowed workspace dependencies |
| --- | --- | --- |
| `term-core` | terminal grid, cells, cursor, modes, scrollback, selection, resize | none |
| `term-parser` | ANSI/VT parsing into terminal-core actions | `term-core` |
| `semantics` | prompt, input, output, command region meaning | `term-core` only when terminal positions are needed |
| `render-core` | renderer-independent scene, damage, overlays, cursor visuals | none |
| `render-wgpu` | WGPU backend, glyph atlas, batching, frame scheduler | `render-core`, `font-system` |
| `font-system` | font discovery, fallback, metrics, glyph rasterization policy | none |
| `transport-core` | byte I/O, resize, lifecycle contracts | none |
| `transport-pty` | local PTY and pseudoconsole backend | `transport-core` |
| `transport-ssh` | SSH transport backend | `transport-core`, `security` |
| `platform-core` | platform capabilities, input, window, clipboard contracts | none |
| `platform-winit` | winit-backed desktop platform adapter | `platform-core` |
| `config-core` | portable internal `AppConfig`, validation, reload impact | none |
| `config-toml` | static TOML frontend | `config-core` |
| `config-lua` | future programmable frontend | `config-core` |
| `security` | host keys, secrets, keychain contracts | none |
| `diagnostics` | doctor reports, fallbacks, performance/security reports | contract crates only; no runtime backends |
| `mux` | tabs, panes, sessions, layout model | `transport-core` when session contracts are needed |
| `shell-integration` | shell hooks and semantic escape support | `semantics` |
| `apps/*`, `tools/*` | composition, executable entrypoints, checks, benches | allowed to compose explicit layers by rule |
| `fuzz` | coverage-guided parser/core/semantic fuzz targets | `term-core`, `term-parser`, `semantics`, `shell-integration` |

`render-wgpu` currently uses `winit` as an external window-handle bridge for
surface creation. That dependency must stay backend-local and must not become a
dependency on `platform-winit`, transports, shells, SSH, or app runtime code.

## Provider Interfaces

These contracts are the approved crossing points between layers:

- Terminal transport: `transport-core::TerminalTransport`
- Renderer surface: `render-core::RendererSurface`
- Clipboard provider: `platform-core::ClipboardProvider`
- Keychain provider: `security::KeychainProvider`
- Window provider: `platform-core::WindowProvider`
- Config provider: `config-core::ConfigProvider`
- Diagnostics provider: `diagnostics::DiagnosticsProvider`

Backend crates implement these interfaces. Lower contract crates must not import
backend crates to call concrete implementations.

## Checks

Run:

```text
cargo xtask layer-check
```

The check scans workspace manifests and fails if a crate imports a workspace
crate outside its allowed boundary list.

`cargo xtask ci` runs:

```text
cargo xtask layer-check
cargo fmt --all --check
cargo check -p term-core
cargo test -p render-core
cargo clippy --workspace --all-targets
cargo test --workspace
cargo build --workspace --exclude xtask
```

The final build excludes the running `xtask` binary so the wrapper does not try
to overwrite itself on Windows. Run `cargo build -p xtask` directly when
checking the wrapper binary.

GitHub Actions also runs the architecture boundary subset on Windows, macOS, and
Ubuntu. This is not full cross-OS runtime verification; it only proves the
crate-boundary checks and contract tests are portable at CI build time.

## Non-Negotiable Failure Cases

These are rejected by the layer check:

- `term-core` depending on `render-core`, `render-wgpu`, `platform-*`,
  `transport-*`, `security`, or app crates.
- `render-core` depending on GPU APIs, PTY, SSH, shell integration, platform
  adapters, or app crates.
- `render-wgpu` depending on PTY, SSH, shell integration, platform adapters, or
  app crates.
- `platform-core` depending on renderer or transport crates.
- `config-core` depending on runtime crates or backends.

If a future feature appears to require one of those imports, redesign the
boundary instead of adding the dependency.
