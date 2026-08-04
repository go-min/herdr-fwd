# Known limitations

- Herdr does not expose its private ControlPath or a port-forward API. The
  wrapper owns a separate SSH master; ssh-agent/keychain is recommended.
- Compatibility is intentionally bounded to Herdr >=0.8.0 and <0.9.0 because
  pre-1.0 minor releases may change the plugin or socket contract. A new Herdr
  minor line must be validated before this guard is widened.
- Password-only SSH may authenticate separately for Herdr and the companion.
- Herdr 0.7.x does not expose attached-client transport identity to plugins.
  Onboarding is suppressed by the wrapper's active session file, but a plain
  `herdr --remote` client that bypasses the wrapper cannot be distinguished
  from a local client.
- Discovery uses `lsof` for TCP listeners owned by foreground pane processes
  and their configured descendant depth. It requires `lsof` and `ps`, supports
  only loopback binds (`127.0.0.1`, `localhost`, and `::1`), and cannot infer whether the listener
  serves HTTP or HTTPS.
- Discovery is driven by Herdr pane events with a 15-second reconciliation
  fallback; Docker/Kubernetes discovery is out of scope.
- The remote dashboard is a terminal UI rather than a separate native sidebar
  section. It publishes a configurable `$port_forward_status` metadata token
  on the source Space, but current Herdr has no plugin-defined sidebar sections.
  It uses a fixed ANSI color palette and terminal attributes instead of parsing
  private theme configuration, so exact shades depend on the terminal palette.
- Herdr's public notification API has no click target/action field, and plugin
  v1 has no API for adding native menu items. Remote attach does not relay
  custom command bindings, so a popup hotkey cannot be installed for this
  remote workflow; use the manifest action from a remote shell instead.
- Multiple simultaneous local clients attached to the same remote Herdr user
  each get their own dashboard workspace; cross-client coordination is out of
  scope.
- Managed installation still uses Herdr's GitHub checkout flow and therefore
  needs `git`, `curl`, `tar`, `lsof`, and outbound GitHub access on the remote
  host. The manifest downloads a checksum-verified prebuilt plugin binary;
  offline installation is not currently automated.
- Managed update/uninstall is intentionally refused while a forwarding session
  is active. Linked development checkouts must be managed with Herdr's
  `plugin link`/`plugin unlink` commands rather than the managed installer.
- There is no background self-updater. Upgrade the local wrapper first, end
  active sessions, and run `remote update` to install the matching plugin tag.
- No UDP, LAN exposure, cloud relay, Docker-aware discovery, or TLS
  termination.
