# Contributing

This is the entry point for development and maintainer documentation. User
installation and plugin behavior belong in [`README.md`](README.md) and
[`docs/plugin.md`](docs/plugin.md).

Herdr Fwd is an out-of-process Herdr plugin plus a local wrapper. Keep changes
inside this repository; Herdr core changes require a separate upstream proposal.

## Development requirements

- Rust stable with the declared Rust 1.82 MSRV available for compatibility;
- OpenSSH, Python 3.11+, and Herdr 0.8.0+;
- `lsof`, `curl`, `tar`, and a SHA-256 utility;
- `shellcheck` for script validation; and
- `cargo-deny` for dependency policy checks.

## Build and validate

```bash
make build
make test
make lint
make ci
```

`make ci` runs formatting, Clippy with warnings denied, every Rust test target,
release builds, manifest and script checks, the formula renderer tests, and a
native package/install/uninstall smoke test. Run the dependency policy separately:

```bash
make deps-check
```

Before opening a pull request:

```bash
make fmt
make ci
git diff --check
```

Document user-visible changes in `CHANGELOG.md`.

## Source-linked plugin

Herdr does not execute manifest build commands for a linked checkout. Build the
plugin binary first, then link it:

```bash
make build
herdr plugin link "$PWD"
```

Switching back to a GitHub-managed install is explicit:

```bash
herdr plugin unlink herdr.fwd
herdr plugin install go-min/herdr-fwd
```

## Remote lifecycle development

`make run` builds the local `hfwd` wrapper and exercises its production remote
lifecycle:

```bash
make run TARGET=developer@dev.example.test
```

The remote host never receives a source checkout and never builds the plugin
with Cargo. `hfwd` first asks remote Herdr to install the version-pinned GitHub
release; if the remote cannot reach GitHub, it transfers only a checksum-verified
release binary for that remote OS/architecture.

Use the disposable Lima environment for the repeatable Linux/SSH topology:

```bash
make lima-up
make attach
```

The full fixture inventory, dashboard checklist, cleanup checks, and
troubleshooting steps are in [`docs/testing.md`](docs/testing.md).

## Test placement

White-box unit tests stay in a `#[cfg(test)]` module beside the implementation
they exercise. Black-box tests that use only the public crate API belong under
Cargo's top-level `tests/` directory. Shell tests exercise installer and package
behavior through temporary directories and real archives rather than source-text
assertions.

## Maintainer documentation

- [`docs/architecture.md`](docs/architecture.md) — component and data flow;
- [`docs/testing.md`](docs/testing.md) — development and integration testing;
- [`docs/release.md`](docs/release.md) — release and Homebrew publication;
- [`docs/research.md`](docs/research.md) — verified Herdr API boundaries;
- [`docs/review.md`](docs/review.md) — production-readiness review;
- [`docs/herdr-proposal.md`](docs/herdr-proposal.md) — upstream API proposal.

Do not add non-loopback listeners, arbitrary SSH targets to the companion API,
or token logging. Preserve the wrapper-owned SSH boundary and validate any new
Herdr API against current upstream documentation and source.
