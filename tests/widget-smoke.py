#!/usr/bin/env python3
"""Widget management and connection settings over the UI's daemon protocol."""
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
import tomllib
from smoke import Endpoint, requests

ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=ROOT/"target/debug"
server=http.server.ThreadingHTTPServer(("127.0.0.1",0),Endpoint)
threading.Thread(target=server.serve_forever,daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix="drops-widget-") as directory:
        base=pathlib.Path(directory)
        cfg=base/"config/config.toml"
        env=dict(os.environ,DROPS_SOCKET=str(base/"drops.sock"))
        log=(base/"daemon.log").open("w+")
        def start():return subprocess.Popen([str(BIN/"buddyd"),"--headless","--config",str(cfg),"--data-dir",str(base/"data")],env=env,stdout=log,stderr=log)
        def ctl(*args,ok=True):
            response=subprocess.run([str(BIN/"buddyctl"),*args],env=env,capture_output=True,text=True,timeout=6)
            assert (response.returncode==0)==ok,response.stdout+response.stderr
            return json.loads(response.stdout.splitlines()[0])
        def state():return ctl("status")["state"]
        def wait(fn):
            deadline=time.monotonic()+8
            while time.monotonic()<deadline:
                if value:=fn():return value
                time.sleep(.03)
            raise AssertionError("widget scenario timed out")
        daemon=start()
        try:
            wait(lambda:(base/"drops.sock").exists())
            assert not state()["panel_open"] and state()["connection"]["model"]==""
            with socket.socket(socket.AF_UNIX) as ipc:
                ipc.settimeout(2);ipc.connect(env["DROPS_SOCKET"]);ipc.sendall(b'{"type":"pointer"}\n')
                assert json.loads(ipc.makefile("rb").readline())["cursor"] is None
            draft=ctl("create","--appearance","cat")["id"]
            blob=state()["blobs"][0]
            assert blob["appearance"]=="cat" and blob["placement"]["anchor"]["kind"]=="panel" and not blob["placement"]["visible"]
            ctl("style",draft,"sprout")
            assert state()["blobs"][0]["appearance"]=="sprout"
            ctl("configure","not-a-url","model",ok=False)
            assert not cfg.exists()
            endpoint=f"http://127.0.0.1:{server.server_port}/v1"
            ctl("configure",endpoint,"mock")
            assert not requests,"saving configuration must not contact the endpoint"
            assert state()["connection"]["base_url"]==endpoint
            saved=tomllib.loads(cfg.read_text())
            assert saved["provider"]["local"]["model"]=="mock"
            assert cfg.stat().st_mode & 0o777==0o600
            with cfg.open("a") as output:output.write('\n[custom]\nkeep="preserved"\n')
            ctl("configure",endpoint,"mock")
            assert tomllib.loads(cfg.read_text())["custom"]["keep"]=="preserved"
            ctl("send",draft,"not placed",ok=False)
            ctl("drop","200","150","--id",draft)
            assert state()["blobs"][0]["appearance"]=="sprout"
            initial=wait(lambda:state()["blobs"][0] if state()["blobs"][0]["context"] else None)
            assert initial["directory"] is None,"Automatic must be the default"
            assert initial["context"]["tool_workspace"]["source"]=="private"
            assert not requests,"placement folder detection must not contact a model"
            work=base/"working-files";work.mkdir()
            ctl("directory",draft,"/",ok=False)
            ctl("directory",draft,str(pathlib.Path.home()),ok=False)
            ctl("directory",draft,"relative/path",ok=False)
            ctl("directory",draft,str(base/"missing"),ok=False)
            ctl("directory",draft,str(work))
            assert state()["blobs"][0]["directory"]==str(work)
            ctl("send",draft,"write something")
            wait(lambda:state()["blobs"][0]["state"]=="waiting")
            assert ctl("context",draft)["context"]["tool_workspace"]["source"]=="selected"
            ctl("directory",draft,ok=False)
            assert not (work/"note.txt").exists(),"write must still wait for approval"
            ctl("confirm",draft)
            wait(lambda:state()["blobs"][0]["state"]=="idle")
            assert (work/"note.txt").read_text()=="approved by smoke test\n"
            assert not (base/"data/projects"/draft/"note.txt").exists()
            ctl("directory",draft)
            assert state()["blobs"][0]["directory"] is None
            ctl("send",draft,"write something")
            wait(lambda:state()["blobs"][0]["state"]=="waiting")
            assert ctl("context",draft)["context"]["tool_workspace"]["source"]=="private"
            ctl("remove",draft)
            assert state()["blobs"]==[]
            assert not (base/"data/sessions"/(draft+".jsonl")).exists()
            a=ctl("create","--appearance","cat")["id"]
            ctl("directory",a,str(work))
            ctl("create","--appearance","sprout")
            ctl("create")
            ctl("create",ok=False)
            ctl("toggle")
            assert state()["panel_open"]
            daemon.send_signal(signal.SIGINT);assert daemon.wait(timeout=5)==0
            daemon=start();wait(lambda:(base/"drops.sock").exists())
            restored=state()
            assert not restored["panel_open"],"restart must not summon the panel"
            assert restored["connection"]["model"]=="mock"
            assert next(b for b in restored["blobs"] if b["id"]==a)["appearance"]=="cat"
            assert next(b for b in restored["blobs"] if b["id"]==a)["directory"]==str(work)
            print("PASS: widget lifecycle, selected-directory writes with approval, busy/invalid directory rejection, automatic reset, directory/appearance persistence, mock-provider turn")
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
