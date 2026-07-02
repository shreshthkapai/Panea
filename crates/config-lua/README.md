# config-lua

- Owns: advanced programmable config frontend and conversion into config-core.
- Must not import: renderer hot paths, terminal parser internals, PTY/SSH implementations, platform event loops.
- Layer: config portability and security.
- Tests required: sandbox policy, deterministic config output, error diagnostics, disabled-cost checks, and migration compatibility.
