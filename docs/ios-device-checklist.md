# iOS Device Validation Checklist

This checklist defines the manual evidence required before the iOS SSH
companion can be called real product behavior. The Rust workspace can enforce
shared contracts, but simulator and device validation must still be collected
with a native UIKit or SwiftUI host.

## Required Devices

- iPhone simulator
- iPad simulator
- at least one physical iPhone
- at least one physical iPad where hardware keyboard behavior can be tested

## Required Cases

| Case | Category | Required target | Expected result |
| --- | --- | --- | --- |
| `ios-ssh-known-host` | SSH | any | Connect to a controlled SSH server after accepting the displayed fingerprint; remote PTY opens and trust is persisted through the Keychain-backed policy. |
| `ios-ssh-changed-host-blocks` | security | any | A changed host key blocks the connection until explicitly resolved. |
| `ios-render-output` | rendering | simulator and device | ASCII, CJK, emoji, cursor, selection, and command-block fixtures render from shared `render-core` scenes without idle redraw loops. |
| `ios-software-keyboard-resize` | keyboard | iPhone | Showing and hiding the software keyboard resizes the remote PTY from safe-area and keyboard-aware terminal dimensions. |
| `ios-hardware-keyboard` | keyboard | iPad | Hardware keyboard input, including terminal control/meta-style shortcuts, reaches the SSH transport without blocking rendering. |
| `ios-touch-selection` | touch | any | Drag selection over mixed ASCII, CJK, and emoji respects shared grapheme/cell boundaries and copies valid UTF-8. |
| `ios-background-reconnect` | lifecycle | any | Backgrounding past the pause timeout leads to explicit disconnect/reconnect behavior; Panea does not promise indefinite background SSH. |
| `ios-remote-semantics` | semantics | any | Remote shell integration markers create semantic command regions without rewriting terminal text. |

## Native Gates

The following must be implemented before the checklist can pass:

- UIKit or SwiftUI app host.
- iOS GPU surface that consumes `render-core::RenderScene`.
- iOS Keychain-backed secret provider.
- Host-key approval and changed-key blocking UI.
- SSH profile editing UI.
- Key import/reference UX.
- Simulator and physical-device report export.

## Completion Rule

Passing Rust tests is not enough for iOS release readiness. A future validation
report must include the target device, OS version, renderer backend, SSH server
fixture, keyboard type, and pass/fail evidence for every required case.
