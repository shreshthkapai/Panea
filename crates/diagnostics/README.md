# diagnostics

- Owns: doctor commands, capability reports, logs, renderer/platform/config/shell diagnostics, and performance overlays.
- Must not import: GPU hot-path internals in a way that affects disabled/default rendering, config frontends executing scripts, transport secrets.
- Layer: diagnostics.
- Tests required: diagnostic report formatting, fallback explanations, redaction, disabled-cost checks, and platform capability reporting.
