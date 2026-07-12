# Terminal Input, Selection, and Navigation

## Design Note

Feature name: terminal input correctness and selection/scrollback UX
Layer: core correctness, with platform event translation and desktop runtime integration
User-facing behavior: keys produce standard terminal byte sequences, application modes are honored, mouse/focus reports are sent only when requested, and users can select, navigate, search, copy, paste, and open detected URLs without changing terminal contents.
Config keys: existing `keyboard.*`, `mouse.*`, `clipboard.*`, `scrollback.*`, and future portable search/hint bindings compiled into `AppConfig`.
macOS behavior: winit translates native layout, Command, Option, dead-key, and IME events; the shared encoder emits terminal protocol bytes.
Windows behavior: winit translates native layout, Ctrl/Alt, AltGr, dead-key, and IME events; the shared encoder emits terminal protocol bytes.
Linux X11 behavior: winit translates X11 layout, modifiers, dead keys, and IME events; the shared encoder emits terminal protocol bytes.
Linux Wayland behavior: winit translates Wayland layout, modifiers, dead keys, and IME events; the shared encoder emits terminal protocol bytes.
Fallback behavior: unidentified keys produce no bytes and remain available to configurable keybindings; unsupported mouse buttons are not reported; URL actions require an explicitly detected URL.
Diagnostics: unhandled bindings and unavailable clipboard/URL providers report a clear diagnostic; terminal protocol behavior can be exercised without a window through unit tests.
Performance cost when disabled: no background work; selection, search, and URL detection run only in response to user actions.
Performance cost when enabled: key encoding is bounded and allocation is limited to the emitted byte sequence; mouse tracking is one bounded coordinate conversion per event; search scans the selected viewport or scrollback only when requested.
Tests: key/control/modifier/application-mode tables, mouse and focus protocol tests, normal/rectangular selection tests, viewport/scrollback tests, search tests, clipboard policy tests, and URL detection/action tests.

## Performance Checklist

- Does this run every frame? No.
- Does this run every input event? Only the bounded encoder or mouse state update for the relevant event.
- Does this run every PTY output batch? No, apart from existing terminal mode updates parsed from output.
- Does this allocate in the hot path? Encoded key bytes allocate a small bounded vector; text input reuses the platform-provided text.
- Does this force full redraw? Selection and viewport changes request redraw; input encoding does not.
- Does this require GPU uploads? No new uploads beyond normal changed-cell or overlay rendering.
- Does this run script/user code? No.
- Can it be cached? Search results and URL hints can be retained until terminal damage invalidates them.
- Can it be disabled to near-zero cost? Yes.
- Can the user budget it? Scrollback size is already bounded by config; search is user initiated.
- Can diagnostics show its cost? Search duration/result count can be added to existing diagnostics if profiling shows a need.

## Rollout

1. Shared key encoding and application cursor/keypad modes.
2. Complete mouse and focus protocol behavior.
3. Pane-aware mouse and keyboard selection.
4. Viewport-aware scrollback navigation.
5. Search, copy/paste, and URL actions.

Each item is accepted independently with focused tests. Real macOS, Linux X11,
and Linux Wayland input behavior remains unverified until exercised on those
hosts.
