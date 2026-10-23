pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Wayland

ShellRoot {
    id: root
    property var state: ({panel_open:false, blobs:[]})
    // Surface identity depends only on blob ids, never on streaming messages.
    property var blobIds: []
    property string selectedId: ""
    property var currentDrop: state.blobs.find(b=>b.id===selectedId) || null
    property var looks: ["drop","cat","sprout"]
    property string pendingLook: "drop"
    property bool settingsOpen: false
    property bool settingsInitialized: false
    property string dragId: ""
    property string dragLook: "drop"
    property var cursor: ({x:0,y:0})
    property bool pointerPending: false
    property bool cursorReady: false
    property bool skipMotion: true
    property bool settling: false
    property string recentDropId: ""
    function loadSettings() {
        const c=state.connection || ({})
        endpoint.text=c.base_url || "http://127.0.0.1:11434/v1"
        modelName.text=c.model || ""
        keyEnv.text=c.api_key_env || ""
        visionMode.checked=!!c.vision
        contentCapture.checked=c.capture_content!==false
        screenCapture.checked=c.screenshots!==false
    }
    property bool chatOpen: false
    property bool capturing: state.blobs.some(b=>b.state==="capturing")
    property var inspectedContext: null
    function inspectContext() {
        inspectedContext=selected ? selected.context : null
        contextView.open()
        if(selected)send({type:"context",id:selectedId})
    }
    function contextMetadata(value) {
        const copy=JSON.parse(JSON.stringify(value || {}))
        if(copy.content && copy.content.image)delete copy.content.image.data_url
        return JSON.stringify(copy,null,2)
    }
    property var selected: chatOpen ? (state.blobs.find(b => b.id === selectedId) || null) : null
    property var appearance: state.appearance || ({})
    property color accent: appearance.accent || "#a7d8f5"
    property color background: appearance.background || "#20251f"
    property color ink: appearance.foreground || "#ecebe4"
    property color muted: appearance.muted || "#92968a"
    property real blobSize: Math.max(20,Math.min(52,appearance.blob_size || 44))
    property string failure: ""
    property bool connected: false
    property bool dragging: false
    property var preview: null
    property string chatSession: ""
    property bool busy: !!selected && ["capturing","thinking","waiting","stopping"].includes(selected.state)
    ListModel { id: chatModel; dynamicRoles: true }
    function syncChat() {
        if (chatSession !== selectedId) { chatModel.clear(); chatSession=selectedId }
        const history = selected ? selected.history : []
        while (chatModel.count > history.length) chatModel.remove(chatModel.count-1)
        for (let i=0;i<history.length;i++) {
            if (i<chatModel.count) chatModel.setProperty(i,"entry",history[i])
            else chatModel.append({entry:history[i]})
        }
    }
    onSelectedChanged: syncChat()
    function send(command) {
        if (!connected) { failure="Drops is reconnecting…"; return }
        bridge.write(JSON.stringify(command)+"\n")
    }
    function drop(id) {
        dragging=false; preview=null; failure="";settling=true;settleTimer.restart()
        const command={type:"drop",x:0,y:0,use_cursor:true}
        if (id) command.id=id
        send(command)
    }
    function openChat(id) {
        selectedId=id; chatOpen=true
        if (!state.panel_open) send({type:"toggle"})
    }
    Process {
        id: bridge
        command: [Quickshell.env("DROPS_CTL") || "buddyctl","bridge"]
        stdinEnabled:true; running:true
        stdout: SplitParser {
            onRead: data => {
                try {
                    const msg=JSON.parse(data)
                    if (msg.type === "state") {
                        if (!root.connected) root.failure=""
                        root.connected=true
                        const ids=msg.state.blobs.map(b=>b.id)
                        if (JSON.stringify(ids)!==JSON.stringify(root.blobIds)) root.blobIds=ids
                        if (!msg.state.panel_open) root.chatOpen=false
                        root.state=msg.state
                        if (!ids.includes(root.selectedId)) {root.selectedId=ids[0] || "";root.chatOpen=false}
                        if (!root.settingsInitialized) {root.loadSettings();root.settingsInitialized=true;root.settingsOpen=!msg.state.connection || !msg.state.connection.model}
                        root.syncChat()
                    } else if (msg.type === "context") {root.inspectedContext=msg.context}
                    else if (msg.type === "pointer") {root.pointerPending=false;if(msg.cursor){root.cursor=msg.cursor;root.cursorReady=true;motionWarmup.start()}}
                    else if(msg.type==="ok" && msg.id) {root.selectedId=msg.id;root.recentDropId=msg.id;recentTimer.restart()}
                    else if(msg.type==="ok" && msg.configured) {root.settingsOpen=false;root.failure=""}
                    else if (msg.type === "preview" && root.dragging) {root.preview=msg.geometry ? msg : null;root.failure=""}
                    else if (msg.type === "error") root.failure=msg.message
                } catch(e) { root.failure=String(e) }
            }
        }
        onExited: { root.connected=false;root.dragging=false;root.settling=false;root.pointerPending=false;root.failure="Start buddyd to connect." }
    }
    Timer { interval:2000; repeat:true; running:!bridge.running; onTriggered:bridge.running=true }
    Timer {id:motionWarmup;interval:32;onTriggered:root.skipMotion=false}
    Timer {id:settleTimer;interval:180;onTriggered:root.settling=false}
    Timer {id:recentTimer;interval:1100;onTriggered:root.recentDropId=""}
    Timer {interval:16;repeat:true;running:root.dragging;onTriggered:{if(!root.pointerPending){root.pointerPending=true;root.send({type:"pointer"})}}}
    Timer { interval:120; repeat:true; running:root.dragging; onTriggered:root.send({type:"preview",x:0,y:0,use_cursor:true}) }

    component DropFace: Rectangle {
        property bool awake:true
        property string look:"drop"
        property color tint:look==="cat" ? "#f2c6a0" : look==="sprout" ? "#b8dca4" : root.accent
        color:tint
        Rectangle {visible:parent.awake && parent.look==="cat";width:parent.width*0.28;height:width;rotation:45;radius:3;color:parent.color;x:parent.width*0.08;y:-height*0.25;z:-1}
        Rectangle {visible:parent.awake && parent.look==="cat";width:parent.width*0.28;height:width;rotation:45;radius:3;color:parent.color;x:parent.width*0.64;y:-height*0.25;z:-1}
        Rectangle {visible:parent.awake && parent.look==="sprout";width:parent.width*0.34;height:parent.width*0.18;radius:height/2;color:"#7fa76a";rotation:30;x:parent.width*0.2;y:-height*0.65}
        Rectangle {visible:parent.awake && parent.look==="sprout";width:parent.width*0.34;height:parent.width*0.18;radius:height/2;color:"#94bd7c";rotation:-30;x:parent.width*0.49;y:-height*0.65}
        radius: width*(typeof root.appearance.roundness === "number" ? Math.max(0,Math.min(0.5,root.appearance.roundness)) : 0.48)
        Behavior on width { NumberAnimation {duration:160} }
        Behavior on height { NumberAnimation {duration:160} }
        Row {
            anchors.centerIn:parent; spacing:parent.width*0.15; visible:parent.awake
            Rectangle { width:parent.parent.width*0.13; height:width*1.2; radius:width; color:"#27322a" }
            Rectangle { width:parent.parent.width*0.13; height:width*1.2; radius:width; color:"#27322a" }
        }
    }
    component DragArea: MouseArea {
        property string blobId:""
        property real sx:0
        property real sy:0
        anchors.fill:parent; cursorShape:Qt.OpenHandCursor
        onPressed: mouse=> {sx=mouse.x;sy=mouse.y;root.dragId=blobId;root.cursorReady=false;root.skipMotion=true;const b=root.state.blobs.find(b=>b.id===blobId);root.dragLook=b ? b.appearance || "drop" : root.pendingLook;root.settling=false}
        onPositionChanged: mouse=> {if(pressed && Math.abs(mouse.x-sx)+Math.abs(mouse.y-sy)>6) root.dragging=true}
        onReleased: mouse=> {
            if (root.dragging) root.drop(blobId)
            else if(blobId && root.state.blobs.find(b=>b.id===blobId && b.placement.anchor.kind!=="panel")) root.openChat(blobId)
            root.dragging=false;root.preview=null
        }
        onCanceled: {root.dragging=false;root.preview=null;root.settling=false;root.pointerPending=false}
    }
    PanelWindow {
        id: panel
        visible:root.state.panel_open && !root.capturing
        anchors {top:true;right:true}
        margins {top:40;right:20}
        implicitWidth:260
        implicitHeight:root.settingsOpen ? 444 : 250+(root.selected && root.selected.context && root.selected.context.content ? 20 : 0)+(root.selected && root.selected.history.length ? 150 : 0)+(root.selected && root.selected.approval ? 180 : 0)+(root.failure ? 26 : 0)
        color:"transparent";exclusionMode:ExclusionMode.Ignore
        WlrLayershell.namespace:"drops-panel"
        WlrLayershell.keyboardFocus:WlrKeyboardFocus.OnDemand
        Shortcut {sequence:"Escape";enabled:panel.visible;onActivated:root.send({type:"toggle"})}
        Rectangle {
            anchors.fill:parent;color:root.background;radius:18;border.color:"#4b5545";border.width:1
            ColumnLayout {
                visible:!root.settingsOpen
                anchors.fill:parent;anchors.margins:14;spacing:8
                Item {
                    Layout.fillWidth:true;Layout.preferredHeight:112
                    DropFace {
                        anchors.centerIn:parent;width:76;height:74
                        look:root.currentDrop ? root.currentDrop.appearance || "drop" : root.pendingLook
                        opacity:!root.currentDrop ? 0.45 : root.dragging && root.dragId===root.currentDrop.id ? 0 : 1
                        Behavior on opacity {NumberAnimation{duration:70}}
                        DragArea {enabled:!!root.currentDrop;blobId:root.currentDrop ? root.currentDrop.id : ""}
                    }
                    ToolButton {
                        anchors.top:parent.top;anchors.right:parent.right
                        text:root.busy ? "■" : "×";font.pixelSize:13;palette.buttonText:root.muted
                        onClicked: root.busy ? root.send({type:"cancel",id:root.selectedId}) : root.send({type:"toggle"})
                    }
                    Row {
                        anchors.bottom:parent.bottom;anchors.horizontalCenter:parent.horizontalCenter;spacing:9
                        visible:root.blobIds.length>1
                        Repeater {
                            model:root.blobIds
                            delegate: Rectangle {
                                required property string modelData
                                width:8;height:8;radius:4;color:root.accent;opacity:root.selectedId===modelData && root.chatOpen ? 1 : 0.4
                                MouseArea {anchors.fill:parent;anchors.margins:-5;onClicked:{root.selectedId=parent.modelData;root.chatOpen=false}}
                            }
                        }
                    }
                }
                Label {
                    Layout.fillWidth:true;horizontalAlignment:Text.AlignHCenter;color:root.muted;font.pixelSize:10;elide:Text.ElideRight
                    text:root.currentDrop ? root.currentDrop.placement.label : "Make a drop, then drag it onto a window"
                    MouseArea {anchors.fill:parent;enabled:!!root.selected;onClicked:root.inspectContext()}
                }
                Label {
                    visible:!!root.selected && !!root.selected.context && !!root.selected.context.content
                    Layout.fillWidth:true;horizontalAlignment:Text.AlignHCenter;color:root.muted;font.pixelSize:10
                    text:!root.selected || !root.selected.context || !root.selected.context.content ? "" : root.selected.context.content.status==="captured" ? "Window content attached · click to inspect" : "Window content unavailable · click for details"
                    MouseArea {anchors.fill:parent;onClicked:root.inspectContext()}
                }
                RowLayout {
                    Layout.fillWidth:true;spacing:5
                    ComboBox {
                        id:lookSelector
                        Layout.fillWidth:true;Layout.preferredHeight:28
                        model:["Drop","Cat","Sprout"];font.pixelSize:11
                        currentIndex:root.looks.indexOf(root.currentDrop ? root.currentDrop.appearance || "drop" : root.pendingLook)
                        onActivated:index=>{root.pendingLook=root.looks[index];if(root.currentDrop)root.send({type:"style",id:root.currentDrop.id,appearance:root.looks[index]})}
                    }
                    Button {id:newDropButton;Accessible.name:"New drop";text:"+";ToolTip.visible:hovered;ToolTip.text:"New drop";Layout.preferredWidth:30;Layout.preferredHeight:28;enabled:root.blobIds.length<3;onClicked:{root.chatOpen=false;root.send({type:"create",appearance:root.currentDrop ? root.currentDrop.appearance || "drop" : root.pendingLook})}}
                    Button {id:deleteDropButton;Accessible.name:"Delete drop";text:"−";ToolTip.visible:hovered;ToolTip.text:"Delete selected drop and conversation";Layout.preferredWidth:30;Layout.preferredHeight:28;enabled:!!root.currentDrop;onClicked:root.send({type:"remove",id:root.currentDrop.id})}
                    ToolButton {text:"⚙";ToolTip.visible:hovered;ToolTip.text:"Model connection";Layout.preferredWidth:28;Layout.preferredHeight:28;onClicked:{root.loadSettings();root.settingsOpen=true;root.failure=""}}
                }
                ScrollView {
                    id:chat;visible:!!root.selected && root.selected.history.length>0
                    Layout.fillWidth:true;Layout.preferredHeight:142;clip:true;contentWidth:availableWidth
                    Column {
                        width:chat.availableWidth;spacing:7
                        Repeater {
                            model:chatModel
                            delegate:Label {
                                required property var entry
                                property bool expanded:root.appearance.thinking_mode==="shown"
                                width:parent.width;wrapMode:Text.Wrap;textFormat:Text.PlainText;font.pixelSize:12
                                color:entry.type==="user" ? root.accent : entry.type==="error" ? "#f4a99a" : root.ink
                                visible:["user","text","error","tool_start","tool_result"].includes(entry.type) || entry.type==="thinking" && !!root.appearance.thinking_mode && root.appearance.thinking_mode!=="hidden"
                                text:entry.type==="thinking" && !expanded ? "Thinking · expand" : entry.type==="tool_start" ? "↳ "+entry.name : entry.type==="user" ? "you / "+entry.text : (entry.text || "")
                                MouseArea {anchors.fill:parent;enabled:parent.entry.type==="thinking";onClicked:parent.expanded=!parent.expanded}
                            }
                        }
                    }
                    onContentHeightChanged: if(contentItem) contentItem.contentY=Math.max(0,contentHeight-height)
                }
                Rectangle {
                    visible:!!root.selected && !!root.selected.approval
                    Layout.fillWidth:true;Layout.preferredHeight:172;color:"#333326";radius:10
                    ColumnLayout {
                        anchors.fill:parent;anchors.margins:8;spacing:5
                        Label {text:"Allow this action?";color:root.accent;font.pixelSize:12}
                        ScrollView {
                            Layout.fillWidth:true;Layout.fillHeight:true;clip:true
                            TextArea {readOnly:true;wrapMode:TextEdit.WrapAnywhere;color:root.ink;font.pixelSize:11;textFormat:TextEdit.PlainText;text:root.selected && root.selected.approval ? JSON.stringify(root.selected.approval,null,2) : ""}
                        }
                        RowLayout {
                            Button {text:"Allow";onClicked:root.send({type:"confirm",id:root.selectedId,approve:true})}
                            Button {text:"Deny";onClicked:root.send({type:"confirm",id:root.selectedId,approve:false})}
                        }
                    }
                }
                TextField {
                    id:input;Layout.fillWidth:true;Layout.preferredHeight:34
                    enabled:!!root.selected && root.selected.placement.anchor.kind!=="panel" && !root.busy
                    placeholderText:root.selected ? "Chat…" : "Place a drop to chat";color:root.ink;font.pixelSize:12
                    background:Rectangle {color:"#303a2c";radius:7;border.color:input.activeFocus ? root.accent : "#4b5545"}
                    onAccepted: {if(text.trim()){root.failure="";root.send({type:"send",id:root.selectedId,text:text});text=""}}
                }
                Label {visible:!!root.failure;Layout.fillWidth:true;text:root.failure;wrapMode:Text.Wrap;color:"#f4a99a";font.pixelSize:10}
            }
            ColumnLayout {
                visible:root.settingsOpen
                anchors.fill:parent;anchors.margins:16;spacing:8
                RowLayout {
                    Label {text:"Model connection";color:root.ink;font.pixelSize:14;Layout.fillWidth:true}
                    ToolButton {text:"×";palette.buttonText:root.muted;onClicked:root.settingsOpen=false}
                }
                Label {text:"Endpoint URL";color:root.muted;font.pixelSize:11}
                TextField {id:endpoint;Layout.fillWidth:true;placeholderText:"http://localhost:11434/v1";font.pixelSize:11;selectByMouse:true}
                Label {text:"Model name";color:root.muted;font.pixelSize:11}
                TextField {id:modelName;Layout.fillWidth:true;placeholderText:"Model served by your endpoint";font.pixelSize:11;selectByMouse:true}
                Label {text:"API-key environment variable (optional)";color:root.muted;font.pixelSize:10}
                TextField {id:keyEnv;Layout.fillWidth:true;placeholderText:"e.g. DROPS_API_KEY";font.pixelSize:11;selectByMouse:true}
                CheckBox {id:contentCapture;text:"Read attached window content";palette.windowText:root.ink;font.pixelSize:11}
                CheckBox {id:screenCapture;text:"Use screenshots / OCR fallback";palette.windowText:root.ink;font.pixelSize:11;enabled:contentCapture.checked}
                CheckBox {id:visionMode;text:"Model supports images";palette.windowText:root.ink;font.pixelSize:11}
                Label {visible:!!root.failure;Layout.fillWidth:true;text:root.failure;color:"#f4a99a";font.pixelSize:10;wrapMode:Text.Wrap}
                Button {id:saveConnectionButton;text:"Save connection";Layout.fillWidth:true;enabled:endpoint.text.trim().length>0 && modelName.text.trim().length>0;onClicked:root.send({type:"configure",base_url:endpoint.text,model:modelName.text,api_key_env:keyEnv.text,vision:visionMode.checked,capture_content:contentCapture.checked,screenshots:screenCapture.checked})}
            }
            Popup {
                id:contextView;x:8;y:8;width:parent.width-16;height:parent.height-16;modal:true;padding:10
                background:Rectangle {color:root.background;radius:12;border.color:root.accent}
                ScrollView {
                    id:contextScroll;anchors.fill:parent;contentWidth:availableWidth;clip:true
                    Column {
                        width:contextScroll.availableWidth;spacing:8
                        Image {
                            width:parent.width;height:visible ? Math.min(200,sourceSize.height*(width/Math.max(1,sourceSize.width))) : 0
                            fillMode:Image.PreserveAspectFit;visible:!!source
                            source:root.inspectedContext && root.inspectedContext.content && root.inspectedContext.content.image ? root.inspectedContext.content.image.data_url || "" : ""
                        }
                        TextArea {width:parent.width;readOnly:true;wrapMode:TextEdit.WrapAnywhere;textFormat:TextEdit.PlainText;color:root.ink;text:root.contextMetadata(root.inspectedContext)}
                    }
                }
            }
        }
    }
    Variants {
        model:Quickshell.screens
        delegate:Scope {
            id:monitorScope
            required property var modelData
            property var monitor:modelData
            PanelWindow {
                screen:monitorScope.monitor;visible:root.dragging || root.settling
                anchors {top:true;left:true;right:true;bottom:true}
                color:"transparent";exclusionMode:ExclusionMode.Ignore;mask:Region {}
                WlrLayershell.namespace:"drops-preview";WlrLayershell.keyboardFocus:WlrKeyboardFocus.None
                Rectangle {
                    visible:root.dragging && !!root.preview
                    property var g:root.preview ? root.preview.geometry : ({x:0,y:0,width:0,height:0})
                    x:(g.x||0)-monitorScope.monitor.x;y:(g.y||0)-monitorScope.monitor.y
                    width:g.width||160;height:g.height||56;radius:10;color:"#12a7d8f5";border.color:root.accent;border.width:2
                }
                DropFace {
                    id:dragGhost
                    visible:root.cursorReady
                    look:root.dragLook;awake:true;width:root.blobSize;height:root.blobSize-2
                    x:root.cursor.x-monitorScope.monitor.x-width/2;y:root.cursor.y-monitorScope.monitor.y-height/2
                    scale:root.dragging ? 1.15 : 1
                    opacity:root.dragging ? 1 : 0
                    Behavior on x {enabled:!root.skipMotion;NumberAnimation{duration:32;easing.type:Easing.OutQuad}}
                    Behavior on y {enabled:!root.skipMotion;NumberAnimation{duration:32;easing.type:Easing.OutQuad}}
                    Behavior on scale {NumberAnimation{duration:140;easing.type:Easing.OutCubic}}
                    Behavior on opacity {NumberAnimation{duration:160}}
                }
            }
            Variants {
                model:root.blobIds
                delegate:PanelWindow {
                    id:placed
                    required property string modelData
                    property var blob:root.state.blobs.find(b=>b.id===modelData) || null
                    property var pos:blob ? blob.placement : ({x:0,y:0,visible:false})
                    screen:monitorScope.monitor
                    visible:!root.capturing && !!blob && pos.visible && pos.x>=monitorScope.monitor.x && pos.x<monitorScope.monitor.x+monitorScope.monitor.width && pos.y>=monitorScope.monitor.y && pos.y<monitorScope.monitor.y+monitorScope.monitor.height
                    anchors {top:true;left:true}
                    margins {top:Math.max(0,pos.y-monitorScope.monitor.y);left:Math.max(0,pos.x-monitorScope.monitor.x)}
                    implicitWidth:56;implicitHeight:56;color:"transparent";exclusionMode:ExclusionMode.Ignore
                    WlrLayershell.namespace:"drops-blob";WlrLayershell.keyboardFocus:WlrKeyboardFocus.None
                    DropFace {
                        anchors.centerIn:parent;awake:placed.blob && (placed.blob.state!=="idle" || root.recentDropId===placed.modelData || root.selectedId===placed.modelData && root.chatOpen)
                        look:placed.blob ? placed.blob.appearance || "drop" : "drop"
                        opacity:root.dragging && root.dragId===placed.modelData ? 0 : 1
                        Behavior on opacity {NumberAnimation{duration:70}}
                        width:awake ? root.blobSize : 13;height:awake ? root.blobSize-2 : 13
                        border.width:placed.blob && placed.blob.state==="waiting" ? 2 : 0
                        border.color:"#eed29b"
                    }
                    DragArea {blobId:placed.modelData}
                }
            }
        }
    }
}
