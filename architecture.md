# Architecture

> This document defines the soul, constraints, and architectural laws of the terminal.  
> It is not a roadmap, not an implementation checklist, and not a list of tickets.  
> All future design and implementation decisions must preserve the rules in this document.
>
> Operational feature routing, rollout order, testing, diagnostics, and release
> readiness rules live in [docs/engineering-rules.md](docs/engineering-rules.md).

## 1. Product Identity

This project is a real terminal emulator, not a terminal-themed application.

It exists to combine four things that are usually treated as tradeoffs:

1. **Speed** — GPU-first rendering, low latency, smooth scrolling, high throughput, and minimal idle cost.
2. **Cross-platform consistency** — macOS, Linux, and Windows should feel like the same product, not three ports with three personalities.
3. **Deep configurability** — users should be able to shape the terminal’s behavior, visuals, keybindings, workflows, and aesthetics without fighting the platform.
4. **Modern terminal semantics** — the terminal should understand prompts, commands, outputs, panes, sessions, shells, and remote contexts without breaking normal terminal compatibility.

The product should feel fast and serious by default, but personal and expressive when the user wants it to be.

The terminal should support traditional workflows, advanced shell users, heavy multiplexer users, aesthetic customization, and remote development. It should not force users to choose between power and performance.

## 2. Core Philosophy

### 2.1 Cross-platform first

Every major user-facing feature must be designed as a terminal feature first, not as a macOS feature, Linux feature, or Windows feature.

The internal implementation may differ per operating system, but the user-facing behavior, configuration model, and visual model should remain consistent.

A configuration created on Linux should load on Windows and macOS. A theme created on macOS should render the same way on Linux and Windows. A cursor animation created on Windows should behave the same way on macOS and Linux.

Where exact behavior is blocked by the operating system, compositor, shell, graphics backend, or system policy, the terminal must degrade explicitly and gracefully. Silent breakage is not acceptable.

### 2.2 Performance first, but not at the cost of expression

The default experience must be fast enough to compete with serious GPU-powered terminals.

Visual features should not poison the core render path. If a feature is disabled, its runtime cost should be zero or close to zero. If a feature is enabled, its cost should be proportional to what it actually does.

A static cursor should be essentially free. A smooth cursor animation should cost very little. A 24–30 FPS animated image cursor is allowed as an opt-in user choice, but it must be bounded, cached, measured, and as efficient as possible.

The terminal cannot break the laws of computing performance, but it must avoid wasting work.

### 2.3 Compatibility before novelty

The terminal must behave correctly as a normal terminal before it behaves beautifully as an enhanced terminal.

Standard shell programs, full-screen TUIs, editors, multiplexers, SSH sessions, command-line tools, and escape-sequence-heavy applications must not be broken by custom visuals.

A user should be able to disable every enhanced feature and still have a fast, correct, modern terminal.

### 2.4 Visual features are overlays, not fake terminal text

Prompt boxes, command blocks, cursor effects, input/output grouping, separators, shadows, glows, badges, and other visual styling must not corrupt the terminal buffer.

The terminal buffer stores what the shell or application emitted. The semantic layer stores meaning. The visual layer draws interpretation.

This rule protects copy/paste, scrollback, terminal applications, tmux/screen/zellij behavior, SSH sessions, and compatibility with existing tools.

### 2.5 The configuration file is a contract

Configuration is not an afterthought. It is one of the product’s primary interfaces.

The config system must be portable, predictable, validated, versioned, and understandable. Advanced users should have power, but normal users should not need to write complex scripts for basic customization.

The config model should support simple static configuration and deeper programmable configuration, but both must compile into one internal configuration model.

Platform overrides are allowed, but platform-specific config islands are not.

## 3. Non-negotiable Product Laws

These laws override convenience, short-term speed, and feature excitement.

1. **The terminal core must be platform-neutral.**  
   It must not depend on macOS, Linux, Windows, any windowing system, any shell, or any GPU backend.

