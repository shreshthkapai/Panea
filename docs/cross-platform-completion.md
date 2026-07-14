# Cross-Platform Runtime Completion

## Feature Design Note

```text
Feature name: desktop platform runtime completion
Layer: platform-core, platform-winit, desktop app, diagnostics
User-facing behavior: one window/input/clipboard/config model across Windows, macOS, Linux X11, and Linux Wayland
Config keys: window.mode, window.linux_backend, window.decoration_strategy, window dimensions/opacity, clipboard.*, platform.* overrides
macOS behavior: winit native window, per-monitor scale events, IME composition, system clipboard, native/fullscreen and frameless modes
Windows behavior: winit native window, per-monitor DPI, IME composition, system clipboard, exclusive/borderless/frameless modes
Linux X11 behavior: explicitly selectable X11 event loop, X11 clipboard/primary selection, WM-governed decoration fallback
Linux Wayland behavior: explicitly selectable Wayland event loop, Wayland clipboard/primary selection, compositor decoration negotiation and explicit fallback
Fallback behavior: unavailable exclusive video mode falls back to borderless; unsupported decoration strategy falls back to native decorated; unavailable backend/window creation fails clearly
Diagnostics: requested/effective window and decoration modes, backend/session/compositor environment, DPI, clipboard and keychain providers through panea doctor
Performance cost when disabled: capability and decoration resolution happen at startup only
Performance cost when enabled: resize/DPI/IME work is event-driven; no polling or continuous redraw
Tests: pure fallback tests, input translation tests, cross-OS CI runners, Linux compositor checklist, and native-host manual verification
```

## Runtime Guarantees

- Linux `auto`, `x11`, and `wayland` select winit's real backend builder rather
  than relying on a diagnostic-only preference.
- DPI scale changes resize the GPU surface, pane layout, terminal grids, and
  PTYs through the same portable event.
- IME preedit text is a transient overlay; only committed text enters the
  active terminal transport.
- Fullscreen attempts an exclusive monitor mode and reports a borderless
  fallback when no mode is exposed.
- Decoration requests resolve once into requested/effective/fallback state.
  Frameless recovery shortcuts remain active.
- Clipboard failures, missing Linux primary selection, unavailable keychain,
  and compositor limitations remain visible rather than silently ignored.

## Verification Boundary

Windows automated tests compile and pass on the current host. macOS, Linux X11,
and Linux Wayland implementations and CI jobs exist, but they are not
`cross-os verified` until native-host reports, screenshot baselines, IME/DPI
checks, and compositor checklists are collected and reviewed.
