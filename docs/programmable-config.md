# Programmable Config

Programmable config belongs to config portability and security. It is a power
user frontend that compiles into the same `config_core::AppConfig` used by
static TOML.

## Design Note

Feature name: advanced programmable config
Layer: config-lua, config-core, apps/desktop
User-facing behavior: users can write a deterministic `config.panea` file with
controlled `panea.*` calls for generated themes, platform conditionals,
keybindings, shell/SSH profile declarations, cursor styles, mux labels, and
visual settings.
Config keys: the script writes the same keys as TOML through `panea.set`, plus
helpers such as `panea.theme`, `panea.key`, and `panea.platform_set`.
macOS behavior: same parser and same generated `AppConfig`; real host reload
validation remains unverified.
Windows behavior: same parser and generated `AppConfig`; explicit
`PANEA_CONFIG=...\config.panea` is supported on the current host.
Linux X11 behavior: same parser; `platform_set("linux_x11", ...)` records X11
specific overrides.
Linux Wayland behavior: same parser; `platform_set("linux_wayland", ...)`
records Wayland specific overrides.
Fallback behavior: invalid programs fail before app startup or reload planning;
static TOML remains supported and is still preferred when `config.toml` exists.
Diagnostics: compile diagnostics identify script actions and validation errors
reuse normal config diagnostics.
Performance cost when disabled: none.
Performance cost when enabled: one parse/compile/validate pass during config
load or explicit reload planning; no script runs in render, input, PTY, or
animation paths.
Tests: config-lua unit tests cover successful compilation, platform
conditionals, explicit platform overrides, validation failure, unsupported API
failure, provider behavior, and reload planning.

## File Discovery

Static TOML remains the normal config path.

Use programmable config explicitly:

```powershell
$env:PANEA_CONFIG = "C:\Users\me\.config\panea\config.panea"
panea doctor config
```

If there is no discovered `config.toml`, the desktop app also checks for:

- Windows: `%APPDATA%\Panea\config.panea`
- Windows fallback: `%USERPROFILE%\.config\panea\config.panea`
- macOS/Linux: `$XDG_CONFIG_HOME/panea/config.panea`
- macOS/Linux fallback: `$HOME/.config/panea/config.panea`

## Supported API

Set a portable config key:

```text
panea.set("font.family", "Cascadia Mono")
panea.set("font.size", 14)
panea.set("command_blocks.enabled", true)
```

Generate a simple theme:

```text
panea.theme("generated-night", "#101820", "#f4f7fb", "#4dd4ac")
```

Add keybindings and cursor mode styles:

```text
panea.key("Ctrl+Alt+T", "new_tab")
panea.cursor_mode("insert", "beam")
```

Declare shell and SSH profiles:

```text
panea.shell_profile("dev-pwsh", "powershell", "pwsh", ["-NoLogo"])
panea.ssh_profile("prod", "example.com", "deploy")
```

Use explicit portable platform overrides:

```text
panea.platform_set("windows", "font.family", "Cascadia Mono")
panea.platform_set("linux_wayland", "window.linux_backend", "wayland")
```

Use load-time conditionals for host-specific generation:

```text
panea.when_platform("windows")
panea.set("window.title", "Panea on Windows")
panea.end()
```

Prefer `panea.platform_set` when the output config should retain all platform
overrides and travel between operating systems.

## Safety Rules

Programmable config cannot:

- mutate terminal buffer contents
- access renderer internals or GPU resources
- spawn processes
- read arbitrary OS state through Panea APIs
- register render/input/PTY callbacks
- run every frame or every input event

The script is compiled into `AppConfig` before runtime systems consume it.
Renderer hot paths receive only precompiled config structs.

## Reload

The static TOML watcher remains the desktop runtime watcher in this slice.
Programmable config supports safe reload classification by compiling the next
program into `AppConfig` and comparing it with the current config using
`AppConfig::reload_plan_from`.

Automatic runtime watching for `config.panea` is intentionally deferred until
the cross-OS watcher behavior has the same previous-valid-config guarantees as
the TOML watcher.
