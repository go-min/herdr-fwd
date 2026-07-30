#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
tag=${1:-}

die() { printf 'release check: %s\n' "$*" >&2; exit 1; }

[ -n "$tag" ] || die "usage: scripts/check-release.sh <vMAJOR.MINOR.PATCH>"
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | sed -n '1p')
plugin_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/herdr-plugin.toml" | sed -n '1p')

[ "$tag" = "v$version" ] || die "tag $tag does not match Cargo version v$version"
[ "$plugin_version" = "$version" ] || die "Cargo and plugin versions differ"
grep -F "## [$version]" "$root/CHANGELOG.md" >/dev/null || \
  die "CHANGELOG.md has no section for $version"
python3 "$root/scripts/check-manifest.py"
printf 'Release metadata is consistent for %s.\n' "$tag"
