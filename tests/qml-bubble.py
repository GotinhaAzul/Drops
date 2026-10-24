#!/usr/bin/env python3
"""Exercise the in-place drop conversation bubble with a fake IPC bridge.

Covers the greeting shown after placement, contextual quick actions, the
inline composer, dismissal, the compact reply preview after a send, and the
automatic close/open lifecycle. A window-anchored send goes through the
"capturing" state to prove the drop itself stays visible while its window is
being read.
"""
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT=pathlib.Path(__file__).resolve().parents[1]
LONG_REPLY=(
    "CPU is calm: about 3% used. The busiest process is quickshell at 1.2%, "
    "followed by buddyd. Memory looks healthy, with 13.4 GiB free. Nothing "
    "needs attention right now, and the load average is well below one."
)
SHORT_REPLY="In short: nothing is wrong."
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
import json,sys,pathlib,time
state=json.loads({initial!r})
turns=0
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
  if cmd["text"]=="please fail":
   emit({{"type":"error","message":"model rejected the turn"}});continue
  b=blob(cmd["id"]);turns+=1
  b["state"]="capturing";b["history"].append({{"type":"user","text":cmd["text"]}});publish()
  time.sleep(0.35)
  b["state"]="thinking";publish()
  time.sleep(0.2)
  b["history"].append({{"type":"text","text":{LONG_REPLY!r} if turns==1 else {SHORT_REPLY!r}}});b["state"]="idle";publish()
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
    source=source.replace("id:placed","id:placed; Component.onCompleted:if(modelData===\"fixture\")root.testDrop=this",1)
    harness='''
    property var testDrop:null
    property var testBubble:null
    property bool sawCapturing:false
    property int errorSentTick:0
    function findNamed(item,name){
        if(!item)return null
        if(item.objectName===name)return item
        const kids=item.children||[]
        for(let i=0;i<kids.length;i++){const found=findNamed(kids[i],name);if(found)return found}
        return null
    }
    function tracked(id){return root.state.blobs.find(b=>b.id===id) || null}
    property int testPhase:0
    Timer {interval:25;running:true;repeat:true;property int ticks:0
        onTriggered:{
            if(++ticks>600){
                const f=root.state.blobs.find(b=>b.id==="fixture")
                console.log("TEST_FAILURE timed out phase "+root.testPhase,
                    "drop",!!root.testDrop,"bubble",!!root.testBubble,"capturing",root.sawCapturing,
                    "state",f?f.state:"?","history",f?f.history.length:"?",
                    "previewMode",root.testBubble?root.testBubble.previewMode:"?",
                    "previewEntry",!!(root.testBubble&&root.testBubble.previewEntry),
                    "chatOpen",root.chatOpen)
                Qt.quit();return
            }
            if(!root.connected)return
            const fixture=root.tracked("fixture")
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
                root.testBubble=v.window
                v.sendSuggestion(0);root.testPhase++;break
            }
            case 2:
                if(!root.testDrop || !root.testBubble || !fixture)return
                if(!root.sawCapturing && fixture.state==="capturing"){
                    root.sawCapturing=true
                    console.log("TEST_CAPTURING", root.chatOpen, root.testDrop.visible, root.chatViews["fixture"]===undefined)
                }
                if(root.chatOpen || !root.sawCapturing || !root.testBubble.previewMode)return
                const preview=root.findNamed(root.testBubble.contentItem,"response-preview-text-fixture")
                if(!preview)return
                console.log("TEST_PREVIEW", root.testBubble.visible, root.testBubble.previewMode, preview.text, preview.lineCount, preview.maximumLineCount, preview.truncated, root.testBubble.implicitHeight)
                console.log("TEST_PREVIEW_AWAKE", root.testDrop.awake)
                const openArea=root.findNamed(root.testBubble.contentItem,"response-preview-open-fixture")
                if(!openArea){console.log("TEST_FAILURE preview has no click target");Qt.quit();return}
                openArea.clicked(null);root.testPhase++;break
            case 3: {
                if(!root.chatOpen || !root.chatViews["fixture"] || root.testBubble.previewMode)return
                const v=root.chatViews["fixture"]
                console.log("TEST_REOPENED", v.window.visible, v.scroll.visible, v.scroll.height, v.scroll.contentItem.contentHeight)
                v.input.text="Explain that simply.";v.submit();root.testPhase++;break
            }
            case 4:
                if(root.chatOpen || !fixture || fixture.state!=="idle" || fixture.history.length<4)return
                if(!root.testBubble.previewMode)return
                const second=root.findNamed(root.testBubble.contentItem,"response-preview-text-fixture")
                if(!second || second.text!==root.previewTextFor(fixture))return
                console.log("TEST_PREVIEW2", second.text, second.lineCount)
                const secondOpen=root.findNamed(root.testBubble.contentItem,"response-preview-open-fixture")
                if(!secondOpen){console.log("TEST_FAILURE second preview has no click target");Qt.quit();return}
                secondOpen.clicked(null);root.testPhase++;break
            case 5:
                if(!root.chatOpen || !root.chatViews["fixture"])return
                console.log("TEST_HISTORY", root.chatViews["fixture"].scroll.contentItem.contentHeight>0, fixture.history.length)
                root.closeChat();root.testPhase++;break
            case 6:
                if(root.chatOpen || root.testBubble.previewMode || root.testBubble.visible)return
                console.log("TEST_SEEN", root.testBubble.visible, root.testBubble.previewMode)
                root.send({type:"rehide",id:"fixture"});root.testPhase++;break
            case 7:
                if(root.chatOpen || root.chatViews["fixture"])return
                console.log("TEST_CLOSED", root.chatOpen, root.chatViews["fixture"]===undefined)
                root.drop("draft");root.testPhase++;break
            case 8: {
                if(!root.chatOpen || root.selectedId!=="draft" || !root.chatViews["draft"])return
                const v=root.chatViews["draft"]
                console.log("TEST_AUTO_OPEN", root.chatOpen, root.selectedId, v.greeting.text, v.suggestionCount)
                root.closeChat();root.testPhase++;break
            }
            case 9:
                if(root.chatOpen || root.chatViews["draft"])return
                root.openChat("draft",true);root.testPhase++;break
            case 10: {
                if(!root.chatOpen || !root.chatViews["draft"])return
                const v=root.chatViews["draft"]
                v.input.text="please fail";v.submit()
                root.errorSentTick=ticks;root.testPhase++;break
            }
            case 11:
                if(!root.failure || root.failure.indexOf("model rejected")<0)return
                // The guard window expires after 1.5s; a rejected send still
                // leaves the bubble open with its error visible.
                if(ticks-root.errorSentTick<70)return
                console.log("TEST_SEND_ERROR", root.chatOpen, root.failure)
                root.closeChat();root.testPhase++;break
            case 12:
                if(root.chatOpen)return
                console.log("TEST_BUBBLE_DONE");Qt.quit();break
            }
        }
    }
'''
    source=source.replace("id: root","id: root\n"+harness,1)
    path=base/"shell.qml";path.write_text(source)
    result=subprocess.run(["quickshell","-p",str(path)],env=dict(os.environ,QT_QPA_PLATFORM="offscreen",DROPS_CTL=str(bridge)),capture_output=True,text=True,timeout=20)
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

    capturing=line("TEST_CAPTURING").split()
    assert capturing[0]=="false",output             # the chat collapses on send
    assert capturing[1]=="true",output              # the placed drop stays visible while capturing
    assert capturing[2]=="true",output              # the full chat surface is gone while capturing

    preview=line("TEST_PREVIEW")
    match=re.match(r"(true|false) (true|false) (.*?) (\d+) (\d+) (true|false) ([0-9.]+)$",preview)
    assert match,preview
    visible,preview_mode,text,lines,maximum,truncated,height=match.groups()
    assert visible=="true" and preview_mode=="true",preview
    assert text.startswith("CPU is calm"),preview
    assert lines=="3" and maximum=="3",preview    # visually truncated to three lines
    assert truncated=="true",preview
    assert 40<float(height)<130,preview            # a compact bubble, not the full chat
    assert line("TEST_PREVIEW_AWAKE").strip()=="true",output   # the reply keeps the avatar awake

    reopened=line("TEST_REOPENED").split()
    assert reopened[0]=="true" and reopened[1]=="true",output
    assert float(reopened[2])>0 and float(reopened[3])>0,output

    second=line("TEST_PREVIEW2").split()
    assert " ".join(second[:-1])==SHORT_REPLY,second
    assert second[-1]=="1",second
    history=line("TEST_HISTORY").split()
    assert history==["true","4"],history
    assert line("TEST_SEEN").split()==["false","false"],output

    assert line("TEST_CLOSED").split()==["false","true"],output
    auto=line("TEST_AUTO_OPEN")
    assert "draft" in auto and "workspace" in auto.lower(),output
    assert "2" in auto.split()[-1],output
    assert line("TEST_SEND_ERROR")=="true model rejected the turn",output

    commands=[json.loads(row) for row in calls.read_text().splitlines()]
    sends=[c for c in commands if c["type"]=="send"]
    assert [c["text"] for c in sends]==["How's my CPU right now?","Explain that simply.","please fail"],commands
    assert [c["id"] for c in sends]==["fixture","fixture","draft"],commands
    assert any(c["type"]=="drop" and c["id"]=="draft" and c["use_cursor"] for c in commands),commands
    print("PASS: placed drops keep their avatar during capture, collapse on send, show a truncated reply preview above the drop, reopen the full conversation on click, and keep send errors visible")
