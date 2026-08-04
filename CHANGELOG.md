# Changelog

All notable changes are documented here. The project follows Semantic
Versioning.

## [Unreleased]

## [0.1.5] - 2026-08-05

### Changed

- Require Herdr 0.8.0 through 0.8.x on local and remote hosts.
- Scope discovery, dashboards, manual mappings, and watcher ownership to the
  exact default or named Herdr session.
- Use one status-first, version-pinned remote plugin lifecycle for attach and
  explicit management commands, with verified exact-platform release fallback.
- Centralize framework/runtime detection and theme-aware dashboard colors.
- Refresh the production Overview recording for the Herdr 0.8 remote flow.

### Fixed

- Keep companion heartbeats independent from listener discovery, batch socket
  inspection, and surface partial reconciliation failures.
- Make forwarding-state writes concurrency-safe and roll live tunnels back when
  persistence fails.
- Fail closed on release and cache integrity errors, publish cache entries
  atomically, and roll back failed managed-bundle activation.
- Preserve the onboarding preference across installs while preventing stale
  provenance from claiming manually reinstalled or relinked plugins.
- Harden demo cleanup, release gates, package smoke tests, and shell checks.

## [0.1.4] - 2026-07-30

### Fixed

- Write valid root-bound remote onboarding provenance and clear it on manual
  plugin reinstallation.
- Keep GitHub Release publication available for the private repository by
  deferring build-provenance attestations.

## [0.1.3] - 2026-07-30

### Fixed

- Make release publication independent of the deferred Homebrew tap rollout.

### Unreleased additions

- Root checksum-verified installer and automated multi-platform Homebrew formula
  pull requests after each GitHub Release.
- Herdr-native plugin installation, one-time local onboarding with optional
  wrapper setup and quick uninstall, and audience-separated user/developer docs.
- Persistent zsh, Bash, and Fish hooks that route `herdr --remote` through the
  wrapper while leaving other Herdr commands unchanged.
- Short `hfwd` executable name while the project, plugin, formula, archives,
  and persistent state retain the `herdr-fwd` identity.
- Checksum-verified exact-platform release fallback for remote plugin hosts
  without GitHub access.

## [0.1.2] - 2026-07-30

### Fixed

- Reset onboarding in Herdr's managed plugin configuration during installation.

## [0.1.1] - 2026-07-30

### Added

- Child-process listener discovery, configurable process-tree depth, dashboard
  mouse support, and persistent onboarding origin state.
- IPv6 loopback forwarding and clearer remote-install onboarding guidance.

## [0.1.0] - 2026-07-28

### Added

- Automatic loopback URL discovery for active remote Herdr panes.
- Session-scoped OpenSSH forwarding with deterministic cleanup.
- Authenticated loopback companion API and remote watcher lease.
- Theme-aware remote dashboard with keyboard management actions.
- Local management, remote plugin lifecycle, doctor, and Lima test tooling.
- Checksum-verified release archives for Linux and macOS on x86_64 and arm64.

### Security

- Ephemeral 256-bit session tokens and private runtime state.
- Strict loopback-only host, bind, URL, and RPC validation.
- Bounded request/response payloads and per-session forward count.
- Terminal-control sanitization for metadata rendered by the dashboard.
