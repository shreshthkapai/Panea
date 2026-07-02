# config-core

- Owns: portable internal configuration model, defaults, validation contracts, and migration targets.
- Must not import: renderers, platform implementations, PTY/SSH implementations, terminal parser internals, Lua/TOML frontend code.
- Layer: config portability.
- Tests required: default values, serde round trips, validation behavior, migration invariants, and dependency-boundary tests.
