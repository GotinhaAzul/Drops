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
    property bool chatOpen: false
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
    property bool busy: !!selected && ["thinking","waiting","stopping"].includes(selected.state)
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
        dragging=false; preview=null; failure=""
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
                        root.syncChat()
                    } else if (msg.type === "preview" && root.dragging) {root.preview=msg.geometry ? msg : null;root.failure=""}
                    else if (msg.type === "error") root.failure=msg.message
                } catch(e) { root.failure=String(e) }
            }
        }
        onExited: { root.connected=false; root.failure="Start buddyd to connect." }
    }
    Timer { interval:2000; repeat:true; running:!bridge.running; onTriggered:bridge.running=true }
    Timer { interval:120; repeat:true; running:root.dragging; onTriggered:root.send({type:"preview",x:0,y:0,use_cursor:true}) }

    component DropFace: Rectangle {
        property bool awake:true
        property color tint:root.accent
        color:tint
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
        onPressed: mouse=> {sx=mouse.x;sy=mouse.y}
        onPositionChanged: mouse=> {if(pressed && Math.abs(mouse.x-sx)+Math.abs(mouse.y-sy)>10) root.dragging=true}
        onReleased: mouse=> {
            if (root.dragging) root.drop(blobId)
            else if(blobId) root.openChat(blobId)
            root.dragging=false;root.preview=null
        }
        onCanceled: {root.dragging=false;root.preview=null}
    }
    PanelWindow {
        id: panel
        visible:root.state.panel_open
        anchors {top:true;right:true}
        margins {top:40;right:20}
        implicitWidth:260
        implicitHeight:210+(root.selected && root.selected.history.length ? 150 : 0)+(root.selected && root.selected.approval ? 180 : 0)+(root.failure ? 26 : 0)
        color:"transparent";exclusionMode:ExclusionMode.Ignore
        WlrLayershell.namespace:"drops-panel"
        WlrLayershell.keyboardFocus:WlrKeyboardFocus.OnDemand
        Shortcut {sequence:"Escape";enabled:panel.visible;onActivated:root.send({type:"toggle"})}
        Rectangle {
            anchors.fill:parent;color:root.background;radius:18;border.color:"#4b5545";border.width:1
            ColumnLayout {
                anchors.fill:parent;anchors.margins:14;spacing:8
                Item {
                    Layout.fillWidth:true;Layout.preferredHeight:112
                    DropFace {
                        anchors.centerIn:parent;width:76;height:74
                        tint:root.selected && root.selected.state==="waiting" ? "#eed29b" : root.accent
                        DragArea {blobId:root.selected ? root.selectedId : ""}
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
                                MouseArea {anchors.fill:parent;anchors.margins:-5;onClicked:root.openChat(parent.modelData)}
                            }
                        }
                    }
                }
                Label {
                    Layout.fillWidth:true;horizontalAlignment:Text.AlignHCenter;color:root.muted;font.pixelSize:10;elide:Text.ElideRight
                    text:root.selected ? root.selected.placement.label : "Drag a drop onto a window"
                    MouseArea {anchors.fill:parent;enabled:!!root.selected;onClicked:contextView.open()}
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
                    enabled:!!root.selected && !root.busy
                    placeholderText:root.selected ? "Chat…" : "Place a drop to chat";color:root.ink;font.pixelSize:12
                    background:Rectangle {color:"#303a2c";radius:7;border.color:input.activeFocus ? root.accent : "#4b5545"}
                    onAccepted: {if(text.trim()){root.failure="";root.send({type:"send",id:root.selectedId,text:text});text=""}}
                }
                Label {visible:!!root.failure;Layout.fillWidth:true;text:root.failure;wrapMode:Text.Wrap;color:"#f4a99a";font.pixelSize:10}
            }
            Popup {
                id:contextView;x:8;y:8;width:parent.width-16;height:parent.height-16;modal:true;padding:10
                background:Rectangle {color:root.background;radius:12;border.color:root.accent}
                ScrollView {anchors.fill:parent;TextArea {readOnly:true;wrapMode:TextEdit.WrapAnywhere;textFormat:TextEdit.PlainText;color:root.ink;text:root.selected ? JSON.stringify(root.selected.context,null,2) : ""}}
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
                screen:monitorScope.monitor;visible:root.dragging && !!root.preview
                anchors {top:true;left:true;right:true;bottom:true}
                color:"transparent";exclusionMode:ExclusionMode.Ignore;mask:Region {}
                WlrLayershell.namespace:"drops-preview";WlrLayershell.keyboardFocus:WlrKeyboardFocus.None
                Rectangle {
                    property var g:root.preview ? root.preview.geometry : ({x:0,y:0,width:0,height:0})
                    x:(g.x||0)-monitorScope.monitor.x;y:(g.y||0)-monitorScope.monitor.y
                    width:g.width||160;height:g.height||56;radius:10;color:"#12a7d8f5";border.color:root.accent;border.width:2
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
                    visible:!!blob && pos.visible && pos.x>=monitorScope.monitor.x && pos.x<monitorScope.monitor.x+monitorScope.monitor.width && pos.y>=monitorScope.monitor.y && pos.y<monitorScope.monitor.y+monitorScope.monitor.height
                    anchors {top:true;left:true}
                    margins {top:Math.max(0,pos.y-monitorScope.monitor.y);left:Math.max(0,pos.x-monitorScope.monitor.x)}
                    implicitWidth:56;implicitHeight:56;color:"transparent";exclusionMode:ExclusionMode.Ignore
                    WlrLayershell.namespace:"drops-blob";WlrLayershell.keyboardFocus:WlrKeyboardFocus.None
                    DropFace {
                        anchors.centerIn:parent;awake:placed.blob && (placed.blob.state!=="idle" || root.selectedId===placed.modelData && root.chatOpen)
                        width:awake ? root.blobSize : 13;height:awake ? root.blobSize-2 : 13
                        tint:placed.blob && placed.blob.state==="waiting" ? "#eed29b" : root.accent
                    }
                    DragArea {blobId:placed.modelData}
                }
            }
        }
    }
}
