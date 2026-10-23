#!/usr/bin/env python3
"""Check that assistant chat text is rendered as Markdown + LaTeX rich text."""
import json
import os
import pathlib
import re
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
assistant = """# Maths

**bold** and *italic* with $E=mc^2$ and $\\frac{a+b}{c}$.

- one
- two

$$\\int_0^1 x^2\\,dx$$

| A | B |
|:---|---:|
| 1 | 2 |

[site](https://example.com)

```js
const x = 1;
```
"""
fixture = {
    "panel_open": False,
    "appearance": {},
    "connection": {"base_url": "http://127.0.0.1:11434/v1", "model": "test", "api_key_env": ""},
    "blobs": [{
        "id": "fixture",
        "appearance": "drop",
        "state": "idle",
        "placement": {"anchor": {"kind": "window", "address": "0x1"}, "x": 420, "y": 420, "visible": True, "label": "Fixture Page"},
        "context": None,
        "approval": None,
        "history": [
            {"type": "user", "text": "Show me"},
            {"type": "text", "text": assistant},
        ],
    }],
}
source = (ROOT / "ui/shell.qml").read_text().replace("PanelWindow {", "FloatingWindow {")
source = re.sub(r"^\s*anchors \{\s*(?:top|left|bottom|right):\s*true.*\}\n", "\n", source, flags=re.M)
source = re.sub(r"^\s*margins \{.*\}\n", "\n", source, flags=re.M)
source = re.sub(r"^\s*WlrLayershell\..*\n", "\n", source, flags=re.M)
source = re.sub(r"exclusionMode:\s*ExclusionMode.Ignore;?", "", source)
source = source.replace(
    "ShellRoot {",
    "ShellRoot {\nComponent.onCompleted: { root.connected=true; root.state="
    + json.dumps(fixture)
    + '; root.blobIds=["fixture"]; root.selectedId="fixture"; root.chatOpen=true; root.syncChat() }',
    1,
)
harness = '''
    function findNamed(item,name){
        if(!item)return null
        if(item.objectName===name)return item
        const kids=item.children||[]
        for(let i=0;i<kids.length;i++){const found=findNamed(kids[i],name);if(found)return found}
        return null
    }
    Timer {interval:25;running:true;repeat:true;property int ticks:0
        onTriggered:{
            if(++ticks>200){console.log("TEST_FAILURE timed out");Qt.quit();return}
            if(!root.chatOpen || !root.selected)return
            const view=root.chatViews["fixture"]
            if(!view||!view.scroll||!view.scroll.contentItem)return
            const assistantLine=root.findNamed(view.scroll.contentItem,"chat-line-1")
            const userLine=root.findNamed(view.scroll.contentItem,"chat-line-0")
            if(!assistantLine||!userLine)return
            console.log("TEST_ASSISTANT", assistantLine.textFormat===Text.RichText, assistantLine.text)
            console.log("TEST_USER", userLine.textFormat===Text.PlainText, userLine.text)
            console.log("TEST_HEIGHT", assistantLine.implicitHeight)
            console.log("TEST_MARKDOWN_DONE");Qt.quit()
        }
    }
'''
source = source.replace("id: root", "id: root\n" + harness, 1)

with tempfile.TemporaryDirectory(prefix="drops-qml-markdown-") as directory:
    path = pathlib.Path(directory) / "shell.qml"
    path.write_text(source)
    (path.parent / "markdown.js").write_text((ROOT / "ui/markdown.js").read_text())
    process = subprocess.Popen(
        ["quickshell", "-p", str(path)],
        env=dict(os.environ, QT_QPA_PLATFORM="offscreen", DROPS_SOCKET=str(pathlib.Path(directory) / "absent.sock")),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        output, _ = process.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        process.terminate()
        output, _ = process.communicate(timeout=5)

    assert "TEST_MARKDOWN_DONE" in output, output
    if os.environ.get("DROPS_QML_VERBOSE"):
        print(output)
    assert "TEST_FAILURE" not in output, output
    assert "ReferenceError" not in output and "TypeError" not in output, output

    rich = re.search(r"TEST_ASSISTANT (true|false) (.+)$", output, re.M)
    assert rich, output
    assert rich.group(1) == "true", output
    rendered = rich.group(2)
    for expected in ("<b>", "<i>", "<ul", "<pre", "<a href=", "<font size=", "E=mc²", "(a+b)/c", "<table", "∫₀¹"):
        assert expected in rendered, (expected, rendered)
    assert "**" not in rendered, rendered
    assert "$" not in rendered, rendered

    user = re.search(r"TEST_USER (true|false) (.+)$", output, re.M)
    assert user, output
    assert user.group(1) == "true", output
    assert user.group(2) == "Show me", output

    height = re.search(r"TEST_HEIGHT ([0-9.]+)$", output, re.M)
    assert height and float(height.group(1)) > 40, output
    print("PASS: assistant text renders Markdown and LaTeX as rich text; user text stays plain")