2. **The renderer must be GPU-first.**  
   The architecture must assume GPU rendering from the beginning rather than treating acceleration as a later optimization.

3. **The same feature should exist on all desktop operating systems wherever technically possible.**  
   If a platform prevents exact behavior, the terminal must provide a fallback and report it clearly.

4. **Disabled features must not slow the terminal down.**  
   Optional visual systems, shell integrations, scripting hooks, and decorations must not create constant overhead when unused.

5. **Input and shell I/O have priority over visual effects.**  
   Typing, PTY output, resize handling, and terminal state updates must not wait for animations or decorations.

6. **Visual styling must never mutate raw terminal output.**  
   Decorations belong in renderer overlays and semantic metadata, not in the terminal’s raw cell buffer.

7. **Configuration must be portable.**  
   A valid config should not collapse when moved from Linux to Windows or macOS.

8. **Shell integration must enhance, not replace, terminal compatibility.**  
   The terminal should work normally without shell integration. Shell integration unlocks richer behavior.

9. **The product must expose performance tradeoffs honestly.**  
   If a user chooses an expensive effect, the terminal should make that cost visible and keep it as small as possible.

10. **No OS-specific feature islands.**  
    Platform-specific implementation is fine. Platform-specific product identity is not.

## 4. Major Product Surfaces

### 4.1 Desktop terminal

The primary product is a GPU-accelerated desktop terminal for:

- macOS
- Linux
- Windows

The desktop terminal must support local shells, remote shells, panes, tabs, configuration, themes, shell integration, command blocks, and normal terminal workflows.

### 4.2 iOS SSH companion

A future companion product may provide an iOS-first GPU-accelerated SSH client that uses the same terminal engine and visual model where possible.

The iOS product is not a replacement for a local desktop shell. It is a secure remote terminal surface centered on SSH.

It should share:

- terminal core behavior
- theme language
- visual identity
- command-block concepts where shell integration is available
- remote session semantics
- security philosophy

It may differ where iOS platform limits require it, especially around background execution, local shell access, file-system access, and mobile input behavior.

### 4.3 Future mobile surfaces

Android may be considered later, but the architecture should not assume that iOS is the only possible mobile surface.

Mobile support must not distort the desktop terminal’s architecture. The shared engine should make mobile possible without making desktop worse.

## 5. Architectural Layers

The product is divided into layers. Each layer owns a specific responsibility and must avoid leaking unnecessary assumptions into other layers.

### 5.1 Terminal Core

The terminal core is the pure terminal engine.

It owns:

- terminal grid state
- cursor state
- scrollback state
- selection model
- alternate screen handling
- terminal modes
- text attributes
- parser state
- raw cell data
- resize behavior
- terminal application compatibility

It does not own:

- windows
- tabs
- GPU devices
- operating-system APIs
- shell-specific behavior
- SSH authentication
- configuration scripting
- prompt decoration visuals

The terminal core must be deterministic, testable, and independent of the platform.

### 5.2 Semantic Layer

The semantic layer gives meaning to terminal regions without changing the underlying terminal buffer.

It owns:

- prompt regions
- command input regions
- command output regions
- command boundaries
- command exit status
- current working directory metadata
- shell identity metadata
- remote host metadata
- command duration metadata
- semantic navigation markers

This layer is powered primarily by shell integration. When shell integration is unavailable, limited heuristic behavior may be used, but it must be clearly treated as less reliable.

The semantic layer enables:

- command blocks
- input/output grouping
- prompt beautification
- jump-to-command navigation
- select-command-output behavior
- per-command styling
- command history visuals
- current-directory awareness
- shell-aware status information

The semantic layer must be optional. A normal terminal session must work without it.

### 5.3 Rendering and Visual Layer

The rendering layer is responsible for drawing the terminal efficiently and consistently.

It owns:

