# Production-readiness review

Review date: 2026-07-28. Scope: local wrapper, companion HTTP API, SSH
transport, remote watcher and dashboard, lifecycle cleanup, installation,
Lima harness, manifests, dependencies, CI, and operator documentation.

The reviewed tree is a release candidate for the documented macOS/Linux
loopback workflow. Runtime ownership, protocol compatibility, bounded failure
behavior, installation, release artifacts, and real Linux/SSH smoke coverage
are now explicit and repeatable. Publishing the first protected tag is the
remaining release operation, not a product-development shortcut.

## Findings resolved

### Critical: macOS ControlPath could exceed the socket limit

The platform `$TMPDIR` can already be long enough that OpenSSH's temporary
ControlPath suffix exceeds the Unix-domain socket path limit. `doctor` failed
before authentication on an actual macOS-to-Lima run. The wrapper now creates
a random private directory directly under `/tmp`, uses a short socket filename,
tests the path budget, and removes the directory through its ownership guard.

### High: transient RPC failure deleted live management state

`list` previously treated every companion connection failure as proof that a
session was stale. A sandbox, temporary resource limit, or short scheduling
pause could therefore delete the token needed to manage a still-running
forward. Session state now includes the wrapper PID. Pruning requires both an
unreachable companion and a dead owner; normal detach still removes the state
immediately.

### High: failed remap rollback could report a nonexistent active forward

When cancel succeeded, the new local mapping failed, and restoration also
failed, the registry retained `enabled=true`. It now records the truthful safe
state (`enabled=false`), persists that transition even on an error response,
and has a regression test for the double-failure path.

### High: the session token was observable in the local SSH process arguments

The remote session document previously formed part of an SSH command argument,
which made its bearer token visible to same-user process inspection. The
wrapper now writes the document through SSH standard input and commits it with
an atomic remote `0600` temporary-file rename. A behavior test pipes a sentinel
document through the same shell command and verifies its content, permissions,
and temporary-file cleanup.

### High: RPC and session compatibility was implicit

Session documents now carry an explicit protocol version and reject unknown or
unsupported versions. Both local and remote Herdr are checked against the
supported `>=0.8.0,<0.9.0` range before attach. Companion, plugin, and dashboard
responses have size limits, untrusted terminal metadata is sanitized, and URL
payload validation requires a matching HTTP(S) loopback address and port.

### High: dependency update broke the declared Rust MSRV

The merged `tempfile` update reintroduced a transient WASI graph incompatible
with the Rust/Cargo 1.82 policy. Temporary directories now use the standard
library plus OS randomness, removing `tempfile`, `getrandom`, `wasip2`, and
their related lockfile surface. CI retains a dedicated Rust 1.82 job.

### Medium: installation, packaging, and remote lifecycle were checkout-oriented

The local wrapper now exposes fixed, non-shell-extensible
`remote install/update/status/uninstall` operations using Herdr's documented
managed GitHub plugin commands. POSIX install/uninstall scripts cover the local
binary and optionally the remote side; remote plugin installation downloads a
version-matched, checksummed native binary and does not require Rust. Make
aliases expose the same workflow. Update and uninstall reject active forwarding
sessions.

### Medium: repository validation missed operational files

CI now checks the Herdr manifest's required invariants, prebuilt plugin install
hook, supported event hooks, relative runtime commands, POSIX script syntax,
and Python tool syntax. The protected Ubuntu check also runs dependency review,
shell analysis, and `cargo-deny`; the macOS and Rust 1.82 checks retain their
platform and MSRV coverage. Dependabot groups compatible routine updates so the
full locked graph is reviewed together.

## Security boundary reviewed

- companion and every local forward bind only to `127.0.0.1`;
- all non-health API routes require an ephemeral 256-bit Bearer token;
- request bodies and port/host/pane metadata are bounded and validated;
- the API accepts neither an SSH target nor a shell command;
- remote management accepts a validated SSH destination, while plugin source,
  plugin id, and remote operations remain compile-time constants;
- SSH invocations use argument arrays, an owned ControlMaster, a parent-owned
  lifetime pipe, `ControlPersist=no`, and explicit forward/cancel operations;
- the session document is uploaded over SSH standard input rather than process
  arguments;
- token files are `0600` inside `0700` state directories and are never logged.

## Verification performed

- `make ci`: format, Clippy with warnings denied, every Rust test target,
  release build, manifest/script checks, and native package/install smoke;
- isolated-prefix install/run/uninstall smoke for the local wrapper;
- Linux/aarch64 locked release build and plugin registration in a disposable
  Lima instance;
- `doctor lima-herdr-release-test`: local/remote Herdr compatibility, private
  ControlMaster, reverse loopback RPC, and enabled plugin;
- interactive remote attach, test server on remote port `31001`, automatic
  mapping to the next
  available local port, successful local HTTP response, automatic removal when
  the server stopped, and detach cleanup;
- post-detach checks found no local SSH runtime directory, local session file,
  remote session file, forward, or automatic dashboard workspace.

## Residual operational work

- release archives, checksums, SPDX SBOM, build provenance, and the Homebrew
  formula PR are automated for four native platform/architecture pairs; the
  manual **Homebrew formula** workflow can recreate a formula PR from an
  already published tag;
- continue running the Rust 1.82 CI job on every dependency update; the exact
  MSRV toolchain is not installed on the development Mac;
- add a noninteractive CI SSH fixture if stable Herdr test binaries become
  available; the current real attach/dashboard test remains a documented Lima
  smoke test;
- native sidebar/theme-token integration and a single shared Herdr SSH
  transport require the core APIs proposed in `docs/herdr-proposal.md`.

## Follow-up verification: retryable dashboard cleanup

Review remediation on 2026-07-30 made dashboard cleanup fallible end-to-end.
Failed Herdr workspace closes now retain both the dashboard marker and remote
session for retry; cleanup removes them only after a successful close or the
explicit `workspace not found` / `workspace already closed` response. The
watcher and orphan cleanup paths propagate a cleanup failure instead of
discarding it.

Verified locally:

- `cargo test --locked --all-targets plugin::session` — 5 passed, 0 failed;
- `cargo test --locked --all-targets plugin::lifecycle` — 13 passed, 0 failed;
- `cargo clippy --locked --all-targets -- -D warnings` — passed.
