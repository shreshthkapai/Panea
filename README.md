# Panea

Panea is a cross-platform terminal emulator built around the architecture in
[architecture.md](architecture.md).

Panea is currently an alpha candidate. The Windows installer and portable
package have passed automated install, diagnostics, local-shell, and uninstall
smoke checks on the development host. This is not yet a cross-OS verified
daily-driver release; see [docs/alpha-scope.md](docs/alpha-scope.md).

The implementation is organized as a Rust workspace with strict layer
boundaries. The first goal is terminal correctness; rendering, configuration,
semantics, SSH, multiplexing, and visual overlays are layered on top without
corrupting the raw terminal model.

## Workspace

- `apps/desktop` - desktop application entrypoint
- `apps/ios` - future iOS SSH companion shell
- `crates/term-core` - platform-neutral terminal state
- `crates/term-parser` - ANSI/VT parser adapter and state application
- `crates/semantics` - prompt, command, and region metadata
- `crates/render-core` - renderer-independent draw and damage model
- `crates/render-wgpu` - GPU renderer implementation
- `crates/font-system` - shaping, fallback, and glyph cache policy
- `crates/transport-core` - session I/O contracts
- `crates/transport-pty` - local PTY and pseudoconsole transports
- `crates/transport-ssh` - SSH transport
- `crates/platform-core` - platform capability traits and common events
- `crates/platform-winit` - desktop window/event integration
- `crates/config-core` - internal config model
- `crates/config-toml` - static config frontend
- `crates/config-lua` - programmable config frontend
- `crates/mux` - tabs, panes, sessions, workspaces, and layouts
- `crates/shell-integration` - shell hooks and semantic events
- `crates/diagnostics` - capability reports and doctor surfaces
- `crates/security` - secret handling and security policy interfaces
- `crates/assets` - built-in themes, cursors, icons, and scripts
- `tools/xtask` - repository automation
- `tools/bench` - benchmark fixtures
- `tools/conformance` - terminal conformance fixtures
- `fuzz` - parser, grid, resize, Unicode, OSC, and shell marker fuzz targets

## License

Panea is available under either the Apache License 2.0 or the MIT License, at
your option. See [LICENSE](LICENSE), [LICENSE-APACHE](LICENSE-APACHE), and
[LICENSE-MIT](LICENSE-MIT).

## Development

```powershell
cargo xtask layer-check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
cargo xtask fuzz-smoke
```

## Docs

- [Getting started](docs/getting-started.md)
- [Engineering rules](docs/engineering-rules.md)
- [Current status](docs/status.md)
- [Alpha scope](docs/alpha-scope.md)
- [Capability matrix](docs/capability-matrix.md)
- [Layer boundaries](docs/layer-boundaries.md)
- [Fuzzing](docs/fuzzing.md)
- [Renderer batching](docs/renderer-batching.md)
- [Renderer device recovery](docs/renderer-device-recovery.md)
- [App compatibility](docs/app-compatibility.md)
- [Configuration](docs/config.md)
- [Native notifications](docs/notifications.md)
- [Themes](docs/themes.md)
- [Cursor customization](docs/cursor-customization.md)
- [Command blocks](docs/command-blocks.md)
- [Shell integration](docs/shell-integration.md)
- [Desktop UX](docs/desktop-ux.md)
- [SSH profiles](docs/ssh-profiles.md)
- [SSH trust and secrets](docs/ssh-trust-and-secrets.md)
- [SSH server smoke tests](docs/ssh-smoke-tests.md)
- [Multiplexer usage](docs/multiplexer.md)
- [iOS SSH companion](docs/ios-companion.md)
- [Performance](docs/performance.md)
- [Platform support](docs/platform-support.md)
- [Linux compositor notes](docs/linux-compositor-notes.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Security](docs/security.md)
- [Packaging](docs/packaging.md)
- [Branding assets](docs/branding.md)
- [Release validation](docs/release-validation.md)
