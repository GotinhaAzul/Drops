#!/usr/bin/env python3
"""Failure-path regressions using a fake compositor; no real desktop is changed."""
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

with tempfile.TemporaryDirectory(prefix="drops-fail-") as directory:
    base = pathlib.Path(directory)
    tools = base / "bin"
    tools.mkdir()
    runtime = base / "run"
    hypr = runtime / "hypr/test"
    hypr.mkdir(parents=True)
    project = base / "project"
    project.mkdir()
    (tools / "hyprctl").write_text(f'''#!{sys.executable}
import json,pathlib,sys,time
base=pathlib.Path({str(base)!r})
kind=next((a for a in sys.argv[1:] if a in ["instances","clients","workspaces","monitors","cursorpos"]),"")
if kind=="clients" and (base/"slow").exists():
    (base/"query-started").touch()
    time.sleep(4)
values={{"instances":[{{"instance":"test","wl_socket":"test"}}],"clients":[],"workspaces":[{{"id":1,"name":"1","monitor":"TEST","monitorID":0}}],"monitors":[{{"id":0,"name":"TEST","x":0,"y":0,"width":1000,"height":800,"scale":1,"focused":True,"activeWorkspace":{{"id":1,"name":"1"}}}}],"cursorpos":{{"x":400,"y":300}}}}
print(json.dumps(values[kind]))
''')
    (tools / "hyprctl").chmod(0o755)
    control = socket.socket(socket.AF_UNIX)
    control.bind(str(hypr / ".socket.sock"))
    events = socket.socket(socket.AF_UNIX)
    events.bind(str(hypr / ".socket2.sock"))
    events.listen()
    events.settimeout(.1)
    closed = threading.Event()
    connections = []
    def accept_events():
        while not closed.is_set():
            try:
                conn, _ = events.accept()
                connections.append(conn)
            except socket.timeout:
                pass
            except OSError:
                break
    thread = threading.Thread(target=accept_events, daemon=True)
    thread.start()
    config = base / "config.toml"
    config.write_text(f'project_dir={json.dumps(str(project))}\n[provider.local]\nbase_url="http://127.0.0.1:1/v1"\nmodel="unused"\ntools=false\n')
    env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"], XDG_RUNTIME_DIR=str(runtime), HYPRLAND_INSTANCE_SIGNATURE="test", DROPS_SOCKET=str(base / "drops.sock"))
    log = (base / "daemon.log").open("w+")
    daemon = None
    def start():
        return subprocess.Popen([str(BIN / "buddyd"), "--config", str(config), "--data-dir", str(base / "data")], env=env, stdout=log, stderr=log)
    def wait(fn, detail):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if value := fn():
                return value
            time.sleep(.02)
        raise AssertionError(detail)
    def command(value):
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(3)
            client.connect(env["DROPS_SOCKET"])
            client.sendall((json.dumps(value) + "\n").encode())
            return json.loads(client.makefile("rb").readline())
    def status():
        return command({"type":"status"})["state"]
    def stop():
        daemon.send_signal(signal.SIGINT)
        assert daemon.wait(timeout=5) == 0
    try:
        daemon = start()
        wait(lambda: pathlib.Path(env["DROPS_SOCKET"]).exists(), "daemon startup")
        dropped = command({"type":"drop","x":400,"y":300})
        blob = dropped["id"]
        assert status()["blobs"][0]["placement"]["x"] == 400
        assert status()["blobs"][0]["offset"] == [400,300]
        preview = command({"type":"preview","x":400,"y":300})
        assert preview["geometry"]["x"] == 400 and preview["geometry"]["y"] == 300
        (base / "slow").touch()
        # Send and controls share one NDJSON connection, just like the QML bridge.
        with socket.socket(socket.AF_UNIX) as bridge:
            bridge.settimeout(2)
            bridge.connect(env["DROPS_SOCKET"])
            stream = bridge.makefile("rwb", buffering=0)
            def on_bridge(value):
                stream.write((json.dumps(value) + "\n").encode())
                return json.loads(stream.readline())
            began = time.monotonic()
            assert on_bridge({"type":"send","id":blob,"text":"slow context"})["type"] == "ok"
            assert time.monotonic()-began < 1, "send waited for slow compositor capture"
            wait(lambda: (base/"query-started").exists(), "slow query did not start")
            for action in ("clear","fork"):
                reply = on_bridge({"type":action,"id":blob})
                assert reply["type"] == "error" and "busy" in reply["message"]
            began = time.monotonic()
            assert on_bridge({"type":"cancel","id":blob})["type"] == "ok"
            wait(lambda: status()["blobs"][0]["state"] == "idle", "cancel during capture did not finish")
            assert time.monotonic()-began < 1, "cancel queued behind context capture"
        (base / "slow").unlink()
        stop()
        # Persisted screen coordinates are stale; offsets and anchors are authoritative.
        saved = json.loads((base/"data/blobs.json").read_text())
        saved["blobs"][0]["placement"].update(x=999,y=999,visible=False)
        (base/"data/blobs.json").write_text(json.dumps(saved))
        daemon = start()
        wait(lambda: pathlib.Path(env["DROPS_SOCKET"]).exists(), "daemon restart")
        placement = status()["blobs"][0]["placement"]
        assert (placement["x"],placement["y"],placement["visible"]) == (400,300,True)
        # Force the atomic-save temp path to fail. A worker must still exist and
        # eventually release the reservation, allowing clear/remove/retry.
        bad_temp = base / "data/blobs.tmp"
        wait(lambda: not bad_temp.exists(), "metadata save still in progress")
        bad_temp.mkdir()
        failed = command({"type":"send","id":blob,"text":"disk failure"})
        assert failed["type"] == "error"
        wait(lambda: status()["blobs"][0]["state"] == "error", "worker was never spawned after save error")
        bad_temp.rmdir()
        wait(lambda: command({"type":"clear","id":blob})["type"] == "ok", "failed persistence left a permanent busy reservation")
        stop()
        print("PASS: cursor offsets/preview, immediate send ack, same-bridge cancel, capture fences, startup anchor refresh, persistence-error recovery")
    except Exception:
        log.flush();log.seek(0);print(log.read())
        raise
    finally:
        if daemon and daemon.poll() is None:
            daemon.terminate()
            try: daemon.wait(timeout=5)
            except subprocess.TimeoutExpired: daemon.kill();daemon.wait()
        closed.set();events.close();control.close()
        for conn in connections: conn.close()
        thread.join(timeout=1)
        log.close()
