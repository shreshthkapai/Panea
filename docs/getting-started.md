# Getting Started

Panea is currently a development workspace, not a packaged daily-driver release.

## Build

```powershell
cargo build --workspace
```

## Run Desktop App

```powershell
cargo run -p panea-desktop
```

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
