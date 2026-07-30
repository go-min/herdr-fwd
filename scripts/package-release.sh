#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
target=${1:-}
output_directory=${2:-"$root/dist"}

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[ -n "$target" ] || die "usage: scripts/package-release.sh <rust-target> [output-directory]"

case "$target" in
  x86_64-unknown-linux-gnu) platform=linux-x86_64 ;;
  aarch64-unknown-linux-gnu) platform=linux-aarch64 ;;
  x86_64-apple-darwin) platform=macos-x86_64 ;;
  aarch64-apple-darwin) platform=macos-aarch64 ;;
  *) die "unsupported release target: $target" ;;
esac

binary_directory="$root/target/$target/release"
host=$(rustc -vV | sed -n 's/^host: //p')
if [ "$target" = "$host" ] && [ ! -d "$binary_directory" ]; then
  binary_directory="$root/target/release"
fi
for binary in hfwd herdr-fwd-plugin; do
  [ -x "$binary_directory/$binary" ] || die "missing release binary: $binary_directory/$binary"
done

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/Cargo.toml" | sed -n '1p')
[ -n "$version" ] || die "could not read package version"

stage=$(mktemp -d "${TMPDIR:-/tmp}/herdr-fwd-package.XXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
install -m 755 "$binary_directory/hfwd" "$stage/hfwd"
install -m 755 "$binary_directory/herdr-fwd-plugin" "$stage/herdr-fwd-plugin"
install -m 644 "$root/herdr-plugin.toml" "$stage/herdr-plugin.toml"
install -m 644 "$root/LICENSE" "$stage/LICENSE"
install -m 644 "$root/README.md" "$stage/README.md"
printf '%s\n' "$version" > "$stage/VERSION"

mkdir -p "$output_directory"
archive="$output_directory/herdr-fwd-$platform.tar.gz"
tar -C "$stage" -czf "$archive" .
printf '%s\n' "$archive"