- GPU-backed text rendering
- glyph caching
- font fallback behavior
- cell rendering
- cursor rendering
- cursor animations
- prompt decorations
- command block decorations
- visual themes
- overlays
- selection visuals
- search highlight visuals
- frame scheduling
- damage tracking
- visual effects budgeting

The rendering layer must preserve the distinction between raw terminal content and visual presentation.

It must be designed so that:

- unchanged content is not repeatedly redrawn unnecessarily
- animations affect only the regions that actually animate
- visual effects can be disabled cleanly
- heavy assets are cached and bounded
- config scripts do not run inside the hot render path
- terminal output and input remain responsive under visual load

The renderer should draw the same visual language across macOS, Linux, and Windows.

### 5.4 Transport Layer

The transport layer moves bytes between the terminal and a session.

It owns:

- local PTY sessions
- Windows pseudoconsole sessions
- SSH sessions
- remote PTY sessions
- session resize propagation
- input byte writing
- output byte reading
- transport lifecycle
- reconnect-aware remote sessions where applicable

The transport layer must not know how rendering works. It only provides session I/O.

The terminal core should not care whether bytes came from a local shell, a remote SSH host, WSL, tmux, or another future transport.

### 5.5 Platform Layer

The platform layer adapts the product to macOS, Linux, and Windows.

It owns:

- native windows
- fullscreen behavior
- frameless windows
- titlebar/decorations behavior
- clipboard integration
- keyboard input translation
- mouse input translation
- IME and composed text handling
- DPI and scaling
- monitor handling
- file paths and config locations
- native notifications
- OS-specific permissions
- Linux X11/Wayland behavior
- Linux compositor fallbacks
- Windows shell and console quirks
- macOS application lifecycle quirks

The platform layer exists to hide OS differences from the rest of the system.

The application core asks for capabilities. The platform layer provides them or reports why it cannot.

### 5.6 Configuration Layer

The configuration layer is a product pillar, not a utility.

It owns:

- config schema
- defaults
- validation
- error reporting
- versioning
- migrations
- platform overrides
- theme loading
- keybinding definitions
- visual effect settings
- performance budgets
- shell profile definitions
- workspace/session defaults
- live reload behavior where safe
- import/compatibility helpers where practical

The config system should support both simple and advanced users.

The internal configuration model must be singular. Different file formats or scripting layers should resolve into the same internal representation.

Configuration must be portable across desktop operating systems. Platform overrides should be explicit and optional.

### 5.7 Multiplexer Layer

The multiplexer layer manages multiple terminal sessions inside the application.

It owns:

- tabs
- panes
- splits
- workspaces
- window/session structure
- layout restoration
- pane movement
- pane zooming
- local sessions
- remote sessions
- future remote domains
- session naming and metadata

The built-in multiplexer must coexist with external multiplexers such as tmux, screen, and zellij.

A user should be able to run a remote SSH session inside a pane, then run tmux inside that SSH session, without the terminal becoming confused.

### 5.8 Shell Integration Layer

The shell integration layer connects shells to the terminal’s semantic model.

It owns:

- shell startup hooks
- prompt boundary reporting
- command boundary reporting
- output boundary reporting
- exit status reporting
- current working directory reporting
- shell-specific metadata
- remote shell integration where available

It should support common shells across platforms, including PowerShell, bash, zsh, fish, and other major shells where practical.

Shell integration must be opt-in or auto-detected in a transparent way. It must not make the terminal unusable when absent.

### 5.9 Security Layer

Security is especially important for SSH and mobile usage.

It owns:

- SSH host identity verification
- known-host behavior
- private key handling
- secure secret storage
- passphrase handling
- authentication policy
- logging policy
- session data sensitivity
- clipboard risk warnings where appropriate
- agent forwarding defaults
- remote helper trust boundaries

The product should never normalize insecure defaults for convenience.

### 5.10 Diagnostics and Capability Layer

