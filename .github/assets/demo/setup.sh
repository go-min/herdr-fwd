#!/usr/bin/env bash
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
demo_root=${HERDR_FWD_DEMO_ROOT:-/private/tmp/herdr-fwd-vhs}
demo_port=${HERDR_FWD_DEMO_PORT:-21373}

if [ -z "${HERDR_FWD_DEMO_BIN:-}" ]; then
  export HERDR_FWD_DEMO_BIN="$repo_root/target/release/herdr-fwd-plugin"
fi
[ -x "$HERDR_FWD_DEMO_BIN" ] || {
  printf 'README demo binary is missing: %s (run make build)\n' "$HERDR_FWD_DEMO_BIN" >&2
  return 1 2>/dev/null || exit 1
}

case "$demo_root" in
  /private/tmp/herdr-fwd-vhs*|/tmp/herdr-fwd-vhs*) ;;
  *)
    printf 'README demo refuses unsafe root: %s\n' "$demo_root" >&2
    return 1 2>/dev/null || exit 1
    ;;
esac

herdr_fwd_demo_cleanup() {
  if [ -f "$demo_root/companion.pid" ]; then
    kill "$(cat "$demo_root/companion.pid")" 2>/dev/null || true
  fi
}

herdr_fwd_demo_cleanup
rm -rf "$demo_root"
mkdir -p "$demo_root/home" "$demo_root/state" "$demo_root/config"

python3 "$repo_root/.github/assets/demo/companion.py" \
  --port "$demo_port" --ready "$demo_root/ready" \
  --pane-map "$demo_root/panes.json" \
  >"$demo_root/companion.log" 2>&1 &
companion_pid=$!
printf '%s\n' "$companion_pid" >"$demo_root/companion.pid"

attempt=0
while [ ! -s "$demo_root/ready" ]; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 40 ]; then
    cat "$demo_root/companion.log" >&2 || true
    printf 'README demo companion did not start\n' >&2
    return 1 2>/dev/null || exit 1
  fi
  sleep 0.05
done

cat >"$demo_root/session.json" <<EOF
{
  "protocolVersion": 1,
  "sessionId": "0123456789abcdef01234567",
  "token": "abababababababababababababababababababababababababababababababab",
  "rpcUrl": "http://127.0.0.1:$demo_port",
  "autoDetect": true
}
EOF

export HOME="$demo_root/home"
export XDG_CONFIG_HOME="$demo_root/config"
export XDG_STATE_HOME="$demo_root/state"
export HERDR_FWD_DEMO_SESSION="$demo_root/session.json"
export HERDR_BIN_PATH="$repo_root/.github/assets/demo/herdr"
export PS1='demo ❯ '
unset NO_COLOR
export TERM=xterm-256color
export CLICOLOR_FORCE=1
export FORCE_COLOR=1
