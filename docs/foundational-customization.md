# Foundational Customization

Panea compiles customization into `config_core::AppConfig` before the desktop
runtime, renderer, input path, or PTY loop consumes it. Static TOML and the
controlled programmable frontend produce the same resolved model.

## Design Note

Feature name: foundational customization
Layer: config-core, config-toml, config-lua, font-system, platform-winit,
render-core, render-wgpu, desktop app
User-facing behavior: portable themes, complete terminal color roles, font
selection and fallback, exact pixel insets, best-effort window opacity,
keyboard/mouse bindings, shell profiles, and performance/visual profiles.
Config keys: `window.*`, `font.*`, `colors.*`, `visual_theme.*`,
`keyboard.keybindings`, `mouse.*`, `shell_profiles`, and `performance.*`.
macOS behavior: the same resolved config and WGPU visual model are used;
opacity and installed-font behavior require real-host verification.
Windows behavior: config, font, renderer, input, screenshot, and focused runtime
tests pass on the current Windows host.
Linux X11 behavior: the same config and renderer model are used; compositor,
opacity, font, keyboard, and mouse behavior require real-host verification.
Linux Wayland behavior: the same config and renderer model are used; compositor
alpha support and input behavior require real-host verification.
Fallback behavior: unresolved configured fonts continue through the portable
fallback chain; unsupported surface transparency is diagnosed and rendered
opaque; unknown config/profile values fail validation or remain explicit custom
theme names rather than selecting an OS-specific substitute.
Diagnostics: config validation reports invalid ranges, bindings, profiles, and
fallbacks; font diagnostics report resolved sources; the desktop reports an
opaque-composition fallback when requested transparency is unavailable.
Performance cost when disabled: profiles and config are resolved before hot
paths; unused bindings and visual options do not create frames or PTY work.
Performance cost when enabled: font/profile changes rebuild bounded caches once;
padding is a coordinate offset; opacity uses normal surface composition; input
bindings perform a bounded linear lookup over the configured binding list.
Tests: config profile/override tests, font shaping tests, input binding tests,
renderer offset/damage tests, screenshot fixtures, and focused benchmarks.

## Portable Profiles

Built-in visual profiles are `balanced`, `plain-fast`, `minimal-aesthetic`, and
`command-blocks`. TOML expands the selected profile first, then applies explicit
settings, so direct user values always win. Arbitrary theme names remain valid
when the user supplies colors and visual values explicitly.

Performance profiles are `maximum_performance`, `balanced`, `visual`, and
`battery_saver`. They resolve cache, frame, and animation budgets once. Explicit
performance fields override profile defaults.

Shell profiles support base values plus macOS, Windows, Linux, Linux X11, and
Linux Wayland refinements. The active refinement is applied before a PTY is
spawned; the transport never reads config files.

## Colors And Fonts

Color configuration covers foreground, background, cursor, optional cursor
text, optional selection foreground, selection background, the configurable
16-color ANSI palette, the standard 6x6x6 indexed-color cube, grayscale indexed
colors, and truecolor output.

Fonts support family, size, line height, ordered fallback families, real style
faces, color emoji fallback, and a ligature switch that controls OpenType
`liga`, `clig`, and `calt` shaping features.

## Window Insets And Opacity

`padding_x`/`padding_y` and `margin_x`/`margin_y` are pixel values. They reduce
the terminal viewport, offset renderer content, map mouse coordinates back into
the correct pane, resize PTYs to the resulting cell grid, and force a retained
frame reset when reloaded.

Window opacity requests transparent window and WGPU surface composition. It is
portable user intent, not a guarantee that every compositor supports alpha.
Panea reports the opaque fallback instead of silently claiming transparency.

## Bindings

Keyboard bindings use portable modifier names and named actions. Mouse gestures
support press/release and wheel directions with Ctrl, Alt/Option, Shift, and
Super/Command modifiers. Supported mouse actions are `copy`, `paste`,
`paste_primary`, `open_url`, `select`, `select_rectangular`, `scroll`, and
`ignore`.

Application mouse-reporting mode retains protocol priority. Shift bypasses
application reporting for local selection and configured local actions, which
preserves editors, TUIs, and external multiplexers.

## Performance Checklist

- Runs every frame: only already-resolved colors, font cache state, and pixel
  offsets are read.
- Runs every input event: bounded binding matching; no parsing or scripting.
- Runs every PTY output batch: no config/profile work.
- Hot-path allocation: no profile/config parsing; existing scene data owns its
  normal render allocations.
- Full redraw: only font, surface, resize, or content-offset changes require it.
- GPU uploads: only newly required glyphs and normal changed batches.
- Script/user code: never during rendering, input handling, or PTY polling.
- Cacheable: font shaping, glyphs, atlases, and resolved profiles are cached.
- Disabled cost: no extra frames and no animation work.
- User budget: performance profile and explicit cache/frame/animation limits.
- Diagnostics: config, font, renderer, and performance diagnostics expose cost
  and fallbacks.
