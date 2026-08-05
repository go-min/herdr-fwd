# Plugin and dashboard guide

This guide covers the Herdr Fwd plugin as a user sees it. Development setup,
source linking, Lima fixtures, and release engineering live in
[`CONTRIBUTING.md`](../CONTRIBUTING.md).

## Installation model

Install the plugin on every machine whose Herdr panes may host development
servers:

```bash
herdr plugin install go-min/herdr-fwd
```

The plugin runs beside the Herdr server that owns those panes. It discovers
loopback listeners there, but it cannot expose them by itself: an outgoing
machine also needs the local `hfwd` command to own the SSH tunnels and local
forwarding process.

This produces two common roles:

- a **local connecting machine** runs `hfwd <target>` and owns the SSH
  forwards;
- a **remote hosting machine** runs Herdr, the plugin watcher, and development
  processes.

One machine can serve both roles.

The canonical setup is five steps:

1. Install `herdr.fwd` on the remote hosting machine.
2. Install `hfwd` on the local connecting machine.
3. Configure an SSH host or alias for the remote machine.
4. Run `hfwd <target> --open`, replacing `<target>` with that host or alias.
5. Start a development server in a remote Herdr pane; its loopback listener is
   forwarded automatically.

## First-run welcome

The first local plugin run opens a session-modal popup on the
machine hosting the plugin and remote Herdr session. It first asks whether that
same machine will connect, host, or do both; setup actions appear only after
that role is selected:

- connecting and combined roles make **Install hfwd** the first action when it
  is missing, then show `hfwd <target>`, explain the `<target>` placeholder,
  and offer the optional `hfwd hook install` shortcut;
- the hosting-only role explains that `hfwd` is optional unless the machine
  will also connect to another host;
- when `hfwd` installed this plugin during a remote connection, a later
  local run explains that origin and adds **Uninstall plugin**; and
- every role offers **Enable port status**, which adds the standalone
  `$port_forward_status` Space sidebar row and reloads the Herdr configuration;
  and
- **Enable dashboard shortcut**, which adds server-side `prefix+shift+f` for
  the dashboard popup. Attach with `--remote-keybindings server` to use it.

The one-click installer pins `hfwd` to the plugin's exact version on that same
plugin-hosting machine; it cannot install `hfwd` on a separate local connecting
machine. Install `hfwd` on a separate connector with the root `install.sh` or
Homebrew. A missing configuration defaults `onboarding` to `true`; installation,
reinstallation, and update preserve an existing preference. Separate durable
state records only a plugin installation first created by `hfwd` on a remote
host, and selects the remote-origin popup only when its recorded plugin root
matches the running checkout. Closing with **Got it**,
**Skip**, `Esc`, or `q` sets only `onboarding = false` in
`~/.config/herdr-fwd/config.toml`.

The shell hook lets users keep entering `herdr --remote <target>`. It routes
only that command shape through `hfwd` and delegates every other `herdr`
invocation directly to Herdr.

An active `hfwd` session suppresses onboarding before a remote Herdr
client attaches. Herdr does not expose client transport identity to
plugins, so a plain `herdr --remote` connection that does not use `hfwd`
cannot be distinguished from a local client. See
[known limitations](limitations.md).

## Automatic forwarding

The watcher starts with foreground processes and can also inspect their child
processes for loopback TCP listeners. The search depth is stored in the plugin's
persistent `config.toml` alongside the `after_forward` preference; `0`
checks only foreground processes. The default is `2`, and the maximum is `8`.
Change it from the dashboard's Integration settings. A single process may own
multiple ports,
and each port receives its own mapping. The plugin removes a mapping when its
listener or owning process disappears and detects process restarts by PID.

Every automatic mapping is bound to local `127.0.0.1`. If its preferred local
port is busy, the companion selects the next available port. Discovery is
idempotent and never creates duplicate mappings for the same process-owned
listener.

Every detected listener is forwarded automatically. Configure the post-forward
destination in the dashboard integration settings: `SPACE`, `POPUP`, or
`NOTHING`.

## Dashboard tree

After an approved forward, the selected post-forward destination opens the
`Port Forwarding` Space, dashboard popup, or nothing. Automatic mappings follow the same Space, tab, and pane
order as Herdr's sidebar:

```text
󰉋 Space
└─ 󰓩 Tab
   └─ terminal pane · process · PID
      ● 127.0.0.1:5173  →  localhost:5173  ·  live 4m
        server 09:31  ·  tunnel 09:32
```

Tree identity uses Herdr ids rather than labels, so two Spaces or tabs with the
same name remain separate. The selected mapping highlights both of its lines.
Active and paused mappings use distinct colors while tree connectors stay
neutral.

