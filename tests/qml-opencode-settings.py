#!/usr/bin/env python3
"""Exercise the Opencode Go settings flow with a fake IPC bridge."""
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="drops-qml-opencode-") as directory:
    base = pathlib.Path(directory)
    bridge = base / "bridge.py"
    calls = base / "calls.jsonl"
    bridge.write_text(f'''#!{sys.executable}
import json,sys,pathlib
custom={{"base_url":"http://localhost:11434/v1","model":"qwen","api_key_env":"","vision":False,"has_api_key":False}}
go={{"base_url":"https://opencode.ai/zen/go/v1","model":"","api_key_env":"OPENCODE_API_KEY","vision":True,"has_api_key":False}}
state={{"panel_open":True,"connection":{{"mode":"custom","capture_content":True,"screenshots":True,"custom":custom,"opencode_go":go}},"blobs":[]}}
def emit(value): print(json.dumps(value),flush=True)
def publish():emit({{"type":"state","state":state}})
def summary(source):
 return {{"base_url":source["base_url"],"model":source["model"],"api_key_env":source["api_key_env"],"vision":source["vision"],"has_api_key":source["has_api_key"]}}
publish()
for line in sys.stdin:
 cmd=json.loads(line)
 with pathlib.Path({str(calls)!r}).open("a") as f:f.write(line)
 kind=cmd["type"]
 if kind=="models":
  assert cmd["api_key"]=="go-secret",cmd
  emit({{"type":"models","models":["deepseek-v4.1-flash","kimi-k3"]}})
 elif kind=="configure":
  assert cmd["mode"]=="opencode_go",cmd
  go["model"]=cmd["model"];go["has_api_key"]=True;go["vision"]=cmd["vision"]
  state["connection"]["mode"]="opencode_go"
  state["connection"]["custom"]=custom
  state["connection"]["opencode_go"]=summary(go)
  state["connection"].update(summary(go))
  publish();emit({{"type":"ok","configured":True}})
''')
    bridge.chmod(0o755)
    source = (ROOT / "ui/shell.qml").read_text().replace("PanelWindow {", "FloatingWindow {")
    source = re.sub(r"^\s*anchors \{\s*(?:top|left|bottom|right):\s*true.*\}\n", "\n", source, flags=re.M)
    source = re.sub(r"^\s*margins \{.*\}\n", "\n", source, flags=re.M)
    source = re.sub(r"^\s*WlrLayershell\..*\n", "\n", source, flags=re.M)
    source = re.sub(r"exclusionMode:\s*ExclusionMode.Ignore;?", "", source)
    harness = '''
    property int testPhase:0
    Timer {interval:25;running:true;repeat:true;property int ticks:0
        onTriggered:{
            if(++ticks>120){console.log("TEST_FAILURE timed out phase "+root.testPhase);Qt.quit();return}
            if(!root.connected)return
            switch(root.testPhase){
            case 0:root.loadSettings();root.settingsOpen=true;connectionType.activated(1);root.testPhase++;break
            case 1:if(root.connectionMode!=="opencode_go")return;
                if(!opencodeKey.visible)return;
                opencodeKey.text="go-secret";loadModelsButton.clicked();root.testPhase++;break
            case 2:if(root.opencodeModels.length<2)return;
                if(root.opencodeModels[1]!=="kimi-k3")return;
                opencodeModel.activated(1);root.testPhase++;break
            case 3:if(root.opencodeSelected!=="kimi-k3")return;
                saveConnectionButton.clicked();root.testPhase++;break
            case 4:if(root.settingsOpen)return;console.log("TEST_OPENCODE_DONE");Qt.quit();break
            }
        }
    }
'''
    source = source.replace("id: root", "id: root\n" + harness, 1)
    path = base / "shell.qml"
    path.write_text(source)
    result = subprocess.run(
        ["quickshell", "-p", str(path)],
        env=dict(os.environ, QT_QPA_PLATFORM="offscreen", DROPS_CTL=str(bridge)),
        capture_output=True,
        text=True,
        timeout=10,
    )
    output = result.stdout + result.stderr
    assert "TEST_OPENCODE_DONE" in output and "TEST_FAILURE" not in output, output
    assert "ReferenceError" not in output and "TypeError" not in output, output
    commands = [json.loads(line) for line in calls.read_text().splitlines()]
    assert any(c["type"] == "models" and c["api_key"] == "go-secret" for c in commands), commands
    configure = next(c for c in commands if c["type"] == "configure")
    assert configure["mode"] == "opencode_go", configure
    assert configure["model"] == "kimi-k3", configure
    assert configure["api_key"] == "go-secret", configure
    assert configure["vision"] is True, configure
    assert "base_url" not in configure, configure
    print("PASS: Opencode Go mode loads the model list and saves key, model and vision")
