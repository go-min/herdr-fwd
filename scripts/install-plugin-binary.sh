#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
repository=${HERDR_FWD_REPOSITORY_URL:-https://github.com/go-min/herdr-fwd}

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

preserve_onboarding() {
  if [ -n "${HERDR_PLUGIN_CONFIG_DIR:-}" ]; then
    config_dir=$HERDR_PLUGIN_CONFIG_DIR
  else
    config_root=${XDG_CONFIG_HOME:-"${HOME:?HOME is required}/.config"}
    config_dir="$config_root/herdr/plugins/config/herdr.fwd"
  fi
  config_path="$config_dir/config.toml"
  mkdir -p "$config_dir"
  temporary_config=$(mktemp "$config_dir/.config.toml.XXXXXX")
  {
    if [ -f "$config_path" ]; then
      onboarding=$(sed 's/[[:space:]]*#.*$//' "$config_path" | sed -n 's/^[[:space:]]*onboarding[[:space:]]*=[[:space:]]*\([^[:space:]]*\)[[:space:]]*$/\1/p' | sed -n '1p')
      case "$onboarding" in
        true|false) ;;
        *) onboarding=true ;;
      esac
      printf 'onboarding = %s\n' "$onboarding"
      sed -e '/^[[:space:]]*onboarding[[:space:]]*=.*/d' \
          -e '/^[[:space:]]*installation_source[[:space:]]*=.*/d' "$config_path"
    else
      printf 'onboarding = true\n'
    fi
  } > "$temporary_config"
  mv "$temporary_config" "$config_path"
}

clear_remote_origin() {
  state_root=${XDG_STATE_HOME:-"${HOME:?HOME is required}/.local/state"}
  rm -f "$state_root/herdr-fwd/plugin-origin.toml"
}

command -v curl >/dev/null 2>&1 || die "curl is required to install the plugin binary"

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$root/herdr-plugin.toml" | sed -n '1p')
[ -n "$version" ] || die "could not read plugin version"

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64) platform=linux-x86_64 ;;
  Linux:aarch64|Linux:arm64) platform=linux-aarch64 ;;
  Darwin:x86_64|Darwin:amd64) platform=macos-x86_64 ;;
  Darwin:arm64|Darwin:aarch64) platform=macos-aarch64 ;;
  *) die "unsupported plugin platform: $(uname -s)/$(uname -m)" ;;
esac

asset="herdr-fwd-$platform.tar.gz"
base=${HERDR_FWD_RELEASE_BASE:-"$repository/releases/download/v$version"}
temporary=$(mktemp -d "${TMPDIR:-/tmp}/herdr-fwd-plugin.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
asset_error="$temporary/asset-download-error"

if http_code=$(curl -sS -fL --retry 3 --connect-timeout 15 \
  --write-out '%{http_code}' "$base/$asset" -o "$temporary/$asset" \
  2> "$asset_error"); then
  :
else
  curl_status=$?
  cat "$asset_error" >&2
  die "failed to download $asset (curl status $curl_status, HTTP ${http_code:-unknown})"
fi

command -v tar >/dev/null 2>&1 || die "tar is required to install the plugin binary"
curl -sS -fL --retry 3 --connect-timeout 15 \
  "$base/SHA256SUMS" -o "$temporary/SHA256SUMS"

expected=$(awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1; exit }' "$temporary/SHA256SUMS")
[ -n "$expected" ] || die "release checksum is missing for $asset"
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$temporary/$asset" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$temporary/$asset" | awk '{print $1}')
else
  die "sha256sum or shasum is required to verify the plugin binary"
fi
[ "$actual" = "$expected" ] || die "checksum verification failed for $asset"

tar -xzf "$temporary/$asset" -C "$temporary" ./herdr-fwd-plugin
mkdir -p "$root/target/release"
install -m 755 "$temporary/herdr-fwd-plugin" \
  "$root/target/release/herdr-fwd-plugin"
preserve_onboarding
if [ "${HERDR_FWD_MANAGED_REMOTE_INSTALL:-}" != 1 ]; then
  clear_remote_origin
fi
printf 'Installed prebuilt remote plugin %s (%s).\n' "$version" "$platform"
