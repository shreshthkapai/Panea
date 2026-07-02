# term-parser

- Owns: ANSI/VT parser adaptation and application of parsed actions to terminal state.
- Must not import: GPU renderers, platform/window APIs, config frontends, mux, SSH, shell integration visuals.
- Layer: core correctness.
- Tests required: escape sequence fixtures, golden terminal-state application, alternate screen, truecolor, mouse reporting, and parser error recovery.
