#!/usr/bin/env python3
"""Exercise the real QML controls and drag interpolation with a fake IPC bridge."""
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT=pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="drops-qml-ui-") as directory:
    base=pathlib.Path(directory)
    bridge=base/"bridge.py"
    calls=base/"calls.jsonl"
    bridge.write_text(f'''#!{sys.executable}
import json,sys,pathlib
state={{"panel_open":True,"connection":{{"base_url":"http://localhost:11434/v1","model":"test","api_key_env":""}},"blobs":[]}}
def emit(value): print(json.dumps(value),flush=True)
def publish():emit({{"type":"state","state":state}})
publish()
for line in sys.stdin:
 cmd=json.loads(line)
 with pathlib.Path({str(calls)!r}).open("a") as f:f.write(line)
 kind=cmd["type"]
 if kind=="create":
  state["blobs"].append({{"id":"test","appearance":cmd["appearance"],"state":"idle","placement":{{"anchor":{{"kind":"panel"}},"x":10,"y":10,"visible":False,"label":"Ready to place"}},"history":[],"context":None,"approval":None}});publish();emit({{"type":"ok","id":"test"}})
 elif kind=="lively":state["lively"]=cmd["enabled"];publish()
 elif kind=="style":state["blobs"][0]["appearance"]=cmd["appearance"];publish()
 elif kind=="sudo":state["sudo"]=cmd["enabled"];publish()
 elif kind=="pointer":emit({{"type":"pointer","cursor":None}})
 elif kind=="preview":emit({{"type":"preview","geometry":None}})
 elif kind=="drop":state["panel_open"]=False;state["blobs"][0]["placement"]["anchor"]={{"kind":"workspace","id":1}};publish();emit({{"type":"ok","id":"test"}})
 elif kind=="configure":state["connection"]={{k:cmd[k] for k in ["base_url","model","api_key_env"]}};publish();emit({{"type":"ok","configured":True}})
 elif kind=="remove":state["blobs"]=[];publish();emit({{"type":"ok"}})
''')
    bridge.chmod(0o755)
    source=(ROOT/"ui/shell.qml").read_text().replace("PanelWindow {","FloatingWindow {")
    source=re.sub(r"^\s*anchors \{\s*(?:top|left|bottom|right):\s*true.*\}\n","\n",source,flags=re.M)
    source=re.sub(r"^\s*margins \{.*\}\n","\n",source,flags=re.M)
    source=re.sub(r"^\s*WlrLayershell\..*\n","\n",source,flags=re.M)
    source=re.sub(r"exclusionMode:\s*ExclusionMode.Ignore;?","",source)
    source=source.replace("look:root.currentDrop ?", "id:testPanelFace;look:root.currentDrop ?",1)
    source=source.replace("id:dragGhost","id:dragGhost; Component.onCompleted:root.testGhost=this")
    harness='''
    property var testGhost:null
    property int testPhase:0
    property bool animationSampled:false
    Timer {id:sample;interval:8;onTriggered:{
        if(root.testGhost.x>=478 || root.testGhost.x<78) console.log("TEST_FAILURE no interpolated drag position: "+root.testGhost.x)
        root.animationSampled=true
    }}
    Timer {interval:25;running:true;repeat:true;property int ticks:0
        onTriggered:{
            if(++ticks>100){console.log("TEST_FAILURE timed out phase "+root.testPhase);Qt.quit();return}
            if(!root.connected)return
            switch(root.testPhase){
            case 10:
                if(!root.lively)return
                if(Math.abs(testPanelFace.rotation)<0.01)return
                livelyToggle.checked=false;livelyToggle.toggled();root.testPhase=11;break
            case 11:
                if(root.lively)return
                if(testPanelFace.rotation!==0){console.log("TEST_FAILURE motion continued in quiet mode");Qt.quit();return}
                root.testPhase=3;break
            case 12:if(root.currentDrop.appearance!=="bunny")return;lookSelector.activated(4);root.testPhase=13;break
            case 13:if(root.currentDrop.appearance!=="robot")return;lookSelector.activated(5);root.testPhase=14;break
            case 14:if(root.currentDrop.appearance!=="cloud")return;lookSelector.activated(1);root.testPhase=15;break
            case 15:if(root.currentDrop.appearance!=="cat")return;livelyToggle.checked=true;livelyToggle.toggled();root.testPhase=10;break
            case 0:root.pendingLook="drop";newDropButton.clicked();root.testPhase++;break
            case 1:if(!root.currentDrop)return;lookSelector.activated(1);root.testPhase++;break
            case 2:if(root.currentDrop.appearance!=="cat" || !root.testGhost)return;
                root.dragLook="cat";root.cursorReady=true;root.skipMotion=true;root.cursor=({x:100,y:100});root.dragging=true;lookSelector.activated(3);root.testPhase=12;break
            case 3:root.skipMotion=false;root.cursor=({x:500,y:400});sample.start();root.testPhase++;break
            case 4:if(!root.animationSampled || Math.abs(root.testGhost.x-478)>1)return;
                root.drop(root.currentDrop.id);root.testPhase++;break
            case 5:if(root.state.panel_open)return;
                root.loadSettings();root.settingsOpen=true;endpoint.text="http://127.0.0.1:1234/v1";modelName.text="demo";keyEnv.text="";saveConnectionButton.clicked();root.testPhase++;break
            case 6:if(root.settingsOpen)return;sudoToggle.checked=true;sudoToggle.toggled();root.testPhase++;break
            case 7:if(!root.sudo)return;sudoToggle.checked=false;sudoToggle.toggled();root.testPhase++;break
            case 8:if(root.sudo)return;deleteDropButton.clicked();root.testPhase++;break
            case 9:if(root.blobIds.length)return;console.log("TEST_INTERACTIONS_DONE");Qt.quit();break
            }
        }
    }
'''
    source=source.replace("id: root","id: root\n"+harness,1)
    path=base/"shell.qml";path.write_text(source)
    result=subprocess.run(["quickshell","-p",str(path)],env=dict(os.environ,QT_QPA_PLATFORM="offscreen",DROPS_CTL=str(bridge)),capture_output=True,text=True,timeout=8)
    output=result.stdout+result.stderr
    assert "TEST_INTERACTIONS_DONE" in output and "TEST_FAILURE" not in output,output
    assert "ReferenceError" not in output and "TypeError" not in output,output
    commands=[json.loads(line) for line in calls.read_text().splitlines()]
    assert any(c["type"]=="style" and c["appearance"]=="cat" for c in commands)
    assert any(c["type"]=="drop" and c["use_cursor"] for c in commands)
    assert any(c["type"]=="configure" and c["base_url"]=="http://127.0.0.1:1234/v1" and c["model"]=="demo" for c in commands)
    assert any(c["type"]=="remove" for c in commands)
    assert [c["enabled"] for c in commands if c["type"]=="lively"]==[True,False]
    assert [c["enabled"] for c in commands if c["type"]=="sudo"]==[True,False]
    for look in ["bunny","robot","cloud"]:
        assert any(c["type"]=="style" and c["appearance"]==look for c in commands)
    print("PASS: real QML create/style/delete controls, endpoint form, sudo toggle, lively motion start/stop, panel close on drop, interpolated pointer-following animation, all new styles")
