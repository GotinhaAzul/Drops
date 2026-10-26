#!/usr/bin/env python3
"""Check the context inspector always has a visible exit and never outlives the panel."""
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT=pathlib.Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="drops-qml-inspector-") as directory:
    base=pathlib.Path(directory)
    bridge=base/"bridge.py"
    bridge.write_text(f'''#!{sys.executable}
import json,sys
state={{"panel_open":True,"connection":{{"base_url":"http://localhost:11434/v1","model":"test","api_key_env":""}},"blobs":[]}}
context={{"mode":"fixture","tool_workspace":{{"root":"/tmp/workspace","source":"configured"}},
          "app_context":{{"status":"available","page":{{"uri":"https://example.com/article","title":"Fixture article"}},
          "documents":[{{"uri":"https://example.com/article","title":"Fixture article","depth":1}}],
          "selections":[{{"text":"Selected fixture sentence","start":0,"end":26}}],"selected_items":[]}},
          "content":{{"status":"captured","text":"fixture"}}}}
state["blobs"].append({{"id":"fixture","appearance":"drop","state":"idle",
    "placement":{{"anchor":{{"kind":"workspace","id":1}},"x":10,"y":10,"visible":True,"label":"Test workspace"}},
    "context":context,"approval":None,"history":[]}})
def emit(value):print(json.dumps(value),flush=True)
def publish():emit({{"type":"state","state":state}})
publish()
for line in sys.stdin:
 cmd=json.loads(line)
 if cmd["type"]=="context":emit({{"type":"context","context":state["blobs"][0]["context"]}})
 elif cmd["type"]=="toggle":
  state["panel_open"]=not state["panel_open"];publish()
''')
    bridge.chmod(0o755)
    source=(ROOT/"ui/shell.qml").read_text().replace("PanelWindow {","FloatingWindow {")
    source=re.sub(r"^\s*anchors \{\s*(?:top|left|bottom|right):\s*true.*\}\n","\n",source,flags=re.M)
    source=re.sub(r"^\s*margins \{.*\}\n","\n",source,flags=re.M)
    source=re.sub(r"^\s*WlrLayershell\..*\n","\n",source,flags=re.M)
    source=re.sub(r"exclusionMode:\s*ExclusionMode.Ignore;?","",source)
    harness='''
    property int testPhase:0
    function togglePanel(open) {
        const next=JSON.parse(JSON.stringify(root.state));next.panel_open=open;root.state=next
    }
    Timer {interval:25;running:true;repeat:true;property int ticks:0
        onTriggered:{
            if(++ticks>120){console.log("TEST_FAILURE timed out phase "+root.testPhase);Qt.quit();return}
            if(!root.connected)return
            switch(root.testPhase){
            case 0:
                if(!root.selected)return
                directoryHover.clicked(null)
                if(!contextView.opened){console.log("TEST_FAILURE inspector did not open");Qt.quit();return}
                const closePoint=contextCloseButton.mapToItem(contextView.contentItem,0,0)
                console.log("TEST_PHASE0",contextView.opened,panel.visible,contextCloseButton.visible,contextCloseButton.width,contextCloseButton.height,closePoint.x,closePoint.y,contextView.width,contextView.height)
                contextCloseButton.clicked();root.testPhase++;break
            case 1:
                if(contextView.opened)return
                if(!panel.visible){console.log("TEST_FAILURE closing inspector hid the panel");Qt.quit();return}
                root.inspectContext();root.testPhase++;break
            case 2:
                if(!contextView.opened)return
                root.togglePanel(false);root.testPhase++;break
            case 3:
                if(panel.visible)return
                if(contextView.opened){console.log("TEST_FAILURE inspector survived its panel");Qt.quit();return}
                root.togglePanel(true);root.testPhase++;break
            case 4:
                if(!panel.visible)return
                if(contextView.opened){console.log("TEST_FAILURE inspector reappeared with its panel");Qt.quit();return}
                root.inspectContext();root.testPhase++;break
            case 5:
                if(!contextView.opened)return
                escapeShortcut.activated();root.testPhase++;break
            case 6:
                if(contextView.opened)return
                if(!panel.visible){console.log("TEST_FAILURE escape hid the panel instead of closing the inspector");Qt.quit();return}
                directoryButton.clicked();root.testPhase++;break
            case 7:
                if(!directoryView.opened)return
                escapeShortcut.activated();root.testPhase++;break
            case 8:
                if(directoryView.opened)return
                if(!panel.visible){console.log("TEST_FAILURE escape hid the panel instead of closing the directory dialog");Qt.quit();return}
                root.openSettings()
                if(!root.settingsOpen || contextView.opened || directoryView.opened){console.log("TEST_FAILURE settings left an overlay open");Qt.quit();return}
                console.log("TEST_INSPECTOR_DONE");Qt.quit();break
            }
        }
    }
'''
    source=source.replace("id: root","id: root\n"+harness,1)
    path=base/"shell.qml";path.write_text(source)
    result=subprocess.run(["quickshell","-p",str(path)],env=dict(os.environ,QT_QPA_PLATFORM="offscreen",DROPS_CTL=str(bridge)),capture_output=True,text=True,timeout=12)
    output=result.stdout+result.stderr
    assert "TEST_INSPECTOR_DONE" in output and "TEST_FAILURE" not in output,output
    assert "ReferenceError" not in output and "TypeError" not in output,output
    phase=re.search(r"TEST_PHASE0 (.+)",output)
    assert phase,output
    values=phase.group(1).split()
    assert values[:3]==["true","true","true"],output
    assert float(values[3])>=18 and float(values[4])>=18,output
    x,y,width,height=(float(value) for value in values[5:9])
    assert x>=0 and y>=0 and x+float(values[3])<=width and y+float(values[4])<=height,output
    print("PASS: context inspector has a visible close control, closes without hiding the panel, and cannot outlive or reopen with its panel")
