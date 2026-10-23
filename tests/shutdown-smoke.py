#!/usr/bin/env python3
"""A strict mock endpoint verifies daemon shutdown and post-crash continuation."""
import http.server
import json
import os
import pathlib
import signal
import subprocess
import sys
import tempfile
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / "target/debug"
rejected = []

class StrictEndpoint(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        pending = set()
        for message in request["messages"]:
            role = message["role"]
            if pending and (role != "tool" or message.get("tool_call_id") not in pending):
                rejected.append(sorted(pending));self.send_error(400,"unanswered tool calls");return
            if role == "tool": pending.discard(message["tool_call_id"])
            elif role == "assistant": pending.update(c["id"] for c in message.get("tool_calls",[]))
        if pending:
            rejected.append(sorted(pending));self.send_error(400,"unanswered tool calls");return
        completed = any(m["role"] == "tool" for m in request["messages"])
        delta = {"content":"Recovered successfully"} if completed else {"tool_calls":[{"index":0,"id":"slow_call","type":"function","function":{"name":"slow","arguments":"{}"}}]}
        body = 'data: '+json.dumps({"choices":[{"index":0,"delta":delta,"finish_reason":"stop"}]})+'\n\ndata: [DONE]\n\n'
        self.send_response(200);self.send_header("Content-Type","text/event-stream");self.send_header("Content-Length",str(len(body.encode())));self.end_headers();self.wfile.write(body.encode())

server=http.server.ThreadingHTTPServer(("127.0.0.1",0),StrictEndpoint)
threading.Thread(target=server.serve_forever,daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix="drops-stop-") as tmp:
        base=pathlib.Path(tmp)
        tool=base/"slow.py"
        tool.write_text('import pathlib,time\npathlib.Path("started").touch()\ntime.sleep(30)\npathlib.Path("leaked").touch()\nprint(\'{"result":"done"}\')\n')
        cfg=base/"config.toml"
        cfg.write_text(f'project_dir={json.dumps(tmp)}\n[[external_tools]]\nname="slow"\ndescription="slow test"\ncommand=[{json.dumps(sys.executable)},{json.dumps(str(tool))}]\nrisk="dangerous"\n[provider.local]\nbase_url="http://127.0.0.1:{server.server_port}/v1"\nmodel="strict-mock"\ntools=true\n')
        env=dict(os.environ,DROPS_SOCKET=str(base/"drops.sock"))
        log=(base/"daemon.log").open("w+")
        def start(): return subprocess.Popen([str(BIN/"buddyd"),"--headless","--config",str(cfg),"--data-dir",str(base/"data")],env=env,stdout=log,stderr=log)
        def ctl(*args): return json.loads(subprocess.check_output([str(BIN/"buddyctl"),*args],env=env,text=True,timeout=5))
        def wait(fn):
            deadline=time.monotonic()+8
            while time.monotonic()<deadline:
                if value:=fn(): return value
                time.sleep(.03)
            raise AssertionError("shutdown scenario timed out")
        daemon=start()
        try:
            wait(lambda:(base/"drops.sock").exists())
            blob=ctl("drop","100","100")["id"]
            ctl("send",blob,"Run the slow test")
            wait(lambda:ctl("status")["state"]["blobs"][0]["state"]=="waiting")
            ctl("confirm",blob)
            wait(lambda:(base/"started").exists())
            began=time.monotonic();daemon.send_signal(signal.SIGINT)
            assert daemon.wait(timeout=4)==0
            assert time.monotonic()-began<3
            assert not (base/"leaked").exists()
            session=base/"data/sessions"/(blob+".jsonl")
            rows=[json.loads(line) for line in session.read_text().splitlines()]
            assert any(row.get("message",{}).get("tool_call_id")=="slow_call" for row in rows)
            # Also simulate an unclean crash after a persisted assistant call.
            with session.open("a") as output:
                output.write(json.dumps({"role":"assistant","content":None,"tool_calls":[{"id":"crash_call","type":"function","function":{"name":"slow","arguments":"{}"}}]})+"\n")
            daemon=start();wait(lambda:(base/"drops.sock").exists())
            ctl("send",blob,"Continue after restart")
            wait(lambda:ctl("status")["state"]["blobs"][0]["state"]=="idle")
            assert not rejected,rejected
            assert "Recovered successfully" in json.dumps(ctl("status"))
            assert not (base/"leaked").exists()
            print("PASS: shutdown kills external tool, closes call results, and repairs a crash before strict-endpoint continuation")
        except Exception:
            log.flush();log.seek(0);print(log.read());raise
        finally:
            if daemon.poll() is None:
                daemon.terminate()
                try:daemon.wait(timeout=5)
                except subprocess.TimeoutExpired:daemon.kill();daemon.wait()
            log.close()
finally:
    server.shutdown();server.server_close()