The diagnostics layer explains what the terminal is doing and why.

It owns:

- platform capability reporting
- renderer capability reporting
- GPU backend reporting
- config validation reporting
- Linux compositor/window-mode diagnostics
- shell integration status
- performance overlays
- expensive-effect warnings
- fallback explanations
- bug-report snapshots that avoid leaking secrets

The terminal should not fail mysteriously. If a feature falls back, the user should be able to find out why.

## 6. Cross-OS Compatibility Contract

The desktop product supports macOS, Linux, and Windows as first-class platforms.

The compatibility contract is:

1. **Same config schema across all desktop OSes.**  
   A config file should not need to be rewritten when moved between platforms.

2. **Same visual language across all desktop OSes.**  
   Themes, prompt decorations, cursor visuals, command blocks, fonts, padding, and colors should aim to render consistently.

3. **Same feature model across all desktop OSes.**  
   Features should not be designed for one OS and later patched onto others.

4. **Explicit platform overrides only when useful.**  
   Platform overrides are for refinement, not survival.

5. **Capability-based fallback behavior.**  
   When a platform, compositor, window manager, shell, or GPU backend prevents exact behavior, the terminal must fall back deliberately.

6. **No silent config collapse.**  
   Unknown, unsupported, or impossible settings should produce clear diagnostics.

7. **Linux means X11 and Wayland.**  
   Linux support must account for X11, Wayland, and major compositor behavior differences.

8. **Windows means PowerShell, cmd, WSL, and ConPTY realities.**  
   Windows support must not be treated as merely “Unix but different.”

9. **macOS means native application behavior, Retina scaling, and platform input expectations.**  
   macOS support must feel native without becoming a separate product.

Exact internal implementation may differ. User-facing behavior should not.

## 7. Linux Windowing and Compositor Philosophy

Linux desktop support must be treated seriously.

The product must account for:

- X11
- Wayland
- GNOME/Mutter
- KDE/KWin
- wlroots-based compositors
- Sway
- Hyprland
- COSMIC
- tiling window managers
- floating window managers
- server-side decorations
- client-side decorations
- fractional scaling
- compositor-specific fullscreen behavior

The product should provide fullscreen and frameless modes with fallbacks. If the compositor blocks or changes behavior, the terminal should explain what happened.

The product should prefer robust behavior over pretending every compositor behaves identically.

## 8. Performance Contract

Performance is a first-class architectural requirement.

The product should be designed around:

- low input latency
- fast output throughput
- smooth scrolling
- high refresh-rate friendliness
- low idle CPU/GPU usage
- efficient resize handling
- efficient glyph caching
- efficient rendering of large grids
- fast pane/tab switching
- efficient command block overlays
- efficient cursor animations
- bounded visual effects

### 8.1 Default performance

With default settings, the terminal should be fast, smooth, and competitive with current serious GPU-powered terminals.

Default visuals must not make the terminal feel heavy.

### 8.2 Optional visual cost

When the user enables heavier features, the terminal should keep the cost minimal and obvious.

Examples:

- static cursor: negligible cost
- basic cursor animation: tiny cost
- prompt decoration: small overlay cost
- command block styling: bounded semantic/overlay cost
- animated image cursor: opt-in cost
- large animated assets: warned and bounded

### 8.3 Performance isolation

Optional features must be isolated from the core path.

Visual effects should not:

- block input
- block PTY reads
- force full-screen redraws unnecessarily
- run scripting code every frame
- decode images on the render thread
- rewrite terminal state
- degrade inactive panes unnecessarily

### 8.4 User-controlled tradeoffs

Users should be able to choose a performance posture:

- maximum performance
- balanced
- visual/aesthetic
- battery-conscious

The terminal should expose enough diagnostics for users to understand the cost of expensive settings.

## 9. Configuration Philosophy

The configuration system should feel powerful but stable.

It should support:

