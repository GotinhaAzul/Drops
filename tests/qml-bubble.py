#!/usr/bin/env python3
"""Exercise the in-place drop conversation bubble with a fake IPC bridge.

Covers the greeting shown after placement, contextual quick actions, the
inline composer, dismissal, and the automatic close/open lifecycle.
"""
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT=pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="drops-qml-bubble-") as directory:
    base=pathlib.Path(directory)
    bridge=base/"bridge.py"
    calls=base/"calls.jsonl"
    fixture={
        "id":"fixture","appearance":"drop","state":"idle",
        "placement":{"anchor":{"kind":"window","address":"0x1"},"x":420,"y":420,"visible":True,"label":"btop — DesktopServer"},
        "context":{"kind":"window","window":{"class":"Alacritty","title":"btop"},"content":{"status":"captured","text":"load average: 0.21, 0.30, 0.35"}},
        "history":[],"approval":None,
    }
    draft={
        "id":"draft","appearance":"cat","state":"idle",
        "placement":{"anchor":{"kind":"panel"},"x":0,"y":0,"visible":False,"label":"Ready to place"},
        "context":None,"history":[],"approval":None,
    }
    initial=json.dumps({"panel_open":False,"connection":{"base_url":"http://127.0.0.1:11434/v1","model":"test","api_key_env":""},"blobs":[fixture,draft]})
    bridge.write_text(f'''#!{sys.executable}
import json,sys,pathlib
state=json.loads({initial!r})
def emit(value): print(json.dumps(value),flush=True)
def publish():emit({{"type":"state","state":state}})
def blob(id):return next(b for b in state["blobs"] if b["id"]==id)
publish()
for line in sys.stdin:
 cmd=json.loads(line)
 with pathlib.Path({str(calls)!r}).open("a") as f:f.write(line)
 kind=cmd["type"]
 if kind=="toggle":
  state["panel_open"]=not state["panel_open"];publish()
 elif kind=="send":
  b=blob(cmd["id"])
  b["state"]="thinking";b["history"].append({{"type":"user","text":cmd["text"]}});publish()
  b["history"].append({{"type":"text","text":"CPU is calm: about 3% used."}});b["state"]="idle";publish()
 elif kind=="drop":
  b=blob(cmd["id"]);b["placement"]={{"anchor":{{"kind":"workspace","id":2}},"x":300,"y":500,"visible":True,"label":"dev"}};state["panel_open"]=False;publish();emit({{"type":"ok","id":cmd["id"]}})
 elif kind=="rehide":
  blob(cmd["id"])["placement"]["visible"]=False;publish()
 elif kind=="pointer":emit({{"type":"pointer","cursor":None}})
 elif kind=="preview":emit({{"type":"preview","geometry":None}})
''')
    bridge.chmod(0o755)
    source=(ROOT/"ui/shell.qml").read_text().replace("PanelWindow {","FloatingWindow {")
    source=re.sub(r"^\s*anchors \{\s*(?:top|left|bottom|right):\s*true.*\}\n","\n",source,flags=re.M)
    source=re.sub(r"^\s*margins \{.*\}\n","\n",source,flags=re.M)
    source=re.sub(r"^\s*WlrLayershell\..*\n","\n",source,flags=re.M)
    source=re.sub(r"exclusionMode:\s*ExclusionMode.Ignore;?","",source)
    harness='''
    function findNamed(item,name){
        if(!item)return null
        if(item.objectName===name)return item
        const kids=item.children||[]
        for(let i=0;i<kids.length;i++){const found=findNamed(kids[i],name);if(found)return found}
        return null
    }
    property int testPhase:0
    Timer {interval:25;running:true;repeat:true;property int ticks:0
        onTriggered:{
            if(++ticks>400){console.log("TEST_FAILURE timed out phase "+root.testPhase);Qt.quit();return}
            if(!root.connected)return
            const view=root.chatViews["fixture"]
            switch(root.testPhase){
            case 0:
                if(root.state.panel_open || !root.selected)return
                root.toggleChat("fixture");root.testPhase++;break
            case 1: {
                if(!root.chatOpen || !root.chatViews["fixture"])return
                const v=root.chatViews["fixture"]
                console.log("TEST_OPEN", v.window.visible, v.greeting.visible, v.suggestionCount, v.window.implicitHeight)
                console.log("TEST_GREETING", v.greeting.text)
                console.log("TEST_CHIP", v.suggestions.children[0].children[0].text)
                v.sendSuggestion(0);root.testPhase++;break
            }
            case 2:
                if(!root.selected || root.selected.history.length<2)return
                console.log("TEST_AFTER_SEND", root.chatOpen, root.chatViews["fixture"].greeting.visible, root.chatViews["fixture"].scroll.visible, root.chatViews["fixture"].scroll.height)
                const v=root.chatViews["fixture"];v.input.text="Explain that simply.";v.submit();root.testPhase++;break
            case 3:
                if(!root.selected || root.selected.history.length<4)return
                root.send({type:"rehide",id:"fixture"});root.testPhase++;break
            case 4:
                if(root.chatOpen || root.chatViews["fixture"])return
                console.log("TEST_CLOSED", root.chatOpen, root.chatViews["fixture"]===undefined)
                root.drop("draft");root.testPhase++;break
            case 5: {
                if(!root.chatOpen || root.selectedId!=="draft" || !root.chatViews["draft"])return
                const v=root.chatViews["draft"]
                console.log("TEST_AUTO_OPEN", root.chatOpen, root.selectedId, v.greeting.text, v.suggestionCount)
                root.closeChat();root.testPhase++;break
            }
            case 6:
                if(root.chatOpen || root.chatViews["draft"])return
                console.log("TEST_BUBBLE_DONE");Qt.quit();break
            }
        }
    }
'''
    source=source.replace("id: root","id: root\n"+harness,1)
    path=base/"shell.qml";path.write_text(source)
    (base/"markdown.js").write_text((ROOT/"ui/markdown.js").read_text())
    result=subprocess.run(["quickshell","-p",str(path)],env=dict(os.environ,QT_QPA_PLATFORM="offscreen",DROPS_CTL=str(bridge)),capture_output=True,text=True,timeout=12)
    output=result.stdout+result.stderr
    assert "TEST_BUBBLE_DONE" in output and "TEST_FAILURE" not in output,output
    assert "ReferenceError" not in output and "TypeError" not in output,output

    def line(name):
        match=re.search(rf"{name} (.+)$",output,re.M)
        assert match,name+"\n"+output
        return match.group(1).strip()

    opened=line("TEST_OPEN").split()
    assert opened[0]=="true",output                 # bubble window visible
    assert opened[1]=="true",output                 # greeting visible before the first message
    assert int(opened[2])==2,output                 # two contextual quick actions
    assert float(opened[3])>120,output              # compact but not a one-line toast
    assert "some processes" in line("TEST_GREETING").lower(),output
    assert line("TEST_CHIP")=="How's my CPU?",output
    after=line("TEST_AFTER_SEND").split()
    assert after[0]=="true" and after[1]=="false" and after[2]=="true",output
    assert float(after[3])>0,output
    assert line("TEST_CLOSED").split()==["false","true"],output
    auto=line("TEST_AUTO_OPEN")
    assert "draft" in auto and "workspace" in auto.lower(),output
    assert "2" in auto.split()[-1],output

    commands=[json.loads(row) for row in calls.read_text().splitlines()]
    sends=[c for c in commands if c["type"]=="send"]
    assert [c["text"] for c in sends]==["How's my CPU right now?","Explain that simply."],commands
    assert all(c["id"]=="fixture" for c in sends),commands
    assert any(c["type"]=="drop" and c["id"]=="draft" and c["use_cursor"] for c in commands),commands
    print("PASS: placed drops open a contextual speech bubble, send quick actions and typed messages, close when the anchor hides, and greet again after placement")
