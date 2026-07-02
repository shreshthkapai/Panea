# iOS Companion

Layer: platform parity, session transport, security, render performance,
semantic meaning, visual overlay.

This crate defines the iOS SSH companion shell contracts around the shared
terminal engine. It is intentionally a thin mobile boundary: terminal parsing,
terminal state, semantic regions, render scenes, portable config, SSH profiles,
host-key policy, and secret boundaries come from shared crates.

It owns:

- mobile app lifecycle policy
- touch, software-keyboard, and hardware-keyboard input models
- safe-area and keyboard-aware terminal sizing
- iOS SSH session UX contracts
- mobile readiness reporting helpers

It must not import:

- `platform-winit`
- `render-wgpu`
- `transport-pty`
- desktop app code
- native mux runtime code

Tests required:

- shared-engine dependency boundary tests
- terminal byte application through the shared parser/core
- viewport-to-terminal-size calculations
- SSH profile conversion preserves security policy
- lifecycle policy does not promise indefinite background sessions
