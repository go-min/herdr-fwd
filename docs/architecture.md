# Architecture

The project uses Rust with small, focused dependencies for typed JSON,
loopback HTTP serving, signal handling, and watcher locking. Private temporary
directories are implemented with the standard library and OS randomness,
avoiding a platform-heavy transient dependency graph. Crossterm provides
portable raw keyboard input, alternate-screen cleanup, and ANSI-palette
styling for the remote dashboard. It builds one
local wrapper/companion binary and one remote watcher binary. Herdr remains
unchanged.

Source code is split by runtime responsibility. The reusable library keeps
authentication, display sanitization, API models, Herdr configuration edits,
and the forward registry
separate. The local binary is assembled from SSH transport, companion HTTP
handling, session CLI/orchestration, platform support, and management-command
units. The remote binary similarly separates lifecycle discovery, dashboard
input and rendering, notifications, RPC, Herdr adaptation, and session-file
handling. This keeps protocol/state logic testable without coupling it to
either CLI.

```text
local wrapper
  ├─ loopback HTTP companion (Bearer session token)
  ├─ owned OpenSSH ControlMaster
  │    ├─ reverse RPC: remote loopback -> local companion
  │    └─ dynamic -O forward/cancel for discovered ports
  └─ herdr --remote target

remote Herdr server
  └─ plugin watcher: pane list/process-info + lsof -> HTTP POST
```

When a listener is detected, the remote plugin asks the local companion to
create a forward immediately. The configured `SPACE`, `POPUP`, or `NOTHING`
destination controls the dashboard. A created workspace is labeled `Port
Forwarding` and runs the dashboard binary in its root pane. Its
`$port_forward_status` metadata token reports `<active> active` and adds
`· <paused> paused` only when that sidebar row is enabled. The dashboard reads
the companion's authenticated forward API. Keyboard selection sends a
`pane.focus` request over the remote Herdr socket; browser actions ask
the local companion to open the corresponding loopback URL. The dashboard can
pause/resume entries, retarget a mapping's local port, and create manual
mappings, but the companion still owns validation and SSH execution.
Local-port changes use cancel/forward with rollback to the original mapping on
failure. Dashboard surfaces select one semantic light or dark ANSI palette from
the public Herdr theme name, with `COLORFGBG` as a terminal fallback. Selection
uses background contrast without changing the foreground. The automatic dashboard is session-owned
and remains available through an empty registry. Additional copies use the
declared plugin pane entrypoint or the context-aware action that starts the
dashboard in `HERDR_PANE_ID`. `After forwarding` is a per-user preference that
defaults to `SPACE` and can also select `POPUP` or `NOTHING`. All copies exit,
and the automatic workspace closes, when the remote-forward session disappears.

The wrapper writes a `0600` session file under the remote user's
`~/.cache/herdr-fwd/sessions/<herdr-session>/`; it contains the Herdr session
identity plus an ephemeral RPC URL and cryptographically random token. The token is never passed in process argv.
The file is removed on shutdown. The companion validates the token,
allows only loopback remote hosts and TCP ports, binds only to loopback, and
deduplicates by remote host/port.

OpenSSH control sockets live in a cryptographically random `0700`
`/tmp/herdr-fwd-*` directory with a one-character socket name. This keeps the
path below Unix-domain limits even when macOS exposes a long per-user
`TMPDIR`; the directory is removed by the wrapper's ownership guard. Local
management state also records the wrapper PID. A transient companion RPC
failure never deletes that state while its owner is alive; unreachable state
is pruned only after the owner has exited.

Manual mappings and paused automatic ports are persisted separately under the
local user's `~/.config/herdr-fwd/`, keyed by both the resolved remote hostname
and Herdr session. They survive reconnects to the same target session without
being shared across hosts or named sessions. Ephemeral tokens and active state
remain attach-scoped.

The remote watcher holds a lock per Herdr socket/session, subscribes to pane updates and
lifecycle events with a 15-second reconciliation fallback, and associates each
batched `lsof` listener with its foreground process, merging Linux `ss` results
when available. One process may therefore produce multiple forwards. An
independent worker sends a two-second heartbeat; only repeated heartbeat
failures remove a stale session, so slow discovery cannot expire a healthy
attach. The local companion expires its lease after ten
seconds, terminates the attach, retries forward cleanup, closes its owned SSH
master, and removes local runtime state. The SSH master remains an owned child
process rather than a daemonized `ssh -f`; the wrapper handles SIGINT, SIGTERM,
and SIGHUP and has a kill fallback after the normal OpenSSH exit request. Its
primary SSH session is held by a parent-owned stdin pipe, so even SIGKILL closes
that pipe and causes `ControlPersist=no` to tear down all wrapper-owned
forwards.
Because Herdr startup hooks do not run on client attach, the wrapper also
best-effort invokes the public manifest `wake` action when attaching to an
already-running server; a newly started server discovers the session through
the normal startup hook.

The installed local wrapper also owns remote plugin operations. Before every
attach, it reuses its private ControlMaster to verify that target's plugin and
installs or updates it only when needed. Its explicit
`remote install/update/status/uninstall` commands run only fixed Herdr plugin
commands over normal SSH: the API never accepts a repository name, plugin id,
or remote shell fragment from the caller. Managed installs use Herdr's public
GitHub installation flow pinned to the wrapper's exact version tag. If that
exact release is unreachable remotely, the local wrapper detects the remote
OS/architecture, downloads and verifies the corresponding archive and
`SHA256SUMS`, and atomically activates a minimal remote bundle. A verified,
versioned local cache is used only when GitHub is unavailable locally too.
Integrity or missing-release errors fail closed and never consume an old cache.
Production hosts do not need a Rust toolchain. Development checkouts continue
to use `plugin link` and local Cargo builds.
Update and uninstall refuse to run while a remote-forward session file exists,
preventing replacement of the watcher that currently owns lifecycle cleanup.

For repeatable manual testing, the remote host is a Lima Linux VM named
`herdr-test`. Lima exposes it through the generated SSH alias
`lima-herdr-test`; setup and the end-to-end scenario are documented in
[`docs/testing.md`](testing.md).

The RPC contract carries an explicit protocol version and Herdr session scope,
and rejects incompatible or cross-session files. Requests, responses, metadata fields, and the registry are
bounded. Untrusted process labels and errors are stripped of terminal control
sequences before dashboard rendering.

## Herdr 0.9 client configuration

The remote dashboard reads local presentation settings with authenticated
`GET /v1/settings/local`. `POST /v1/settings/sidebar` accepts only an `enabled`
boolean; `POST /v1/settings/notifications` enables Herdr toast delivery. The
companion chooses its own config path; requests cannot supply a path or an
arbitrary configuration patch. Shared TOML transformations preserve unrelated
settings, with a stable file lock and atomic replacement for concurrent writers.
Plugin shortcut bindings remain server-side. Client presentation reload is
performed from Herdr's menu, since the public server reload API is insufficient.
