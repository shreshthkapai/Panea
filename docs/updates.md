# Downloads and Updates

Panea publishes immutable, versioned artifacts through GitHub Releases. The
release tag, Cargo workspace version, binary version, and package metadata must
agree exactly.

Panea uses normal semantic versions such as `v0.1.0` and `v0.1.1`. The planned
update implementation has one `stable` channel. Additional release channels are
intentionally deferred until there is a demonstrated need for them.

## Current Implementation Status

GitHub Release publication, native platform packages, SHA-256 manifests, and
GitHub provenance attestations are implemented. Panea does not yet implement
background update checks, the `panea update` command family, automatic install,
or `panea-update-v1.json`. Users currently update explicitly by downloading a
newer immutable release and running the platform installer or replacing their
portable installation.

The `v0.1.0` and `v0.1.1` Windows and macOS artifacts are intentionally
unsigned while the project has no release certificates. GitHub Release notes
identify that state; SmartScreen and Gatekeeper warnings are expected.
Checksums and attestations provide integrity and build provenance, but they are
not substitutes for an operating-system publisher signature. The release
workflow requires signing by default after the temporary repository policy is
removed.

The remaining sections define the updater's future security and UX contract;
they must not be read as claiming that the updater is currently available.

## User Contract

Panea may check for updates in the background at a bounded interval, outside
the render, input, parser, PTY, and SSH hot paths. An available update is
reported through a non-blocking notification and diagnostics.

Panea never downloads, installs, closes sessions, or restarts without explicit
user approval. Before installation, Panea shows:

- the installed and available versions;
- a link to the release notes;
- the artifact and target platform selected;
- whether integrity and platform signature checks passed;
- whether local or remote sessions must close;
- the installation action that will occur.

Rejecting or dismissing an update leaves the running application unchanged.
Network, manifest, download, verification, and installer failures never block
terminal startup or damage the currently installed version.

The command-line interface is:

```text
panea --version
panea update check
panea update download
panea update install
panea doctor update
```

`panea update install` is interactive unless an explicit future automation
contract defines an equally safe confirmation mechanism. There is no silent or
unattended installation mode in the initial implementation.

## Download Sources

GitHub Releases is the canonical public download source. The README and install
guide link to the latest release while the update manifest uses immutable,
version-specific asset URLs.

Every release contains the formats supported by its native release runners:

```text
Windows: installer executable and portable ZIP
macOS: DMG and application ZIP
Linux: AppImage, portable tarball, DEB, and RPM
```

The release also contains `SHA256SUMS.txt`, release notes, license files where
the package format requires them, and GitHub artifact attestations. Native
package jobs verify checksums, embedded versions, contents, and bounded launch
smokes before publication. Until an automated public-asset verifier is added,
the maintainer downloads and checks the published asset set after release.

## Update Manifest

`panea-update-v1.json` is a versioned, machine-readable release index. It
contains:

```text
schema version
Panea version and Git tag
publication time
release-notes URL
minimum supported updater version
per-target artifact URL, format, size, and SHA-256 digest
platform signing and provenance metadata
```

Unknown manifest schema versions, mismatched versions or tags, duplicate target
entries, insecure URLs, unsupported targets, invalid sizes, and malformed
digests are rejected. Target selection is deterministic and includes operating
system, architecture, package format, and installation ownership.

## Architecture

Update behavior is isolated from terminal operation:

```text
update-core
  manifest parsing, semantic-version comparison, target selection,
  update state machine, download limits, and digest verification contracts

update-platform
  operating-system cache locations, package ownership detection,
  installer handoff, staged replacement, and restart behavior

apps/desktop
  CLI commands, notification and confirmation UI, session coordination,
  and application shutdown/restart orchestration

tools/xtask
  release manifest generation, checksums, package metadata validation,
  publication gates, and public-asset verification

security
  signature, publisher identity, and provenance verification helpers

diagnostics
  update availability, source, selected artifact, verification state,
  last error, and platform fallback reporting
```