- colors
- themes
- fonts
- font fallback
- ligatures
- cursor style
- cursor animation
- prompt decoration
- command block styling
- panes
- tabs
- keybindings
- mouse bindings
- shell profiles
- SSH profiles
- window behavior
- fullscreen behavior
- frameless behavior
- opacity/blur where possible
- padding and margins
- scrollback
- search
- clipboard behavior
- bell behavior
- URL hints
- shell integration behavior
- performance budgets
- platform overrides

The configuration model should be expressive enough to match the power users expect from advanced terminals, while preserving a simple path for normal users.

A good config system should not require users to understand the internal architecture.

## 10. Visual System Philosophy

The visual system is one of the product’s signatures.

It should support:

- custom cursor shapes
- custom static cursors
- cursor animations
- typing animations
- prompt beautification
- input/output grouping
- command blocks
- styled command headers
- styled command outputs
- success/error visual states
- duration badges
- current-directory badges
- shell badges
- remote-host badges
- theme-defined spacing
- theme-defined borders
- overlays and decorations

The visual system must remain separate from terminal content.

The user should be able to choose:

- a plain traditional terminal
- a lightly enhanced terminal
- a heavily themed terminal
- a command-block-oriented terminal
- a custom personal workflow

No visual mode should break correctness.

## 11. Command Blocks and Semantic I/O Grouping

Command blocks are a major product feature.

A command block groups:

- prompt
- user input
- command output
- exit status
- duration
- working directory
- shell or host metadata

This allows the terminal to present commands as meaningful units rather than only a raw stream of text.

Command blocks should support:

- per-command styling
- success/error styling
- output selection
- navigation between commands
- collapsible output where appropriate
- copy raw command output
- copy command plus output
- visual separators
- boxes or cards
- minimal separators
- fully disabled traditional mode

Command blocks must be powered by the semantic layer and rendered by the visual layer. They must not alter the raw terminal buffer.

When shell integration is unavailable, command blocks may fall back to limited heuristics, but the terminal should not pretend those heuristics are perfect.

## 12. Multiplexer Philosophy

The terminal should support both native multiplexing and external multiplexer compatibility.

Native multiplexing includes:

- tabs
- panes
- split layouts
- workspaces
- saved layouts
- local and remote sessions
- pane zooming
- pane movement
- session restoration

External compatibility includes:

- tmux
- screen
- zellij
- nested terminal sessions
- remote multiplexers over SSH

The native multiplexer should not fight external multiplexers. Users should be able to combine both.

The multiplexer model should understand that a pane contains a session, and a session may be local, remote, or future transport-backed.

## 13. SSH and Remote Philosophy

SSH is not just a command to run inside the terminal. It is also a first-class transport for remote sessions.

The architecture should support:

- local terminal sessions
- SSH terminal sessions
- remote shell integration where available
- remote current-directory awareness where available
- remote command blocks where available
- secure host verification
- secure secret storage
- reconnect-aware behavior where practical
- future mobile SSH usage

The terminal should remain compatible with normal SSH usage. Enhanced remote features should be additive.

## 14. Standard Terminal Baseline

The product should include the expected baseline of a serious modern terminal without requiring each common feature to be rediscovered individually.

The baseline includes:

- correct terminal emulation
- truecolor
- ANSI/VT compatibility
- Unicode support
- font fallback
- configurable fonts
- themes
- scrollback
- search
- copy/paste
- bracketed paste
- mouse support
- alternate screen support
- focus events
- cursor styles
- selection behavior
- URL detection and hints
- keyboard shortcuts
- custom keymaps
- shell profiles
- working-directory control
- tabs
- panes
- session restore
- clipboard integration
- OSC clipboard behavior where appropriate
- bell configuration
- window configuration
- fullscreen and frameless modes
- resize handling
- config reload behavior where safe
- diagnostics
- tmux/screen/zellij compatibility

