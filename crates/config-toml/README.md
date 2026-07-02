# config-toml

- Owns: static TOML config loading, parsing, schema-facing errors, and conversion into config-core.
- Must not import: renderers, platform implementations, PTY/SSH implementations, mux runtime, shell integration.
- Layer: config portability.
- Tests required: TOML fixtures, defaults merging, validation diagnostics, schema compatibility, and portable path handling.
