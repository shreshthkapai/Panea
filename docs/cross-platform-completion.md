# Cross-Platform Runtime Completion

## Transparent Window Transitions

```text
Feature name: transparent window transition finalization
Layer: platform-winit
User-facing behavior: transparent windows do not retain a previous decorated frame across windowed, maximized, and fullscreen transitions
Config keys: window.opacity, window.mode, window.decoration_strategy
macOS behavior: Cocoa fullscreen remains asynchronous through winit; Windows compositor operations are never applied
Windows behavior: transparent borderless fullscreen and native maximize transitions are coalesced and re-presented once after the resize event batch with DWM synchronization
Linux X11 behavior: fullscreen and maximize remain EWMH/window-manager requests through winit; compositor bypass and Windows refresh logic are not applied
Linux Wayland behavior: fullscreen and maximize remain compositor-negotiated xdg-shell states through winit; client geometry is not forced
Fallback behavior: a failed Windows composition refresh leaves the effective window mode unchanged and reports the failure
Diagnostics: transparent_window_composition names the requested transition, retained effective mode, and DWM error
Performance cost when disabled: zero; opaque windows never queue transition work
Performance cost when enabled: one coalesced platform operation per affected transition, with no render-frame, input, or PTY hot-path work
Tests: policy tests cover Windows coalescing/rearming, opaque windows, and macOS/X11/Wayland isolation; native Windows verification covers borderless fullscreen to windowed to system maximize
```

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