`update-core` must not depend on windowing, rendering, terminal state, PTY, SSH,
or operating-system APIs. Platform-specific mechanics remain behind a stable
provider contract. Update work runs on a bounded worker and communicates with
the desktop application through events; it never executes in a frame, input,
parser, or transport callback.

The update lifecycle is explicit:

```text
Idle -> Checking -> Available -> Downloading -> Verifying -> Ready
Ready -> AwaitingApproval -> Installing -> RestartPending
any active state -> Failed -> Idle
```

Cancellation is supported during checking and downloading. Verification and
installation failures retain the current installation and produce actionable
diagnostics.

## Platform Installation Behavior

### Windows

The updater downloads and verifies the signed Panea installer. After explicit
approval, the desktop application closes sessions cleanly, exits, and launches
the installer. The existing installer performs a staged per-user replacement
with rollback when activation fails, then restarts Panea when requested.

### macOS

The updater accepts only a correctly signed and notarized Panea artifact for a
public release. After approval it hands off to the platform update package. App
replacement occurs only after Panea exits; failure leaves the installed bundle
unchanged.

### Linux

Package ownership controls the update path:

- DEB and RPM installations are updated by their package manager; Panea reports
  the available version and the appropriate handoff rather than overwriting
  managed files.
- AppImage and portable installations may use a verified staged replacement
  helper after Panea exits.
- Unknown or read-only installation layouts receive explicit manual download
  instructions.

X11 and Wayland use the same update contract. The active display backend does
not change artifact selection or trust policy.

## Security and Trust

- Tagged release jobs fail when required Windows or macOS signing credentials
  are unavailable unless maintainers explicitly set the temporary repository
  policy `PANEA_ALLOW_UNSIGNED_RELEASES=true`.
- Assets are downloaded only over HTTPS from the configured official release
  origin.
- SHA-256 verification is mandatory before installer handoff.
- Platform signatures and publisher identity are verified where available.
- GitHub artifact attestations are generated and checked by release automation.
- A checksum obtained beside an artifact is an integrity check, not by itself a
  complete defense against publisher-account compromise.
- Downloaded files use restrictive permissions where the platform supports
  them and are removed after cancellation or failed verification.
- Update logs never contain terminal contents, environment variables, SSH
  secrets, clipboard data, authentication material, or private configuration.
- Panea never replaces a running executable in place or silently terminates
  active local or remote sessions.

## Configuration

Update settings are portable and conservative:

```toml
[updates]
enabled = true
check_on_startup = true
check_interval_hours = 24
notify = true
```

These settings control checks and notifications only. They do not grant
permission to download or install. Platform overrides are unnecessary for
normal update behavior.

## Diagnostics

`panea doctor update` reports:

```text
installed version
update manifest source
last successful check
available version
selected target and package format
installation ownership
download and verification status
platform signature/provenance status
last bounded failure and fallback
```

Diagnostics distinguish unavailable networking, invalid metadata, unsupported
platforms, package-manager ownership, signature failures, and installer launch
failures. They do not claim that an unverified artifact is safe.

## Release Contract

Release tags and assets are immutable. Maintainers never move a published tag
or silently replace an asset. A correction receives a new semantic version.

A release can be published only when:

1. the Cargo version, tag, binary version, and package metadata agree;
2. native package jobs build and pass their required tests and package smokes;
3. release artifacts satisfy signing policy;
4. checksums and attestations are generated;
5. public assets are downloaded and independently re-verified after
   publication, manually until the post-release verifier is automated.

The release workflow reports unsupported or unverified platforms honestly. It
does not turn a Windows-only result into a cross-platform claim.

## Test Contract

Required automated coverage includes:

- manifest parsing, schema rejection, and target selection;
- semantic-version comparison and no-downgrade behavior;
- size limits, cancellation, timeout, and partial-download cleanup;
- checksum and signature success and failure paths;
- explicit-approval state transitions;
- package ownership and unsupported-layout fallbacks;
- installer handoff using fake platform providers;
- release version-consistency checks;
- public-asset checksum, version, content, and launch verification.

Native manual verification remains required for installer UI, application
restart, operating-system trust presentation, and preserving the previous
installation after an induced update failure.
