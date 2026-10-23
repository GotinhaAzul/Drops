#!/usr/bin/env python3
"""Opt-in live test of an installed prefix: one click starts and toggles Drops.
Run after installing with PREFIX under .test-data/install-real/prefix.
"""
import json
import os
import pathlib
import signal
import subprocess
import tempfile
import time

root = pathlib.Path(__file__).resolve().parents[1]
prefix = root / ".test-data/install-real/prefix"
instance = json.loads(subprocess.check_output(["hyprctl", "instances", "-j"], text=True))[0]
with tempfile.TemporaryDirectory(prefix="drops-launcher-") as tmp:
    directory = pathlib.Path(tmp)
    env = dict(os.environ, WAYLAND_DISPLAY=instance["wl_socket"], HYPRLAND_INSTANCE_SIGNATURE=instance["instance"], DROPS_SOCKET=str(directory / "drops.sock"), XDG_CONFIG_HOME=str(directory / "config"), XDG_DATA_HOME=str(directory / "data"), XDG_STATE_HOME=str(directory / "state"))
    config = directory / "config/drops/config.toml"
    config.parent.mkdir(parents=True)
    config.write_text('[provider.local]\nbase_url="http://127.0.0.1:1/v1"\nmodel="unused"\ntools=false\n')
    shell = str(prefix / "share/drops/ui/shell.qml")
    try:
        for expected in (True, False, True):
            subprocess.run([str(prefix / "bin/drops")], env=env, check=True, timeout=15)
            state = json.loads(subprocess.check_output([str(prefix / "bin/buddyctl"), "status"], env=env, text=True))["state"]
            assert state["panel_open"] is expected
        time.sleep(.5)
        output = (directory / "state/drops/ui.log").read_text()
        assert "Configuration Loaded" in output, output
        assert "ReferenceError" not in output and "TypeError" not in output, output
        print("PASS: installed drops launcher starts daemon/UI once and toggles the compact panel")
    finally:
        subprocess.run(["quickshell", "kill", "-p", shell], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
        pid_file = directory / "state/drops/daemon.pid"
        if pid_file.exists():
            try:
                os.kill(int(pid_file.read_text()), signal.SIGTERM)
            except ProcessLookupError:
                pass
        for _ in range(50):
            if not (directory / "drops.sock").exists():
                break
            time.sleep(.05)
