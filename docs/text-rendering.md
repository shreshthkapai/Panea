# Text Shaping and Font Fallback

This document defines Panea's portable text-rendering contract. Terminal cell
ownership remains in `term-core`; shaping, fallback selection, style-face
resolution, and glyph rasterization remain in `font-system`.

## Feature Design Note

```text
Feature name: Unicode and font rendering
Layer: render performance
User-facing behavior: terminal graphemes render through OpenType shaping, configured fallback chains, real style faces, and color emoji sources without changing raw terminal cells.
Config keys: font.family, font.fallback_families, font.size, font.line_height
macOS behavior: system font discovery plus the same Rust shaping/raster contract; Apple Color Emoji is an automatic fallback candidate.
Windows behavior: system font discovery plus the same Rust shaping/raster contract; Cascadia Mono, Consolas, and Segoe UI Emoji are automatic candidates.
Linux X11 behavior: system font discovery plus the same Rust shaping/raster contract; DejaVu and Noto-family fonts are used when installed.
Linux Wayland behavior: identical to Linux X11 because shaping and discovery do not depend on the display protocol.
Fallback behavior: configured families are tried in order per grapheme, then portable system candidates; an unresolved glyph uses a visible missing-glyph box.
Diagnostics: panea doctor fonts reports primary/fallback source paths and regular, bold, italic, and bold-italic face availability.
Performance cost when disabled: shaping is required for correct text; ASCII cells are grouped into cached style runs and unchanged glyphs remain atlas-resident.
Performance cost when enabled: fallback is selected per grapheme only when a run is first shaped; color glyph pixels use the same bounded atlas and batch count.
Tests: shaping clusters, fallback selection, CJK/emoji sequences, style diagnostics, color glyph composition, run grouping, cache reuse, and screenshot fixtures.
```

## Performance Note

```text
Does this run every frame? Only damaged text runs are prepared for a requested frame.
Does this run every input event? No.
Does this run every PTY output batch? Only changed terminal content reaches frame preparation.
Does this allocate in the hot path? A new text run allocates shaping output; run and glyph caches remove repeated work.
Does this force full redraw? No.
Does this require GPU uploads? Only glyphs not already resident in the atlas are uploaded.
Does this run script/user code? No.
Can it be cached? Shaped runs, rasterized glyphs, face loads, and atlas entries are cached.
Can it be disabled to near-zero cost? Color and fallback paths have no extra work for runs satisfied by the primary font.
Can the user budget it? Font size/fallback chain and renderer performance profiles bound the user-controlled surface.
Can diagnostics show its cost? Glyph cache hits/misses and atlas uploads/occupancy are instrumented.
```

## Implementation Contract

- `font-system` queries real regular/bold/italic/bold-italic faces through
  `fontdb`; it does not synthesize style metadata.
- `font.size` is measured in typographic points. The desktop runtime converts
  points to physical pixels using the active window scale factor, and rebuilds
  cell metrics when a window moves between displays.
- The generic `monospace` family resolves through Panea's portable modern
  fallback order before the host's legacy generic alias.
- Rustybuzz shapes OpenType runs and preserves cluster offsets.
- Rustybuzz positions are converted with the same `ab_glyph::PxScale`
  factor used by cell metrics and rasterization (`font.size_px /
  font.height_unscaled()`). Units-per-em must not be used as a substitute:
  mixing those scales makes text advances diverge from cell and cursor
  coordinates.
- Fallback selection occurs per Unicode grapheme, including combining marks,
  variation selectors, emoji modifiers, and ZWJ sequences.
- Swash rasterizes monochrome outlines, COLR/CPAL color outlines, and embedded
  color bitmaps. The GPU atlas stores RGBA data so color emoji are not tinted by
  the terminal foreground color.
- Compatible adjacent ASCII cells are shaped as one run, allowing configured
  font ligatures while terminal selection and cursor positions remain cell-based.
- CJK and emoji cells remain independently owned terminal graphemes; wide-cell
  occupancy remains a `term-core` responsibility.
- Shaped glyph advances remain floating point until final pixel placement so
  fractional advances cannot accumulate one rounding error per glyph.
- Tests require a primary monospace run's shaped advance to match its terminal
  cell span and require prepared glyph geometry to remain aligned with the
  following cursor cell.

## Verification Boundary

Automated implementation tests pass on the current Windows host. Installed-font
sets differ by OS, so macOS, Linux X11, and Linux Wayland screenshot/app reports
must still be collected before this feature is marked cross-OS verified.
