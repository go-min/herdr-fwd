# Development and integration testing with Lima

This is a contributor guide. User-facing plugin behavior is documented in
[`plugin.md`](plugin.md).

## Test layout

The repository follows these placement rules:

- white-box unit tests live in a `#[cfg(test)]` module at the end of the source
  file they exercise, for example `src/registry.rs`,
  `src/bin/local/ssh.rs`, or `src/bin/plugin/lifecycle.rs`;
- black-box integration tests live under Cargo's conventional top-level
  `tests/` directory and use only the crate's public API.

The `make test-ssh` target runs a standalone real SSH/Herdr integration test. End-to-end SSH,
Herdr, process discovery, and forwarding behavior is exercised with the Lima
scenario below. There are no shared catch-all `src/tests.rs` or
`src/bin/**/tests.rs` files.

Run every test target with:

```bash
make test
```

The repository uses [Lima](https://lima-vm.io/) as the recommended remote
test host. Lima provides a real Linux VM with SSH, independent processes, and
a normal SSH config entry. This is closer to the production Herdr topology
than running Herdr inside a container.

The instance and SSH port are configurable, so an isolated regression VM can
run without modifying a developer's active default session:

```bash
make lima-run LIMA_INSTANCE=herdr-release-test LIMA_SSH_PORT=2223
```

## Prerequisites

On macOS:

```bash
brew install lima
```

The local machine also needs Rust/Cargo and OpenSSH. The test VM downloads
packages and the Herdr installer, so the first provisioning requires network
access.

## Create and provision the VM

Run this from the repository root:

```bash
make lima-up
```

The script creates a Lima instance named `herdr-test`, using Apple
Virtualization (`vz`) on macOS and QEMU elsewhere. The repository is copied
directly into the guest's writable home directory: `~/Cargo.toml`, `~/src`,
`~/scripts`, and the remaining project files. Provisioning replaces
project-owned source paths on each provisioning run while preserving
the guest-native Cargo target cache. The guest never builds inside a read-only
host mount, incremental rebuilds remain fast, and no `cd` into a checkout
subdirectory is needed. It installs Cargo/Rust, Python, Herdr, builds the remote
plugin, links it, and enables it. It also installs Node/npm, Bun, Deno, Nginx,
and Caddy, then prepares the disposable web-server fixtures under
`~/.cache/herdr-fwd/dev-servers`.

After changing the plugin manifest, restart an already-running remote Herdr
server before testing so it reloads startup and event hooks:

```bash
ssh lima-herdr-test 'export PATH="$HOME/.local/bin:$PATH"; herdr server stop'
```

Lima writes its SSH config to `~/.lima/herdr-test/ssh.config`. The `up` command
configures OpenSSH automatically. To repair the config later, run:

```bash
scripts/test-server.sh configure-ssh
```

This adds the following idempotently to `~/.ssh/config`:

```sshconfig
Include ~/.lima/*/ssh.config
```

Verify the remote host:

```bash
ssh lima-herdr-test
herdr --version
herdr plugin list
```

The generated SSH config is also available with:

```bash
scripts/test-server.sh ssh-config
```

## End-to-end test

Provision the default Lima VM and attach a Herdr client in one command:

```bash
make lima-run
```

Use `make attach` when the VM is already provisioned. For a non-Lima SSH target,
`make run TARGET=developer@host` builds the local wrapper and uses the production
remote lifecycle; the remote receives neither this checkout nor a Cargo build.
`hfwd` first requests Herdr's version-pinned GitHub install, then falls back to
a checksum-verified binary for the remote platform only when GitHub is
unavailable. Build just the local release binaries with:

```bash
make build
```

For a fixture topology, run the development server from this local checkout:

```bash
make serve
```

To run a single fixture server at a particular starting port instead:

```bash
make serve PORT=3012
```

The default creates `Apps` (Vite, Astro, SvelteKit), `Services` (Next.js,
Storybook, Nuxt, Bun, Deno, and a dual-port fixture), and `Gateways` (Nginx,
Caddy), on `3000–3002` and `4000–4008`. It starts the panes progressively and
waits for each listener before continuing. Every pane owns one server process;
the dual-port process exposes both `4007` and `4008`.

All fixture servers bind only to remote loopback. The single-server form prints
the selected port:

```text
Local: http://localhost:3000/
```

Within a few seconds the remote `Port Forwarding` dashboard and a Herdr
notification should show a mapping such as:

```text
dev server  remote 127.0.0.1:3000 → local 127.0.0.1:3000
```

Verify the forwarded page locally:

```bash
target/release/hfwd list
curl http://127.0.0.1:<reported-local-port>/
```

Lima automatically mirrors some guest listening ports onto the host. It may
occupy the matching local port before this plugin does, in which case the
companion selects another local port. Do not use a successful request to
Lima's automatic mirror as evidence that the plugin works; verify the mapping
in `list` and request the port reported there.

The wrapper must not print forward-created, removed, toggled, or remapped
messages over the active Herdr UI. Dashboard state and Herdr notifications are
the user-facing lifecycle surfaces; use `list` only from a separate local pane.

Stop one fixture process from its Herdr pane. The plugin should remove every
forward owned by that process, show a notification in the remote Herdr UI, and
remove the mappings from `hfwd status` on the local machine. In particular,
stopping the dual-port fixture should remove both `4007` and `4008`.

Approve a detected listener with the `SPACE` destination to create and focus a
`Port Forwarding` Space in the remote Herdr server. In that pane, enter a
forward and press `Enter` to focus the source pane, `o` to open its local URL,
or `Space` to pause/resume it. Press `c` on a selected mapping, type `3012`,
and press `Enter` to remap its local endpoint (for example `3000 → 3012`).
Press `a` to add a new custom mapping; submitting the untouched form creates
`3000 → 3000`, while the two fields allow a different remote/local pair. The
footer should show `d remove` only for a selected custom mapping and `o open`
only for an enabled one. Trigger pause/resume and verify its result message
disappears after three seconds; trigger it again and move selection to verify
the message clears immediately. The dashboard must remain visible after every
mapping has been removed. From
another selected shell pane, invoke the plugin action
`herdr.fwd.open-dashboard-here`; it should replace that pane's
shell UI with another live dashboard. Also verify the managed pane entrypoint:

```bash
herdr plugin pane open --plugin herdr.fwd \
  --entrypoint dashboard --placement split
```

For the session-modal quick view, invoke:

```bash
herdr plugin action invoke herdr.fwd.open-dashboard-popup
```

The popup should show the same live mappings without changing the tiled layout.
Press `h` in the dashboard to open **Integration settings**. Toggle
`Port-forward status` and verify that it only adds or removes the standalone
`["$port_forward_status"]` row. When the `rows` setting is absent, it must
derive the current default rows through `herdr --default-config`, preserve
them, and append only this row. It must never change other rows.
With that row enabled, the `Port Forwarding` Space should display
`<active> active` beneath its name, with `· <paused> paused` only when needed.
Source Spaces continue to show their individual mappings. When
`Notifications inside Herdr` shows `SET UP`, select it and press `Space`; it must
set `[ui.toast] delivery = "herdr"` without changing the other toast settings.
For a newly detected listener, verify every port is forwarded without an
approval popup. Choose each `After forwarding` destination (`SPACE`, `POPUP`,
`NOTHING`) and verify the matching result.
When `Popup shortcut` shows `SET UP`, select it and press `Space`. It must add
the server-side `prefix+shift+f` binding for
`herdr.fwd.open-dashboard-popup` without changing existing bindings. Reattach
with `--remote-keybindings server` and verify that shortcut opens the popup.
The source Space sidebar should then show a compact list: a same-port forward
uses `→<port>`, while a remapped forward uses `remote→local`. It should clear
after its final enabled automatic forward is removed.

The Space is closed automatically when the remote attach ends. Repeated
discovery of the same process-owned listener must not create a duplicate.
Finally exit the remote Herdr client and verify that no wrapper-owned forward
remains. Reconnect to the same host and verify that manual mappings retain their
enabled/paused state and previously paused automatic ports remain paused. A
different remote host must not inherit that state.

## Lifecycle commands

```bash
make lima-status          # VM state
make lima-run             # provision and attach with forwarding
make lima-shell           # interactive VM shell
make lima-down            # stop, preserve VM disk
make lima-destroy         # permanently remove the VM
```

The underlying `scripts/test-server.sh` commands remain available, including
`configure-ssh` and `ssh-config`, for troubleshooting.

For a separate local status pane, open a Herdr pane and run:

```bash
hfwd status
```

`down` is safe for repeatable development. Use `destroy` only when the VM is
no longer needed; provisioning and downloaded Herdr artifacts will need to be
repeated afterward.

The `dev` command is a convenience for checking the fixture itself from a
plain VM shell. For plugin autodetection, always run the fixture inside a
Herdr pane as shown above.

## Troubleshooting

If `ssh lima-herdr-test` fails, check the generated config and VM state:

```bash
scripts/test-server.sh status
scripts/test-server.sh ssh-config
ssh -F "$(scripts/test-server.sh ssh-config)" lima-herdr-test
```

If the plugin is not listed or does not discover a pane's listeners:

```bash
scripts/test-server.sh install-plugin
ssh lima-herdr-test 'export PATH="$HOME/.local/bin:$PATH"; herdr server stop'
ssh lima-herdr-test 'export PATH="$HOME/.local/bin:$PATH"; herdr plugin list'
```

The wrapper target must be `lima-herdr-test`, not the VM name `herdr-test`.
Lima's generated SSH alias includes the correct guest user, key, and local SSH
port.

## Herdr 0.9 regression checks

Use Herdr 0.9.0 on the connecting machine and the fixture host. Verify that
0.8.x and 0.10.x are rejected by `hfwd doctor` before starting a connection.

In the remote dashboard, press `h`, toggle port status, and enable Herdr
notifications. Confirm only the connecting machine's UI config changes, then
select **reload config** in Herdr's menu and check the status row. A remote
server reload alone must not be treated as applying client presentation changes.
Check that the popup shortcut still edits the remote server's bindings.

Start a listener immediately after attaching and after a watcher event-stream
reconnect. Check foreground identity probes and settling scans as well as pane
lifecycle/metadata events. The 15-second fallback still covers late listeners
inside unchanged processes: `pane.updated` is not a raw output stream. Test popup
open/close, split dashboards, source-pane focus, and removal of all forwards when
the source process exits. Public source-pane focus currently
affects all Herdr clients attached to the server; it is not client-scoped.

## Isolated loopback SSH test

```bash
make test-ssh
```

Requires Unix, Herdr 0.9.x, Python 3.11+, OpenSSH client/server (`sshd`),
`ssh-keygen`, `ps`, and `lsof`. This uses real Herdr, `hfwd`, and SSH transport;
only SSH configuration is redirected to the fixture. No SSH daemon or key is
installed globally. Some environments require permission to run a local sshd.

The fixture creates temporary keys/configuration, a loopback-only SSH listener,
and a uniquely named Herdr session. It creates and removes only that session's
remote forwarding cache directory. It verifies delayed listener discovery,
HTTP traffic through forwarded ports, collision handling, paused state across
transport recovery, two connectors, ambiguous dashboard rejection, and cleanup
on normal exit and sustained outage. Failure logs are kept under
`target/ssh-e2e-failure`; session files and bearer tokens are not copied there.
