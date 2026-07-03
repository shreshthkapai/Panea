# Conformance

Terminal behavior fixtures and golden tests live here as the project grows.

Correctness comes before visual features.

## Compatibility Smoke Matrix

These app-level checks are required before the baseline compatibility phase can
be called product-complete across all targets.

| Application | Current status |
| --- | --- |
| bash | Optional `cargo xtask compat` probe; host-dependent |
| zsh | Optional `cargo xtask compat` probe; host-dependent |
| fish | Optional `cargo xtask compat` probe; host-dependent |
| PowerShell | Required Windows `cargo xtask compat` PTY smoke passed on current host |
| cmd | Required Windows `cargo xtask compat` PTY smoke passed on current host |
| vim/neovim | Optional version probe; full-screen behavior remains manual |
| emacs terminal mode | Manual verification required |
| less | Manual verification required |
| man | Manual verification required |
| htop/btop-style TUI | Optional version probe; interactive TUI behavior remains manual |
| fzf | Optional version probe; interactive behavior remains manual |
| ripgrep output | Manual or future fixture required |
| git log/diff | Optional git probe; pager/diff behavior remains manual |
| tmux | Optional version probe; nested session behavior remains manual |
| screen | Optional version probe; nested session behavior remains manual |
| zellij | Optional version probe; nested session behavior remains manual |
| local SSH host | Use `cargo xtask ssh-smoke run ...`; collected platform reports remain required |

The current automated coverage is lower-level golden testing in `term-parser`
and `term-core`, plus ignored real local PTY smoke tests in `transport-pty`.
App-level smoke automation now exists through `cargo xtask compat`, but it must
run per platform before parity claims are made. Fixture scripts live under
`tools/conformance/compat/`.
