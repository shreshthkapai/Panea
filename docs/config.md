# Configuration

Panea has one portable internal config model: `config-core::AppConfig`.
Frontend formats compile into that model. Runtime code should not invent
parallel config structs unless they are backend adapters.

## Static TOML

The static frontend lives in `config-toml`.

Config discovery:

- Explicit path: `PANEA_CONFIG`
- Windows: `%APPDATA%\Panea\config.toml`
- Windows fallback: `%USERPROFILE%\.config\panea\config.toml`
- macOS/Linux: `$XDG_CONFIG_HOME/panea/config.toml`
- macOS/Linux fallback: `$HOME/.config/panea/config.toml`

The desktop app loads TOML at startup, applies platform overrides, validates the
result, and prints warnings to stderr.

Generate a default config:

```powershell
cargo xtask config-default
```

Generate editor-facing schema data:

```powershell
cargo xtask config-schema
```

## Main Sections

Baseline configurable sections:

- `window`
- `renderer`
- `font`
- `colors`
- `cursor`
- `scrollback`
- `keyboard`
- `mouse`
- `paste`
- `shell_profiles`
- `ssh_profiles`
- `mux`
- `performance`
- `platform`
- `diagnostics`

The public key is `font`. `fonts` is accepted as an alias for compatibility
while the static schema stabilizes.

## Platform Overrides

Platform overrides refine the base config. They should not be required for the
terminal to start normally.

```toml
[window]
title = "Panea"

[platform.windows.window]
title = "Panea on Windows"

[platform.linux.window]
decoration_strategy = "auto"

[platform.linux_x11.window]
linux_backend = "x11"

[platform.linux_wayland.window]
linux_backend = "wayland"
```

Supported platform keys:

- `platform.macos`
- `platform.windows`
- `platform.linux`
- `platform.linux_x11`
- `platform.linux_wayland`

## Validation

Validation catches:

- invalid window sizes and unsafe frameless recovery settings
- invalid font size and line-height ranges
- malformed palette lengths
- cursor blink/thickness ranges
- keybinding conflicts
- duplicate or missing shell/SSH profile references
- performance budget ranges
- invalid platform override profile references

Unknown and deprecated settings produce diagnostics. Validation errors stop
startup.

## Reload Contract

`config-core` can classify config changes into:

- live-reloadable changes: colors, fonts, cursor, padding, keybindings, input,
  diagnostics, and semantic visual settings
- restart-required changes: GPU backend, major window backend, shell profile
  startup settings, SSH profiles, and platform override changes

A file watcher and runtime live-reload applier are intentionally deferred until
the app lifecycle is ready to apply those changes safely.
