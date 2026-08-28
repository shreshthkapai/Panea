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

## Default Controls

```text
Ctrl+Shift+C / Super+C       copy selection
Ctrl+Shift+V / Super+V       paste
Ctrl+Backspace               delete previous shell word (sends Ctrl+W)
Shift+PageUp/PageDown        scroll one viewport
Ctrl+Shift+Home/End          oldest scrollback / live bottom
Ctrl+Shift+S                 interactive scrollback search
Ctrl+Shift+Space             keyboard normal selection mode
Ctrl+Alt+Shift+Space         keyboard rectangular selection mode
Alt+mouse drag               rectangular mouse selection
Shift+mouse drag             bypass application mouse reporting
Ctrl+left click URL          open validated HTTP(S) URL
```

While search is active, typing updates the query, Enter/Down selects the next
match, Shift+Enter/Up selects the previous match, Backspace edits by grapheme,
and Escape closes search. While keyboard selection is active, arrows,
Home/End, and PageUp/PageDown extend the range; Enter keeps the selection and
leaves selection mode, while Escape clears it.

## Keyboard Protocols

Applications can ask for richer key reporting than legacy VT bytes allow. Panea
supports two such protocols and never guesses between them by terminal or
application name; the choice follows only the modes the application sets.

### Legacy VT (default)

With no protocol enabled, keys are encoded as the traditional bytes: printable
text as itself, Ctrl+letter as a control character, navigation and function keys
as CSI/SS3 sequences honouring normal/application cursor and keypad modes. Key
releases and bare modifier presses produce nothing, because legacy encodings
have no way to express them.

### Kitty keyboard protocol (`CSI > flags u`)

Flag 1 (disambiguate) keeps the legacy encoding for every key that has one, and
uses CSI-u only where legacy is ambiguous or absent: Escape, F13 and above,
Super-modified keys, and modified Enter/Tab/Backspace. Flag 8 (report all keys)
moves everything to CSI-u. Flag 2 adds release and repeat events. An
application that pushes flags but cannot parse CSI-u still gets working input,
because flag 1 alone never changes plain text.

### Win32 input mode (`CSI ? 9001 h`)

This is the ConPTY contract used by Windows-native applications, including
Windows-native multiplexers. Each key becomes a serialized
`KEY_EVENT_RECORD`:

```
CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _
```

- `Vk` - Windows virtual-key code, `0` for text with no key behind it (IME).
- `Sc` - PC set-1 scan code, `0` when unknown.
- `Uc` - one UTF-16 code unit. A character outside the BMP is sent as two
  records, one per surrogate, exactly as Windows reports it.
- `Kd` - `1` for press, `0` for release.
- `Cs` - control-key state bits: right Alt `0x01`, left Alt `0x02`, right Ctrl
  `0x04`, left Ctrl `0x08`, Shift `0x10`, enhanced key `0x100`. AltGr is right
  Alt plus left Ctrl (`0x09`), which is what Windows itself reports.
- `Rc` - repeat count, at least `1`.

Because the record carries the virtual key, the scan code, the character, and
the modifier state independently, it expresses what legacy encodings cannot:
Shift+Enter distinct from Enter distinct from Ctrl+Enter, Ctrl+Shift+letter,
a modifier pressed on its own, key releases, left versus right modifiers, and
enhanced (navigation cluster) keys.

### Precedence

When win32 input mode is on it wins, and kitty encoding is suppressed for as
long as it stays on. An application that has enabled 9001 is reading console
records; CSI-u sequences arriving on the same channel are either discarded or
mistaken for text. Turning 9001 off restores whichever protocol the
application had asked for before.

## Implementation Status

- Shared key encoding covers printable/control text, Alt/AltGr, navigation,
  editing keys, F1-F12, normal/application cursor keys, and normal/application
  keypad keys.
- The kitty keyboard protocol and win32 input mode are both implemented, with
  win32 input mode taking precedence while it is set. Records carry virtual
  key, scan code, UTF-16 unit, press/release, and control-key state, so key
  releases, bare modifiers, AltGr, and Ctrl/Shift+Enter are all expressible.
- Mouse reporting covers normal, button-motion, all-motion, wheel, legacy, and
  SGR reports. Focus reports are emitted only when requested by terminal mode.
- Mouse and keyboard normal/rectangular selection use absolute buffer positions.
- Scrollback wheel, page, top, bottom, anchored viewport, and search navigation
  are implemented without PTY-output-loop search work.
- URL hit testing accounts for wide terminal cells and only permits HTTP(S).
- Linux primary selection uses the Linux clipboard provider and falls back with
  diagnostics when a compositor does not expose the required protocol.
- Windows focused tests pass. Real macOS, Linux X11, and Linux Wayland runtime
  verification remains a Step 16 platform-verification gate.
