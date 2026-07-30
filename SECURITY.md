# Security policy

## Supported versions

Security fixes are provided for the latest released minor version. This is a
local developer tool: it does not expose a public network service, but it does
control SSH forwards and should still be updated promptly.

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability. Use GitHub's private
security advisory flow for `go-min/herdr-fwd` and include:

- affected version and operating systems;
- the remote/local topology and SSH configuration relevant to the issue;
- reproduction steps with secrets, hostnames, and terminal output redacted;
- the expected security boundary and observed impact.

Reports will be acknowledged as soon as practical. A fix, advisory, and new
release will be coordinated before public disclosure when the report is valid.

## Security boundary

The companion, RPC tunnel, and forwarded ports are loopback-only. An ephemeral
Bearer token protects every companion route except `/health`; the API accepts
neither arbitrary SSH targets nor shell commands. The wrapper owns a separate
OpenSSH ControlMaster and closes it when the attach session ends.

The remote plugin and local wrapper run with the current user's privileges.
They do not protect against a fully compromised process running as that same
user on either host.
