# shell-integration

- Owns: shell hook contracts, OSC semantic events, installer helpers, and shell metadata extraction.
- Must not import: GPU renderers, platform window backends, mux layout internals, PTY/SSH implementation details.
- Layer: semantic meaning.
- Tests required: OSC parsing fixtures, shell hook generation, opt-in/opt-out behavior, and semantic event mapping.
