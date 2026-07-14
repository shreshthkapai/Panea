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
macOS behavior: `Panea.app` bundle plus ZIP and DMG distribution artifacts
Windows behavior: portable directory/ZIP plus a self-contained per-user
installer with atomic upgrade, Start menu shortcuts, user PATH registration,
and uninstall
Linux X11 behavior: portable directory/tarball plus a Debian package, desktop
file, and icon
Linux Wayland behavior: same Linux package layout as X11; backend behavior is
selected and diagnosed at runtime
Fallback behavior: portable archives remain available when signing credentials
or optional distro tooling are unavailable; signing/notarization, AppImage,
and rpm are explicit release-toolchain steps
Diagnostics: packaged `panea doctor --json` and `panea shell-smoke --json`
are smoke-tested
Performance cost when disabled: none; packaging is offline build tooling
Performance cost when enabled: one desktop binary build plus filesystem staging
Tests: xtask unit tests, package content verification, packaged doctor and
headless shell-session smoke, Windows install/installed-binary/uninstall smoke,
and cross-OS release runners

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
panea shell-smoke --json
```

On Windows it also installs into a temporary per-user-style directory, runs
both commands from the installed binary, uninstalls, and verifies cleanup.

`shell-smoke` starts a bounded local PTY session, runs a one-shot marker command
through the selected/default shell profile, observes output, and shuts the
transport down. Full GUI launch remains a manual release smoke on each target
OS.

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

The Windows build also emits:

```text
panea-<version>-windows-portable-<arch>-<profile>.zip
panea-<version>-windows-installer-<arch>-<profile>.exe
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

The macOS host also emits a ZIP and compressed DMG containing `Panea.app`.
Signing and notarization are intentionally separate because they require the
release owner's Apple credentials.

Linux portable:

```text
panea-<version>-linux-portable-<profile>/
  bin/panea
  share/applications/panea.desktop
  share/icons/hicolor/512x512/apps/panea.png
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

The Linux host also emits:

```text
panea-<version>-linux-portable-<arch>-<profile>.tar.gz
panea-<version>-linux-<arch>-<profile>.deb
```

All package layouts include `themes/`, `cursor-profiles/`, static and
programmable config examples, and shell integration scripts.
Each platform build emits a `SHA256SUMS.txt` manifest, and package smoke
recomputes every listed digest before launching packaged binaries.

## Release Boundaries

- Windows installer and portable artifacts are implemented and passed the
  current-host development package smoke. Authenticode signing requires a
  release certificate.
- macOS ZIP/DMG generation is implemented but must run and be validated on a
  macOS host. Code signing/notarization requires Apple credentials.
- Linux portable tarball and deb generation are implemented but must run and
  be validated on Linux. AppImage/rpm remain additional distribution formats.
- Full GUI launch and interaction remain manual release checks on every target
  OS; headless doctor and real-PTY shell startup are automated.
- Source and packaged artifacts carry the dual `MIT OR Apache-2.0` license.
  Package layouts include the license selector and both complete license texts.
