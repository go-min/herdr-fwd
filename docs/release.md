# Release process

Releases are immutable GitHub tags with native archives for:

- Linux x86_64 and aarch64, built on Ubuntu 22.04;
- macOS x86_64 and arm64.

Each archive contains the local wrapper, remote plugin binary, manifest,
license, README, and version marker. `SHA256SUMS` covers every archive and the
SPDX JSON software bill of materials. GitHub build-provenance attestations are
published for the native archives.

Every tag release verifies `HOMEBREW_TAP_TOKEN`, exercises the rendered formula
on Linux and macOS, then proposes a formula update in `go-min/homebrew-tap`.
The release itself remains independent: a missing or invalid tap credential
fails before the formula proposal and does not alter the immutable release
artifacts.

## Prepare

1. Update the version in `Cargo.toml` and `herdr-plugin.toml`.
2. Add the matching section to `CHANGELOG.md`.
3. Run the full local suite and metadata check:

   ```bash
   make ci
   make release-check VERSION_TAG=v0.1.0
   ```

4. Complete the documented Lima scenario in `docs/testing.md`.
5. Merge the release preparation through the protected `main` branch.

## Publish

Create and push an annotated tag from the verified `main` commit:

```bash
git tag -a v0.1.0 -m "herdr-fwd v0.1.0"
git push origin v0.1.0
```

The tag-triggered workflow verifies version consistency, tests and builds all
four platforms, creates consistent package layouts, publishes checksums, SPDX
SBOM, and provenance, creates the GitHub Release, then opens or updates the
matching Homebrew formula PR. It does not run for branches or untagged commits.

For an already published release whose formula PR must be recreated, run the
**Homebrew formula** workflow manually with the exact tag. It downloads the
published `SHA256SUMS` rather than rebuilding or replacing the release.

After publication, verify one `install.sh` installation, then run `doctor` and
the manual remote forwarding scenario. Do not move or recreate a published tag;
issue a patch release for corrections.

## Upgrade compatibility

On first use for each SSH target, the local wrapper installs the remote plugin
from the exact matching Git tag through its existing private ControlMaster.
Install the new local wrapper first, disconnect active forwarding sessions, and
then run:

```bash
hfwd remote update developer@dev.example.test
```

The internal RPC path is versioned independently. Both components reject a
session file using an unsupported protocol version instead of attempting a
partially compatible session.
