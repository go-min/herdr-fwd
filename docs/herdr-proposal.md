# Minimal future Herdr proposal

No Herdr core changes are implemented here.

The smallest useful extension would be a documented remote-attach capability
that gives a plugin a session identity and a validated loopback-only TCP
forward request channel. Herdr could authorize the request and reuse its
existing SSH bridge without exposing a filesystem ControlPath. A separate
explicit client action could open a URL, while remaining opt-in.

A minimal API could provide:

1. `remote.client.attached` / `remote.client.detached` events with an opaque
   attach id, never SSH credentials or a raw control socket path.
2. A typed `remote.forward.create/cancel/list` API restricted by default to
   local and remote loopback TCP endpoints, scoped to that attach id.
3. Automatic cancellation of attach-scoped forwards when its client leaves.
4. An explicit `client.open_url` request that the local client may deny or gate
   behind user configuration.

That would remove the second SSH connection, reverse RPC tunnel, session token,
local companion, and polling lease while preserving Herdr's transport
encapsulation. Native sidebar extensibility is useful but not required for the
forwarding lifecycle itself.
