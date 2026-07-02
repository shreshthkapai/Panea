# Packaging

Packaging must preserve one product contract across desktop operating systems.

Targets:

- macOS app bundle
- Windows installer
- Windows portable build
- Linux AppImage or equivalent
- Linux distro packages later

Packages must include:

- desktop binary
- built-in themes
- cursor assets
- icons
- shell integration scripts
- config examples
- license and notices

Packages must preserve platform config discovery paths. Secrets, SSH keys,
terminal contents, and user command output must not be placed in package logs or
diagnostics.

Run:

```powershell
cargo xtask package-plan
```

Current status: package plans exist, but release artifacts are not automated yet.
