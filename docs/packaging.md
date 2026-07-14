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
Linux X11 behavior: portable directory/tarball, Debian package, AppImage, RPM,
desktop file, and icon
Linux Wayland behavior: same Linux package layout as X11; backend behavior is
selected and diagnosed at runtime
Fallback behavior: portable archives remain available for local unsigned
development builds; tagged release jobs require signing credentials, and Linux
format builders fail clearly when their release tools are unavailable
Diagnostics: packaged `panea doctor --json`, `panea shell-smoke --json`, and
`panea gui-smoke --json` are smoke-tested
Performance cost when disabled: none; packaging is offline build tooling
Performance cost when enabled: one desktop binary build plus filesystem staging
Tests: xtask unit tests, package content verification, packaged doctor and
headless shell-session smoke, first-frame GPU GUI smoke, Windows
install/installed-binary/uninstall smoke, and cross-OS release runners

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
panea gui-smoke --json
```

On Windows it also installs into a temporary per-user-style directory, runs
all three commands from the installed binary, uninstalls, and verifies cleanup.

`shell-smoke` starts a bounded local PTY session, runs a one-shot marker command
through the selected/default shell profile, observes output, and shuts the
transport down. `gui-smoke` creates the real platform window, initializes the
GPU renderer and session, presents the first frame, then shuts down within a
bounded timeout. Interaction remains a separate manual release check.

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
panea-<version>-linux-<arch>-<profile>.AppImage
panea-<version>-linux-<arch>-<profile>.rpm
```

All package layouts include `themes/`, `cursor-profiles/`, `cursor-vectors/`, static and
programmable config examples, and shell integration scripts.
Each platform build emits a `SHA256SUMS.txt` manifest, and package smoke
recomputes every listed digest before launching packaged binaries.

## Signing And Notarization

Local package builds remain unsigned unless credentials are supplied. Tagged
release CI sets `PANEA_REQUIRE_SIGNING=1`, which converts missing credentials
into a hard failure instead of silently publishing unsigned artifacts.

Windows hooks:

```text
PANEA_WINDOWS_SIGN_CERTIFICATE=<path-to-pfx>
PANEA_WINDOWS_SIGN_PASSWORD=<secret>
PANEA_WINDOWS_SIGNTOOL=<optional-path-to-signtool.exe>
PANEA_WINDOWS_TIMESTAMP_URL=<optional-RFC3161-url>
```

The staged executable and installer are both Authenticode signed. macOS uses:

```text
PANEA_MACOS_SIGN_IDENTITY=<Developer ID Application identity>
PANEA_MACOS_NOTARY_PROFILE=<notarytool keychain profile>
```

The app is hardened-runtime signed and verified before archive creation; the
DMG is submitted with `notarytool --wait` and stapled. Secrets stay in CI secret
storage and are never written to package manifests or logs by Panea tooling.

Linux builders require `dpkg-deb`, `rpmbuild`, and `appimagetool` (or
`PANEA_APPIMAGETOOL`). Release CI installs/pins those tools before packaging.

## TERM/Terminfo Decision

Panea deliberately ships no custom terminfo entry for the alpha. It advertises
`xterm-256color`, which matches the current compatibility contract and works on
remote hosts without installation. A Panea-specific TERM will only be added
after stable, distinct capabilities and a remote fallback strategy exist.

## Release Boundaries

- Windows installer and portable artifacts are implemented and passed the
  current-host development package smoke. Authenticode hooks are implemented;
  a release certificate is still an external release credential.
- macOS ZIP/DMG generation is implemented but must run and be validated on a
  macOS host. Signing/notarization pipeline hooks are implemented; Apple
  credentials and final host verification remain external release evidence.
- Linux tarball, deb, AppImage, and RPM generation are implemented but still
  require collected Linux-host release reports.
- Automated first-frame GUI launch exists; full interaction remains manual on
  every target OS.
- Source and packaged artifacts carry the dual `MIT OR Apache-2.0` license.
  Package layouts include the license selector and both complete license texts.
