#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
prefix=${HERDR_FWD_PREFIX:-${PREFIX:-"$HOME/.local"}}
remote_target=

usage() {
  cat <<'EOF'
usage: uninstall.sh [--prefix PATH] [--remote TARGET]

Removes the local hfwd command. With --remote, it first disables and removes the
GitHub-managed plugin from the SSH target. Active forwarding sessions are
refused; disconnect them first.
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
    --remote)
      [ "$#" -ge 2 ] || die "--remote requires an SSH target"
      remote_target=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

installed="$prefix/bin/hfwd"
manager=$installed
if [ ! -x "$manager" ] && [ -x "$root/target/release/hfwd" ]; then
  manager="$root/target/release/hfwd"
fi

if [ -n "$remote_target" ]; then
  [ -x "$manager" ] || die "build or install hfwd before remote uninstall"
  "$manager" remote uninstall "$remote_target"
fi

if [ -x "$manager" ]; then
  # This also prunes stale session files before the active-session guard.
  "$manager" list >/dev/null 2>&1 || true
fi

state_root=${XDG_RUNTIME_DIR:-"$HOME/.cache"}/herdr-fwd
for state in "$state_root"/session-*.json; do
  [ -e "$state" ] || continue
  die "an active forwarding session exists; disconnect it before uninstalling"
done

if [ -e "$installed" ]; then
  rm -f "$installed"
  printf 'Removed hfwd: %s\n' "$installed"
else
  printf 'hfwd is not installed at %s\n' "$installed"
fi
