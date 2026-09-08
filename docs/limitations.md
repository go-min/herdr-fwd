# Known limitations

- Herdr does not expose its private ControlPath or a port-forward API. The
  wrapper owns a separate SSH master; ssh-agent/keychain is recommended.
- Compatibility is intentionally bounded to Herdr >=0.9.0 and <0.10.0 because
  pre-1.0 minor releases may change the plugin or socket contract. A new Herdr
  minor line must be validated before this guard is widened.
- Password-only SSH may authenticate separately for Herdr and the companion.
- Herdr does not expose attached-client transport identity to plugins.
  Onboarding is suppressed by the wrapper's session-scoped active file, but a plain
  `herdr --remote` client that bypasses the wrapper cannot be distinguished
  from a local client.
- Discovery uses `lsof` for TCP listeners owned by foreground pane processes
  and their configured descendant depth, merging `ss` results on Linux. It
  requires `ps` plus `lsof` on macOS or either `lsof` or `ss` on Linux,
  supports only loopback binds (`127.0.0.1`, `localhost`, and `::1`), and
  cannot infer whether the listener serves HTTP or HTTPS.
- Discovery is driven by Herdr pane lifecycle/metadata events with a 15-second
  reconciliation fallback. Starting a listener in an existing pane need not emit
  such an event, so discovery can wait for that fallback. Docker/Kubernetes
  discovery is out of scope.
- The remote dashboard is a terminal UI rather than a separate native sidebar
  section. It publishes a configurable `$port_forward_status` metadata token
  on the source Space, but current Herdr has no plugin-defined sidebar sections.
  It uses semantic ANSI palettes selected from the public Herdr theme name and
  terminal fallback metadata, so exact shades still depend on the terminal palette.
- Herdr's public notification API has no click target/action field, and plugin
  v1 has no API for adding native menu items. Remote attach does not relay
  local custom command bindings. The popup shortcut is installed on the
  remote server and requires `--remote-keybindings server`.
- Multiple simultaneous local clients attached to the same remote Herdr user
  each get their own dashboard workspace; cross-client coordination is out of
  scope. In Herdr 0.9, public socket `pane.focus` requests update all attached
  clients, so the dashboard's focus action is not private to its connector.
- The wrapper still handles one `herdr --remote <target>` connection. Machines
  added or switched through `herdr machine` are not automatically enrolled in
  forwarding. Herdr's reconnect UI does not restore a failed companion SSH
  master; reconnect with `hfwd` if the companion lease expires.
- Herdr 0.9 reads sidebar layouts and notification delivery from the local
  client. After editing them in the dashboard, use the Herdr menu's
  **reload config** action or reconnect; remote server reloads do not apply them.
- Managed installation first uses Herdr's GitHub checkout flow. When the exact
  release is unreachable remotely, `hfwd` can transfer a locally downloaded,
  checksum-verified binary for the remote platform. If GitHub is unavailable
  locally too, only an exact version/platform artifact from the verified local
  cache is eligible; a missing artifact is an actionable hard failure.
- Managed update/uninstall is intentionally refused while a forwarding session
  is active. Linked development checkouts must be managed with Herdr's
  `plugin link`/`plugin unlink` commands rather than the managed installer.
- There is no background self-updater. Upgrade the local wrapper first, end
  active sessions, and run `remote update` to install the matching plugin tag.
- No UDP, LAN exposure, cloud relay, Docker-aware discovery, or TLS
  termination.
