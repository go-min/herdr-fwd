# User installation and operations

## Install the plugin

Install and enable the plugin for the current Herdr user:

```bash
herdr plugin install go-min/herdr-fwd
```

The first local run opens the setup popup described in the
[plugin guide](plugin.md). The `hfwd` command remains optional unless this
machine initiates remote connections.

## Install hfwd

For a normal released installation:

```bash
curl -fsSL \
  https://raw.githubusercontent.com/go-min/herdr-fwd/main/install.sh | sh
```

This downloads and checksum-verifies the native `hfwd` command, then installs it
at `~/.local/bin/hfwd`. The first `hfwd <ssh-target>` session uses its
already-authenticated private ControlMaster to install or update the matching
managed plugin on that target. An SSH target is either an alias in
`~/.ssh/config` (for example `workbox`) or a regular OpenSSH destination such as
`developer@dev.example.test`.

Override the local prefix without changing the repository:

```bash
curl -fsSL https://raw.githubusercontent.com/go-min/herdr-fwd/main/install.sh \
  -o /tmp/herdr-fwd-install.sh
sh /tmp/herdr-fwd-install.sh --prefix "$HOME/.local"
```

The Homebrew installation uses the same native release archives:

```bash
brew install go-min/tap/herdr-fwd
```

Both installation methods provide the `hfwd` executable. The repository,
plugin, release archives, and Homebrew formula keep the `herdr-fwd` name.

The remote host needs Herdr 0.8.x, OpenSSH access, `ps`, and listener inspection
through `lsof` (macOS/Linux) or `ss` (Linux). Herdr's preferred install path
also uses `git`, `curl`, `tar`, a SHA-256 utility, and outbound GitHub access.
If that exact release is unreachable remotely, the local host downloads the
archive for the remote platform, verifies `SHA256SUMS`, and transfers only the
plugin bundle. A versioned verified cache is the final fallback when GitHub is
unavailable locally too. A missing release, checksum failure, or absent exact
cache artifact is an actionable error; production never sends a locally built
or incompatible binary.

## Intercept `herdr --remote`

Install a persistent hook for the shell named by `$SHELL`:

```bash
hfwd hook install
```

The installer supports zsh, Bash, and Fish. It adds one marked block to
`~/.zshrc` or `~/.bashrc`; Fish receives
`~/.config/fish/conf.d/herdr-fwd.fish`. Repeated installation is idempotent and
preserves existing configuration. An explicit shell overrides `$SHELL`:

```bash
hfwd hook install bash
```

The hook intercepts only `herdr --remote <target>`. All other arguments are
passed to the original `herdr` executable unchanged. To enable it for only the
current zsh or Bash process, use `eval "$(hfwd hook zsh)"` or
`eval "$(hfwd hook bash)"` instead.

## Routine management

```bash
hfwd doctor developer@dev.example.test
hfwd remote update developer@dev.example.test
hfwd remote uninstall developer@dev.example.test
```

Update and uninstall stop before changing remote state when an active
`session-*.json` exists. Disconnect the `hfwd` session first; its normal
shutdown cancels forwards, closes the owned ControlMaster, stops the companion,
and removes both local and remote session files.

Upgrade the two components in order so their versions remain aligned:

```bash
curl -fsSL \
  https://raw.githubusercontent.com/go-min/herdr-fwd/main/install.sh | sh
hfwd remote update developer@dev.example.test
hfwd doctor developer@dev.example.test
```

The second command installs the exact tag embedded in the new `hfwd` command.
It will not update or uninstall a plugin while a forwarding session is active.

## Forward management

```bash
hfwd list
hfwd status
hfwd close fwd-1
hfwd close 5173
hfwd close-all
```

`list` also prunes stale local state after confirming both that its companion
is unreachable and that its `hfwd` PID has exited. A temporary RPC failure
therefore cannot discard a live session's management token. State files are
user-only and contain the ephemeral token needed by these local commands; they
must not be copied into logs or support bundles.

## Uninstall

Remove the plugin from the current machine:

```bash
herdr plugin uninstall herdr.fwd
```

Use `herdr plugin unlink herdr.fwd` for a source-linked development checkout.
The quick onboarding action and `hfwd remote uninstall` detect the managed
fallback bundle, unlink it, remove its files, and clear remote-origin metadata.

When `hfwd` originally installed the plugin, the first-run popup provides
the same quick action.

Remove a managed remote plugin and the local `hfwd` command together:

```bash
hfwd remote uninstall developer@dev.example.test
curl -fsSL \
  https://raw.githubusercontent.com/go-min/herdr-fwd/main/uninstall.sh \
  | sh
```

For a Homebrew installation, remove only the local `hfwd` command with:

```bash
brew uninstall herdr-fwd
```

The downloaded uninstall script is also available for a script installation;
it defaults to `~/.local/bin` and accepts `--prefix PATH` when saved locally.

The uninstall script prunes stale state and refuses to remove `hfwd` while
a live local forwarding session remains. It does not delete Rust caches, Herdr
configuration, SSH configuration, linked source checkouts, or Lima instances.

## Recovery

If a process was killed abruptly, the SSH lifetime pipe and
`ControlPersist=no` close the actual forwards. Then run:

```bash
hfwd list
hfwd doctor developer@dev.example.test
```

The first command removes unreachable local state. The remote watcher removes
its stale session and automatic dashboard after repeated failed heartbeats.
If the plugin was updated while Herdr was stopped, start or reattach Herdr so
it reloads the manifest.
