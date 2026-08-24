# Release Validation

Before any daily-driver release candidate, run:

- unit tests
- integration tests
- parser tests
- conformance fixtures
- renderer smoke tests
- benchmark suite
- config compatibility tests
- shell integration tests
- SSH tests
- platform parity matrix
- manual smoke tests on macOS, Windows, Linux X11, and Linux Wayland

Local commands:

```powershell
cargo xtask ci
cargo xtask bench all
cargo xtask compat run --timeout-ms 5000
cargo xtask doctor
cargo xtask hardening
cargo xtask security-review
cargo xtask release-check
```

Performance comparisons against other terminals must wait until benchmark
fixtures, machines, settings, and competing terminal configs are public and
fair. Results must include scenarios where Panea loses.

## Windows Alpha Evidence - 2026-08-24

Artifact source commit: `c86815052f88dacbf870e81dc1c7c56b93549333`

Host:

```text
OS: Microsoft Windows 11 Home Single Language
Version: 10.0.26200 (build 26200), x86_64
Renderer: wgpu Vulkan
GPU: NVIDIA GeForce RTX 4060 Laptop GPU
Install: C:\Users\shres\AppData\Local\Programs\Panea
Installer SHA-256: 99bf6439d52f423b6abc817516fed5736ecde96e1ec6c922c92699310d72abfd
```

Passed gates:

```text
cargo fmt --all -- --check
cargo xtask layer-check
cargo check --workspace
cargo test -p platform-core window_chrome_action
cargo test -p platform-winit window_chrome_action
cargo test -p render-wgpu retained_frame -- --nocapture
cargo test -p render-wgpu window_chrome
cargo test -p panea-desktop fullscreen_chrome --lib
cargo xtask screenshot capture --platform windows
cargo xtask screenshot verify --platform windows
cargo xtask package build --target-platform windows --profile release
cargo xtask package smoke --target-platform windows --profile release --timeout-ms 15000
```

Screenshot verification reported exact matches for all 17 Windows fixtures,
including five fullscreen chrome states. Package smoke passed doctor, local
PTY, no-input single-prompt GUI startup, terminal input/output, temporary
install, uninstall, and cleanup. The staged and normally installed `panea.exe`
and `panea-gui.exe` hashes matched, and the installed GUI remained running
after launch.

The user manually accepted the fullscreen transition behavior on runtime
commit `35039d0`. The later `c868150` artifact changes only deterministic
screenshot fixtures and documentation, but the exact artifact's full
hover/control/DPI/monitor checklist remains a manual release sign-off item.
macOS, Linux X11, and Linux Wayland remain unverified.
