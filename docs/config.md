# Configuration

Panea has one portable internal config model: `config-core::AppConfig`.
Frontend formats compile into that model. Runtime code should not invent
parallel config structs unless they are backend adapters.

The current schema version is `2`. Generated configs include
`schema_version = 2`. Version-1 or unversioned files are migrated in memory
before deserialization; deprecated `fonts`, `platform_overrides`, `shells`,
`font.font_size`, and `window.decorations` spellings map to current fields with
diagnostics. Configs from a newer unsupported schema fail clearly instead of
being guessed.

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

## Programmable Config

The advanced frontend lives in `config-lua`. It is intentionally controlled:
`config.panea` files use deterministic `panea.*` calls that compile into the
same `AppConfig` before any renderer, input, PTY, or animation hot path can see
the result.

Static TOML remains supported and remains the simple default. Programmable
config is loaded when `PANEA_CONFIG` points at a `.panea` or `.lua` file. If no
static `config.toml` is discovered, the desktop app also checks for
`config.panea` in the normal platform config directories.

See [programmable-config.md](programmable-config.md) for the API and safety
rules.

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
- `clipboard`
- `paste`
- `shell_profiles`
- `ssh_profiles`
- `mux`
- `performance`
- `platform`
- `diagnostics`

Mux settings include `enabled`, `restore_sessions`, `default_workspace`,
`show_tab_bar`, `tab_title_format`, `status_format`, `pane_resize_step`, and
`remember_working_directory`. Session restore persists layout/profile identity
only; it does not promise process resurrection.

The public key is `font`. `fonts` is accepted as an alias for compatibility
while the static schema stabilizes.

## Clipboard

Clipboard behavior is configured through `clipboard`. The older `paste`
section remains available for low-level paste sanitization compatibility while
the product-facing clipboard policy lives under `clipboard`.

```toml
[clipboard]
enabled = true
copy_on_select = false
paste_protection = true
bracketed_paste = true
middle_click_paste = true
prefer_primary_selection_on_linux = true
log_operations = false

[clipboard.osc52]
enabled = true
allow_local = true
allow_remote = false
max_bytes = 1048576
confirm_remote_writes = true
```

Remote OSC 52 clipboard writes are denied by default. Large OSC 52 writes are
capped before they can touch the system clipboard. When `allow_remote = true`
and `confirm_remote_writes = true`, Panea shows a renderer overlay containing
the session, target, and payload size but never the clipboard contents. `Y`
allows that one write; `N` or Escape denies it.

## Notifications

Native session notifications use one portable configuration on every desktop
platform:

```toml
[notifications]
enabled = true
only_when_unfocused = true
session_closed = true
transport_errors = true
```

Delivery is queued outside the render, input, and PTY paths. The worker starts
only when the first notification is needed. Windows uses toast notifications,
macOS uses Notification Center, and Linux uses the freedesktop D-Bus protocol.
Unavailable or permission-blocked providers report an explicit diagnostic.

## Shell Integration

Shell integration is optional. It enables semantic prompt, input, output,
current-directory, shell, exit-status, and command-duration events without
changing terminal buffer text.

```toml
[shell_integration]
enabled = true
activation = "auto_detect"
auto_install = false
enabled_shells = ["bash", "zsh", "fish", "powershell", "pwsh"]
disabled_shell_profiles = []
remote_instructions = true
```

Supported `activation` values are `full`, `auto_detect` / `auto`, `manual`,
`heuristic`, and `disabled` / `off`. `full` injects a runtime hook for
supported local shells. `auto_detect` accepts semantic escape sequences and
injects only when `auto_install = true`. `off` disables semantic event parsing
for the session.

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
explicit trust decision for unknown hosts. Passwords, passphrases, and private
key contents are never config fields; they flow through secret/keychain
providers at the app boundary.

## Visual Theme

Visual features are overlays. They must not rewrite terminal text and they must
compile into renderer-independent primitives before a backend draws them.

Baseline visual sections:

- `visual_theme`: names the active theme/profile set, grouping style, spacing,
  borders, badges, prompt/command/input/output/badge colors, and success/error
  accent colors
- `cursor`: shape, thickness, corner radius, inactive style, bounded animation
  flags, and opt-in image cursor asset settings
- `prompt_decorations`: minimal separator, rounded box, and pill/header styles
- `command_blocks`: command grouping style, status/duration badges, copy/jump
  action flags, output grouping/collapse, and alternate-screen overlay policy
- `performance`: animation FPS, cursor asset size, active animation, and
  animated-region budgets

Recognized visual profiles are `balanced`, `plain-fast`,
`minimal-aesthetic`, and `command-blocks`. Recognized performance profiles are
`maximum_performance`, `balanced`, `visual`, and `battery_saver`. Profiles are
expanded before explicit TOML values, so explicit values win.

The color model includes foreground/background, cursor/cursor text,
selection foreground/background, a configurable 16-color ANSI palette, the
standard indexed 256-color cube/grayscale range, and truecolor. Font config
includes family, size, line height, ordered fallback families, and OpenType
ligature control.

Window `padding_x`/`padding_y` and `margin_x`/`margin_y` are exact pixel insets.
Opacity requests transparent window/surface composition and reports an opaque
fallback if the active backend cannot provide it.

Renderer diagnostics can optionally request GPU timestamp queries:

```toml
[renderer]
gpu_timestamps = false

[diagnostics]
performance_overlay = false
performance_overlay_position = "top_right" # top_left, top_right, bottom_left, bottom_right
performance_overlay_detail = "compact"     # compact or detailed
persist_performance_overlay = true
```

