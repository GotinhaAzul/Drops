#!/usr/bin/env python3
"""Check QML controls offscreen, replacing only the unsupported layer surface.
This does not test PanelWindow, placement, dragging or Wayland focus semantics.
"""
import os
import json
import pathlib
import re
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / "ui/shell.qml").read_text().replace("PanelWindow {", "FloatingWindow {")
source = re.sub(r"^\s*anchors \{\s*(?:top|left|bottom|right):\s*true.*\}\n", "\n", source, flags=re.M)
source = re.sub(r"^\s*margins \{.*\}\n", "\n", source, flags=re.M)
source = re.sub(r"^\s*WlrLayershell\..*\n", "\n", source, flags=re.M)
source = re.sub(r"exclusionMode:\s*ExclusionMode.Ignore;?", "", source)
fixture = {"panel_open": True, "blobs": [{"id": "fixture", "state": "waiting", "placement": {"anchor": {"kind": "workspace", "id": 1}, "x": 10, "y": 10, "visible": True, "label": "Test workspace"}, "context": {"mode": "fixture"}, "approval": {"name": "write", "arguments": {"path": "test.txt", "content": "hello"}, "preview": {"diff": "--- a/test.txt\n+++ b/test.txt\n-old\n+approval-preview-fixture\n"}}, "history": [{"type": "user", "text": "Hello"}, {"type": "text", "text": "Ready"}]}]}
source = source.replace("ShellRoot {", "ShellRoot {\nComponent.onCompleted: { root.state=" + json.dumps(fixture) + '; root.blobIds=["fixture"]; root.selectedId="fixture"; root.chatOpen=true }', 1)
source = source.replace('objectName:"approval-details-"+(bubble.blob ? bubble.blob.id : "")', 'objectName:"approval-details-"+(bubble.blob ? bubble.blob.id : ""); Component.onCompleted: Qt.callLater(function() { console.log("TEST_APPROVAL_DIFF", text) })')
source = source.replace("id:placed", 'id:placed; Component.onCompleted: console.log("TEST_BLOB_CREATED")')
source = source.replace("required property var entry", 'Component.onCompleted: console.log("TEST_CHAT_CREATED"); required property var entry')
source = source.replace("id: root", 'id: root\nTimer { interval: 50; running: true; repeat: true; property int ticks:0; onTriggered: { if(ticks++<10) { let s=JSON.parse(JSON.stringify(root.state));s.blobs[0].history[1].text+=".";root.state=s;root.syncChat() } else stop() } }\n',1)

with tempfile.TemporaryDirectory(prefix="drops-qml-") as directory:
    path = pathlib.Path(directory) / "shell.qml"
    path.write_text(source)
    process = subprocess.Popen(["quickshell", "-p", str(path)], env=dict(os.environ, QT_QPA_PLATFORM="offscreen", DROPS_SOCKET=str(pathlib.Path(directory)/"absent.sock")), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        output, _ = process.communicate(timeout=3)
    except subprocess.TimeoutExpired:
        process.terminate()
        output, _ = process.communicate(timeout=5)
    assert "Configuration Loaded" in output, output
    assert "Failed to load" not in output, output
    assert "ReferenceError" not in output and "TypeError" not in output, output
    assert "TEST_APPROVAL_DIFF" in output and "+approval-preview-fixture" in output, output
    assert output.count("TEST_BLOB_CREATED") == 1, output
    assert output.count("TEST_CHAT_CREATED") == 2, output
    print("PASS: populated QML controls load; streaming preserves blob surface and chat delegates (layer-shell requires Hyprland)")
