# Getting Started

Panea is currently an alpha candidate, not a cross-OS verified daily-driver
release. Native distribution builders exist, but only the Windows development
installer path has been exercised on the current host.

## Build

```powershell
cargo build --workspace
```

## Run Desktop App

```powershell
cargo run -p panea-desktop
```

## Build An Installer Or Portable Package

On the target OS, build release artifacts with:

```powershell
cargo xtask package build --profile release
```

Windows emits a portable ZIP and per-user installer EXE. macOS emits an app
ZIP and DMG. Linux emits a portable tarball, deb, AppImage, and RPM. Validate staged binaries
and, on Windows, the full install/uninstall path with:

```powershell
cargo xtask package smoke --profile release
```

Artifacts are written under `target/packages/`. macOS signing/notarization and
Windows signing require release-owner credentials; tagged release CI fails if
those credentials are absent. Panea is distributed under
the dual `MIT OR Apache-2.0` license.

The app loads the default portable config when no user config exists. Generate a
sample config with:

```powershell
cargo xtask config-default
```

## Check Readiness

```powershell
cargo xtask doctor
cargo xtask hardening
cargo xtask security-review
cargo xtask release-check
```

Daily-driver readiness requires successful validation on macOS, Windows, Linux
X11, and Linux Wayland.
