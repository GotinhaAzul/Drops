#!/usr/bin/env python3
"""Check that reasoning renders as a compact, expandable block offscreen."""
import json
import os
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

thought = " ".join(["reason"] * 400)
fixture = {
    "panel_open": True,
    "appearance": {},
    "blobs": [{
        "id": "fixture",
        "appearance": "drop",
        "state": "idle",
        "placement": {"anchor": {"kind": "workspace", "id": 1}, "x": 10, "y": 10, "visible": True, "label": "Test workspace"},
        "context": None,
        "approval": None,
        "history": [
            {"type": "user", "text": "What is open?"},
            {"type": "thinking", "text": thought},
            {"type": "text", "text": "A terminal."},
        ],
    }],
}
source = source.replace(
    "ShellRoot {",
    "ShellRoot {\nComponent.onCompleted: { root.state=" + json.dumps(fixture) + '; root.blobIds=["fixture"]; root.selectedId="fixture"; root.chatOpen=true }',
    1,
)
source = source.replace("id:thought", 'objectName:"thinking-"+chatEntry.index;id:thought')
harness = '''
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
            if(++ticks>160){console.log("TEST_FAILURE timed out phase "+root.testPhase);Qt.quit();return}
            if(!root.selected)return
            const view=root.chatViews["fixture"]
            if(!view||!view.scroll||!view.scroll.contentItem)return
            const thoughtItem=root.findNamed(view.scroll.contentItem,"thinking-1")
            switch(root.testPhase){
            case 0:
                if(!thoughtItem)return
                console.log("TEST_PHASE0", root.thinkingMode, thoughtItem.visible, thoughtItem.expanded, thoughtItem.summary, thoughtItem.height, thoughtItem.parent.height)
                root.setThinkingExpanded(root.chatSession+":1",true);root.testPhase++;break
            case 1:
                if(!thoughtItem || !thoughtItem.expanded)return
                console.log("TEST_PHASE1", thoughtItem.visible, thoughtItem.expanded, thoughtItem.height, thoughtItem.parent.height)
                root.setThinkingExpanded(root.chatSession+":1",false);root.testPhase++;break
            case 2:
                if(!thoughtItem || thoughtItem.expanded)return
                console.log("TEST_PHASE2", thoughtItem.visible, thoughtItem.expanded, thoughtItem.height, view.scroll.contentItem.contentHeight)
                const hidden=JSON.parse(JSON.stringify(root.state));hidden.appearance={thinking_mode:"hidden"};root.state=hidden;root.syncChat();root.testPhase++;break
            case 3:
                if(!thoughtItem || thoughtItem.visible)return
                console.log("TEST_PHASE3", thoughtItem.visible, thoughtItem.parent.height, view.scroll.contentItem.contentHeight)
                const shown=JSON.parse(JSON.stringify(root.state));shown.appearance={thinking_mode:"shown"};root.state=shown;root.thinkingExpanded={};root.syncChat();root.testPhase++;break
            case 4:
                if(!thoughtItem || !thoughtItem.visible || !thoughtItem.expanded)return
                console.log("TEST_PHASE4", thoughtItem.expanded, thoughtItem.height, thoughtItem.parent.height)
                console.log("TEST_THINKING_DONE");Qt.quit();break
            }
        }
    }
'''
source = source.replace("id: root", "id: root\n" + harness, 1)

with tempfile.TemporaryDirectory(prefix="drops-qml-thinking-") as directory:
    path = pathlib.Path(directory) / "shell.qml"
    path.write_text(source)
    (path.parent / "markdown.js").write_text((root / "ui/markdown.js").read_text())
    process = subprocess.Popen(
        ["quickshell", "-p", str(path)],
        env=dict(os.environ, QT_QPA_PLATFORM="offscreen", DROPS_SOCKET=str(pathlib.Path(directory) / "absent.sock")),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        output, _ = process.communicate(timeout=8)
    except subprocess.TimeoutExpired:
        process.terminate()
        output, _ = process.communicate(timeout=5)

    assert "TEST_THINKING_DONE" in output, output
    assert "TEST_FAILURE" not in output, output
    assert "ReferenceError" not in output and "TypeError" not in output, output

    def phase(number):
        match = re.search(rf"TEST_PHASE{number} (.+)", output)
        assert match, output
        return match.group(1).split()

    collapsed = phase(0)
    assert collapsed[0] == "collapsed", output
    assert collapsed[1] == "true" and collapsed[2] == "false", output
    assert collapsed[3:6] == ["Thinking", "·", "400"], output
    assert float(collapsed[7]) == 20 and float(collapsed[8]) == 20, output

    expanded = phase(1)
    assert expanded[0] == "true" and expanded[1] == "true", output
    assert 100 < float(expanded[2]) <= 160, output
    assert expanded[2] == expanded[3], output

    recollapsed = phase(2)
    assert recollapsed[0] == "true" and recollapsed[1] == "false" and recollapsed[2] == "20", output

    hidden = phase(3)
    assert hidden[0] == "false" and float(hidden[1]) == 0, output
    removed = float(recollapsed[3]) - float(hidden[2])
    assert 25 <= removed <= 30, f"hidden thinking left a gap: {removed:.1f}px\n{output}"

    shown = phase(4)
    assert shown[0] == "true", output
    assert 100 < float(shown[1]) <= 160, output

print("PASS: reasoning renders collapsed by default, expands on demand, hides without a blank row, and can default to shown")
