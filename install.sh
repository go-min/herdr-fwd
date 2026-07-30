#!/bin/sh
set -eu

root=
if resolved_root=$(CDPATH='' cd -- "$(dirname -- "$0")" 2>/dev/null && pwd); then
  root=$resolved_root
fi
prefix=${HERDR_FWD_PREFIX:-${PREFIX:-"$HOME/.local"}}
version=latest
from_source=false
repository=${HERDR_FWD_REPOSITORY_URL:-https://github.com/go-min/herdr-fwd}

usage() {
  cat <<'EOF'
usage: install.sh [--prefix PATH] [--version VERSION] [--from-source]

Downloads and checksum-verifies the local hfwd command.
--from-source builds hfwd from this checkout instead.
EOF
}

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix)
      [ "$#" -ge 2 ] || die "--prefix requires a path"
      prefix=$2
      shift 2
      ;;
    --version)
      [ "$#" -ge 2 ] || die "--version requires a release version"
      version=${2#v}
      shift 2
      ;;
    --from-source)
      from_source=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

if [ "$version" != latest ]; then
  case "$version" in
    ''|*[!0-9A-Za-z.+-]*) die "invalid release version: $version" ;;
  esac
  case "$version" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) die "invalid release version: $version" ;;
  esac
fi

command -v install >/dev/null 2>&1 || die "the install utility is required"

destination="$prefix/bin/hfwd"
install -d "$prefix/bin"

if [ "$from_source" = true ]; then
  [ "$version" = latest ] || die "--version cannot be combined with --from-source"
  command -v cargo >/dev/null 2>&1 || die "Rust/Cargo is required for --from-source"
  if [ -z "$root" ] || [ ! -f "$root/Cargo.toml" ]; then
    die "--from-source must run from a source checkout"
  fi
  (cd "$root" && cargo build --locked --release --bin hfwd)
  install -m 755 "$root/target/release/hfwd" "$destination"
else
  command -v curl >/dev/null 2>&1 || die "curl is required"
  command -v tar >/dev/null 2>&1 || die "tar is required"
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64|Linux:amd64) platform=linux-x86_64 ;;
    Linux:aarch64|Linux:arm64) platform=linux-aarch64 ;;
    Darwin:x86_64|Darwin:amd64) platform=macos-x86_64 ;;
    Darwin:arm64|Darwin:aarch64) platform=macos-aarch64 ;;
    *) die "unsupported platform: $(uname -s)/$(uname -m)" ;;
  esac
  asset="herdr-fwd-$platform.tar.gz"
  if [ -n "${HERDR_FWD_RELEASE_BASE:-}" ]; then
    base=$HERDR_FWD_RELEASE_BASE
  elif [ "$version" = latest ]; then
    base="$repository/releases/latest/download"
  else
    base="$repository/releases/download/v$version"
  fi
  temporary=$(mktemp -d "${TMPDIR:-/tmp}/herdr-fwd-install.XXXXXX")
  trap 'rm -rf "$temporary"' EXIT HUP INT TERM
  curl -fL --retry 3 --connect-timeout 15 "$base/$asset" -o "$temporary/$asset"
  curl -fL --retry 3 --connect-timeout 15 "$base/SHA256SUMS" -o "$temporary/SHA256SUMS"
  expected=$(awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1; exit }' "$temporary/SHA256SUMS")
  [ -n "$expected" ] || die "release checksum is missing for $asset"
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temporary/$asset" | awk '{print $1}')
  elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$temporary/$asset" | awk '{print $1}')
  else
    die "sha256sum or shasum is required to verify the release"
  fi
  [ "$actual" = "$expected" ] || die "checksum verification failed for $asset"
  tar -xzf "$temporary/$asset" -C "$temporary" ./hfwd
  install -m 755 "$temporary/hfwd" "$destination"
fi
printf 'Installed hfwd: %s\n' "$destination"

case ":${PATH:-}:" in
  *":$prefix/bin:"*) ;;
  *) printf 'Add %s/bin to PATH to run hfwd directly.\n' "$prefix" ;;
esac
