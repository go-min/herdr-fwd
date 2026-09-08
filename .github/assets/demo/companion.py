#!/usr/bin/env python3
"""Deterministic loopback companion fixture for the README dashboard demo."""

import argparse
import json
import time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def initial_forwards():
    now = int(time.time())
    return [
        {"id": "fwd-vite", "remotePort": 5173, "localPort": 5173, "remoteHost": "127.0.0.1", "paneId": "pane-web", "process": "Vite", "detectedUrl": "http://localhost:5173/", "enabled": True, "automatic": True, "serverStartedAt": now - 480, "processId": 4242, "tunnelOpenedAt": now - 180},
        {"id": "fwd-storybook", "remotePort": 6006, "localPort": 6006, "remoteHost": "127.0.0.1", "paneId": "pane-storybook", "process": "Storybook", "detectedUrl": "http://localhost:6006/", "enabled": True, "automatic": True, "serverStartedAt": now - 720, "processId": 4343, "tunnelOpenedAt": now - 210},
        {"id": "fwd-api", "remotePort": 8080, "localPort": 8080, "remoteHost": "127.0.0.1", "paneId": "manual", "process": "", "detectedUrl": "http://localhost:8080/api", "enabled": False, "automatic": False, "serverStartedAt": None, "processId": None, "tunnelOpenedAt": 0},
    ]


FORWARDS = initial_forwards()
LOCAL_SETTINGS = {
    "sidebar_ports_enabled": False,
    "toast_delivery": None,
    "popup_shortcut_enabled": False,
    "theme_name": None,
}


def forwards_with_runtime_panes(pane_map_path):
    pane_map = {}
    if pane_map_path:
        try:
            pane_map = json.loads(Path(pane_map_path).read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            pass
    return [{**forward, "paneId": pane_map.get(forward["id"], forward["paneId"])} for forward in FORWARDS]


class Handler(BaseHTTPRequestHandler):
    def send_json(self, payload, status=200):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/v1/settings/local":
            self.send_json(LOCAL_SETTINGS)
            return
        if self.path == "/v1/forwards":
            self.send_json(forwards_with_runtime_panes(args.pane_map))
            return
        self.send_error(404)

    def do_POST(self):
        if self.path == "/v1/settings/notifications":
            LOCAL_SETTINGS["toast_delivery"] = "herdr"
            self.send_json(LOCAL_SETTINGS)
            return
        if self.path == "/v1/settings/sidebar":
            try:
                length = int(self.headers.get("Content-Length", "0"))
                payload = json.loads(self.rfile.read(length))
                enabled = payload["enabled"]
                if not isinstance(enabled, bool):
                    raise ValueError("expected boolean")
            except (json.JSONDecodeError, KeyError, ValueError):
                self.send_error(400, "expected a JSON enabled boolean")
                return
            LOCAL_SETTINGS["sidebar_ports_enabled"] = enabled
            self.send_json(LOCAL_SETTINGS)
            return
        prefix = "/v1/forwards/"
        suffix = "/toggle"
        if not self.path.startswith(prefix) or not self.path.endswith(suffix):
            self.send_error(404)
            return
        forward_id = self.path[len(prefix) : -len(suffix)]
        try:
            length = int(self.headers.get("Content-Length", "0"))
            payload = json.loads(self.rfile.read(length))
            enabled = payload["enabled"]
        except (json.JSONDecodeError, KeyError, ValueError):
            self.send_error(400, "expected a JSON enabled boolean")
            return
        if not isinstance(enabled, bool):
            self.send_error(400, "expected a JSON enabled boolean")
            return
        for forward in FORWARDS:
            if forward["id"] == forward_id:
                forward["enabled"] = enabled
                forward["tunnelOpenedAt"] = int(time.time()) if enabled else 0
                self.send_json(forward)
                return
        self.send_error(404)

    def log_message(self, _format, *_args):
        pass


parser = argparse.ArgumentParser()
parser.add_argument("--port", type=int, required=True)
parser.add_argument("--ready", required=True)
parser.add_argument("--pane-map")
args = parser.parse_args()

server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
with open(args.ready, "w", encoding="utf-8") as ready:
    ready.write(str(server.server_address[1]))
server.serve_forever()
