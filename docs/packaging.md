# Packaging

Packaging belongs to platform parity, config portability, diagnostics, and
security. A package must feel like the same Panea product on every desktop OS,
while keeping installer-specific mechanics behind platform packaging layers.

## Design Note

Feature name: packaging artifacts
Layer: apps/desktop, assets, config-toml, shell-integration, diagnostics, xtask
User-facing behavior: users receive a packaged `panea` binary plus resources,
default config, shell integration scripts, documentation, and `panea doctor`
Config keys: no package-only config keys; packaged config templates use the
same portable `AppConfig`
macOS behavior: `Panea.app` bundle layout with binary under
`Contents/MacOS` and resources under `Contents/Resources`
Windows behavior: portable directory with `panea.exe` at the package root
Linux X11 behavior: portable directory with `bin/panea`, `share/panea`,
desktop file, and icon
Linux Wayland behavior: same Linux package layout as X11; backend behavior is
selected and diagnosed at runtime
Fallback behavior: MSI/DMG/AppImage/deb/rpm/signing/notarization are deferred;
the portable/staged artifact remains inspectable
Diagnostics: packaged `panea doctor --json` is smoke-tested
Performance cost when disabled: none; packaging is offline build tooling
Performance cost when enabled: one desktop binary build plus filesystem staging
Tests: xtask unit tests, package content verification, packaged doctor smoke,
and cross-OS runner integration

## Commands

Plan packaging work:

```powershell
cargo xtask package plan
cargo xtask package-plan
```

Build a release package for the current host OS:

```powershell
cargo xtask package build --profile release
```

Build and smoke a development package:

```powershell
cargo xtask package smoke --profile dev --build
```

The smoke command verifies required package contents and runs:

```text
panea doctor --json
```

Shell launch is still a manual package smoke because the current installed
binary does not expose a headless shell-session command. On each target OS,
launch the packaged app/binary and confirm that the default shell starts.

## Generated Layouts

Windows portable:

```text
panea-<version>-windows-portable-<profile>/
  panea.exe
  share/panea/
    config/default.toml
    config/schema.json
    config/examples/*.toml
    shell-integration/panea.{bash,zsh,fish,ps1}
    docs/*.md
    README.md
    LICENSE
    INSTALL.md
    WINDOWS.txt
    package-manifest.json
```

macOS app bundle:

```text
panea-<version>-macos-app-<profile>/
  Panea.app/
    Contents/Info.plist
    Contents/MacOS/panea
    Contents/Resources/
      config/default.toml
      config/schema.json
      config/examples/*.toml
      shell-integration/panea.{bash,zsh,fish,ps1}
      docs/*.md
      README.md
      LICENSE
      INSTALL.md
      package-manifest.json
```

Linux portable:

```text
panea-<version>-linux-portable-<profile>/
  bin/panea
  share/applications/panea.desktop
  share/icons/hicolor/scalable/apps/panea.svg
  share/panea/
    config/default.toml
    config/schema.json
    config/examples/*.toml
    shell-integration/panea.{bash,zsh,fish,ps1}
    docs/*.md
    README.md
    LICENSE
    INSTALL.md
    package-manifest.json
```

## Deferred

- Windows MSI/installer, Start menu integration, and PATH mutation.
- macOS zip/DMG, signing, and notarization.
- Linux AppImage, deb, rpm, dependency policy, and terminfo installation.
- Cross-OS collected package reports for macOS, Linux X11, and Linux Wayland.
