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
and uninstall; normal desktop launches use a GUI-subsystem entrypoint while
the console entrypoint remains available for diagnostics
Linux X11 behavior: portable directory/tarball, Debian package, AppImage, RPM,
desktop file, and icon
Linux Wayland behavior: same Linux package layout as X11; backend behavior is
selected and diagnosed at runtime
Fallback behavior: portable archives remain available for local unsigned
development builds; tagged release jobs require signing credentials unless the
repository explicitly enables its temporary unsigned-release policy, and Linux
format builders fail clearly when their release tools are unavailable
Diagnostics: packaged `panea doctor --json`, `panea shell-smoke --json`, and
`panea gui-smoke --startup --json` and
`panea gui-smoke --terminal-io --json` are smoke-tested
Performance cost when disabled: none; packaging is offline build tooling
Performance cost when enabled: one desktop binary build plus filesystem staging
Tests: xtask unit tests, package content verification, packaged doctor and
headless shell-session smoke, terminal-I/O GPU GUI smoke, Windows
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
panea gui-smoke --startup --json
panea gui-smoke --terminal-io --json
```

On Windows it also installs into a temporary per-user-style directory, runs
all three commands from the installed binary, uninstalls, and verifies cleanup.

`shell-smoke` starts a bounded local PTY session, runs a one-shot marker command
through the selected/default shell profile, observes output, and shuts the
transport down. `gui-smoke --startup` verifies that launch settles on exactly
one shell prompt without sending input. `gui-smoke --terminal-io` creates the real platform window,
initializes the GPU renderer and session, waits for the shell prompt, sends a
marker command, observes both input echo and command output, presents that
frame, and then shuts down within a bounded timeout. Broader interaction
remains a separate manual release check.

## Generated Layouts

Windows portable:

```text
panea-<version>-windows-portable-<profile>/
  panea.exe
  panea-gui.exe
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

`panea-gui.exe` and `panea.exe` execute the same desktop runtime. The GUI
entrypoint is linked with the Windows GUI subsystem, so Start-menu and portable
desktop launches do not create a console window. The console entrypoint keeps
stdout/stderr available for `doctor`, shell integration, and smoke commands.
The installer shortcut targets `panea-gui.exe`; the user `PATH` still exposes
`panea.exe`. During the initial window-focus handoff, launcher activation keys
that can remain held by Start menu or desktop activation are quarantined until
release; they are never forwarded to the new PTY session as terminal input.

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
release CI sets `PANEA_REQUIRE_SIGNING=1` by default, which converts missing
credentials into a hard failure instead of silently publishing unsigned
artifacts.

Maintainers can explicitly set the repository variable
`PANEA_ALLOW_UNSIGNED_RELEASES=true` while release certificates are not yet
available. That temporary policy permits unsigned Windows and macOS artifacts,
adds a prominent warning to the GitHub Release, and does not bypass native
package smoke tests, checksums, or provenance attestations. Remove the variable
or set it to `false` once signing credentials are configured; absence is the
secure, signing-required default.

Windows hooks:

```text
PANEA_WINDOWS_SIGN_CERTIFICATE=<path-to-pfx>
PANEA_WINDOWS_SIGN_PASSWORD=<secret>
PANEA_WINDOWS_SIGNTOOL=<optional-path-to-signtool.exe>
PANEA_WINDOWS_TIMESTAMP_URL=<optional-RFC3161-url>
```

When the Windows signing variables are configured, both staged Windows
entrypoints and the installer are Authenticode signed.
macOS uses:

```text
PANEA_MACOS_SIGN_IDENTITY=<Developer ID Application identity>
PANEA_MACOS_NOTARY_PROFILE=<notarytool keychain profile>
```

When the macOS signing variables are configured, the app is hardened-runtime
signed and verified before archive creation; the DMG is submitted with
`notarytool --wait` and stapled. Secrets stay in CI secret storage and are never
written to package manifests or logs by Panea tooling.

Linux builders require `dpkg-deb`, `rpmbuild`, and `appimagetool` (or
`PANEA_APPIMAGETOOL`). Release CI installs/pins those tools before packaging.

## TERM/Terminfo Decision

Panea deliberately ships no custom terminfo entry for the current compatibility
baseline. It advertises `xterm-256color`, which matches the current compatibility
contract and works on remote hosts without installation. A Panea-specific TERM
will only be added after stable, distinct capabilities and a remote fallback
strategy exist.

## Release Boundaries

- Desktop startup arms its launcher-input quarantine immediately before the
  native event loop begins consuming events. Slow font discovery or GPU setup
  therefore cannot expire the guard and forward the Windows Search activation
  key into the shell. Local PTY startup begins before GPU device creation so
  initial shell output can be produced concurrently with renderer setup.
- Windows installer and portable artifacts are implemented and passed the
  current-host development package smoke. Authenticode hooks are implemented;
  a release certificate is still an external release credential.
- macOS ZIP/DMG generation and native-runner package smoke are implemented.
  Signing/notarization pipeline hooks are implemented; Apple credentials remain
  external release credentials.
- Linux tarball, deb, AppImage, and RPM generation and the X11 software-GPU
  package smoke are implemented. Broader compositor validation remains tracked
  separately.
- Automated terminal-I/O GUI launch exists; broader interaction remains manual on
  every target OS.
- Source and packaged artifacts carry the dual `MIT OR Apache-2.0` license.
  Package layouts include the license selector and both complete license texts.
