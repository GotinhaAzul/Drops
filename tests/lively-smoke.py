#!/usr/bin/env python3
"""Verify new styles, legacy state loading, and the lively preference over restarts."""
import json
import os
import pathlib
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / "target/debug"
with tempfile.TemporaryDirectory(prefix="drops-lively-") as directory:
    base = pathlib.Path(directory)
    data = base / "data"
    data.mkdir()
    saved = data / "blobs.json"
    # Simulate a snapshot written before the lively preference existed.
    saved.write_text(json.dumps({"panel_open": False, "blobs": [], "version": 1}))
    env = dict(os.environ, DROPS_SOCKET=str(base / "daemon.sock"))

    def ctl(*args):
        result = subprocess.run([str(BIN / "buddyctl"), *args], env=env,
                                capture_output=True, text=True, timeout=5, check=True)
        return json.loads(result.stdout.splitlines()[0])

    for boot in range(3):
        with (base / "daemon.log").open("w") as log:
            daemon = subprocess.Popen([str(BIN / "buddyd"), "--headless", "--config",
                                       str(base / "missing.toml"), "--data-dir", str(data)],
                                      env=env, stdout=log, stderr=log)
            try:
                for _ in range(100):
                    assert daemon.poll() is None, (base / "daemon.log").read_text()
                    try:
                        state = ctl("status")["state"]
                        break
                    except (subprocess.CalledProcessError, ConnectionError):
                        time.sleep(.03)
                else:
                    raise AssertionError("Daemon did not become ready")
                assert state["lively"] == (boot == 1), state
                if boot == 0:
                    for look in ["bunny", "robot", "cloud"]:
                        ctl("create", "--appearance", look)
                    assert [b["appearance"] for b in ctl("status")["state"]["blobs"]] == ["bunny", "robot", "cloud"]
                    ctl("style", ctl("status")["state"]["blobs"][0]["id"], "cloud")
                    ctl("lively", "on")
                    assert ctl("status")["state"]["lively"] is True
                elif boot == 1:
                    assert [b["appearance"] for b in state["blobs"]] == ["cloud", "robot", "cloud"]
                    ctl("lively", "off")
                assert json.loads(saved.read_text())["lively"] == (boot == 0)
            finally:
                daemon.terminate()
                daemon.wait(timeout=5)
print("PASS: legacy snapshots default to quiet; CLI styles and lively on/off persist across restarts")
