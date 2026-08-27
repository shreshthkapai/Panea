# Throughput and Per-Frame Cost Design

## Goal

Reduce terminal-output wake pressure and eliminate avoidable whole-grid, whole-scene, and whole-surface work while preserving retained-damage correctness and existing rendering behavior.

## Scope and constraints

- Implement checklist items X1 through X9 in the existing desktop, transport, terminal, renderer, and TOML configuration flows.
- Preserve the public behavior of terminal parsing, pane layout, selection, search, IME, transparency, and device-loss recovery.
- Retain the existing full-frame fallback where partial presentation is unavailable.
- Do not overwrite concurrent work. Recheck target-file hashes before edits and stop on unexpected changes.
- Mark an item DONE in `C:/Users/shres/Downloads/panea-fix-list.md` only after its implementation and focused verification pass.

## Event-loop and I/O design

`TransportWakeHandle` will own a shared atomic pending flag. A producer sends a user event only when it changes the flag from false to true. The event loop clears the flag at the start of `UserEvent`, then drains transport output. Clearing before draining permits a producer racing with the drain to schedule the next wake without losing work.

Desktop output polling will be routed through one helper and invoked only before latency-sensitive input handling and on explicit wakes. It will not run for unrelated pointer/window events or unconditionally in `AboutToWait`. Pane title, current-directory, and status metadata will be materialized only for panes whose output or session state changed.

The config watcher will use cheap `(modified time, length)` metadata comparisons before reading and hashing file contents. A watcher thread will notify the event loop when supported; metadata polling remains a safe fallback. Generated shell-hook content will be cached by profile/configuration hash so unchanged hooks are not synchronously rewritten.

## Terminal and scene design

Each terminal line will carry a monotonically changing generation. Mutations advance the affected line generation, while structural operations conservatively invalidate the affected range. Terminal state will expose borrowed visible-row iteration plus row generations, leaving the existing owned snapshot API available for compatibility.

The desktop scene builder will write pane cells directly into the reusable retained scene with row and column offsets. Per-frame vectors will be cleared while retaining capacity. Pane layouts will be computed once per model revision and shared by scene assembly, borders, hit testing, and related consumers.

Selection, search, and IME decoration work will be indexed by visible row. Search updates remain incremental as input or terminal generations change. Performance accounting will consume a running terminal byte counter instead of rescanning visible text.

## Renderer preparation design

Damage will be represented during preparation as row buckets containing an optional horizontal span. Cell membership then becomes constant time per row. Arbitrary rectangle inputs will be normalized with a sort-and-sweep merge rather than repeated restart-on-union scans. Preparation APIs will borrow `Option<&[DamageRegion]>`; full preparation will not clone a scene simply to call the shared path.

Text runs will track occupied display cells as they are built, eliminating repeated prefix-width scans. Cache probes will hash borrowed row/text keys without allocating an owned lookup key. The cache will have bounded least-recently-used eviction.

Rounded rectangles will use one quad carrying bounds/radius data and evaluate corner coverage in the fragment shader. Monochrome glyph coverage will use an `R8Unorm` atlas, while colored glyphs and image assets remain in an `Rgba8UnormSrgb` atlas with the appropriate sampler and shader path.

Repeated quads will use compact instance records and shared unit-quad geometry. Buffer uploads will be coalesced by batch. Retained-frame texture copies will be restricted to merged damage rectangles; the cursor/front-overlay pass will remain clipped to the same regions. Full-surface copies remain only for explicit full redraw or backend fallback.

## Correctness and failure handling

- Wake deduplication must never lose a wake that races with output draining.
- Generation wrap is handled by ordinary inequality; a full reset invalidates all rows.
- Empty or out-of-bounds damage produces no copy or draw commands.
- Atlas exhaustion follows the existing controlled restart path and cannot invalidate already emitted instances.
- Watcher thread errors fall back to metadata polling and do not terminate the event loop.

## Testing and verification

Implementation follows red-green-refactor cycles. Focused tests will cover wake false-to-true deduplication and race rearming, line-generation mutation, borrowed visible rows, direct offset scene writes, layout-cache invalidation, row-bucket damage membership, sort-and-sweep merging, non-allocating text-run behavior through cache hit/reuse, SDF rounded-rect instance emission, atlas format routing, and damage-limited texture copies.

After focused tests, run the affected crate suites, renderer and desktop clippy with warnings denied where the repository permits, `cargo check -p panea-desktop`, rustfmt checks on touched Rust files, and `git diff --check` on target paths. Checklist headings are updated only for requirements supported by the final diff and passing verification.