This baseline is not the differentiator. It is the foundation.

## 15. Standard Compatibility vs Enhanced Experience

The terminal has two modes of value:

1. **Standard compatibility** — everything works like a serious terminal should.
2. **Enhanced experience** — shell integration, command blocks, prompt visuals, custom cursor animation, semantic navigation, and personal themes.

The enhanced experience must never be required for standard compatibility.

A user should be able to turn off the enhanced layer and still have a fast, correct terminal.

## 16. Failure and Fallback Philosophy

Failure should be understandable.

When something cannot work exactly as requested, the terminal should answer:

- what failed
- why it failed
- what fallback was used
- how to fix or override it if possible

Examples:

- a compositor rejected a window decoration mode
- a font is unavailable on this OS
- shell integration is not installed
- a GPU backend lacks a required capability
- an expensive visual effect exceeded a performance budget
- a config setting is valid but unsupported by the current platform state

The user should not have to guess.

## 17. Extensibility Philosophy

The terminal should be extensible, but extension points must be safe and performance-aware.

Preferred extension model:

- declarative themes
- declarative visual profiles
- configurable cursor profiles
- configurable command block styles
- safe programmable config
- controlled event hooks
- bounded visual assets

Risky extension model:

- arbitrary code in the render loop
- unbounded custom shaders by default
- plugins that can mutate the terminal buffer directly
- extensions that bypass platform compatibility rules
- extensions that silently add background cost

Extensibility should make the terminal more personal, not less stable.

## 18. Product Boundaries

This project should not become everything at once.

It is allowed to become powerful, but its power must remain centered on terminal work.

The product is not:

- a web browser
- an Electron shell
- a general desktop environment
- a terminal-themed dashboard first and terminal second
- a platform-specific experiment
- a toy renderer that becomes a terminal later
- a visual effects engine that happens to run shells

The product is:

- a fast terminal
- a cross-platform terminal
- a configurable terminal
- a semantic terminal
- a visually personal terminal
- a local and remote terminal surface
- a tool users can trust as a daily driver

## 19. Decision Framework

Every major feature should be judged by these questions:

1. Does it preserve standard terminal compatibility?
2. Can it work across macOS, Linux, and Windows?
3. If not perfectly, can it degrade clearly and gracefully?
4. Does it avoid slowing the terminal when disabled?
5. Is its enabled cost proportional and bounded?
6. Does it keep raw terminal output separate from visuals?
7. Does it fit the configuration model?
8. Does it improve the terminal experience rather than distract from it?
9. Can it coexist with panes, SSH, shell integration, and external multiplexers?
10. Can diagnostics explain failures or limitations?

If a feature violates these rules, it must be redesigned before being accepted.

## 20. Architectural Commitments

The current accepted commitments are:

- GPU-accelerated desktop terminal
- macOS, Linux, and Windows support
- strong cross-OS feature parity
- same configuration model across platforms
- simple and advanced configuration paths
- standard modern terminal baseline
- fast default performance
- performance-aware optional visuals
- custom cursors
- cursor animations
- prompt beautification
- command blocks
- input/output grouping
- user-customizable visual themes
- fullscreen support
- frameless/titlebarless window support
- Linux X11 and Wayland support
- Linux compositor fallback philosophy
- tabs, panes, sessions, and workspaces
- external multiplexer compatibility
- local PTY sessions
- SSH sessions
- future iOS SSH companion using shared terminal concepts
- diagnostics for platform, config, shell integration, and performance

These commitments define the product’s direction, not its implementation order.

## 21. Final Principle

This terminal should feel like one product everywhere.

Not a Linux terminal that sort of runs on Windows.  
Not a Windows terminal that sort of runs on macOS.  
Not a macOS app with Linux as an afterthought.

One terminal.  
One configuration philosophy.  
One visual language.  
One performance standard.  
Different platform backends hidden underneath.

The architecture exists to protect that promise.
