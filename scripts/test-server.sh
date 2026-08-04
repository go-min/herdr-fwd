#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
instance=${HERDR_FWD_LIMA_INSTANCE:-herdr-test}
ssh_port=${HERDR_FWD_LIMA_SSH_PORT:-2222}

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
command -v limactl >/dev/null 2>&1 || die "Lima is required: brew install lima"

lima() { limactl shell "$instance" -- "$@"; }

guest_home_path() {
  # shellcheck disable=SC2016 # HOME must expand inside the guest shell.
  lima bash -lc 'printf %s "$HOME"'
}

sync_repo() {
  guest_home=$(guest_home_path)
  guest_cache="$guest_home/.cache/herdr-fwd"
  guest_archive="$guest_cache/source.tar.gz"
  host_archive=$(mktemp "${TMPDIR:-/tmp}/herdr-fwd.XXXXXX.tar.gz")

  # Copy only source files into the writable guest filesystem. Excluding the
  # host target directory avoids transferring macOS artifacts into Linux.
  if [ "$(uname -s)" = Darwin ]; then
    COPYFILE_DISABLE=1 tar --no-xattrs -C "$root" --exclude='./.git' --exclude='./target' --exclude='./dist' --exclude='./.idea' -czf "$host_archive" . || archive_failed=1
  else
    tar -C "$root" --exclude='./.git' --exclude='./target' --exclude='./dist' --exclude='./.idea' -czf "$host_archive" . || archive_failed=1
  fi
  if [ "${archive_failed:-0}" -eq 1 ]; then
    rm -f "$host_archive"
    die "failed to archive the source checkout"
  fi
  lima mkdir -p "$guest_cache"
  if ! limactl copy "$host_archive" "$instance:$guest_archive"; then
    rm -f "$host_archive"
    die "failed to copy the source checkout into Lima"
  fi
  rm -f "$host_archive"
  lima bash -lc "set -eu
    # This VM is a disposable, single-project harness. Keep tools and user
    # configuration plus Cargo's guest-native target cache in place, but
    # remove stale project-owned source paths.
    rm -rf '$guest_home/.github' '$guest_home/.idea' '$guest_home/cmd' \
      '$guest_home/dist' '$guest_home/docker' '$guest_home/docs' \
      '$guest_home/internal' '$guest_home/scripts' '$guest_home/src' \
      '$guest_home/tests' '$guest_home/herdr-fwd'
    rm -f '$guest_home/Cargo.lock' '$guest_home/Cargo.toml' \
      '$guest_home/CHANGELOG.md' '$guest_home/CONTRIBUTING.md' \
      '$guest_home/deny.toml' '$guest_home/.gitignore' \
      '$guest_home/LICENSE' '$guest_home/Makefile' '$guest_home/README.md' \
      '$guest_home/rust-toolchain.toml' '$guest_home/SECURITY.md' \
      '$guest_home/herdr-plugin.toml'
    tar -xzf '$guest_archive' -C '$guest_home'
    rm -f '$guest_archive'
  "
}

