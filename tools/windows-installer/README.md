# Panea Windows Installer

This release-tool crate builds a self-contained per-user Windows installer from
the package directory named by `PANEA_PACKAGE_ROOT`. It owns Windows install,
upgrade, shortcut, user-PATH, and uninstall mechanics. It must not be imported
by terminal, renderer, transport, platform, or config crates.

Normal workspace builds embed an empty development payload. `cargo xtask
package build` supplies the staged release payload and emits the distributable
installer. The installed Start menu shortcut targets the Windows GUI-subsystem
`panea-gui.exe`; `panea.exe` remains available on `PATH` for diagnostics and
other CLI commands.
