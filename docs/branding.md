# Branding Assets

Feature name: Panea application branding

Layer: assets and platform packaging

User-facing behavior: Panea uses the same application mark in desktop windows,
portable packages, launchers, and future installers.

Config keys: none; branding is a product asset rather than user configuration.

macOS behavior: the app bundle contains `Panea.icns` and references it through
`CFBundleIconFile`.

Windows behavior: the desktop executable embeds `panea.ico`; portable packages
also include the ICO under `share/panea/icons`.

Linux X11 behavior: packages install the 512 px PNG through the freedesktop
hicolor icon layout and reference it as `panea` from the desktop entry.

Linux Wayland behavior: identical to Linux X11; the compositor or launcher
selects the appropriate hicolor icon.

Fallback behavior: the unbranded executable remains launchable if a desktop
environment ignores package icon metadata. Packaging verification reports a
missing required icon as an error.

Diagnostics: package-content smoke tests verify each platform asset. Runtime
window-icon diagnostics can be added when native window icon reporting exists.

Performance cost when disabled: none.

Performance cost when enabled: none in the renderer or PTY paths; icons are
loaded by the operating system or desktop shell.

Tests: asset container signatures, deterministic generation, package-content
checks, and platform package smoke tests.

## Regenerating

The authoritative source is `crates/assets/branding/panea-source.png`. Generate
all derived PNG, ICO, and ICNS assets with:

```text
cargo xtask branding
```

The generator preserves the source colors, detects the mark bounds, centers it
on a square canvas, and emits every platform format from one master image.
