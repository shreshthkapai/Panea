# Screenshot Conformance

This directory stores deterministic renderer screenshot baselines.

```text
baselines/windows
baselines/macos
baselines/linux-x11
baselines/linux-wayland
```

Generate or refresh baselines on the target host with:

```powershell
cargo xtask screenshot capture --platform windows
```

Verify baselines with:

```powershell
cargo xtask screenshot verify --platform windows
```

Only capture a platform baseline on that platform. Do not copy a Windows
baseline into macOS or Linux directories to make verification look complete.

