#!/usr/bin/env python3
"""Exercise real QML bubble geometry without requiring a Wayland desktop.

Only the unsupported layer surfaces are replaced; assert the computed placement,
not FloatingWindow's platform positioning or compositor stacking.
"""
import json
import os
import pathlib
import re
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
fixture = {
    'panel_open': False,
    'connection': {'model': 'fixture'},
    'blobs': [{
        'id': 'fixture', 'appearance': 'drop', 'state': 'waiting',
        'placement': {'anchor': {'kind': 'workspace', 'id': 1},
                      'x': 350, 'y': 420, 'visible': True, 'label': 'Test'},
        'context': None,
        'history': [{'type': 'user', 'text': 'Please make a change. ' * 12},
                    {'type': 'text', 'text': 'Here is what I will change. ' * 30}],
        'approval': {'name': 'write', 'preview': {'diff': '+approved change'}},
    }],
}
source = (ROOT / 'ui/shell.qml').read_text().replace('PanelWindow {', 'FloatingWindow {')
source = re.sub(r'^\s*anchors \{\s*(?:top|left|bottom|right):\s*true.*\}\n', '\n', source, flags=re.M)
source = re.sub(r'^\s*margins \{.*\}\n', '\n', source, flags=re.M)
source = re.sub(r'^\s*WlrLayershell\..*\n', '\n', source, flags=re.M)
source = re.sub(r'exclusionMode:\s*ExclusionMode.Ignore;?', '', source)
harness = '''
    Component.onCompleted: {
        root.state=FIXTURE;root.blobIds=["fixture"];root.selectedId="fixture";root.chatOpen=true
    }
    Timer {interval:350;running:true;onTriggered:{
        const w=root.chatViews["fixture"].window
        console.log("POSITION_RESULT "+JSON.stringify({
            top:w.top,height:w.implicitHeight,left:w.left,width:w.implicitWidth,
            above:w.above,tailX:w.tailX,tailSize:w.tailSize,
            place:w.place,monitor:{x:w.monitor.x,y:w.monitor.y,width:w.monitor.width,height:w.monitor.height},
            size:root.blobSize,logHeight:root.chatViews["fixture"].scroll.height
        }))
        Qt.quit()
    }}
'''
source_template = source.replace('id: root', 'id: root\n' + harness, 1)
cases = [(350, 420, 44), (350, 350, 44), (350, 350, 20),
         (350, 420, 52), (0, 40, 44), (720, 730, 44)]
for x, y, size in cases:
    fixture['appearance'] = {'blob_size': size}
    fixture['blobs'][0]['placement'].update(x=x, y=y)
    source = source_template.replace('FIXTURE', json.dumps(fixture))
    with tempfile.TemporaryDirectory(prefix='drops-position-') as directory:
        base = pathlib.Path(directory)
        bridge = base / 'bridge.py'
        bridge.write_text('#!/usr/bin/python3\nimport time\ntime.sleep(5)\n')
        bridge.chmod(0o755)
        path = base / 'shell.qml'
        path.write_text(source)
        result = subprocess.run(['quickshell', '-p', str(path)], env=dict(
            os.environ, QT_QPA_PLATFORM='offscreen', DROPS_CTL=str(bridge)),
            capture_output=True, text=True, timeout=8)
        output = result.stdout + result.stderr
        match = re.search(r'POSITION_RESULT (.+)', output)
        assert match, output
        assert not any(x in output for x in ['ReferenceError', 'TypeError', 'Binding loop']), output
        g = json.loads(match[1])
        # 72px avatar surface, positioned at place-8, with the face centered inside.
        face_top = max(g['monitor']['y'], g['place']['y'] - 8) + (72 - (g['size'] - 2)) / 2
        face_bottom = face_top + g['size'] - 2
        bubble_bottom = g['top'] + g['height']
        print(f'case x={x}, y={y}, size={size}: side={"above" if g["above"] else "below"}, history={g["logHeight"]:.0f}px', flush=True)
        assert g['top'] >= g['monitor']['y'] + 8 - 1
        assert bubble_bottom <= g['monitor']['y'] + g['monitor']['height'] - 8 + 1
        if g['above']:
            assert bubble_bottom <= face_top, f'Bubble overlaps the avatar by {bubble_bottom-face_top:.1f}px'
        else:
            assert g['top'] >= face_bottom, f'Bubble overlaps the avatar by {face_bottom-g["top"]:.1f}px'
        assert abs(g['left'] + g['tailX'] - (max(g['monitor']['x'], g['place']['x'] - 8) + 36)) < 1
print('PASS: approval bubbles fit without avatar overlap at both sides, screen edges and supported sizes')
