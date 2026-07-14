# platform-winit

- Owns: desktop window/event integration through winit, clipboard/native-notification providers, and translation into platform-core contracts.
- Must not import: terminal parser internals, PTY/SSH implementations, semantic drawing, config frontends running scripts.
- Layer: platform parity.
- Tests required: event translation, capability reporting per OS/backend, compositor fallbacks, DPI/monitor updates, clipboard behavior, and non-blocking notification fallback behavior.
