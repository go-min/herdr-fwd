#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

die() { printf 'release smoke: %s\n' "$*" >&2; exit 1; }

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64) platform=linux-x86_64 ;;
  Linux:aarch64|Linux:arm64) platform=linux-aarch64 ;;
  Darwin:x86_64|Darwin:amd64) platform=macos-x86_64 ;;
  Darwin:arm64|Darwin:aarch64) platform=macos-aarch64 ;;
  *) die "unsupported platform: $(uname -s)/$(uname -m)" ;;
esac

asset="herdr-fwd-$platform.tar.gz"
[ -f "$root/dist/$asset" ] || die "missing package: dist/$asset"

temporary=$(mktemp -d "${TMPDIR:-/tmp}/herdr-fwd-release-smoke.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
release="$temporary/release"
prefix="$temporary/prefix"
test_home="$temporary/home"
test_runtime="$temporary/runtime"
mkdir -p "$release" "$test_home" "$test_runtime"
chmod 700 "$test_home" "$test_runtime"
cp "$root/dist/$asset" "$release/$asset"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$release" && sha256sum "$asset") > "$release/SHA256SUMS"
elif command -v shasum >/dev/null 2>&1; then
  (cd "$release" && shasum -a 256 "$asset") > "$release/SHA256SUMS"
else
  die "sha256sum or shasum is required"
fi

if HERDR_FWD_RELEASE_BASE="file://$release" "$root/install.sh" \
  --prefix "$temporary/invalid-prefix" --version '../bad' \
  >"$temporary/invalid-stdout" 2>"$temporary/invalid-stderr"; then
  die "installer accepted an invalid release version"
fi
grep -F 'invalid release version: ../bad' "$temporary/invalid-stderr" >/dev/null

HOME="$test_home" XDG_RUNTIME_DIR="$test_runtime" HERDR_FWD_RELEASE_BASE="file://$release" \
  "$root/install.sh" --prefix "$prefix"
"$prefix/bin/hfwd" --version >/dev/null
[ ! -e "$prefix/bin/herdr-fwd" ] || die "legacy wrapper executable was installed"
plugin_config="$temporary/config-home/herdr/plugins/config/herdr.fwd"
XDG_CONFIG_HOME="$temporary/config-home" HERDR_FWD_RELEASE_BASE="file://$release" \
  "$root/scripts/install-plugin-binary.sh"
"$root/target/release/herdr-fwd-plugin"
[ "$(sed -n 's/^onboarding = //p' "$plugin_config/config.toml")" = true ] || \
  die "normal plugin install did not reset local onboarding"
[ -z "$(sed -n 's/^installation_source = //p' "$plugin_config/config.toml")" ] || \
  die "normal plugin install persisted installation source in config"
manual_state="$temporary/manual-state/herdr-fwd"
mkdir -p "$manual_state"
cat > "$manual_state/plugin-origin.toml" <<'EOF'
origin = "hfwd_remote"
plugin_root = "/remote/plugin"
version = "0.1.3"
EOF
XDG_CONFIG_HOME="$temporary/config-home" XDG_STATE_HOME="$temporary/manual-state" \
  HERDR_FWD_RELEASE_BASE="file://$release" "$root/scripts/install-plugin-binary.sh" >/dev/null
[ ! -e "$manual_state/plugin-origin.toml" ] || \
  die "manual plugin reinstall retained remote provenance"
cat > "$manual_state/plugin-origin.toml" <<'EOF'
origin = "hfwd_remote"
plugin_root = "/remote/plugin"
version = "0.1.3"
EOF
XDG_CONFIG_HOME="$temporary/config-home" XDG_STATE_HOME="$temporary/manual-state" \
  HERDR_FWD_MANAGED_REMOTE_INSTALL=1 HERDR_FWD_RELEASE_BASE="file://$release" \
  "$root/scripts/install-plugin-binary.sh" >/dev/null
[ -f "$manual_state/plugin-origin.toml" ] || \
  die "hfwd-managed plugin install cleared remote provenance"
cat > "$plugin_config/config.toml" <<'EOF'
onboarding = false
after_forward = "popup"
process_tree_depth = 7
EOF
XDG_CONFIG_HOME="$temporary/config-home" HERDR_FWD_RELEASE_BASE="file://$release" \
  "$root/scripts/install-plugin-binary.sh" >/dev/null
[ "$(sed -n 's/^onboarding = //p' "$plugin_config/config.toml")" = true ] || \
  die "local reinstall did not reset onboarding"
[ -z "$(sed -n 's/^installation_source = //p' "$plugin_config/config.toml")" ] || \
  die "local reinstall retained installation source in config"
grep -F 'after_forward = "popup"' "$plugin_config/config.toml" >/dev/null || \
  die "local reinstall discarded after_forward"
grep -F 'process_tree_depth = 7' "$plugin_config/config.toml" >/dev/null || \
  die "local reinstall discarded process_tree_depth"
wrapper_checkout="$temporary/wrapper-checkout"
mkdir -p "$wrapper_checkout/scripts"
cp "$root/herdr-plugin.toml" "$wrapper_checkout/herdr-plugin.toml"
cp "$root/scripts/install-plugin-binary.sh" "$wrapper_checkout/scripts/install-plugin-binary.sh"
HERDR_PLUGIN_CONFIG_DIR="$temporary/wrapper-config" \
  HERDR_FWD_RELEASE_BASE="file://$release" \
  "$wrapper_checkout/scripts/install-plugin-binary.sh" >/dev/null
[ "$(sed -n 's/^onboarding = //p' "$temporary/wrapper-config/config.toml")" = true ] || \
  die "wrapper plugin install did not reset onboarding"

cat > "$temporary/wrapper-config/config.toml" <<'EOF'
onboarding = false
after_forward = "nothing"
process_tree_depth = 6
EOF
HERDR_PLUGIN_CONFIG_DIR="$temporary/wrapper-config" \
  HERDR_FWD_RELEASE_BASE="file://$release" \
  "$wrapper_checkout/scripts/install-plugin-binary.sh" >/dev/null
[ "$(sed -n 's/^onboarding = //p' "$temporary/wrapper-config/config.toml")" = true ] || \
  die "remote reinstall did not reset onboarding"
grep -F 'after_forward = "nothing"' "$temporary/wrapper-config/config.toml" >/dev/null || \
  die "remote reinstall discarded after_forward"
grep -F 'process_tree_depth = 6' "$temporary/wrapper-config/config.toml" >/dev/null || \
  die "remote reinstall discarded process_tree_depth"

fallback_checkout="$temporary/fallback-checkout"
fake_bin="$temporary/fake-bin"
mkdir -p "$fallback_checkout/scripts" "$fake_bin"
cp "$root/Cargo.toml" "$fallback_checkout/Cargo.toml"
cp "$root/herdr-plugin.toml" "$fallback_checkout/herdr-plugin.toml"
cp "$root/scripts/install-plugin-binary.sh" \
  "$fallback_checkout/scripts/install-plugin-binary.sh"
cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
printf 'simulated missing release asset\n' >&2
printf '404'
exit 22
EOF
chmod 755 "$fake_bin/curl"
HERDR_PLUGIN_CONFIG_DIR="$temporary/fallback-config" PATH="$fake_bin:$PATH" \
  "$fallback_checkout/scripts/install-plugin-binary.sh" \
  > "$temporary/fallback-stdout" 2> "$temporary/fallback-stderr" && \
  die "missing release asset unexpectedly installed a source build"
grep -F 'failed to download' "$temporary/fallback-stderr" >/dev/null || \
  die "missing release asset did not return an actionable download error"

HOME="$test_home" XDG_RUNTIME_DIR="$test_runtime" \
  "$root/uninstall.sh" --prefix "$prefix" >/dev/null
[ ! -e "$prefix/bin/hfwd" ] || die "uninstall left the wrapper behind"
printf 'Release install and plugin package smoke passed for %s.\n' "$platform"
