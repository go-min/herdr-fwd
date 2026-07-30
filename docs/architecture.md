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
authentication, display sanitization, API models, and the forward registry
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
failure. Dashboard surfaces use a fixed ANSI color palette, white-on-dark-gray
selection, and dim/bold attributes; exact shades follow the terminal palette
and no private theme files are parsed. The automatic dashboard is session-owned
and remains available through an empty registry. Additional copies use the
declared plugin pane entrypoint or the context-aware action that starts the
dashboard in `HERDR_PANE_ID`. `After forwarding` is a per-user preference that
defaults to `SPACE` and can also select `POPUP` or `NOTHING`. All copies exit,
and the automatic workspace closes, when the remote-forward session disappears.

The wrapper writes a `0600` session file under the remote user's
`~/.cache/herdr-fwd/`; it contains only an ephemeral RPC URL
and cryptographically random token. The token is never passed in process argv.
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
local user's `~/.config/herdr-fwd/`, keyed by the resolved remote hostname. They
survive reconnects to the same machine without being shared across different
hosts. Ephemeral session tokens and active-forward state remain session-scoped.

The remote watcher holds a per-user file lock, subscribes to pane updates and
lifecycle events with a 15-second reconciliation fallback, and associates each
`lsof` listener with its foreground process. One process may therefore produce
multiple forwards. It sends a two-second heartbeat and removes stale sessions
after three failed RPC checks. The local companion expires its lease after ten
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
GitHub installation flow pinned to the wrapper's exact version tag. The
manifest build hook downloads a native release archive, verifies `SHA256SUMS`,
and installs only the plugin binary; production hosts do not need a Rust
toolchain. Development checkouts continue to use `plugin link` and local Cargo
builds.
Update and uninstall refuse to run while a remote-forward session file exists,
preventing replacement of the watcher that currently owns lifecycle cleanup.

For repeatable manual testing, the remote host is a Lima Linux VM named
`herdr-test`. Lima exposes it through the generated SSH alias
`lima-herdr-test`; setup and the end-to-end scenario are documented in
[`docs/testing.md`](testing.md).

The RPC contract carries an explicit protocol version and rejects incompatible
session files. Requests, responses, metadata fields, and the registry are
bounded. Untrusted process labels and errors are stripped of terminal control
sequences before dashboard rendering.
