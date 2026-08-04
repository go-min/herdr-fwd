#!/usr/bin/env bash
# shellcheck disable=SC2029 # Validated local identifiers are intentionally expanded into remote commands.
set -euo pipefail

repo_root=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
instance=${HERDR_FWD_LIMA_INSTANCE:-herdr-test}
target=${HERDR_FWD_DEMO_TARGET:-lima-$instance}
demo_root=${HERDR_FWD_DEMO_ROOT:-/private/tmp/herdr-fwd-vhs-overview}
local_herdr=${HERDR_FWD_DEMO_HERDR:-$(command -v herdr)}
session=${HERDR_FWD_DEMO_SESSION:-herdr-fwd-overview-$RANDOM-$RANDOM}
herdr_theme=${HERDR_FWD_DEMO_THEME:-dracula}

demo_parent=${demo_root%/*}
demo_name=${demo_root##*/}
case "$demo_parent:$demo_name" in
  /private/tmp:herdr-fwd-vhs-overview*|/tmp:herdr-fwd-vhs-overview*) ;;
  *) printf 'Overview demo refuses unsafe root: %s\n' "$demo_root" >&2; exit 1 ;;
esac
[ ! -L "$demo_root" ] || {
  printf 'Overview demo refuses a symlink root: %s\n' "$demo_root" >&2
  exit 1
}
case "/$demo_root/" in
  */../*|*/./*) printf 'Overview demo refuses non-canonical root: %s\n' "$demo_root" >&2; exit 1 ;;
esac
case "$target" in
  lima-*) ;;
  *) printf 'Overview demo refuses non-Lima target: %s\n' "$target" >&2; exit 1 ;;
esac

mkdir -p "$demo_root"
case "$session" in
  *[!A-Za-z0-9-]*|'') printf 'Overview demo has invalid session name: %s\n' "$session" >&2; exit 1 ;;
esac

"$local_herdr" --version | grep -Fx 'herdr 0.8.0' >/dev/null || {
  printf 'Overview demo needs Herdr 0.8.0; set HERDR_FWD_DEMO_HERDR if it is not on PATH\n' >&2
  exit 1
}

cleanup() {
  if [ -n "${dashboard_action_pid:-}" ]; then
    kill "$dashboard_action_pid" 2>/dev/null || true
    wait "$dashboard_action_pid" 2>/dev/null || true
  fi
  ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; herdr session stop '$session' --json >/dev/null 2>&1 || true; herdr session delete '$session' --json >/dev/null 2>&1 || true; rm -f -- \"\$HOME/.cache/herdr-fwd/overview-server-$session.log\""
  if [ -n "${config_backup_ready:-}" ]; then
    ssh "$target" "config=\"\$HOME/.config/herdr/config.toml\"; backup=\"\$HOME/.cache/herdr-fwd/demo-config-$session\"; if [ -f \"\$backup\" ]; then mv -f \"\$backup\" \"\$config\"; elif [ -f \"\$backup.absent\" ]; then rm -f -- \"\$config\"; fi; rm -f -- \"\$backup\" \"\$backup.absent\"" || true
  fi
  rm -rf -- "$demo_root"
}
trap cleanup EXIT HUP INT TERM

ssh "$target" "set -eu; config=\"\$HOME/.config/herdr/config.toml\"; backup=\"\$HOME/.cache/herdr-fwd/demo-config-$session\"; install -d -m 700 \"\$HOME/.cache/herdr-fwd\"; rm -f -- \"\$backup\" \"\$backup.absent\"; if [ -f \"\$config\" ]; then cp -p \"\$config\" \"\$backup\"; else : > \"\$backup.absent\"; fi"
config_backup_ready=1

config=$(cat <<EOF
[theme]
name = "$herdr_theme"

[keys]

[[keys.command]]
key = "prefix+shift+f"
type = "plugin_action"
command = "herdr.fwd.open-dashboard-popup"
description = "Open port-forward dashboard"

[ui.sidebar.spaces]
rows = [["state_icon", "workspace"], ["branch", "git_status"], ["\$port_forward_status"]]

[ui.toast]
delivery = "herdr"
EOF
)
printf '%s\n' "$config" | ssh "$target" 'mkdir -p "$HOME/.config/herdr"; cat > "$HOME/.config/herdr/config.toml"'
ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; nohup herdr --session '$session' server >\"\$HOME/.cache/herdr-fwd/overview-server-$session.log\" 2>&1 &"

for _ in {1..40}; do
  if ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; herdr --session '$session' status server" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; herdr --session '$session' status server" >/dev/null

workspace=$(ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; herdr --session '$session' workspace create --cwd \"\$HOME\" --label 'Remote app'")
pane_id=$(printf '%s' "$workspace" | python3 -c 'import json, sys; print(json.load(sys.stdin)["result"]["root_pane"]["pane_id"])')
start_server() {
  local pane_id=$1 server=$2 port=$3
  ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; herdr --session '$session' pane send-text '$pane_id' 'export PS1=\"demo@remote:~\\$ \"; clear; \$HOME/scripts/test-dev-server --server $server $port'; herdr --session '$session' pane send-keys '$pane_id' enter"
  for _ in {1..80}; do
    if ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; herdr --session '$session' pane process-info --pane '$pane_id'" | grep -F -- "--port $port" >/dev/null; then
      return
    fi
    sleep 0.1
  done
  printf 'Overview demo server did not start: %s on %s\n' "$server" "$port" >&2
  return 1
}

start_server "$pane_id" storybook 6006
vite_pane=$(ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; herdr --session '$session' pane split '$pane_id' --direction right --no-focus" | python3 -c 'import json, sys; print(json.load(sys.stdin)["result"]["pane"]["pane_id"])')
start_server "$vite_pane" vite 5173
next_pane=$(ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; herdr --session '$session' pane split '$vite_pane' --direction down --no-focus" | python3 -c 'import json, sys; print(json.load(sys.stdin)["result"]["pane"]["pane_id"])')
start_server "$next_pane" next 4000

( sleep 24
  ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; herdr --session '$session' plugin action invoke wake --plugin herdr.fwd" \
    >"$demo_root/dashboard-action.log" 2>&1 || true
  sleep 8
  ssh "$target" "export PATH=\"\$HOME/.local/bin:\$PATH\"; herdr --session '$session' plugin action invoke open-dashboard-popup --plugin herdr.fwd" \
    >"$demo_root/dashboard-action.log" 2>&1 || true
) &
dashboard_action_pid=$!

PATH="$(dirname "$local_herdr"):$PATH" \
  "$repo_root/target/release/hfwd" "$target" -- --session "$session" --handoff --remote-keybindings server
