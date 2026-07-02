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

