# Unicode Correctness

This document is the Phase 2 design note for terminal text correctness. The
implementation belongs to terminal core and parser boundaries only.

## Feature Design Note

```text
Feature name: Unicode, grapheme, emoji, and width correctness
Layer: core correctness
User-facing behavior: mixed Unicode text stores, moves, selects, resizes, and copies without splitting grapheme clusters
Config keys: none
macOS behavior: same terminal-core/parser behavior
Windows behavior: same terminal-core/parser behavior
Linux X11 behavior: same terminal-core/parser behavior
Linux Wayland behavior: same terminal-core/parser behavior
Fallback behavior: unsupported or invalid byte sequences are ignored rather than converted into corrupt terminal cells; one-column grids keep wide graphemes in one occupied cell
Diagnostics: covered by status docs and future conformance/fuzz diagnostics; no runtime user diagnostic is needed for normal valid Unicode
Performance cost when disabled: not optional; ASCII remains a fast path with no grapheme allocation
Performance cost when enabled: non-ASCII printable scalars may consult Unicode grapheme segmentation; no renderer, platform, config, or PTY blocking is introduced
Tests: Unicode scalar buffering, combining accents, CJK wide cells, emoji modifiers, ZWJ emoji, variation selectors, mixed text, cursor movement, selection, resize, scrollback, overwrite, erase, and delete behavior
```

## Cell Occupancy Rules

- A terminal cell stores one displayed grapheme cluster in `Cell::text`.
- A narrow grapheme occupies one cell.
- A wide grapheme occupies one base cell plus one continuation cell.
- Continuation cells never own text and are skipped during text extraction.
- A wide grapheme in a one-column grid is stored in the only available cell so
  the grid remains internally valid.
- Combining marks, emoji modifiers, variation selectors, regional-indicator
  pairs, and zero-width-joiner sequences extend the previous grapheme when
  Unicode segmentation says the combined text is still one grapheme cluster.

## Editing Rules

- Cursor back/forward movement lands on grapheme starts, not continuation cells.
- Backspace moves over a whole grapheme cluster.
- Selection expands across continuation cells so copied text is valid.
- Erase, delete, insert, overwrite, and resize operations do not leave orphan
  continuation cells or half-wide characters.
- Visual features must still treat these cells as terminal content; overlays
  must not rewrite Unicode text.