`renderer.gpu_timestamps` is portable and defaults off. If a backend does not
support timestamp queries, Panea reports the timing status as unsupported and
continues rendering.
The default `Ctrl+Shift+F12` binding toggles the in-window performance overlay.
Clicking its first metrics row opens controls for detail, placement, and hide.
When persistence is enabled, those runtime choices are stored in Panea's OS
state directory without rewriting the portable config file.

Mux drag behavior is portable and can be disabled independently:

```toml
[mux]
drag_tabs = true
drag_panes = true
```

Drag tabs directly across the tab bar. Hold `Ctrl+Shift` while dragging a pane
onto another pane to swap their split-tree positions; terminal and transport
ownership move with the pane model rather than being recreated.

Battery adaptation is portable and enabled by default:

```toml
[performance]
profile = "balanced"
disable_expensive_effects_on_battery = true
```

Panea samples power state outside render/input/PTY paths. While discharging it
temporarily caps optional animation/cache budgets; returning to AC restores the
configured values. Set the key to `false` to disable both adaptation and power
provider polling.

```toml
[cursor]
shape = "block"
blink = true
animations_enabled = false
smooth_movement = false
typing_pulse = false
typing_stretch = false
trail = false
blink_easing = false
short_lived_glow = false
shadow = false

[cursor.image]
enabled = false
path = ""
fps = 24
warn_if_expensive = true

[prompt_decorations]
enabled = true
style = "minimal_separator"
show_shell_badge = false
show_current_directory = false
show_remote_host = false
show_admin_badge = false
show_previous_status_accent = false
allow_in_alternate_screen = false

[command_blocks]
enabled = true
style = "subtle"
separate_prompt_input_output = true
show_duration = true
show_exit_status = true
show_current_directory = true
show_shell_host = true
copy_actions_enabled = true
jump_actions_enabled = true
collapse_long_output = false
collapse_after_lines = 200
collapsed_preview_lines = 1
allow_in_alternate_screen = false

[visual_theme.spacing]
block_margin_px = 3
block_padding_px = 6
badge_gap_px = 4
```

`cursor.image.path` accepts GIF animation or a static PNG. Relative paths are
resolved from the config file directory; `~/...` uses the current platform home
directory. Image cursors are opt-in, decoded off the render thread, and bounded
by the performance cursor-asset and animation budgets. See
`crates/assets/config-examples/custom-cursor.toml`.

Command block styles are `traditional`, `subtle`, `card`, `split`,
`minimal_header`, and `custom_theme`. `Ctrl+Shift+G` toggles collapse for the
current or most recent command. Collapse changes only renderer presentation;
raw terminal text remains available to selection, search, and copy.

The alternate-screen defaults protect full-screen TUIs. Setting either
`allow_in_alternate_screen` key to `true` is portable, but validation emits a
warning because overlays may obscure applications such as editors, pagers, or
multiplexers.

Shipped example configs live in `crates/assets/config-examples`:

- `plain-fast.toml`
- `balanced.toml`
- `command-blocks.toml`
- `minimal-aesthetic.toml`
- `heavy-visual-demo.toml`
- `foundational-customization.toml`

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
- clipboard and OSC 52 policy ranges
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

## Multiplexer Layouts

Startup workspaces use transport-neutral recursive layouts. The same config is
valid on every desktop OS:

```toml
[mux]
restore_sessions = true
show_tab_bar = true
tab_title_format = "{index}: {title}"

[mux.appearance]
active_tab_background = { red = 54, green = 62, blue = 75, alpha = 255 }
active_pane_border = { red = 80, green = 150, blue = 255, alpha = 255 }
pane_border_width = 1

[[mux.startup_workspaces]]
name = "work"

[[mux.startup_workspaces.tabs]]
name = "mixed"

[mux.startup_workspaces.tabs.layout]
kind = "split"
axis = "horizontal"
ratio = 0.6

[mux.startup_workspaces.tabs.layout.first]
kind = "pane"
transport = "local"
profile = "dev"

[mux.startup_workspaces.tabs.layout.second]
kind = "pane"
transport = "ssh"
profile = "prod"
```

Named keybinding actions can open a specific profile, for example
`new_ssh_tab:prod`, `split_ssh_vertical:prod`, `new_local_tab:dev`, or
`split_local_horizontal:dev`. Workspace actions support
`new_workspace:name`, `switch_workspace:name`, and `rename_workspace:name`.
`reconnect_session` reconnects the active SSH pane while preserving its local
scrollback; the default binding is `Ctrl+Alt+R`.
Restoration recreates layouts and starts fresh transports; it does not claim
process resurrection.

## Reload Contract

`config-core` can classify config changes into:

- live-reloadable changes: colors, fonts, cursor, padding, keybindings, input,
  diagnostics, performance budgets, window title, mux appearance/settings, and semantic
  visual settings
- restart-required changes: GPU backend, renderer scheduling/damage policy,
  major window settings/backend changes, shell profile startup settings, SSH
  profiles, mux startup/restoration layouts, scrollback storage policy, and
  platform override changes

The desktop runtime watches the active TOML or programmable config path with a
debounced portable polling watcher. Valid live-reloadable changes are applied without
restarting the shell session. Invalid config or runtime apply failures keep the
previous valid config active and report diagnostics.

See [config-reload.md](config-reload.md) for the current live-reload contract
and deferred cross-OS validation work.
