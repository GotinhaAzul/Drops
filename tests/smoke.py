#!/usr/bin/env python3
"""Headless daemon/CLI integration against an ephemeral OpenAI-compatible SSE server.
No real model, desktop, credentials, or global configuration is used.
"""
import http.server
import json
import os
import pathlib
import signal
import socket
import subprocess
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
        if self.path != "/v1/chat/completions":
            self.send_error(404)
            return
        messages = body["messages"]
        has_result = bool(messages) and messages[-1].get("role") == "tool"
        prompt = next((m.get("content", "") for m in reversed(messages) if m.get("role") == "user"), "")
        filename = "cancelled.txt" if "cancel" in prompt else "denied.txt" if "deny" in prompt else "note.txt"
        if "remember" in prompt:
            tool = {"index": 0, "id": "call_memory", "type": "function", "function": {
                "name": "task_memory",
                "arguments": json.dumps({
                    "action": "update",
                    "goal": prompt,
                    "pending": [{"text": "Finish the smoke check"}],
                    "verified": [{"text": "Mock endpoint reached", "evidence": "request received"}],
                }),
            }}
        else:
            tool = {"index": 0, "id": "call_smoke", "type": "function", "function": {"name": "write", "arguments": json.dumps({"path": filename, "content": "approved by smoke test\n"})}}
        delta = {"content": "Hello from the mock endpoint.", "reasoning_content": "The file write succeeded."} if has_result else {"tool_calls": [tool]}
        events = [{"choices": [{"index": 0, "delta": delta, "finish_reason": None}]}, {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}]
        payload = "".join("data: " + json.dumps(e) + "\n\n" for e in events) + "data: [DONE]\n\n"
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
        with tempfile.TemporaryDirectory(prefix="drops-smoke-") as tmp:
            base = pathlib.Path(tmp)
            project = base / "project"
            project.mkdir()
            cfg = base / "config.toml"
            cfg.write_text(f'project_dir = {json.dumps(str(project))}\n[provider.local]\nbase_url = "http://127.0.0.1:{server.server_port}/v1"\nmodel = "mock"\nvision = false\ntools = true\n')
            env = dict(os.environ, DROPS_SOCKET=str(base / "daemon.sock"))
            log = (base / "daemon.log").open("w+")
            daemon = subprocess.Popen([str(BIN / "buddyd"), "--headless", "--config", str(cfg), "--data-dir", str(base / "data")], env=env, stdout=log, stderr=log)
            def ctl(*args, success=True):
                result = subprocess.run([str(BIN / "buddyctl"), *args], env=env, capture_output=True, text=True, timeout=10)
                if success:
                    assert result.returncode == 0, result.stderr + result.stdout
                else:
                    assert result.returncode != 0, result.stdout
                return json.loads(result.stdout.splitlines()[0])
            def status():
                return ctl("status")["state"]
            def wait(predicate):
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    state = status()
                    if predicate(state):
                        return state
                    time.sleep(.05)
                raise AssertionError("Timed out: " + json.dumps(state))
            try:
                for _ in range(100):
                    if pathlib.Path(env["DROPS_SOCKET"]).exists():
                        break
                    assert daemon.poll() is None, "daemon exited during startup"
                    time.sleep(.05)
                assert pathlib.Path(env["DROPS_SOCKET"]).stat().st_mode & 0o777 == 0o600
                blob = ctl("drop", "100", "100")["id"]
                ctl("send", blob, "Create note.txt for me")
                wait(lambda s: s["blobs"][0]["state"] == "waiting")
                assert not (project / "note.txt").exists(), "write ran without approval"
                ctl("confirm", blob)
                done = wait(lambda s: s["blobs"][0]["state"] == "idle")
                assert (project / "note.txt").read_text() == "approved by smoke test\n"
                assert "Hello from the mock endpoint." in json.dumps(done)
                history = done["blobs"][0]["history"]
                assert any(entry.get("type") == "thinking" and "write succeeded" in entry.get("text", "") for entry in history)
                assert any(entry.get("type") == "text" and "Hello from the mock endpoint." in entry.get("text", "") for entry in history)
                exported = ctl("export", blob)["jsonl"]
                messages = [json.loads(line).get("message", {}) for line in exported.splitlines() if line.strip()]
                assert any(message.get("reasoning_content") == "The file write succeeded." for message in messages)
                assert len(requests) == 2
                inspected = ctl("context", blob)["context"]
                assert inspected["mode"] == "headless"
                assert inspected["brief"]["anchor"]["kind"] == "headless", inspected["brief"]
                assert inspected["brief"]["freshness"]["state"] == "captured-this-turn"
                assert ctl("export", blob)["jsonl"]
                fork = ctl("fork", blob)["id"]
                assert ctl("export", fork)["jsonl"] == ctl("export", blob)["jsonl"]
                second = ctl("drop", "200", "200")["id"]
                ctl("drop", "300", "300", success=False)
                ctl("remove", fork)
                assert not (base / "data/sessions" / (fork + ".jsonl")).exists()
                ctl("send", second, "cancel this pending action")
                wait(lambda s: next(b for b in s["blobs"] if b["id"] == second)["state"] == "waiting")
                ctl("send", second, "duplicate", success=False)
                ctl("clear", second, success=False)
                ctl("cancel")
                wait(lambda s: next(b for b in s["blobs"] if b["id"] == second)["state"] == "idle")
                assert not (project / "cancelled.txt").exists()
                ctl("clear", second)
                ctl("send", second, "deny this pending action")
                wait(lambda s: next(b for b in s["blobs"] if b["id"] == second)["state"] == "waiting")
                ctl("confirm", second, "--deny")
                wait(lambda s: next(b for b in s["blobs"] if b["id"] == second)["state"] == "idle")
                assert not (project / "denied.txt").exists()
                # Task memory is session-scoped, exported with the JSONL, copied
                # by fork and deleted by clear.
                ctl("send", blob, "remember the release plan")
                wait(lambda s: next(b for b in s["blobs"] if b["id"] == blob)["state"] == "idle")
                exported = ctl("export", blob)["jsonl"]
                assert '"type":"memory"' in exported, exported
                assert "remember the release plan" in exported
                memory_fork = ctl("fork", blob)["id"]
                assert "remember the release plan" in ctl("export", memory_fork)["jsonl"]
                ctl("remove", memory_fork)
                assert not (base / "data/sessions" / (memory_fork + ".jsonl")).exists()
                ctl("clear", blob)
                assert not ctl("export", blob)["jsonl"]
                # Malformed input must not kill the daemon.
                with socket.socket(socket.AF_UNIX) as sock:
                    sock.connect(env["DROPS_SOCKET"])
                    sock.sendall(b'{"type":"unknown"}\n')
                    assert json.loads(sock.recv(4096))["type"] == "error"
                ctl("cancel", second)
                assert next(b for b in status()["blobs"] if b["id"] == second)["state"] == "idle"
                ctl("cancel")
                assert len(status()["blobs"]) == 2
                ctl("send", second, "remember the restart state")
                wait(lambda s: next(b for b in s["blobs"] if b["id"] == second)["state"] == "idle")
                daemon.send_signal(signal.SIGINT)
                assert daemon.wait(timeout=5) == 0
                # Simulate stale UI metadata left by a mid-turn crash. JSONL is
                # authoritative when the daemon rebuilds the conversation.
                saved = json.loads((base / "data/blobs.json").read_text())
                recovered = next(b for b in saved["blobs"] if b["id"] == second)
                recovered["history"] = []
                recovered["context"] = None
                (base / "data/blobs.json").write_text(json.dumps(saved))
                daemon = subprocess.Popen([str(BIN / "buddyd"), "--headless", "--config", str(cfg), "--data-dir", str(base / "data")], env=env, stdout=log, stderr=log)
                for _ in range(100):
                    if pathlib.Path(env["DROPS_SOCKET"]).exists():
                        break
                    time.sleep(.05)
                restarted = status()
                assert len(restarted["blobs"]) == 2, "blob metadata did not survive restart"
                recovered = next(b for b in restarted["blobs"] if b["id"] == second)
                assert recovered["context"]["mode"] == "headless"
                assert "deny this pending action" in json.dumps(recovered["history"])
                assert "remember the restart state" in ctl("export", second)["jsonl"]
                private_env = {k:v for k,v in env.items() if k not in ("DROPS_SOCKET", "XDG_RUNTIME_DIR")}
                refused = subprocess.run([str(BIN / "buddyctl"), "status"], env=private_env, capture_output=True, text=True)
                assert refused.returncode != 0 and "explicit private" in refused.stderr
                print("PASS: streaming tool round trip, approval gate, file write, context, export/fork/clear, blob cap, malformed IPC, denial, active cancellation, busy-session guard, JSONL recovery, session removal, socket permissions")
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
                log.close()
    finally:
        server.shutdown()
        server.server_close()

if __name__ == "__main__":
    main()
