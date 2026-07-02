# semantics

- Owns: prompt, input, output, command-region, shell, and remote metadata contracts over terminal positions.
- Must not import: renderers, GPU APIs, platform/window APIs, transports, PTY/SSH implementations, config frontends.
- Layer: semantic meaning.
- Tests required: semantic region ordering, command status transitions, shell metadata mapping, and dependency-boundary tests.