provision() {
  sync_repo
  guest_home=$(guest_home_path)
  lima bash -lc "set -eu
    export PATH=\"\$HOME/.local/bin:\$PATH\"
    sudo apt-get update
    sudo apt-get install -y --no-install-recommends ca-certificates caddy cargo curl git lsof make nginx nodejs npm python3 rustc unzip
    if ! command -v bun >/dev/null 2>&1; then
      curl -fsSL https://bun.sh/install | BUN_INSTALL=\$HOME/.local bash
    fi
    if ! command -v deno >/dev/null 2>&1; then
      curl -fsSL https://deno.land/install.sh | DENO_INSTALL=\$HOME/.local sh
    fi
    herdr_version=0.8.0
    herdr_asset=herdr-linux-aarch64
    herdr_sha256=f647ac66468d9efbc642fe534fb284468f0aea60641606fc008dfc0d82a3ca87
    case \$(uname -m) in
      aarch64|arm64) ;;
      *)
        printf '%s\\n' \"unsupported Lima guest architecture for Herdr \$herdr_version: \$(uname -m)\" >&2
        exit 1
        ;;
    esac
    if ! herdr --version 2>/dev/null | grep -Fx \"herdr \$herdr_version\" >/dev/null; then
      herdr_download=\$(mktemp)
      trap 'rm -f "\$herdr_download"' EXIT HUP INT TERM
      curl -fsSL \"https://github.com/herdrdev/herdr/releases/download/v\$herdr_version/\$herdr_asset\" -o \"\$herdr_download\"
      printf '%s  %s\\n' \"\$herdr_sha256\" \"\$herdr_download\" | sha256sum -c -
      install -m 755 \"\$herdr_download\" \"\$HOME/.local/bin/herdr\"
      rm -f "\$herdr_download"
      trap - EXIT HUP INT TERM
    fi
    cd '$guest_home'
    CARGO_BUILD_JOBS=1 \
      CARGO_PROFILE_RELEASE_LTO=false \
      CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
      cargo build --locked --release --bin herdr-fwd-plugin
    scripts/test-dev-server --prepare
    herdr plugin link '$guest_home'
    # A stopped server may return ENOENT while the enabled state is already
    # persisted. Accept that narrow case only after confirming the registry.
    if ! herdr plugin enable herdr.fwd; then
      herdr plugin list | grep -F 'herdr.fwd' | grep -F 'enabled' >/dev/null
    fi
  "
}

configure_ssh() {
  mkdir -p "$HOME/.ssh"
  chmod 700 "$HOME/.ssh"
  touch "$HOME/.ssh/config"
  chmod 600 "$HOME/.ssh/config"
  lima_config="$HOME/.lima/$instance/ssh.config"
  [ -f "$lima_config" ] || die "Lima SSH config not found: $lima_config (run '$0 up' first)"
  include_line="Include $lima_config"
  if [ "$(sed -n '1p' "$HOME/.ssh/config")" != "$include_line" ]; then
    tmp_config=$(mktemp "$HOME/.ssh/config.XXXXXX")
    {
      printf '%s\n' "$include_line"
      awk -v include_line="$include_line" '$0 != include_line { print }' "$HOME/.ssh/config"
    } > "$tmp_config"
    chmod 600 "$tmp_config"
    mv "$tmp_config" "$HOME/.ssh/config"
  fi
  printf 'Added Lima SSH config include to %s\n' "$HOME/.ssh/config"
}

case "${1:-}" in
  up)
    if limactl list --format '{{.Name}}' 2>/dev/null | grep -Fxq "$instance"; then
      # `limactl edit` cannot modify a running instance. `start` is safe for
      # both stopped and already-running instances and keeps this command
      # idempotent.
      limactl start "$instance"
    else
      vm_type="qemu"
      if [ "$(uname -s)" = Darwin ]; then vm_type="vz"; fi
      # The checkout is copied into the guest below, so no host mount is
      # needed. This avoids read-only host mounts and stale absolute paths.
      limactl start --name "$instance" --vm-type "$vm_type" --ssh-port "$ssh_port" --mount-none
    fi
    provision
    configure_ssh
    printf '\nLima SSH config is generated at ~/.lima/%s/ssh.config\n' "$instance"
    printf 'Connect with: ssh lima-%s\n' "$instance"
    printf 'Start dev server: %s dev\n' "$0"
    ;;
  down) limactl stop "$instance" ;;
  destroy) limactl delete --force "$instance" ;;
  status) limactl list "$instance" ;;
  shell) lima bash ;;
  dev)
    guest_home=$(guest_home_path)
    requested_port=${2:-3000}
    case "$requested_port" in
      ''|*[!0-9]*) die "port must be a number between 1 and 65535" ;;
    esac
    if [ "$requested_port" -lt 1 ] || [ "$requested_port" -gt 65535 ]; then
      die "port must be a number between 1 and 65535"
    fi
    lima "$guest_home/scripts/test-dev-server" "$requested_port"
    ;;
  install-plugin) provision ;;
  configure-ssh) configure_ssh ;;
  ssh-config) limactl list --format '{{.SSHConfigFile}}' "$instance" ;;
  *)
    printf 'usage: %s {up|down|destroy|status|shell|dev|install-plugin|configure-ssh|ssh-config}\n' "$0" >&2
    exit 2
    ;;
esac
