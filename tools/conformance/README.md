# Conformance

Terminal behavior fixtures and golden tests live here as the project grows.

Correctness comes before visual features.

## Compatibility Smoke Matrix

These app-level checks are required before the baseline compatibility phase can
be called product-complete across all targets.

| Application | Current status |
| --- | --- |
| bash | Not yet verified |
| zsh | Not yet verified |
| fish | Not yet verified |
| PowerShell | Not yet verified in Phase 6 app suite |
| cmd | Not yet verified in Phase 6 app suite |
| vim/neovim | Not yet verified |
| emacs terminal mode | Not yet verified |
| less | Not yet verified |
| man | Not yet verified |
| htop/btop-style TUI | Not yet verified |
| fzf | Not yet verified |
| ripgrep output | Not yet verified |
| git log/diff | Not yet verified |
| tmux | Not yet verified |
| screen | Not yet verified |
| zellij | Not yet verified |
| local SSH host | Not yet verified |

The current automated coverage is lower-level golden testing in `term-parser`
and `term-core`, plus ignored real local PTY smoke tests in `transport-pty`.
App-level smoke automation must run per platform before parity claims are made.
