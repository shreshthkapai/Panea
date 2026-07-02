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
- `visual_theme`
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

Mux settings include `enabled`, `restore_sessions`, `default_workspace`,
`show_tab_bar`, `pane_resize_step`, and `remember_working_directory`. Session
restore persists layout/profile identity only; it does not promise process
resurrection.

The public key is `font`. `fonts` is accepted as an alias for compatibility
while the static schema stabilizes.

## SSH Profiles

SSH profiles describe remote sessions; defining one does not automatically
connect on startup.

```toml
[[ssh_profiles]]
name = "prod"
host = "example.com"
port = 22
username = "deploy"
auth_method = "public_key"
identity_file = "~/.ssh/id_ed25519"
known_hosts_policy = "require_known"
remote_working_directory = "/srv/app"
shell_integration = true
agent_forwarding = false
```

Supported `auth_method` values are `agent`, `public_key`, `password`,
`keyboard_interactive`, and `none`. The current SSH backend does not support
`none` authentication and fails clearly if selected.

Supported `known_hosts_policy` values are `ask`, `require_known`,
`trust_on_first_use`, and pinned fingerprints:

```toml
known_hosts_policy = { pin_fingerprint = { sha256 = "SHA256:..." } }
```

Host-key checks are security-sensitive. The default `ask` policy requires an
explicit trust decision for unknown hosts; app UI for that decision is deferred.

## Visual Theme

Visual features are overlays. They must not rewrite terminal text and they must
compile into renderer-independent primitives before a backend draws them.

Baseline visual sections:

- `visual_theme`: names the active theme/profile set, grouping style, spacing,
  borders, badges, and success/error accent colors
- `cursor`: shape, thickness, corner radius, inactive style, and bounded
  animation flags
- `prompt_decorations`: minimal separator, rounded box, and pill/header styles
- `command_blocks`: command grouping style, status/duration badges, copy/jump
  action flags, and output grouping groundwork
- `performance`: animation FPS, cursor asset size, active animation, and
  animated-region budgets

Shipped example configs live in `crates/assets/config-examples`:

- `plain-fast.toml`
- `balanced.toml`
- `command-blocks.toml`
- `minimal-aesthetic.toml`
- `heavy-visual-demo.toml`

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
- visual theme names, spacing, border, and animation budget ranges
- cursor blink/thickness ranges
- keybinding conflicts
- duplicate or missing shell/SSH profile references
- performance budget ranges
- invalid platform override profile references

Unknown and deprecated settings produce diagnostics. Validation errors stop
startup.

## Performance Profiles

Supported portable profile names:

- `maximum_performance`
- `balanced`
- `visual`
- `battery_saver`

The deprecated spelling `battery_conscious` is still accepted as an alias.

## Reload Contract

`config-core` can classify config changes into:

- live-reloadable changes: colors, fonts, cursor, padding, keybindings, input,
  diagnostics, and semantic visual settings
- restart-required changes: GPU backend, major window backend, shell profile
  startup settings, SSH profiles, and platform override changes

A file watcher and runtime live-reload applier are intentionally deferred until
the app lifecycle is ready to apply those changes safely.
