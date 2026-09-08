#!/usr/bin/env python3
"""Exercise real hfwd/Herdr/OpenSSH over an isolated loopback sshd (Unix).

Requires built release binaries, Herdr 0.9.x, sshd, ssh-keygen, ps, and lsof.
Only temporary keys/configs and one uniquely named Herdr session are used.
"""
import fcntl
import getpass
import json
import os
from pathlib import Path
import pty
import shlex
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import urllib.request

REPO = Path(__file__).resolve().parent.parent


def eventually(check, timeout=15):
    deadline = time.monotonic() + timeout
    last_error = None
    while time.monotonic() < deadline:
        try:
            result = check()
            if result:
                return result
        except (OSError, ValueError, KeyError, AssertionError, RuntimeError) as error:
            last_error = error
        time.sleep(0.1)
    raise AssertionError(f"condition timed out: {last_error}")


def unused_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def run(*args, **kwargs):
    try:
        return subprocess.run(args, check=True, capture_output=True, text=True, timeout=15, **kwargs).stdout
    except subprocess.CalledProcessError as error:
        raise RuntimeError(f"{args[0]} failed: {error.stderr}") from error


def stop(process):
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def main():
    herdr = shutil.which("herdr")
    sshd = shutil.which("sshd") or "/usr/sbin/sshd"
    ssh = shutil.which("ssh")
    assert herdr and ssh and Path(sshd).exists(), "Herdr and OpenSSH server/client are required"
    assert run(herdr, "--version").startswith("herdr 0.9."), "requires Herdr 0.9.x"
    wrapper = REPO / "target/release/hfwd"
    plugin = REPO / "target/release/herdr-fwd-plugin"
    assert wrapper.is_file() and plugin.is_file(), "run make build first"
    ssh_server = herdr_server = None
    clients = []
    collision = None
    with tempfile.TemporaryDirectory(prefix="hse-", dir="/tmp") as directory:
        root = Path(directory).resolve()
        session = "hse-" + root.name.rsplit("-", 1)[1]
        alias = session
        remote_sessions = Path.home() / ".cache/herdr-fwd/sessions" / ("session-" + session.encode().hex())
        assert not remote_sessions.exists(), "refusing to reuse an existing session directory"
        for folder in ("bin", "fixture", "remote-config/herdr/plugins/config/herdr.fwd", "remote-state", "local-config", "runtime", "local-state"):
            (root / folder).mkdir(parents=True)
        remote_env = dict(os.environ, XDG_CONFIG_HOME=str(root / "remote-config"), XDG_STATE_HOME=str(root / "remote-state"), HERDR_CONFIG_PATH=str(root / "remote-config/herdr/config.toml"))
        for key in ("HERDR_ENV", "HERDR_SOCKET_PATH", "HERDR_SESSION", "HERDR_FWD_SESSION_PATH", "HERDR_PLUGIN_CONFIG_DIR"):
            remote_env.pop(key, None)
        (root / "remote-config/herdr/config.toml").write_text('[terminal]\ndefault_shell = "/bin/sh"\nshell_mode = "non_login"\n')
        (root / "remote-config/herdr/plugins/config/herdr.fwd/config.toml").write_text('onboarding = false\nafter_forward = "space"\n')
        manifest = (REPO / "herdr-plugin.toml").read_text().replace("./target/release/herdr-fwd-plugin", str(plugin))
        (root / "fixture/herdr-plugin.toml").write_text(manifest)
        run(herdr, "plugin", "link", str(root / "fixture"), env=remote_env)
        for key in ("host", "client"):
            run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / key))
        port = unused_port()
        bootstrap = root / "remote.sh"
        bootstrap.write_text("#!/bin/sh\n" + "\n".join("export " + name + "=" + shlex.quote(remote_env[name]) for name in ("XDG_CONFIG_HOME", "XDG_STATE_HOME", "HERDR_CONFIG_PATH")) + '\nexec /bin/sh -c "$SSH_ORIGINAL_COMMAND"\n')
        bootstrap.chmod(0o700)
        (root / "sshd_config").write_text(f'''Port {port}
ListenAddress 127.0.0.1
HostKey {root}/host
PidFile {root}/sshd.pid
AuthorizedKeysFile {root}/client.pub
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
AllowUsers {getpass.getuser()}
AllowTcpForwarding yes
PermitTTY yes
ForceCommand {bootstrap}
LogLevel ERROR
''')
        (root / "ssh_config").write_text(f'''Host {alias}
  HostName 127.0.0.1
  Port {port}
  User {getpass.getuser()}
  IdentityFile {root}/client
  IdentitiesOnly yes
  IdentityAgent none
  StrictHostKeyChecking no
  UserKnownHostsFile {root}/known_hosts
  BatchMode yes
  ConnectTimeout 3
''')
        # Supply only fixture SSH config, including when Herdr supplies its
        # generated -F config. The actual OpenSSH executable handles every hop.
        shim = root / "bin/ssh"
        shim.write_text(f'''#!{sys.executable}
import os,sys
args=[]
i=1
while i<len(sys.argv):
    if sys.argv[i]=='-F': i+=2
    else: args.append(sys.argv[i]); i+=1
os.execv({ssh!r},[{ssh!r},'-F',{str(root/'ssh_config')!r}]+args)
''')
        shim.chmod(0o700)
        local_env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"], XDG_CONFIG_HOME=str(root / "local-config"), XDG_STATE_HOME=str(root / "local-state"), XDG_RUNTIME_DIR=str(root / "runtime"), HERDR_CONFIG_PATH=str(root / "local-config/herdr.toml"), TERM="xterm-256color")
        for key in ("HERDR_ENV", "HERDR_SOCKET_PATH", "HERDR_SESSION"):
            local_env.pop(key, None)
        (root / "local-config/herdr.toml").write_text('[ui.toast]\ndelivery = "off"\n')

        def start_sshd():
            return subprocess.Popen([sshd, "-D", "-e", "-f", str(root / "sshd_config")], stdout=subprocess.DEVNULL, stderr=open(root / "sshd.log", "ab"))

        def remote(*args):
            return run(ssh, "-F", str(root / "ssh_config"), alias, shlex.join(args))

        def start_client(*extra):
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 110, 0, 0))
            def controlling_terminal():
                os.setsid()
                fcntl.ioctl(0, termios.TIOCSCTTY, 0)
            child = subprocess.Popen([str(wrapper), alias, *extra, "--", "--session", session], env=local_env, stdin=slave, stdout=slave, stderr=slave, preexec_fn=controlling_terminal)
            os.close(slave)
            clients.append(child)
            def drain():
                with open(root / f"client-{child.pid}.log", "wb") as output:
                    try:
                        while chunk := os.read(master, 65536):
                            output.write(chunk)
                            output.flush()
                    except OSError:
                        pass
                    finally:
                        os.close(master)
            threading.Thread(target=drain, daemon=True).start()
            return child

        def state(child):
            for path in (root / "runtime/herdr-fwd").glob("session-*.json"):
                value = json.loads(path.read_text())
                if value["wrapperPid"] == child.pid:
                    return value
            return None

        def body(local_port):
            with urllib.request.urlopen(f"http://127.0.0.1:{local_port}", timeout=2) as response:
                return response.read() == b"hfwd-ssh-e2e"

        def api(child, path, data):
            current = state(child)
            request = urllib.request.Request(current["companionUrl"] + path, data=json.dumps(data).encode(), headers={"Authorization": "Bearer " + current["token"], "Content-Type": "application/json"})
            return json.load(urllib.request.urlopen(request, timeout=4))

        def master_pid(child):
            for line in run("ps", "-axo", "pid=,ppid=,command=").splitlines():
                fields = line.split(None, 2)
                if len(fields) == 3 and fields[1] == str(child.pid) and " -M " in fields[2] and alias in fields[2]:
                    return int(fields[0])
            raise AssertionError("wrapper SSH master missing")

        try:
            ssh_server = start_sshd()
            eventually(lambda: remote("printf", "ready") == "ready")
            herdr_server = subprocess.Popen([herdr, "--session", session, "server"], env=remote_env, stdout=open(root / "herdr.log", "wb"), stderr=subprocess.STDOUT)
            eventually(lambda: remote(herdr, "--session", session, "status", "server"))
            child = start_client()
            eventually(lambda: state(child))
            workspace = json.loads(remote(herdr, "--session", session, "workspace", "create", "--cwd", str(root), "--label", "SSH test"))["result"]
            pane = workspace["root_pane"]["pane_id"]
            ports = [unused_port(), unused_port()]
            listener = root / "listener.py"
            listener.write_text('''import http.server,threading,time
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200);self.end_headers();self.wfile.write(b'hfwd-ssh-e2e')
 def log_message(self,*args): pass
time.sleep(2)
''' + "\n".join(f"threading.Thread(target=http.server.HTTPServer(('127.0.0.1',{port}),Handler).serve_forever,daemon=True).start()" for port in ports) + "\nthreading.Event().wait()\n")
            began = time.monotonic()
            remote(herdr, "--session", session, "pane", "run", pane, shlex.join([sys.executable, str(listener)]))
            forwards = eventually(lambda: (state(child) or {}).get("forwards") if len((state(child) or {}).get("forwards", [])) == 2 else None, 12)
            assert time.monotonic() - began < 12, "foreground discovery fell back to the slow scan"
            assert all(f["localPort"] != f["remotePort"] and body(f["localPort"]) for f in forwards)
            print("PASS: real SSH attach, delayed listener discovery, occupied local ports, HTTP through tunnels", flush=True)
            paused, live = forwards
            api(child, f'/v1/forwards/{paused["id"]}/toggle', {"enabled": False})
            # A missing listener plus killed master simulates a transport outage;
            # the independently running remote Herdr server keeps its panes.
            original_markers = eventually(lambda: list(remote_sessions.glob("*.dashboard.json")))
            original_marker = original_markers[0].read_text()
            stop(ssh_server)
            os.kill(master_pid(child), signal.SIGKILL)
            collision = socket.socket()
            collision.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            eventually(lambda: bind_collision(collision, live["localPort"]))
            time.sleep(12)
            ssh_server = start_sshd()
            recovered = eventually(lambda: recovered_mapping(state(child), live, paused), 30)
            eventually(lambda: body(recovered["localPort"]))
            assert child.poll() is None
            print("PASS: SSH outage recovery, occupied-port remap, paused mapping preserved", flush=True)
            assert original_markers[0].exists() and original_markers[0].read_text() == original_marker, "recoverable outage destroyed the dashboard"
            print("PASS: forwarding dashboard survives a 12-second SSH outage", flush=True)
            peer = start_client("--no-auto-detect")
            eventually(lambda: state(peer))
            eventually(lambda: len([path for path in remote_sessions.glob("session-*.json") if not path.name.endswith(".dashboard.json")]) == 2)
            ambiguous = subprocess.run([ssh, "-F", str(root / "ssh_config"), alias, f'HERDR_SESSION={shlex.quote(session)} {shlex.quote(str(plugin))} open-dashboard-popup'], capture_output=True, text=True, timeout=10)
            assert ambiguous.returncode != 0 and "2 forwarding sessions" in ambiguous.stderr, ambiguous.stderr
            stop(peer)
            eventually(lambda: len([path for path in remote_sessions.glob("session-*.json") if not path.name.endswith(".dashboard.json")]) == 1)
            assert body(recovered["localPort"])
            print("PASS: ambiguous dashboard rejected; peer disconnect leaves other tunnels alive", flush=True)
            api(child, f'/v1/forwards/{paused["id"]}/toggle', {"enabled": True})
            remote(herdr, "--session", session, "workspace", "close", workspace["workspace"]["workspace_id"])
            eventually(lambda: state(child) is not None and not state(child)["forwards"], 12)
            stop(child)
            eventually(lambda: state(child) is None)
            eventually(lambda: not list(remote_sessions.glob("session-*.json")), 75)
            print("PASS: process exit removes mappings; wrapper exit cleans session files", flush=True)
            # A sustained outage must terminate cleanly instead of retrying forever.
            failed_client = start_client()
            eventually(lambda: state(failed_client))
            failure_workspace = json.loads(remote(herdr, "--session", session, "workspace", "create", "--cwd", str(root), "--label", "Failure test"))["result"]
            failure_listener = root / "failure-listener.py"
            failure_listener.write_text(listener.read_text().replace(str(ports[0]), str(unused_port())).replace(str(ports[1]), str(unused_port())))
            remote(herdr, "--session", session, "pane", "run", failure_workspace["root_pane"]["pane_id"], shlex.join([sys.executable, str(failure_listener)]))
            failure_forwards = eventually(lambda: (state(failed_client) or {}).get("forwards"), 12)
            stop(ssh_server)
            os.kill(master_pid(failed_client), signal.SIGKILL)
            eventually(lambda: failed_client.poll() is not None, 45)
            assert failed_client.returncode != 0
            eventually(lambda: state(failed_client) is None)
            assert all(port_closed(forward["localPort"]) for forward in failure_forwards)
            assert "could not restore SSH forwards" in (root / f"client-{failed_client.pid}.log").read_text(errors="replace")
            eventually(lambda: not list(remote_sessions.glob("session-*.json")), 75)
            print("PASS: sustained outage stops retrying and leaves no forwarding state or listeners", flush=True)
        except BaseException:
            # Retain diagnostic logs, never session files or bearer tokens.
            evidence = REPO / "target/ssh-e2e-failure"
            evidence.mkdir(parents=True, exist_ok=True)
            for log in root.glob("*.log"):
                shutil.copyfile(log, evidence / log.name)
            print(f"SSH test logs: {evidence}", file=sys.stderr)
            raise
        finally:
            for child in clients:
                stop(child)
            if collision:
                collision.close()
            try:
                if herdr_server and herdr_server.poll() is None:
                    run(herdr, "--session", session, "server", "stop", env=remote_env)
            finally:
                stop(herdr_server)
                stop(ssh_server)
                shutil.rmtree(remote_sessions, ignore_errors=True)


def port_closed(port):
    try:
        connection = socket.create_connection(("127.0.0.1", port), timeout=1)
    except OSError:
        return True
    connection.close()
    return False


def bind_collision(listener, port):
    listener.bind(("127.0.0.1", port))
    listener.listen()
    return True


def recovered_mapping(current, original, paused):
    if not current:
        return None
    mappings = {f["id"]: f for f in current["forwards"]}
    assert not mappings[paused["id"]]["enabled"]
    forward = mappings[original["id"]]
    return forward if forward["enabled"] and forward["localPort"] != original["localPort"] else None


if __name__ == "__main__":
    main()
