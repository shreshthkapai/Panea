# platform-winit

- Owns: desktop window/event integration through winit, client-chrome action execution, clipboard/native-notification providers, and translation into platform-core contracts.
- Must not import: terminal parser internals, PTY/SSH implementations, semantic drawing, config frontends running scripts.
- Layer: platform parity.
- Tests required: event translation, capability reporting per OS/backend, compositor fallbacks, window-chrome action mapping/fallbacks, DPI/monitor updates, clipboard behavior, and non-blocking notification fallback behavior.

Client-owned fullscreen chrome sends platform-neutral actions from
`platform-core`. This crate maps them to winit window requests. Interactive
move rejection is returned as an explicit fallback. Close remains an app-owned
exit intent and never terminates the process from this layer. Capability tests
cover all desktop backends, but native runtime verification remains a separate
per-OS release gate.
