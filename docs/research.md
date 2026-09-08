# Herdr research

Research refreshed: 2026-09-08. Source checked: Herdr v0.9.0 source, bundled
documentation, and an isolated local Herdr 0.9.0 server. This repository does not modify Herdr core.

| Question | Verified answer | Evidence |
| --- | --- | --- |
| Remote command | `herdr --remote <ssh-target>`; named sessions add `--session <name>`. | [CLI reference](https://herdr.dev/docs/cli-reference/) |
| SSH transport | OpenSSH `ssh`; Herdr creates a temporary config and private per-attach `ControlPath` when `remote.manage_ssh_config` is enabled. | [`src/remote/attach.rs`](https://github.com/herdrdev/herdr/blob/v0.9.0/src/remote/attach.rs) (`RemoteSsh`, `write_managed_ssh_config`, `bridge_connection`) |
| `remote.manage_ssh_config` | Enabled by default; it includes the user SSH config first, adds keepalive fallbacks, and reuses one private ControlMaster. False uses plain SSH. | [configuration](https://herdr.dev/docs/configuration/), [`src/main.rs`](https://github.com/herdrdev/herdr/blob/v0.9.0/src/main.rs) |
| Plugin model | Ordinary out-of-process commands declared by `herdr-plugin.toml`; no SDK or sandbox. Use `HERDR_BIN_PATH` for the CLI. | [plugin docs](https://herdr.dev/docs/plugins/) |
| Remote placement | Plugin hooks are launched by the Herdr server that owns the panes. In remote mode this is the remote host; no copy runs automatically on the local attach client. | [plugin runtime environment](https://herdr.dev/docs/plugins/) |
| Lifecycle | `[[startup]]` and `[[events]]` are one-shot hooks. Startup does not run on client attach, so the wrapper invokes the manifest `wake` action on an already-running server. `pane.created`, `pane.closed`, and `pane.exited` are valid hooks. | [plugin startup/actions docs](https://github.com/herdrdev/herdr/blob/v0.9.0/docs/next/website/src/content/docs/plugins.mdx#startup-hooks), [`events.rs`](https://github.com/herdrdev/herdr/blob/v0.9.0/src/api/schema/events.rs) |
| `pane.updated` | Available to socket subscriptions for pane metadata updates, not every output byte. Raw output changes are not a generic public subscription; quiet listener starts may need the reconciliation fallback. | [`events.rs`](https://github.com/herdrdev/herdr/blob/v0.9.0/src/api/schema/events.rs), `plugin_hook_event_names()` |
| Socket subscriptions | The public socket API supports event subscriptions, including high-volume events that are not manifest hooks. Subscriptions start at live events, without replay. The watcher subscribes before its initial snapshot, rescans after reconnect, and retains a 15-second reconciliation fallback. | [socket API](https://herdr.dev/docs/socket-api/), [`events.rs`](https://github.com/herdrdev/herdr/blob/v0.9.0/src/api/schema/events.rs) |
| Named-session sockets | Plugins receive `HERDR_SOCKET_PATH`. Named sessions use `~/.config/herdr/sessions/<name>/herdr.sock`; the default uses `~/.config/herdr/herdr.sock`. | [socket API](https://herdr.dev/docs/socket-api/) |
| Pane payloads | `pane.list` returns pane records and ids; `pane.process-info` returns foreground process metadata. `pane.read --source recent-unwrapped` and `pane.wait-for-output` are available but are not used for listener discovery. The CLI wraps structured responses in its JSON result envelope. | [socket API](https://herdr.dev/docs/socket-api/), [`panes.rs`](https://github.com/herdrdev/herdr/blob/v0.9.0/src/api/schema/panes.rs) |
| Attached client/API gaps | Plugin runtime context can identify workspace/tab/pane, but exposes no stable attached remote-client identity, local-client action channel, browser opener, or port-forward API. | [documented plugin context](https://github.com/herdrdev/herdr/blob/v0.9.0/docs/next/website/src/content/docs/plugins.mdx#commands-and-environment), [API schema](https://github.com/herdrdev/herdr/tree/v0.9.0/src/api/schema) |
| Notifications | `notification show` is a public Herdr CLI action. The server emits semantic notifications; each client applies its own toast configuration. | [CLI reference](https://herdr.dev/docs/cli-reference/) |
| Sidebar metadata | `workspace.report_metadata` can publish source-owned tokens; configured Space sidebar rows can render `$port_forward_status`. When an explicit layout is absent, `herdr --default-config` exposes the current binary's default rows for safe extension. | [socket API](https://herdr.dev/docs/socket-api/), [configuration](https://herdr.dev/docs/configuration/), [config reference](https://herdr.dev/docs/config-reference/) |
| Installation lifecycle | `plugin install owner/repo --yes` creates a Herdr-managed GitHub checkout and runs manifest builds; reinstall replaces a managed checkout, while linked development plugins must be unlinked explicitly. `plugin uninstall` removes only the managed checkout. | [current CLI reference](https://herdr.dev/docs/cli-reference/#plugins) |

## Implementation consequence

The remote watcher runs on the Herdr server and reacts to pane socket events,
with a 15-second fallback scan. It reads foreground process PIDs from
`pane.process_info`, uses `lsof` to extract loopback TCP listeners, and POSTs
them to the local companion through a reverse SSH forward. A single foreground
process may own multiple listeners and therefore create multiple forwards.
Lifecycle hooks trigger rescans. The wrapper owns a uniquely named
ControlMaster because Herdr's private ControlPath is an internal detail and is
not exposed to plugins.

With keychain/ssh-agent authentication, both connections reuse the loaded key
and do not ask for credentials per port. Password-only SSH may prompt once for
Herdr's bridge and once for the companion master; this is a current Herdr
extension-surface limitation, not a hidden private-socket integration.

No TCP port-forwarding implementation was found in Herdr core or the official
plugin example collection as of the research date. The closest extension
surface remains ordinary manifest actions and socket/CLI calls.
[Official examples](https://github.com/ogulcancelik/herdr-plugin-examples) and
[Herdr source](https://github.com/herdrdev/herdr) were checked; this is a
repository-search conclusion, not a compatibility guarantee for third-party
plugins published later.

## Herdr 0.9 migration findings

- The manifest minimum is 0.9.0; the wrapper accepts only 0.9.x and rejects
  the next minor line until its contracts have been checked.
- [Client configuration ownership](https://github.com/herdrdev/herdr/blob/v0.9.0/docs/next/website/src/content/docs/configuration.mdx)
  moved sidebar layout and presentation settings to the connecting client.
  Dashboard settings therefore use the companion, while plugin command bindings
  remain on the server.
- [Public focus requests](https://github.com/herdrdev/herdr/blob/v0.9.0/src/server/headless/client_views.rs)
  focus all attached shell clients. There is still no public plugin attach ID
  for scoping our focus action or forwards to a specific Herdr client.
- `herdr machine` adds multi-machine navigation, but does not expose the
  loopback forwarding API required to replace the companion. Only the existing
  `hfwd <target>` attach path is integrated here.
- The local smoke check exercised real pane events, process inspection,
  loopback listener discovery, metadata writes, plugin linking, popup open/close,
  split dashboards, and removal cleanup with a mock HTTP companion. It did not
  validate independent multi-client UI interaction. The subsequent
  `make test-ssh` fixture additionally validates real loopback SSH attach,
  delayed discovery, transport recovery, port conflicts, multiple connectors,
  and shutdown cleanup.
