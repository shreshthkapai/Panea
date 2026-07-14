# Native Notifications

## Design Note

Feature name: native session notifications

Layer: platform parity, diagnostics

User-facing behavior: when enabled, Panea can notify the user when a background
local or SSH session exits or encounters a transport error. Notifications do
not include terminal contents, command output, clipboard contents, or secrets.

Config keys: `notifications.enabled`, `notifications.only_when_unfocused`,
`notifications.session_closed`, and `notifications.transport_errors`.

macOS behavior: delivery uses the native Notification Center backend exposed by
the desktop provider. OS notification permissions may deny delivery.

Windows behavior: delivery uses Windows toast notifications. Portable and
installed builds use the same provider contract; OS registration or policy
failures are surfaced through diagnostics.

Linux X11 behavior: delivery uses the freedesktop notification protocol over
D-Bus. A desktop notification service must be running.

Linux Wayland behavior: delivery uses the same freedesktop D-Bus protocol; it
does not depend on compositor-specific window APIs.

Fallback behavior: unsupported, permission-denied, missing-service, full-queue,
and stopped-worker states return explicit diagnostics. Panea never blocks a PTY
or renderer while waiting for notification delivery.

Diagnostics: `panea doctor notifications` reports configured behavior, backend,
availability, and the most recent provider state. Runtime failures are logged
without terminal or secret data.

Performance cost when disabled: no worker thread, queue, native call, polling,
or per-frame work.

Performance cost when enabled: no per-frame or per-output-batch native work. A
bounded worker and queue are created lazily on the first qualifying event.

Tests: provider contract bounds, disabled fast path, backend selection,
background-only session transition routing, config/TOML/programmable overrides,
live reload, and doctor fallback reporting.

## Defaults

```toml
[notifications]
enabled = true
only_when_unfocused = true
session_closed = true
transport_errors = true
```

These defaults avoid notifications while the user is already looking at Panea.
The provider queue is bounded; excess events are dropped with a diagnostic
instead of delaying input or session I/O.
