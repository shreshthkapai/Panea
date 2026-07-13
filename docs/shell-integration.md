# Shell Integration

Shell integration enhances semantic behavior but is never required for normal
terminal compatibility. A session without integration must still behave like a
plain terminal.

## Semantic Events

Panea tracks semantic events separately from terminal text:

- prompt start and end
- input start and end
- output start and end
- command finished with exit status and duration
- current working directory
- shell metadata
- remote metadata

Semantic regions reference buffer positions. They must not mutate, own, or
rewrite terminal content.

## Escape Sequences

The shell integration parser accepts these families:

- `OSC 133` prompt and command boundaries used by common shell integrations
- `OSC 633` prompt and command boundaries used by VS Code-style integrations
- `OSC 7` current working directory
- `OSC 777` Panea-private semantic metadata

The internal timeline model is stable even if supported escape sequence
frontends evolve.

## Supported Scripts

Initial hook scripts exist for:

- bash
- zsh
- fish
- PowerShell / pwsh

Later work may add nushell and limited cmd support.

## Activation

Configuration controls shell integration with:

- `shell_integration.enabled`
- `shell_integration.activation`
- `shell_integration.auto_install`
- `shell_integration.enabled_shells`
- `shell_integration.disabled_shell_profiles`
- `shell_integration.remote_instructions`

Activation modes:

- `full`: inject a runtime hook for supported local shells.
- `auto_detect` / `auto`: accept semantic escape sequences and inject only
  when `auto_install = true`.
- `manual`: do not inject; report install instructions.
- `heuristic`: reserve heuristic semantic mode without claiming shell
  integration accuracy.
- `disabled` / `off`: do not parse shell semantic events for the session.

Runtime activation is applied at session startup, before PTY spawn. The desktop
app maps the portable config into a `ShellIntegrationPolicy`, asks the
`shell-integration` crate for an activation plan, and then applies
backend-specific startup mechanics behind the local transport profile:

- bash uses a temporary init file.
- zsh uses a temporary `ZDOTDIR`.
- fish uses startup command execution.
- PowerShell / pwsh uses `-NoExit -Command`.
- cmd and unsupported shells run without hooks and produce diagnostics.

Markers are applied incrementally at their byte offset in each transport
batch. This keeps prompt, input, and output regions attached to the actual
scrollback positions even when a shell emits visible output and several OSC
markers in one read. Command duration is measured by the semantic timeline
when the shell does not provide an explicit duration.

Runtime actions are wired to the active pane:

- `jump_to_previous_command`
- `jump_to_next_command`
- `select_current_command_output`
- `copy_current_command_output`
- `copy_command_and_output`

Selection and copy use raw terminal text. Semantic metadata never rewrites the
terminal buffer.

Windows PowerShell 5 does not reliably preserve trailing OSC markers returned
from `prompt`. Panea therefore emits its prompt-end/input-start marker directly
before the visible prompt on that backend and reports
`prompt_marker=fallback_prefix` in shell metadata. Command output boundaries,
exit status, duration, cwd, navigation, and output selection remain reliable;
PowerShell 7 can use the same portable protocol. Panea only wraps the default
PSReadLine `AcceptLine` Enter binding; a custom Enter handler is preserved and
reported as prompt-only semantic integration.

The terminal must continue working when shell integration is disabled,
unsupported, or not installed on a remote host.

## Diagnostics

Diagnostics should report:

- detected shell
- active or inactive integration
- last semantic event
- command block confidence
- heuristic mode
- remote integration status

## Phase 11 Design Note

Feature name: Shell integration activation and verification

Layer: semantic meaning, with session-transport startup wiring in the desktop
app.

User-facing behavior: supported local shells can emit prompt/input/output,
current-directory, shell metadata, exit-status, and duration events without
mutating terminal text. Missing integration falls back to auto/manual/heuristic
or off modes.

Config keys: `shell_integration.enabled`,
`shell_integration.activation`, `shell_integration.auto_install`,
`shell_integration.enabled_shells`,
`shell_integration.disabled_shell_profiles`,
`shell_integration.remote_instructions`.

macOS behavior: bash, zsh, and fish use the same runtime activation contract;
real host verification is still required.

Windows behavior: PowerShell / pwsh activation is wired through the same
contract; the bounded real PowerShell semantic smoke passed on the current
Windows host. cmd remains basic/no-hook mode.

Linux X11 behavior: shell activation is independent of the window backend;
real X11 host shell verification is still required.

Linux Wayland behavior: shell activation is independent of the compositor;
real Wayland host shell verification is still required.

Fallback behavior: unsupported shells, disabled profiles, manual mode, explicit
args that block safe injection, and disabled config all avoid injection and
report diagnostics.

Diagnostics: activation plans carry status messages; semantic diagnostics
report detected shell, last event, active/inactive state, confidence, heuristic
mode, and remote status.

Performance cost when disabled: no shell hook injection and no semantic escape
parsing for `off`.

Performance cost when enabled: startup-only profile shaping plus incremental,
bounded OSC parsing per PTY output batch. Disabled sessions do not run the
semantic parser.

Tests: unit tests cover activation plans, complete marker sets, streaming
marker offsets, shell detection, desktop profile shaping, semantic navigation,
raw output selection/copy, disabled/off behavior, and explicit-args fallback.
Bounded real-shell tests exist for PowerShell, bash, zsh, and fish; only shells
actually available on a host can be verified there.