Manual forwards appear in a separate `󰖟 MANUAL FORWARDS` section with the same
mapping graphics, colors, timestamps, and selection behavior.

## Keyboard actions

| Key | Action |
| --- | --- |
| `↑`/`↓`, `j`/`k` | Move through visible mappings |
| `g`/`G` | Select first or last mapping |
| `Enter` | Focus the automatic mapping's source pane |
| `o` | Open an active mapping in the local browser |
| `Space`, `e` | Pause or resume the selected mapping |
| `c` | Change the mapping's local-machine port |
| `a`, `n` | Add a manual loopback mapping |
| `d` | Confirm removal of a selected manual mapping; press `d` or `Delete` again to remove it |
| `h` | Open integration settings |
| `?` | Show contextual help |
| `Esc`, `q` | Close a popup dashboard |

The footer only shows actions valid for the current selection. Status messages
remain visible for three seconds and clear immediately when selection changes.

## Paused and manual mappings

Pausing an automatic mapping closes its current SSH tunnel without forgetting
the port. It remains paused after reconnect and after the owning process
restarts. Resuming it chooses the requested local port again, with normal
fallback when that port is busy.

Manual mappings accept only remote loopback hosts (`localhost`, `127.0.0.1`, or
`::1`) and always bind
locally to `127.0.0.1`. Their enabled or paused state is persisted per resolved
remote hostname and named Herdr session under `~/.config/herdr-fwd/`. Multiple
remote machines or Herdr sessions cannot inherit one another's mappings.

You can also add a manual mapping from the local machine while its session is
active:

```bash
hfwd forward <target> 4173       # remote 4173 to local 4173
hfwd forward <target> 4173 5174  # remote 4173 to local 5174
```

The target selects one active remote session, which keeps mappings separated
when several machines are connected. If no session is active, start it with
`hfwd <target>`. If more than one session matches, end one session and retry.
Port values must be in `1..=65535`; the CLI prints a safe retry command for an
invalid port. Dashboard removal is similarly deliberate: the first `d` opens a
confirmation and only a second `d` or `Delete` removes the manual mapping;
`Esc` cancels it. Automatic mappings are never removed there—use `Space` to
pause them instead.

## Timestamps and process identity

The process row shows the owning PID. A forward's detail line separates:

- **server** — when the current owning process started;
- **tunnel** — when the current SSH forward opened; and
- **live/paused duration** — elapsed time in the current forwarding state.

Pausing clears the live tunnel timestamp. Resuming creates a new one. A process
restart refreshes the server timestamp even when its display label is unchanged.

## Integration settings

Press `h` to configure:

- **After forwarding** — chooses `SPACE`, `POPUP`, or `NOTHING` as the default;
- **Port-forward status** — adds the standalone `$port_forward_status` Space
  sidebar row; and
- **Notifications inside Herdr** — safely sets Herdr toast delivery to `herdr`;
  and
- **Popup shortcut** — adds `prefix+shift+f` for the dashboard popup to the
  remote Herdr config. Start the attach with `--remote-keybindings server` to
  use it.

Configuration edits parse and validate the existing TOML, preserve unrelated
rows and toast settings, and create an atomic backup. Invalid or incompatible
configuration is left untouched.

With the status row enabled, the forwarding Space shows active and paused
counts. Source Spaces receive compact mapping metadata such as `→5173` or
`3000→3012`; it clears after their final enabled automatic forward disappears.

## Additional dashboard views

Open a session-modal quick view without changing the layout:

```bash
herdr plugin action invoke herdr.fwd.open-dashboard-popup
```

Open another managed dashboard in a split:

```bash
herdr plugin pane open --plugin herdr.fwd \
  --entrypoint dashboard --placement split
```

The `herdr.fwd.open-dashboard-here` action replaces the selected shell pane with
a dashboard. All dashboard instances read the same session and exit when that
remote forwarding session ends.

## Notifications and lifecycle

Forward creation and removal can emit Herdr notifications. Delivery follows the
remote Herdr server's toast configuration. The `hfwd` process does not print
lifecycle messages over the active alternate-screen UI.

When the attached `hfwd` process exits, it removes its local and remote session
files, cancels every SSH forward, closes its owned ControlMaster, and stops the
companion. The plugin then closes session-owned dashboards and clears sidebar
metadata.

## Security boundaries

The companion listens on a random local loopback port. Forward, heartbeat, and
browser-open requests require an exact ephemeral Bearer token. Only TCP ports
and loopback hosts are accepted; the API accepts neither SSH targets nor shell
commands. Runtime state uses user-only permissions.

For diagnosis and recovery, continue with
[installation and operations](operations.md).
