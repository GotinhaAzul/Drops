#!/usr/bin/env python3
"""Opt-in live Hyprland test. Moves/closes only a terminal created by this test.
Run while the user permits desktop testing. Does not edit compositor config.
"""
import json
import os
import pathlib
import signal
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / "target/debug"

def main():
    instances = json.loads(subprocess.check_output(["hyprctl", "instances", "-j"], text=True))
    assert instances, "No Hyprland session"
    instance = instances[0]
    env = dict(os.environ, HYPRLAND_INSTANCE_SIGNATURE=instance["instance"], WAYLAND_DISPLAY=instance["wl_socket"], QT_QPA_PLATFORM="wayland")
    def hypr(*args):
        result = subprocess.run(["hyprctl", *args], env=env, capture_output=True, text=True)
        assert result.returncode == 0, result.stdout + result.stderr
        return result.stdout
    with tempfile.TemporaryDirectory(prefix="drops-desktop-") as directory:
        tmp = pathlib.Path(directory)
        cfg = tmp / "config.toml"
        cfg.write_text(f'project_dir = {json.dumps(directory)}\n[provider.local]\nbase_url = "http://127.0.0.1:1/v1"\nmodel = "unused"\nvision = false\ntools = false\n')
        env["DROPS_SOCKET"] = str(tmp / "drops.sock")
        env["DROPS_CTL"] = str(BIN / "buddyctl")
        log = (tmp / "daemon.log").open("w+")
        qlog = (tmp / "qml.log").open("w+")
        daemon = subprocess.Popen([str(BIN / "buddyd"), "--config", str(cfg), "--data-dir", str(tmp / "data")], env=env, stdout=log, stderr=log)
        terminal = None
        ui = None
        def ctl(*args):
            return json.loads(subprocess.check_output([str(BIN / "buddyctl"), *args], env=env, text=True))
        def wait(fn, detail):
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                value = fn()
                if value:
                    return value
                time.sleep(.1)
            raise AssertionError(detail)
        try:
            wait(lambda: pathlib.Path(env["DROPS_SOCKET"]).exists(), "daemon startup")
            title = "Drops integration " + str(os.getpid())
            terminal = subprocess.Popen(["foot", "--title", title, "sh", "-c", "printf 'Drops disposable integration window\\n'; sleep 90"], cwd=tmp, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            client = wait(lambda: next((c for c in json.loads(hypr("clients", "-j")) if c["title"] == title), None), "test terminal did not map")
            x, y = client["at"]
            w, h = client["size"]
            blob = ctl("drop", str(x + w//2), str(y + h//2))["id"]
            def placed():
                return next(b for b in ctl("status")["state"]["blobs"] if b["id"] == blob)["placement"]
            assert placed()["anchor"] == {"kind": "window", "address": client["address"]}
            ctl("toggle")
            ui = subprocess.Popen(["quickshell", "-p", str(ROOT / "ui/shell.qml")], env=env, stdout=qlog, stderr=qlog)
            time.sleep(1)
            assert ui.poll() is None, "UI failed to launch"
            qlog.flush(); qlog.seek(0)
            initial_qml_log = qlog.read()
            assert "Configuration Loaded" in initial_qml_log, initial_qml_log
            if os.environ.get("DROPS_MEASURE_IDLE"):
                def cpu_ticks():
                    fields = pathlib.Path(f"/proc/{daemon.pid}/stat").read_text().split()
                    return int(fields[13])+int(fields[14])
                before = cpu_ticks(); began = time.monotonic()
                time.sleep(5)
                cpu = (cpu_ticks()-before)/os.sysconf("SC_CLK_TCK")/(time.monotonic()-began)*100
                rss = next(line.split()[1] for line in pathlib.Path(f"/proc/{daemon.pid}/status").read_text().splitlines() if line.startswith("VmRSS:"))
                print(f"Idle sample with one window drop: {cpu:.2f}% of one CPU core, {int(rss)/1024:.1f} MiB daemon RSS (5 seconds, debug build)")
            capture = os.environ.get("DROPS_CAPTURE")
            if capture:
                def panel_layer(node):
                    if isinstance(node, dict):
                        if node.get("namespace") == "drops-panel":
                            return node
                        for value in node.values():
                            found = panel_layer(value)
                            if found:
                                return found
                    elif isinstance(node, list):
                        for value in node:
                            found = panel_layer(value)
                            if found:
                                return found
                    return None
                layer = panel_layer(json.loads(hypr("layers", "-j")))
                assert layer, "panel layer missing"
                geometry = f'{layer["x"]},{layer["y"]} {layer["w"]}x{layer["h"]}'
                try:
                    subprocess.run(["grim", "-g", geometry, capture], env=env, check=True, timeout=5)
                except (subprocess.TimeoutExpired, subprocess.CalledProcessError):
                    print("Screenshot unavailable; continuing desktop checks.")
            used = {v["id"] for v in json.loads(hypr("workspaces", "-j"))}
            workspace = next(v for v in range(9000, 9100) if v not in used)
            address = client["address"]
            original = client["workspace"]["id"]
            hypr("eval", f'hl.dispatch(hl.dsp.window.move({{workspace="{workspace}",window="address:{address}",follow=false}}))')
            wait(lambda: not placed()["visible"], "blob did not hide on invisible workspace")
            hypr("eval", f'hl.dispatch(hl.dsp.window.move({{workspace="{original}",window="address:{address}",follow=false}}))')
            wait(lambda: placed()["visible"], "blob did not return with window")
            # Workspace events alone are not proof of following real geometry.
            hypr("eval", f'hl.dispatch(hl.dsp.window.float({{window="address:{address}",action="set"}}))')
            time.sleep(.25)
            monitor = next(m for m in json.loads(hypr("monitors", "-j")) if m["id"] == client["monitor"])
            hypr("eval", f'hl.dispatch(hl.dsp.window.move({{window="address:{address}",x={monitor["x"]+80},y={monitor["y"]+100},relative=false}}))')
            moved = next(c for c in json.loads(hypr("clients", "-j")) if c["address"] == address)
            offset = next(b for b in ctl("status")["state"]["blobs"] if b["id"] == blob)["offset"]
            wait(lambda: placed()["x"] == moved["at"][0]+offset[0] and placed()["y"] == moved["at"][1]+offset[1], "blob did not follow floating window geometry")
            terminal.terminate(); terminal.wait(timeout=5); terminal = None
            wait(lambda: placed()["anchor"]["kind"] == "workspace", "closed-window fallback failed")
            qlog.flush(); qlog.seek(0)
            output = qlog.read()
            assert "ReferenceError" not in output and "TypeError" not in output and "Unable to assign" not in output, output
            print("PASS: real window hit test, connected Quickshell UI, workspace hide/return, floating movement, window-close fallback; no runtime QML errors")
        except Exception:
            for f in (log, qlog):
                f.flush(); f.seek(0); print(f.read())
            raise
        finally:
            for child in (ui, terminal, daemon):
                if child and child.poll() is None:
                    child.send_signal(signal.SIGINT if child is daemon else signal.SIGTERM)
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill(); child.wait()
            log.close(); qlog.close()

if __name__ == "__main__":
    main()
