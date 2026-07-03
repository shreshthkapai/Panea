# iOS SSH Companion

The iOS companion is a mobile SSH client built around the same Panea terminal
engine contracts. It is not a separate terminal product.

## Shared Engine

The iOS shell must reuse:

- `term-core` for terminal state
- `term-parser` for ANSI/VT byte application
- `semantics` for command regions and remote metadata
- `render-core` for renderer-independent scenes and overlays
- `config-core` for portable settings and SSH profile shape
- `transport-core` for session lifecycle contracts
- `transport-ssh` where the backend can be linked and verified on iOS
- the visual theme model for cursor, prompt, and command-block concepts

Desktop-only crates such as `platform-winit`, `render-wgpu`, and
`transport-pty` must not become mobile dependencies.

## iOS App Shell

The current workspace provides Rust contracts for:

- foreground and pause/disconnect lifecycle
- touch input events
- software keyboard modes
- hardware keyboard behavior
- safe-area and keyboard-aware terminal sizing
- mobile SSH session specifications
- quick reconnect actions
- native app shell bridge boundaries for lifecycle, frame requests,
  diagnostics, host-key decisions, and secret prompts

A native UIKit or SwiftUI host is still required before this can become an
actual iOS app.

## Rendering

iOS rendering consumes `render-core::RenderScene`. The GPU implementation may
use a native iOS backend where necessary, but it must preserve the same visual
contract used by the desktop renderer.

The iOS renderer must not redraw idle frames continuously. Cursor animations and
semantic overlays remain opt-in, bounded, and isolated from input handling.

The Rust foundation now records an `IosGpuSurfaceSpec` so the native renderer
path can state whether it is damage-driven, whether idle redraws are disabled,
and whether GPU timing is available. `Unavailable` remains the honest status
until a native iOS surface is implemented and profiled.

## SSH Profile UI

The mobile profile form is derived from the portable `SshProfile` model.
Validation is intentionally shared with the desktop security posture:

- profile name and host are required
- port must be non-zero
- public-key authentication requires an identity-file reference
- host-key policy defaults to explicit trust
- agent forwarding remains opt-in

The native app still needs the editing UI, key import/reference UX, and
connection error surfaces.

## SSH Security

The iOS companion inherits the same SSH rules:

- never silently skip host-key verification
- changed host keys block connection until explicitly resolved
- profiles compile into the shared SSH profile contract
- secrets flow through `SecretProvider`
- key and passphrase storage require iOS Keychain integration

The current Rust foundation does not implement Keychain storage or native
host-key approval UI. It does expose the iOS Keychain provider capability and
trust-prompt model so the native host cannot silently skip unknown or changed
host-key decisions.

## Lifecycle

iOS does not allow a terminal to promise indefinite background SSH sessions.
The companion should support foreground live sessions, graceful pause or
disconnect handling, quick reconnect, and clear recommendations for tmux or
similar remote persistence.

## Diagnostics

Run:

```text
cargo xtask ios-readiness
```

The report must keep native app, renderer, secure storage, and real-device
verification status explicit until those pieces are actually implemented and
tested.

Device validation cases live in
[iOS Device Validation Checklist](ios-device-checklist.md). Passing Rust unit
tests does not replace simulator and physical-device evidence.
