#!/usr/bin/env python3
"""End-to-end check for the on-demand `window_content` tool.

A fake compositor describes one browser-like window, a mock AT-SPI helper
returns a short preview for ordinary sends and a larger page for the deep
capture, and a local SSE server drives the model through one tool call.

No real desktop window, model, credential, or browser profile is touched.
"""
import http.server
import json
import os
import pathlib
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / "target/debug"

requests = []


class Endpoint(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(body)
        tool_messages = [m for m in body["messages"] if m.get("role") == "tool"]
        if not tool_messages:
            names = [tool["function"]["name"] for tool in body.get("tools", [])]
            assert "window_content" in names, names
            context = next(
                m["content"]
                for m in body["messages"]
                if m.get("role") == "user"
                and isinstance(m.get("content"), str)
                and "Context for this request" in m["content"]
            )
            assert "preview alpha" in context, context
            assert '"status": "on-demand"' in context, context
            assert '"tool": "window_content"' in context, context
            delta = {
                "tool_calls": [
                    {
                        "index": 0,
                        "id": "call_content",
                        "type": "function",
                        "function": {
                            "name": "window_content",
                            "arguments": json.dumps(
                                {"action": "search", "query": "unique-needle"}
                            ),
                        },
                    }
                ]
            }
            events = [
                {"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        elif len(tool_messages) == 1:
            result = tool_messages[-1]["content"]
            assert "unique-needle" in result, result
            delta = {
                "tool_calls": [
                    {
                        "index": 0,
                        "id": "call_read",
                        "type": "function",
                        "function": {
                            "name": "window_content",
                            "arguments": json.dumps(
                                {"action": "read", "offset": 0, "limit": 1000}
                            ),
                        },
                    }
                ]
            }
            events = [
                {"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        else:
            result = tool_messages[-1]["content"]
            assert "full page alpha" in result, result
            delta = {"content": "Found the page text."}
            events = [
                {"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            ]
        payload = "".join(
            "data: " + json.dumps(event) + "\n\n" for event in events
        ) + "data: [DONE]\n\n"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload.encode())))
        self.end_headers()
        self.wfile.write(payload.encode())


def main():
    assert (BIN / "buddyd").exists(), "Run cargo build --workspace first"
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix="drops-content-") as directory:
            base = pathlib.Path(directory)
            tools = base / "bin"
            tools.mkdir()
            runtime = base / "run"
            hypr = runtime / "hypr/test"
            hypr.mkdir(parents=True)
            project = base / "project"
            project.mkdir()

            client = {
                "address": "0x1001",
                "mapped": True,
                "hidden": False,
                "at": [0, 0],
                "size": [800, 600],
                "workspace": {"id": 1, "name": "1"},
                "floating": False,
                "pinned": False,
                "fullscreen": 0,
                "monitor": 0,
                "class": "firefox",
                "initialClass": "firefox",
                "title": "Fixture Page",
                "initialTitle": "Fixture Page",
                "pid": os.getpid(),
                "xwayland": False,
                "focusHistoryID": 0,
            }
            values = {
                "instances": [{"instance": "test", "wl_socket": "test"}],
                "clients": [client],
                "workspaces": [
                    {
                        "id": 1,
                        "name": "1",
                        "monitor": "TEST",
                        "monitorID": 0,
                        "windows": 1,
                        "focused": True,
                    }
                ],
                "monitors": [
                    {
                        "id": 0,
                        "name": "TEST",
                        "x": 0,
                        "y": 0,
                        "width": 1000,
                        "height": 800,
                        "scale": 1,
                        "focused": True,
                        "disabled": False,
                        "activeWorkspace": {"id": 1, "name": "1"},
                        "specialWorkspace": {"id": 0, "name": ""},
                        "solitaryBlockedBy": [],
                    }
                ],
                "cursorpos": {"x": 400, "y": 300},
            }
            hyprctl = tools / "hyprctl"
            hyprctl.write_text(
                f"#!{sys.executable}\n"
                "import json,sys\n"
                f"values={values!r}\n"
                "kind=next((a for a in sys.argv[1:] if a in values),'')\n"
                "print(json.dumps(values[kind]))\n"
            )
            hyprctl.chmod(0o755)

            capture = base / "mock-capture-python"
            capture_counter = base / "capture-calls.txt"
            capture.write_text(
                "#!/bin/sh\n"
                f'printf \'x\\n\' >> "{capture_counter}"\n'
                "input=$(cat)\n"
                'case "$input" in\n'
                "  *'\"max_nodes\":50000'*)\n"
                '    printf \'%s\\n\' \'{"sources":[{"kind":"accessibility","scope":"matched-window","text":"full page alpha unique-needle omega"}],"warnings":[]}\'\n'
                "    ;;\n"
                "  *)\n"
                '    printf \'%s\\n\' \'{"sources":[{"kind":"accessibility","scope":"matched-window","text":"preview alpha"}],"warnings":[]}\'\n'
                "    ;;\n"
                "esac\n"
            )
            capture.chmod(0o755)

            control = socket.socket(socket.AF_UNIX)
            control.bind(str(hypr / ".socket.sock"))
            events = socket.socket(socket.AF_UNIX)
            events.bind(str(hypr / ".socket2.sock"))
            events.listen()
            events.settimeout(0.1)
            closed = threading.Event()
            connections = []

            def accept_events():
                while not closed.is_set():
                    try:
                        connections.append(events.accept()[0])
                    except socket.timeout:
                        pass
                    except OSError:
                        break

            threading.Thread(target=accept_events, daemon=True).start()

            config = base / "config.toml"
            config.write_text(
                f"project_dir={json.dumps(str(project))}\n"
                "[capture]\n"
                "enabled=true\n"
                "screenshots=false\n"
                "ocr=false\n"
                "[provider.local]\n"
                f'base_url="http://127.0.0.1:{server.server_port}/v1"\n'
                'model="mock"\ntools=true\nvision=false\n'
            )
            env = dict(
                os.environ,
                PATH=str(tools) + os.pathsep + os.environ["PATH"],
                XDG_RUNTIME_DIR=str(runtime),
                HYPRLAND_INSTANCE_SIGNATURE="test",
                DROPS_SOCKET=str(base / "drops.sock"),
                DROPS_CAPTURE_PYTHON=str(capture),
            )
            log = (base / "daemon.log").open("w+")
            daemon = subprocess.Popen(
                [str(BIN / "buddyd"), "--config", str(config), "--data-dir", str(base / "data")],
                env=env,
                stdout=log,
                stderr=log,
            )

            def ctl(*args, success=True):
                result = subprocess.run(
                    [str(BIN / "buddyctl"), *args],
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
                if success:
                    assert result.returncode == 0, result.stderr + result.stdout
                return json.loads(result.stdout.splitlines()[0])

            def wait(predicate, detail):
                deadline = time.monotonic() + 15
                state = None
                while time.monotonic() < deadline:
                    state = ctl("status")["state"]
                    if predicate(state):
                        return state
                    time.sleep(0.05)
                raise AssertionError(detail + ": " + json.dumps(state))

            def wait_socket():
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    if pathlib.Path(env["DROPS_SOCKET"]).exists():
                        return
                    assert daemon.poll() is None, "daemon exited during startup"
                    time.sleep(0.05)
                raise AssertionError("daemon socket did not appear")

            try:
                wait_socket()
                blob = ctl("drop", "400", "300")["id"]
                ctl("send", blob, "What does the fixture page say?")
                wait(
                    lambda state: state["blobs"][0]["state"] in ("idle", "error"),
                    "turn did not finish",
                )
                state = ctl("status")["state"]
                assert state["blobs"][0]["state"] == "idle", json.dumps(state)
                assert state["blobs"][0]["context"]["content"]["text"] == "preview alpha"
                assert state["blobs"][0]["context"]["content"]["full"]["status"] == "on-demand"
                assert len(requests) == 3, len(requests)
                assert len(capture_counter.read_text().splitlines()) == 2, (
                    "expected one preview and one cached deep capture"
                )
                print(
                    "PASS: window_content advertised, deep capture cached across "
                    "search/read, results returned to the model"
                )
            except Exception:
                log.flush()
                log.seek(0)
                print(log.read())
                raise
            finally:
                if daemon.poll() is None:
                    daemon.send_signal(signal.SIGINT)
                    try:
                        daemon.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait()
                closed.set()
                events.close()
                control.close()
                for conn in connections:
                    conn.close()
                log.close()
    finally:
        server.shutdown()


if __name__ == "__main__":
    main()
