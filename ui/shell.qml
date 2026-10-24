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
    property var selected: state.blobs.find(b => b.id === selectedId) || null
    property var currentDrop: selected
    property var looks: ["drop","cat","sprout","bunny","robot","cloud"]
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
    property string autoChatId: ""
    property bool autoChatPending: false
    property var chatViews: ({})
    property var seenResponses: ({})
    property bool responseHistoryInitialized: false
    property string connectionMode: "custom"
    property var opencodeModels: []
    property string opencodeSelected: ""
    property bool loadingModels: false
    function loadSettings() {
        const c=state.connection || ({})
        const custom=c.custom || c
        const go=c.opencode_go || ({})
        endpoint.text=custom.base_url || "http://127.0.0.1:11434/v1"
        modelName.text=custom.model || ""
        keyEnv.text=custom.api_key_env || ""
        opencodeSelected=go.model || ""
        opencodeModels=opencodeSelected ? [opencodeSelected] : []
        opencodeKey.text=""
        opencodeKey.placeholderText=go.has_api_key ? "Key available — leave blank to keep" : "Paste your Opencode Go API key"
        connectionMode=c.mode==="opencode_go" ? "opencode_go" : "custom"
        visionMode.checked=!!(connectionMode==="opencode_go" ? go.vision : custom.vision)
        contentCapture.checked=c.capture_content!==false
        screenCapture.checked=c.screenshots!==false
        loadingModels=false
    }
    function selectConnectionMode(index) {
        const mode=index===1 ? "opencode_go" : "custom"
        if (mode===connectionMode) return
        connectionMode=mode
        const c=state.connection || ({})
        const saved=mode==="opencode_go" ? (c.opencode_go || ({})) : (c.custom || ({}))
        visionMode.checked=!!saved.vision
    }
    function opencodeHasKey() {
        const go=state.connection && state.connection.opencode_go || ({})
        return opencodeKey.text.trim().length>0 || !!go.has_api_key
    }
    function loadModels() {
        if (!connected) {failure="Drops is reconnecting…";return}
        failure="";loadingModels=true
        send({type:"models",api_key:opencodeKey.text})
    }
    function saveConnection() {
        const vision=visionMode.checked
        const capture_content=contentCapture.checked
        const screenshots=screenCapture.checked
        if (connectionMode==="opencode_go") {
            send({type:"configure",mode:"opencode_go",model:opencodeSelected,api_key:opencodeKey.text,vision:vision,capture_content:capture_content,screenshots:screenshots})
        } else {
            send({type:"configure",mode:"custom",base_url:endpoint.text,model:modelName.text,api_key_env:keyEnv.text,vision:vision,capture_content:capture_content,screenshots:screenshots})
        }
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
    function openSettings() {
        loadSettings();settingsOpen=true;failure=""
        if(!state.panel_open)send({type:"toggle"})
    }
    readonly property bool lively: state.lively === true
    property var appearance: state.appearance || ({})
    property color accent: appearance.accent || "#a7d8f5"
    property color background: appearance.background || "#20251f"
    property color ink: appearance.foreground || "#ecebe4"
    property color muted: appearance.muted || "#92968a"
    property real blobSize: Math.max(20,Math.min(52,appearance.blob_size || 44))
    readonly property int dropSurfaceSize: 72
    readonly property int dropSurfaceInset: 8
    property string failure: ""
    property bool connected: false
    property bool dragging: false
    property var preview: null
    property string chatSession: ""
    property bool hasModel: !!(state.connection && String(state.connection.model || "").trim())
    property bool selectedBusy: !!selected && ["capturing","thinking","waiting","stopping"].includes(selected.state)
    ListModel { id: chatModel; dynamicRoles: true }
    property var thinkingExpanded: ({})
    property string thinkingMode: ["shown","collapsed","hidden"].includes(appearance.thinking_mode) ? appearance.thinking_mode : "collapsed"
    function entryVisible(entry) {
        if (!entry) return false
        if (entry.type === "thinking") return root.thinkingMode !== "hidden" && String(entry.text || "").trim().length > 0
        if (entry.type === "tool_result") return !!entry.is_error
        return ["user","text","error","tool_start"].includes(entry.type)
    }
    function isThinkingExpanded(key) {
        const value=root.thinkingExpanded[key]
        return value===undefined ? root.thinkingMode==="shown" : !!value
    }
    function setThinkingExpanded(key,value) {
        const next=Object.assign({},root.thinkingExpanded)
        next[key]=!!value
        root.thinkingExpanded=next
    }
    function syncChat() {
        if (chatSession !== selectedId) { chatModel.clear(); chatSession=selectedId; thinkingExpanded={} }
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
    function shortLabel(blob) {
        if (!blob) return "this window"
        const label=String(blob.placement && blob.placement.label || "").trim()
        if (!label || label==="Ready to place") return blob.placement && blob.placement.anchor && blob.placement.anchor.kind==="workspace" ? "this workspace" : "this window"
        return label.length>38 ? label.slice(0,36).trim()+"…" : label
    }
    function workingLabel(blob) {
        const context=blob && blob.context || ({})
        const working=context.working_context || ({})
        const workspace=context.tool_workspace || ({})
        const document=working.document || ({})
        if(document.path)return String(document.path).split("/").pop()+" · saved file"
        if(workspace.source==="anchor" || workspace.source==="configured")return String(workspace.root || "").split("/").pop()+" · workspace"
        if(working.status==="ambiguous")return "Workspace unclear · inspect context"
        return ""
    }
    function contextKind(blob) {
        if (!blob) return "window"
        const anchor=blob.placement && blob.placement.anchor || ({})
        if (anchor.kind==="workspace") return "workspace"
        const context=blob.context || ({})
        const window=context.window || ({})
        const hay=(String(window.class || "")+" "+String(window.title || "")+" "+String(blob.placement && blob.placement.label || "")).toLowerCase()
        const text=String(context.content && context.content.text || "").toLowerCase()
        if (/(^|\s)(htop|btop|top|glances)(\s|$)|system monitor|process (list|manager)/.test(hay) || /load average|cpu\(s\)|%cpu|mib mem/.test(text)) return "processes"
        if (/terminal|kitty|alacritty|foot|ghostty|wezterm|konsole|xterm/.test(hay)) return "terminal"
        if (/firefox|chromium|chrome|brave|zen browser|epiphany|webkit/.test(hay)) return "browser"
        if (/(^|\s)(code|code-oss|codium|zed|nvim|vim|emacs|helix)(\s|$)|text editor|sublime/.test(hay)) return "editor"
        return "window"
    }
    function greetingFor(blob) {
        const kind=contextKind(blob)
        const label=shortLabel(blob)
        if (kind==="processes") return "Oh, some processes! What do you need my help for?"
        if (kind==="terminal") return "I'm on your terminal. Want a second pair of eyes?"
        if (kind==="browser") return "I'm reading this page with you. Want the short version?"
        if (kind==="editor") return "I'm on " + label + ". Want me to take a look?"
        if (kind==="workspace") return "This workspace is all mine. What should we do?"
        return "I'm on " + label + ". What do you need?"
    }
    function suggestionsFor(blob) {
        const kind=contextKind(blob)
        if (kind==="processes") return [
            {label:"How's my CPU?",prompt:"How's my CPU right now?"},
            {label:"Biggest memory hog?",prompt:"Which process is using the most memory?"}
        ]
        if (kind==="terminal") return [
            {label:"What just happened?",prompt:"Look at this terminal and tell me what just happened."},
            {label:"Find the error",prompt:"Is there an error in this terminal? Explain it simply."}
        ]
        if (kind==="browser") return [
            {label:"Summarize this page",prompt:"Summarize this page in three short bullets."},
            {label:"What should I notice?",prompt:"What are the most important points on this page?"}
        ]
        if (kind==="editor") return [
            {label:"Review this file",prompt:"Review what I'm looking at and point out anything risky."},
            {label:"Explain this",prompt:"Explain what this file does in plain language."}
        ]
        if (kind==="workspace") return [
            {label:"What's open here?",prompt:"What's open on this workspace?"},
            {label:"What can I close?",prompt:"What's open here, and what can I close?"}
        ]
        return [
            {label:"What am I looking at?",prompt:"What am I looking at in this window?"},
            {label:"What could go wrong?",prompt:"Is there anything here I should worry about?"}
        ]
    }
    function stateCaption(blob) {
        if (!blob) return ""
        if (blob.state==="capturing") return "reading this window"
        if (blob.state==="thinking") return "thinking"
        if (blob.state==="waiting") return "needs your go-ahead"
        if (blob.state==="stopping") return "stopping"
        if (blob.state==="error") return "hit a snag"
        return ""
    }
    // The last finished answer, but only when it follows the latest user turn.
    // Thinking/tool events between the answer and now are ignored.
    function latestResponse(blob) {
        if (!blob || !blob.history) return null
        const history=blob.history
        for (let i=history.length-1;i>=0;i--) {
            const entry=history[i]
            if (!entry) continue
            if (entry.type==="user") return null
            if (entry.type==="text" || entry.type==="error") {
                return String(entry.text || "").trim().length>0 ? entry : null
            }
        }
        return null
    }
    // A compact reply is offered only for a placed, visible, idle drop whose
    // latest answer has not been read in the full chat yet.
    function responsePreviewFor(blob) {
        if (!blob || !blob.placement || !blob.placement.visible || blob.placement.anchor.kind==="panel") return null
        if (["capturing","thinking","waiting","stopping"].includes(blob.state)) return null
        const entry=root.latestResponse(blob)
        const seen=root.seenResponses[blob.id]
        // The text guard keeps a cleared session from suppressing an answer
        // that happens to restore the same history length.
        if (seen && seen.length===blob.history.length && seen.text===(entry ? String(entry.text || "") : "")) return null
        return entry
    }
    function markResponseSeen(blob) {
        if (!blob) return
        const entry=root.latestResponse(blob)
        const next=Object.assign({},root.seenResponses)
        next[blob.id]={length:blob.history.length,text:entry ? String(entry.text || "") : ""}
        root.seenResponses=next
    }
    function previewTextFor(blob) {
        const entry=root.responsePreviewFor(blob)
        return entry ? String(entry.text || "").trim().slice(0,600) : ""
    }
    function openChat(id, focus) {
        if (!id) return
        const blob=state.blobs.find(b=>b.id===id)
        if (!blob || !blob.placement || blob.placement.anchor.kind==="panel") return
        selectedId=id;chatOpen=true;failure=""
        root.markResponseSeen(blob)
        if (state.panel_open) send({type:"toggle"})
        if (focus!==false) focusTimer.restart()
    }
    function closeChat() { root.markResponseSeen(selected); chatOpen=false }
    function toggleChat(id) {
        if (chatOpen && selectedId===id) closeChat()
        else openChat(id,true)
    }
    function registerChat(id,view) {
        const next=Object.assign({},chatViews)
        next[id]=view
        chatViews=next
    }
    function unregisterChat(id,view) {
        if (view !== undefined && chatViews[id] !== view) return
        const next=Object.assign({},chatViews)
        delete next[id]
        chatViews=next
    }
    function requestChatFocus() {
        const view=chatViews[selectedId]
        if (view && view.focusInput) view.focusInput()
    }
    function submitChat(id,text) {
        if (!text || !String(text).trim()) return
        failure=""
        send({type:"send",id:id,text:String(text).trim()})
    }
    function drop(id) {
        dragging=false; preview=null; failure="";settling=true;settleTimer.restart()
        const command={type:"drop",x:0,y:0,use_cursor:true}
        if (id) command.id=id
        autoChatPending=!!id
        send(command)
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
                        root.state=msg.state
                        // Restored history is not a new reply; do not pop a
                        // preview above every drop when the UI reconnects.
                        if (!root.responseHistoryInitialized) {
                            root.responseHistoryInitialized=true
                            const seen={}
                            for (const b of msg.state.blobs) {
                                const entry=root.latestResponse(b)
                                seen[b.id]={length:b.history.length,text:entry ? String(entry.text || "") : ""}
                            }
                            root.seenResponses=seen
                        }
                        if (!ids.includes(root.selectedId)) {root.selectedId=ids[0] || "";root.chatOpen=false}
                        else if (root.chatOpen) {
                            const openBlob=msg.state.blobs.find(b=>b.id===root.selectedId)
                            if (!openBlob || !openBlob.placement.visible || openBlob.placement.anchor.kind==="panel") root.chatOpen=false
                        }
                        if (!root.settingsInitialized) {root.loadSettings();root.settingsInitialized=true;root.settingsOpen=!msg.state.connection || !msg.state.connection.model}
                        root.syncChat()
                    } else if (msg.type === "context") {root.inspectedContext=msg.context}
                    else if (msg.type === "pointer") {root.pointerPending=false;if(msg.cursor){root.cursor=msg.cursor;root.cursorReady=true;motionWarmup.start()}}
                    else if(msg.type==="ok" && msg.id) {
                        root.selectedId=msg.id;root.recentDropId=msg.id;recentTimer.restart()
                        if(root.autoChatPending){root.autoChatPending=false;root.autoChatId=msg.id;autoChatTimer.restart()}
                    }
                    else if(msg.type==="ok" && msg.configured) {root.settingsOpen=false;root.loadingModels=false;root.opencodeKey.text="";root.failure=""}
                    else if(msg.type==="models") {
                        root.loadingModels=false
                        let models=Array.isArray(msg.models) ? msg.models.slice() : []
                        if(root.opencodeSelected && models.indexOf(root.opencodeSelected)<0) models.unshift(root.opencodeSelected)
                        root.opencodeModels=models
                        if(!root.opencodeSelected && models.length>0) root.opencodeSelected=models[0]
                        root.failure=models.length>0 ? "" : "The gateway returned no models."
                    }
                    else if (msg.type === "preview" && root.dragging) {root.preview=msg.geometry ? msg : null;root.failure=""}
                    else if (msg.type === "error") {root.loadingModels=false;root.failure=msg.message}
                } catch(e) { root.failure=String(e) }
            }
        }
        onExited: { root.connected=false;root.dragging=false;root.settling=false;root.pointerPending=false;root.failure="Start buddyd to connect." }
    }
    Timer { interval:2000; repeat:true; running:!bridge.running; onTriggered:bridge.running=true }
    Timer {id:motionWarmup;interval:32;onTriggered:root.skipMotion=false}
    Timer {id:settleTimer;interval:180;onTriggered:root.settling=false}
    Timer {id:recentTimer;interval:1100;onTriggered:root.recentDropId=""}
    Timer {id:focusTimer;interval:90;onTriggered:root.requestChatFocus()}
    Timer {
        id:autoChatTimer;interval:520
        onTriggered:{
            const blob=root.state.blobs.find(b=>b.id===root.autoChatId)
            if(blob && blob.placement && blob.placement.anchor.kind!=="panel" && blob.placement.visible && !root.dragging) root.openChat(blob.id,false)
        }
    }
    Timer {interval:16;repeat:true;running:root.dragging;onTriggered:{if(!root.pointerPending){root.pointerPending=true;root.send({type:"pointer"})}}}
    Timer { interval:120; repeat:true; running:root.dragging; onTriggered:root.send({type:"preview",x:0,y:0,use_cursor:true}) }

    component ThinkingBlock: Item {
        id:block
        property var entry: null
        property bool expanded: false
        property bool streaming: false
        signal toggleRequested()
        implicitHeight: header.height + (expanded ? 4 + body.height : 0)
        height: implicitHeight
        property string summary: {
            if (streaming) return "Thinking…"
            const text=String(entry && entry.text || "").trim()
            if (!text) return "Thinking"
            const words=text.split(/\s+/).filter(word=>word.length>0).length
            if (words>1) return "Thinking · "+(words>=1000 ? (words/1000).toFixed(1)+"k words" : words+" words")
            return "Thinking · "+(text.length>=1000 ? (text.length/1000).toFixed(1)+"k chars" : text.length+" chars")
        }
        Rectangle {
            id:header
            width:parent.width;height:20;radius:5
            color:Qt.rgba(root.muted.r,root.muted.g,root.muted.b,0.10)
            Accessible.role:Accessible.Button
            Accessible.name:block.expanded ? "Collapse thinking" : "Expand thinking"
            RowLayout {
                anchors.fill:parent;anchors.leftMargin:6;anchors.rightMargin:6;spacing:5
                Text {text:block.expanded ? "▾" : "▸";color:root.muted;font.pixelSize:9;Layout.alignment:Qt.AlignVCenter}
                Text {text:block.summary;color:root.muted;font.pixelSize:10;Layout.fillWidth:true;elide:Text.ElideRight;Layout.alignment:Qt.AlignVCenter}
            }
            MouseArea {anchors.fill:parent;cursorShape:Qt.PointingHandCursor;onClicked:block.toggleRequested()}
        }
        Rectangle {
            id:body
            visible:block.expanded
            y:header.height+4;width:parent.width
            height:Math.min(thought.implicitHeight+12,132)
            radius:6;color:Qt.rgba(root.muted.r,root.muted.g,root.muted.b,0.06);clip:true
            Flickable {
                id:thoughtScroll
                anchors.fill:parent;anchors.margins:6
                contentWidth:width;contentHeight:thought.implicitHeight
                clip:true;boundsBehavior:Flickable.StopAtBounds
                Text {
                    id:thought
                    width:thoughtScroll.width
                    wrapMode:Text.Wrap;textFormat:Text.PlainText;font.pixelSize:11;color:root.ink
                    text:String(block.entry && block.entry.text || "")
                }
            }
        }
    }

    component DropFace: Rectangle {
        id:face
        property bool awake:true
        property bool hovered:false
        property string look:"drop"
        property string mood:"idle"
        property bool unread:false
        property real sway:0
        property real hop:0
        readonly property bool expressive:root.lively && awake
        property color tint:look==="bunny" ? "#e9bfd5" : look==="robot" ? "#b6c5ee" : look==="cloud" ? "#d7e6ef" : look==="cat" ? "#f2c6a0" : look==="sprout" ? "#b8dca4" : root.accent
        property real eyeOpen:1
        color:tint
        scale:(hovered && awake ? 1.06 : 1)*(expressive ? 1+sway*0.018 : 1)
        rotation:expressive ? sway*(mood==="thinking" ? 4 : 1.8) : 0
        transform:Translate {y:face.expressive ? face.hop : 0}
        SequentialAnimation on hop {
            running:face.expressive && face.unread && face.visible
            NumberAnimation {from:0;to:-3;duration:160;easing.type:Easing.OutQuad}
            NumberAnimation {to:0;duration:240;easing.type:Easing.OutBounce}
        }
        SequentialAnimation on sway {
            running:face.expressive && face.visible
            loops:Animation.Infinite
            NumberAnimation {to:1;duration:1500;easing.type:Easing.InOutSine}
            NumberAnimation {to:-1;duration:1500;easing.type:Easing.InOutSine}
        }
        Behavior on scale {NumberAnimation{duration:140;easing.type:Easing.OutCubic}}
        Timer {
            interval:3200;repeat:true;running:face.awake
            onTriggered:{face.eyeOpen=0.08;reopen.restart();interval=2600+Math.random()*3200}
        }
        Timer {id:reopen;interval:115;onTriggered:face.eyeOpen=1}
        Rectangle {visible:parent.awake && parent.look==="cat";width:parent.width*0.28;height:width;rotation:45;radius:3;color:parent.color;x:parent.width*0.08;y:-height*0.25;z:-1}
        Rectangle {visible:parent.awake && parent.look==="cat";width:parent.width*0.28;height:width;rotation:45;radius:3;color:parent.color;x:parent.width*0.64;y:-height*0.25;z:-1}
        Rectangle {visible:parent.awake && parent.look==="sprout";width:parent.width*0.34;height:parent.width*0.18;radius:height/2;color:"#7fa76a";rotation:30;x:parent.width*0.2;y:-height*0.65}
        Rectangle {visible:parent.awake && parent.look==="sprout";width:parent.width*0.34;height:parent.width*0.18;radius:height/2;color:"#94bd7c";rotation:-30;x:parent.width*0.49;y:-height*0.65}
        // Silhouettes stay available in quiet mode, independently of motion.
        Repeater {
            model:2
            delegate:Rectangle {
                required property int index
                visible:face.awake && face.look==="bunny"
                width:face.width*0.19;height:face.height*0.39;radius:width/2
                x:face.width*(index===0 ? 0.19 : 0.62);y:-height*0.36
                color:face.tint;rotation:index===0 ? -12 : 12;z:-1
                Rectangle {anchors.centerIn:parent;width:parent.width*0.45;height:parent.height*0.65;radius:width/2;color:"#cc8ca9"}
            }
        }
        Rectangle {visible:face.awake && face.look==="robot";anchors.horizontalCenter:parent.horizontalCenter;y:-6;width:3;height:8;color:face.tint}
        Rectangle {visible:face.awake && face.look==="robot";anchors.horizontalCenter:parent.horizontalCenter;y:-8;width:7;height:7;radius:4;color:"#e5b96f"}
        Repeater {
            model:3
            delegate:Rectangle {
                required property int index
                visible:face.awake && face.look==="cloud"
                width:face.width*0.4;height:width;radius:width/2;color:face.tint
                x:face.width*(0.02+index*0.28);y:-height*(index===1 ? 0.32 : 0.12);z:-1
            }
        }
        Rectangle {
            visible:face.expressive
            anchors.horizontalCenter:parent.horizontalCenter;y:parent.height*0.66
            width:face.width*0.18;height:face.mood==="waiting" ? face.width*0.13 : 3
            radius:height/2;color:"#27322a"
            rotation:face.mood==="error" ? -12 : 0
        }
        Repeater {
            model:2
            delegate:Rectangle {
                required property int index
                visible:face.expressive && (face.hovered || face.unread)
                width:face.width*0.16;height:face.width*0.07;radius:height/2;color:"#df8f9f";opacity:0.65
                x:face.width*(index===0 ? 0.12 : 0.72);y:face.height*0.59
            }
        }
        radius: look==="robot" ? width*0.22 : width*(typeof root.appearance.roundness === "number" ? Math.max(0,Math.min(0.5,root.appearance.roundness)) : 0.48)
        Behavior on width { NumberAnimation {duration:160} }
        Behavior on height { NumberAnimation {duration:160} }
        Row {
            anchors.centerIn:parent; spacing:parent.width*0.15; visible:parent.awake
            Rectangle {
                width:parent.parent.width*0.13;height:width*1.2*parent.parent.eyeOpen;radius:width;color:"#27322a";clip:true
                Behavior on height {NumberAnimation{duration:90;easing.type:Easing.OutQuad}}
                Rectangle {width:parent.width*0.3;height:width;radius:width;color:"#ffffff";opacity:0.5;x:parent.width*0.16;y:parent.height*0.14}
            }
            Rectangle {
                width:parent.parent.width*0.13;height:width*1.2*parent.parent.eyeOpen;radius:width;color:"#27322a";clip:true
                Behavior on height {NumberAnimation{duration:90;easing.type:Easing.OutQuad}}
                Rectangle {width:parent.width*0.3;height:width;radius:width;color:"#ffffff";opacity:0.5;x:parent.width*0.16;y:parent.height*0.14}
            }
        }
    }
    component DragArea: MouseArea {
        property string blobId:""
        property real sx:0
        property real sy:0
        anchors.fill:parent; cursorShape:Qt.OpenHandCursor; hoverEnabled:true
        onPressed: mouse=> {
            sx=mouse.x;sy=mouse.y;root.dragId=blobId;root.cursorReady=false;root.skipMotion=true
            const b=root.state.blobs.find(b=>b.id===blobId)
            root.dragLook=b ? b.appearance || "drop" : root.pendingLook
            root.settling=false
        }
        onPositionChanged: mouse=> {
            if(pressed && Math.abs(mouse.x-sx)+Math.abs(mouse.y-sy)>6){
                if(!root.dragging && root.chatOpen && root.selectedId===blobId)root.closeChat()
                root.dragging=true
            }
        }
        onReleased: mouse=> {
            if (root.dragging) root.drop(blobId)
            else if(blobId && root.state.blobs.find(b=>b.id===blobId && b.placement.anchor.kind!=="panel")) root.toggleChat(blobId)
            root.dragging=false;root.preview=null
        }
        onCanceled: {root.dragging=false;root.preview=null;root.settling=false;root.pointerPending=false}
    }

    // The conversation lives with the drop: a compact speech bubble that grows
    // above it, collapses to a truncated reply preview after sending, and
    // disappears when the anchor does.
    component ChatBubble: PanelWindow {
        id:bubble
        objectName:bubble.blob ? (bubble.previewMode ? "response-preview-" : "chat-bubble-")+bubble.blob.id : (bubble.previewMode ? "response-preview" : "chat-bubble")
        required property var blob
        required property var monitor
        required property var place
        required property bool hostVisible
        property bool open: !!blob && root.chatOpen && root.selectedId===blob.id
        property bool active: open && hostVisible
        property var previewEntry: root.responsePreviewFor(blob)
        property bool previewMode: !open && hostVisible && !root.dragging && !root.capturing && !!previewEntry
        property bool collapseWhenBusy: false
        property var registeredView: null
        property bool busy: !!blob && ["capturing","thinking","waiting","stopping"].includes(blob.state)
        property bool waiting: !!blob && blob.state==="waiting" && !!blob.approval
        property bool hasHistory: !!blob && blob.history.length>0
        property bool intro: false
        property int dotPhase: 0
        property real previousLogHeight: 0
        property real edgePad: 8
        property real avatarGap: 10
        property real avatarCenterX: Math.max(monitor.x,place.x-root.dropSurfaceInset)+root.dropSurfaceSize/2
        property real avatarTop: Math.max(monitor.y,place.y-root.dropSurfaceInset)+(root.dropSurfaceSize-(root.blobSize-2))/2
        property real avatarBottom: avatarTop+root.blobSize-2
        property real spaceAbove: Math.max(0, avatarTop-monitor.y-edgePad-avatarGap)
        property real spaceBelow: Math.max(0, monitor.y+monitor.height-edgePad-avatarBottom-avatarGap)
        property bool above: spaceAbove > spaceBelow
        property real bodySpace: Math.max(0,(above ? spaceAbove : spaceBelow)-tailSize)
        // Reserve the header, approvals, composer and their spacing before
        // sizing history. Clamping an oversized bubble to the screen edge
        // would otherwise push its tail back over the avatar.
        property real bodyChromeHeight: {
            let height=24, count=0
            for (const item of body.children) {
                if (!item.visible) continue
                count++
                if (item!==chatLog) height+=item.height
            }
            return height+Math.max(0,count-1)*body.spacing
        }
        property real tailSize: 9
        property real left: Math.max(monitor.x+edgePad, Math.min(avatarCenterX-bubble.implicitWidth/2, monitor.x+monitor.width-bubble.implicitWidth-edgePad))
        property real top: above ? Math.max(monitor.y+edgePad, avatarTop-avatarGap-bubble.implicitHeight) : Math.min(monitor.y+monitor.height-bubble.implicitHeight-edgePad, avatarBottom+avatarGap)
        property real tailX: Math.max(20, Math.min(bubble.implicitWidth-20, avatarCenterX-left))
        function submit() {
            if(bubble.busy)return
            const value=chatInput.text
            if(!value.trim())return
            chatInput.text=""
            // Collapse only once the daemon reports the turn as busy. A
            // rejected send keeps this window open so its error stays visible.
            bubble.collapseWhenBusy=true
            root.submitChat(bubble.blob.id,value)
        }
        function viewHandle() {
            return {
                focusInput:function(){chatInput.forceActiveFocus()},
                submit:function(){bubble.submit()},
                sendSuggestion:function(index){bubble.sendSuggestion(index)},
                input:chatInput,
                scroll:chatLog,
                suggestions:suggestionFlow,
                suggestionCount:root.suggestionsFor(blob).length,
                greeting:greetingText,
                window:bubble
            }
        }
        function sendSuggestion(index) {
            const items=root.suggestionsFor(blob)
            if(items[index]){bubble.collapseWhenBusy=true;root.submitChat(blob.id,items[index].prompt)}
        }
        function activate() {
            if(!bubble.blob)return
            intro=false;introTimer.restart()
            previousLogHeight=0
            const view=bubble.viewHandle()
            bubble.registeredView=view
            root.registerChat(bubble.blob.id,view)
            if(bubble.blob.history.length>0)scrollToEnd.restart()
        }
        function deactivate() {
            intro=false
            bubble.collapseWhenBusy=false
            const view=bubble.registeredView
            bubble.registeredView=null
            if(view&&bubble.blob)root.unregisterChat(bubble.blob.id,view)
        }
        screen: monitor
        visible: (bubble.active && !root.capturing) || bubble.previewMode
        anchors {top:true;left:true}
        margins {top:Math.max(0,bubble.top-monitor.y);left:Math.max(0,bubble.left-monitor.x)}
        implicitWidth: bubble.open ? 312 : 236
        implicitHeight: (bubble.open ? bubbleBody.height : previewBody.height)+bubble.tailSize
        color:"transparent"; exclusionMode:ExclusionMode.Ignore
        WlrLayershell.namespace:"drops-chat"
        WlrLayershell.keyboardFocus: bubble.active ? WlrKeyboardFocus.OnDemand : WlrKeyboardFocus.None
        Shortcut {sequence:"Escape";enabled:bubble.active;onActivated:root.closeChat()}
        Timer {
            interval:300;repeat:true;running:bubble.visible && bubble.busy
            onTriggered:bubble.dotPhase=(bubble.dotPhase+1)%3
        }
        onActiveChanged: {
            if(active)activate()
            else deactivate()
        }
        Component.onCompleted: if(active && !bubble.registeredView)activate()
        Component.onDestruction: deactivate()
        onBusyChanged: {
            if(busy && bubble.collapseWhenBusy){
                bubble.collapseWhenBusy=false
                if(bubble.open)root.closeChat()
            }
        }
        // A rejected send never becomes busy; stop waiting and keep the
        // failure text visible in the open bubble.
        Timer {id:collapseGuard;interval:1500;running:bubble.collapseWhenBusy;onTriggered:bubble.collapseWhenBusy=false}
        Timer {id:introTimer;interval:16;onTriggered:bubble.intro=true}
        Timer {id:scrollToEnd;interval:30;onTriggered:{if(chatLog.contentItem)chatLog.contentItem.contentY=Math.max(0,chatLog.contentHeight-chatLog.height)}}

        Item {
            id:bubbleBody
            visible:bubble.open
            width:parent.width
            height:body.implicitHeight+24
            y:bubble.above ? 0 : bubble.tailSize
            opacity:bubble.intro ? 1 : 0
            scale:bubble.intro ? 1 : 0.96
            transformOrigin:Item.Bottom
            Behavior on opacity {NumberAnimation{duration:150;easing.type:Easing.OutCubic}}
            Behavior on scale {NumberAnimation{duration:190;easing.type:Easing.OutCubic}}
            Rectangle {
                anchors.fill:bg;anchors.margins:-2;radius:19
                color:Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.10)
            }
            Rectangle {
                id:bg
                anchors.fill:parent;radius:16
                color:Qt.lighter(root.background,1.08)
                border.color:Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.30);border.width:1
            }
            Item {
                width:18;height:bubble.tailSize
                x:bubble.tailX-width/2
                y:bubble.above ? bubbleBody.height : -bubble.tailSize
                clip:true
                Rectangle {
                    width:14;height:14;rotation:45;radius:2
                    x:2;y:bubble.above ? -7 : 2
                    color:Qt.lighter(root.background,1.08)
                    border.color:Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.30);border.width:1
                }
            }
            Column {
                id:body
                x:14;y:11;width:parent.width-28;spacing:8
                Row {
                    width:parent.width;height:20;spacing:6
                    Rectangle {
                        width:6;height:6;radius:3;anchors.verticalCenter:parent.verticalCenter
                        color:bubble.waiting ? "#eed29b" : bubble.busy ? root.accent : "#7d8a72"
                    }
                    Text {
                        id:headerLine
                        width:parent.width-54;elide:Text.ElideRight
                        anchors.verticalCenter:parent.verticalCenter
                        color:root.muted;font.pixelSize:10;textFormat:Text.PlainText
                        text:(root.workingLabel(bubble.blob) || "on "+root.shortLabel(bubble.blob))+(bubble.busy ? "  ·  "+root.stateCaption(bubble.blob) : "")
                        MouseArea {anchors.fill:parent;cursorShape:Qt.PointingHandCursor;onClicked:{root.selectedId=bubble.blob.id;root.inspectContext()}}

                    }
                    Item {width:4;height:1}
                    Item {
                        width:18;height:18;anchors.verticalCenter:parent.verticalCenter
                        Rectangle {anchors.fill:parent;radius:9;color:hoverClose.containsMouse ? Qt.rgba(root.muted.r,root.muted.g,root.muted.b,0.18) : "transparent"}
                        Text {anchors.centerIn:parent;text:"×";color:root.muted;font.pixelSize:13}
                        MouseArea {id:hoverClose;anchors.fill:parent;hoverEnabled:true;cursorShape:Qt.PointingHandCursor;onClicked:root.closeChat()}
                        Accessible.role:Accessible.Button;Accessible.name:"Close chat"
                    }
                }
                Text {
                    id:greetingText
                    visible:!bubble.hasHistory
                    width:parent.width
                    wrapMode:Text.Wrap;textFormat:Text.PlainText
                    color:root.ink;font.pixelSize:13;lineHeight:1.16
                    text:root.greetingFor(bubble.blob)
                }
                Flow {
                    id:suggestionFlow
                    visible:!bubble.hasHistory
                    width:parent.width;spacing:6
                    Repeater {
                        model:root.suggestionsFor(bubble.blob)
                        delegate:Rectangle {
                            required property var modelData
                            required property int index
                            property bool hot:false
                            width:suggestion.implicitWidth+18;height:26;radius:13
                            color:hot ? Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.24) : Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.13)
                            border.color:Qt.rgba(root.accent.r,root.accent.g,root.accent.b,hot ? 0.75 : 0.42);border.width:1
                            Behavior on color {ColorAnimation{duration:110}}
                            Text {
                                id:suggestion
                                anchors.centerIn:parent
                                text:modelData.label;color:root.accent;font.pixelSize:11
                            }
                            MouseArea {
                                anchors.fill:parent;hoverEnabled:true;cursorShape:Qt.PointingHandCursor
                                onEntered:parent.hot=true;onExited:parent.hot=false
                                onClicked:bubble.sendSuggestion(index)
                            }
                        }
                    }
                }
                Rectangle {
                    visible:!root.hasModel
                    width:parent.width;height:30;radius:8
                    color:Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.10)
                    Text {anchors.centerIn:parent;text:"Connect a model to start  →";color:root.accent;font.pixelSize:11}
                    MouseArea {anchors.fill:parent;cursorShape:Qt.PointingHandCursor;onClicked:root.openSettings()}
                }
                ScrollView {
                    id:chatLog
                    objectName:bubble.blob ? "chat-log-"+bubble.blob.id : "chat-log"
                    visible:bubble.hasHistory
                    width:parent.width
                    height:Math.min(chatColumn.implicitHeight,180,Math.max(0,bubble.bodySpace-bubble.bodyChromeHeight))
                    clip:true;contentWidth:availableWidth
                    ScrollBar.vertical.policy:ScrollBar.AsNeeded
                    Column {
                        id:chatColumn
                        width:chatLog.availableWidth;spacing:7
                        Repeater {
                            model:bubble.open ? chatModel : null
                            delegate:Item {
                                id:chatEntry
                                required property var entry
                                required property int index
                                property bool thinking:entry.type==="thinking"
                                property string thinkingKey:root.chatSession+":"+index
                                property bool shown:root.entryVisible(entry)
                                width:parent.width
                                visible:shown
                                height:shown ? implicitHeight : 0
                                implicitHeight:thinking ? thought.implicitHeight : line.implicitHeight
                                Label {
                                    id:line
                                    visible:!chatEntry.thinking && chatEntry.shown
                                    x:chatEntry.entry.type==="user" ? 22 : 0
                                    width:parent.width-(chatEntry.entry.type==="user" ? 22 : 0)
                                    wrapMode:Text.Wrap;textFormat:Text.PlainText;font.pixelSize:12
                                    horizontalAlignment:chatEntry.entry.type==="user" ? Text.AlignRight : Text.AlignLeft
                                    color:chatEntry.entry.type==="user" ? root.accent : chatEntry.entry.type==="error" ? "#f4a99a" : chatEntry.entry.type==="tool_start" ? root.muted : root.ink
                                    text:chatEntry.entry.type==="tool_start" ? "↳ "+chatEntry.entry.name : chatEntry.entry.type==="user" ? chatEntry.entry.text : (chatEntry.entry.text || "")
                                }
                                ThinkingBlock {
                                    id:thought
                                    visible:chatEntry.thinking && chatEntry.shown
                                    width:parent.width
                                    entry:chatEntry.entry
                                    expanded:root.isThinkingExpanded(chatEntry.thinkingKey)
                                    streaming:chatEntry.index===chatModel.count-1 && !!bubble.blob && bubble.blob.state==="thinking"
                                    onToggleRequested:root.setThinkingExpanded(chatEntry.thinkingKey,!thought.expanded)
                                }
                            }
                        }
                        Row {
                            visible:bubble.busy && !bubble.waiting
                            height:visible ? 14 : 0;spacing:3
                            Rectangle {
                                width:5;height:5;radius:2.5;anchors.verticalCenter:parent.verticalCenter
                                color:root.accent;opacity:bubble.dotPhase===0 ? 1 : 0.3
                                Behavior on opacity {NumberAnimation{duration:200}}
                            }
                            Rectangle {
                                width:5;height:5;radius:2.5;anchors.verticalCenter:parent.verticalCenter
                                color:root.accent;opacity:bubble.dotPhase===1 ? 1 : 0.3
                                Behavior on opacity {NumberAnimation{duration:200}}
                            }
                            Rectangle {
                                width:5;height:5;radius:2.5;anchors.verticalCenter:parent.verticalCenter
                                color:root.accent;opacity:bubble.dotPhase===2 ? 1 : 0.3
                                Behavior on opacity {NumberAnimation{duration:200}}
                            }
                        }
                    }
                    onContentHeightChanged: {
                        if (!contentItem) return
                        const wasAtBottom=bubble.previousLogHeight<=0 || contentItem.contentY+height>=bubble.previousLogHeight-8
                        bubble.previousLogHeight=contentHeight
                        if (wasAtBottom) contentItem.contentY=Math.max(0,contentHeight-height)
                    }
                }
                Rectangle {
                    visible:bubble.waiting
                    width:parent.width
                    height:136
                    color:"#333326";radius:10
                    Column {
                        x:8;y:8;width:parent.width-16;spacing:5
                        Text {text:"Allow this action?";color:root.accent;font.pixelSize:11}
                        ScrollView {
                            width:parent.width;height:72;clip:true;contentWidth:availableWidth
                            TextArea {
                                width:parent.width;readOnly:true;wrapMode:TextEdit.WrapAnywhere
                                objectName:"approval-details-"+(bubble.blob ? bubble.blob.id : "")
                                textFormat:TextEdit.PlainText;color:root.ink;font.pixelSize:10
                                text:bubble.blob && bubble.blob.approval ? (bubble.blob.approval.preview && bubble.blob.approval.preview.diff ? bubble.blob.approval.preview.diff : JSON.stringify(bubble.blob.approval,null,2)) : ""
                            }
                        }
                        Row {
                            spacing:6
                            Rectangle {
                                width:62;height:24;radius:12;color:root.accent
                                Text {anchors.centerIn:parent;text:"Allow";color:"#1e2a22";font.pixelSize:11}
                                MouseArea {anchors.fill:parent;cursorShape:Qt.PointingHandCursor;onClicked:root.send({type:"confirm",id:bubble.blob.id,approve:true})}
                            }
                            Rectangle {
                                width:62;height:24;radius:12;color:"#414a3d"
                                Text {anchors.centerIn:parent;text:"Deny";color:root.ink;font.pixelSize:11}
                                MouseArea {anchors.fill:parent;cursorShape:Qt.PointingHandCursor;onClicked:root.send({type:"confirm",id:bubble.blob.id,approve:false})}
                            }
                        }
                    }
                }
                Rectangle {
                    id:composer
                    width:parent.width;height:38;radius:19
                    color:"#2b3428"
                    border.color:chatInput.activeFocus ? root.accent : (bubble.waiting ? "#eed29b" : "#465142");border.width:1
                    Behavior on border.color {ColorAnimation{duration:120}}
                    TextField {
                        id:chatInput
                        anchors.left:parent.left;anchors.right:sendButton.left;anchors.top:parent.top;anchors.bottom:parent.bottom
                        leftPadding:13;rightPadding:4
                        enabled:!bubble.busy
                        placeholderText:bubble.busy ? "Working…" : "Ask about this…"
                        placeholderTextColor:root.muted
                        color:root.ink;font.pixelSize:12;selectByMouse:true
                        background:Item {}
                        onAccepted:bubble.submit()
                    }
                    Item {
                        id:sendButton
                        width:30;height:30;anchors.right:parent.right;anchors.rightMargin:4;anchors.verticalCenter:parent.verticalCenter
                        Rectangle {
                            anchors.fill:parent;radius:15
                            color:bubble.busy ? "#eed29b" : root.accent
                            opacity:bubble.busy || chatInput.text.trim().length>0 ? 1 : 0.55
                            Behavior on opacity {NumberAnimation{duration:120}}
                        }
                        Text {
                            anchors.centerIn:parent
                            text:bubble.busy ? "■" : "→"
                            color:"#1e2a22";font.pixelSize:bubble.busy ? 9 : 14
                        }
                        MouseArea {
                            anchors.fill:parent;cursorShape:Qt.PointingHandCursor
                            onClicked:bubble.busy ? root.send({type:"cancel",id:bubble.blob.id}) : bubble.submit()
                        }
                        Accessible.role:Accessible.Button
                        Accessible.name:bubble.busy ? "Stop" : "Send"
                    }
                }
                Text {
                    visible:!!root.failure && root.chatOpen && root.selectedId===bubble.blob.id
                    width:parent.width;wrapMode:Text.Wrap
                    text:root.failure;color:"#f4a99a";font.pixelSize:10
                    textFormat:Text.PlainText
                }
            }
        }
        // The closed conversation collapses to a short preview above the drop.
        // Clicking it opens the full bubble; the drop itself remains clickable.
        Item {
            id:previewBody
            visible:bubble.previewMode
            width:parent.width
            height:previewColumn.implicitHeight+20
            y:bubble.above ? 0 : bubble.tailSize
            opacity:bubble.previewMode ? 1 : 0
            Behavior on opacity {NumberAnimation{duration:140;easing.type:Easing.OutCubic}}
            Rectangle {
                anchors.fill:previewBg;anchors.margins:-2;radius:17
                color:Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.10)
            }
            Rectangle {
                id:previewBg
                anchors.fill:parent;radius:14
                color:Qt.lighter(root.background,1.08)
                border.color:Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.30);border.width:1
            }
            Item {
                width:18;height:bubble.tailSize
                x:bubble.tailX-width/2
                y:bubble.above ? previewBody.height : -bubble.tailSize
                clip:true
                Rectangle {
                    width:14;height:14;rotation:45;radius:2
                    x:2;y:bubble.above ? -7 : 2
                    color:Qt.lighter(root.background,1.08)
                    border.color:Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.30);border.width:1
                }
            }
            Column {
                id:previewColumn
                x:12;y:10;width:parent.width-24;spacing:3
                Text {
                    id:previewText
                    objectName:bubble.blob ? "response-preview-text-"+bubble.blob.id : "response-preview-text"
                    width:parent.width
                    text:root.previewTextFor(bubble.blob)
                    color:bubble.previewEntry && bubble.previewEntry.type==="error" ? "#f4a99a" : root.ink
                    font.pixelSize:12;lineHeight:1.15
                    wrapMode:Text.WrapAtWordBoundaryOrAnywhere
                    maximumLineCount:3;elide:Text.ElideRight
                    textFormat:Text.PlainText
                }
                Text {
                    visible:previewText.lineCount>=previewText.maximumLineCount
                    width:parent.width;horizontalAlignment:Text.AlignRight
                    text:"Click to open";color:root.muted;font.pixelSize:9
                }
            }
            MouseArea {
                id:previewOpen
                objectName:bubble.blob ? "response-preview-open-"+bubble.blob.id : "response-preview-open"
                anchors.fill:parent;cursorShape:Qt.PointingHandCursor
                onClicked:root.openChat(bubble.blob.id,true)
            }
            Accessible.role:Accessible.Button
            Accessible.name:"Open the conversation"
        }
    }

    PanelWindow {
        id: panel
        visible:root.state.panel_open && !root.capturing
        anchors {top:true;right:true}
        margins {top:40;right:20}
        implicitWidth:260
        implicitHeight:root.settingsOpen ? 444 : 314+(root.selected && root.selected.context && root.selected.context.content ? 18 : 0)+(root.failure ? 26 : 0)
        color:"transparent";exclusionMode:ExclusionMode.Ignore
        WlrLayershell.namespace:"drops-panel"
        WlrLayershell.keyboardFocus:WlrKeyboardFocus.OnDemand
        Shortcut {sequence:"Escape";enabled:panel.visible && !root.chatOpen;onActivated:root.send({type:"toggle"})}
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
                        mood:root.currentDrop ? root.currentDrop.state : "idle"
                        hovered:panelAvatar.containsMouse
                        opacity:!root.currentDrop ? 0.45 : root.dragging && root.dragId===root.currentDrop.id ? 0 : 1
                        Behavior on opacity {NumberAnimation{duration:70}}
                        DragArea {id:panelAvatar;enabled:!!root.currentDrop;blobId:root.currentDrop ? root.currentDrop.id : ""}
                    }
                    ToolButton {
                        anchors.top:parent.top;anchors.right:parent.right
                        text:root.selectedBusy ? "■" : "×";font.pixelSize:13;palette.buttonText:root.muted
                        onClicked: root.selectedBusy ? root.send({type:"cancel",id:root.selectedId}) : root.send({type:"toggle"})
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
                Button {
                    id:chatButton
                    Layout.fillWidth:true;Layout.preferredHeight:32
                    enabled:!!root.currentDrop && root.currentDrop.placement.anchor.kind!=="panel"
                    text:!root.currentDrop ? "Make a drop to start" : enabled ? "Chat with this drop  →" : "Drag me onto a window"
                    background:Rectangle {
                        radius:9
                        color:chatButton.enabled ? Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.18) : Qt.rgba(root.muted.r,root.muted.g,root.muted.b,0.08)
                        border.color:chatButton.enabled ? Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.5) : "#3d473a";border.width:1
                    }
                    contentItem:Text {
                        text:chatButton.text;font.pixelSize:11
                        color:chatButton.enabled ? root.accent : root.muted
                        horizontalAlignment:Text.AlignHCenter;verticalAlignment:Text.AlignVCenter
                        elide:Text.ElideRight
                    }
                    onClicked:root.openChat(root.currentDrop.id,true)
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
                        model:["Drop","Cat","Sprout","Bunny","Robot","Cloud"];font.pixelSize:11
                        currentIndex:root.looks.indexOf(root.currentDrop ? root.currentDrop.appearance || "drop" : root.pendingLook)
                        onActivated:index=>{root.pendingLook=root.looks[index];if(root.currentDrop)root.send({type:"style",id:root.currentDrop.id,appearance:root.looks[index]})}
                    }
                    Button {id:newDropButton;Accessible.name:"New drop";text:"+";ToolTip.visible:hovered;ToolTip.text:"New drop";Layout.preferredWidth:30;Layout.preferredHeight:28;enabled:root.blobIds.length<3;onClicked:{root.chatOpen=false;root.send({type:"create",appearance:root.currentDrop ? root.currentDrop.appearance || "drop" : root.pendingLook})}}
                    Button {id:deleteDropButton;Accessible.name:"Delete drop";text:"−";ToolTip.visible:hovered;ToolTip.text:"Delete selected drop and conversation";Layout.preferredWidth:30;Layout.preferredHeight:28;enabled:!!root.currentDrop;onClicked:root.send({type:"remove",id:root.currentDrop.id})}
                    ToolButton {text:"⚙";ToolTip.visible:hovered;ToolTip.text:"Model connection";Layout.preferredWidth:28;Layout.preferredHeight:28;onClicked:root.openSettings()}
                }
                Switch {
                    id:livelyToggle
                    Accessible.name:"Lively Drops"
                    text:"Lively Drops";font.pixelSize:11;palette.windowText:root.ink
                    Layout.fillWidth:true;Layout.preferredHeight:30
                    checked:root.lively;enabled:root.connected
                    onToggled:root.send({type:"lively",enabled:checked})
                    ToolTip.visible:hovered
                    ToolTip.text:"Gentle motion, expressive faces and status badges. Turn off for quiet mode."
                }
                Label {
                    Layout.fillWidth:true;wrapMode:Text.Wrap;color:root.muted;font.pixelSize:9;horizontalAlignment:Text.AlignHCenter
                    text:root.currentDrop && root.currentDrop.placement.anchor.kind==="panel" ? "Drag me out, then click me to chat." : "Click a placed drop to talk right there."
                }
                Label {visible:!!root.failure;Layout.fillWidth:true;text:root.failure;wrapMode:Text.Wrap;color:"#f4a99a";font.pixelSize:10}
            }
            ScrollView {
                id:settingsScroll
                visible:root.settingsOpen
                anchors.fill:parent;anchors.margins:12
                contentWidth:availableWidth;clip:true
                ColumnLayout {
                    width:settingsScroll.availableWidth;spacing:7
                    RowLayout {
                        Layout.fillWidth:true
                        Label {text:"Model connection";color:root.ink;font.pixelSize:14;Layout.fillWidth:true}
                        ToolButton {text:"×";palette.buttonText:root.muted;onClicked:root.settingsOpen=false}
                    }
                    Label {visible:!!root.failure;Layout.fillWidth:true;text:root.failure;color:"#f4a99a";font.pixelSize:10;wrapMode:Text.Wrap}
                    Label {text:"Connection type";color:root.muted;font.pixelSize:10}
                    ComboBox {
                        id:connectionType
                        Layout.fillWidth:true;Layout.preferredHeight:28;font.pixelSize:11
                        model:["Custom endpoint","Opencode Go"]
                        currentIndex:root.connectionMode==="opencode_go" ? 1 : 0
                        onActivated:index=>root.selectConnectionMode(index)
                    }
                    ColumnLayout {
                        Layout.fillWidth:true;spacing:6
                        visible:root.connectionMode==="custom"
                        Label {text:"Endpoint URL";color:root.muted;font.pixelSize:11}
                        TextField {id:endpoint;Layout.fillWidth:true;placeholderText:"http://localhost:11434/v1";font.pixelSize:11;selectByMouse:true}
                        Label {text:"Model name";color:root.muted;font.pixelSize:11}
                        TextField {id:modelName;Layout.fillWidth:true;placeholderText:"Model served by your endpoint";font.pixelSize:11;selectByMouse:true}
                        Label {text:"API-key environment variable (optional)";color:root.muted;font.pixelSize:10}
                        TextField {id:keyEnv;Layout.fillWidth:true;placeholderText:"e.g. DROPS_API_KEY";font.pixelSize:11;selectByMouse:true}
                    }
                    ColumnLayout {
                        Layout.fillWidth:true;spacing:6
                        visible:root.connectionMode==="opencode_go"
                        Label {text:"API key";color:root.muted;font.pixelSize:11}
                        TextField {
                            id:opencodeKey
                            Layout.fillWidth:true;font.pixelSize:11;selectByMouse:true
                            echoMode:TextInput.Password;placeholderText:"Paste your Opencode Go API key"
                        }
                        Label {
                            Layout.fillWidth:true;wrapMode:Text.Wrap;color:root.muted;font.pixelSize:9
                            text:"The key is stored only in Drops' config file (mode 0600). Set OPENCODE_API_KEY instead if you prefer the environment."
                        }
                        RowLayout {
                            Layout.fillWidth:true;spacing:6
                            Label {text:"Model";color:root.muted;font.pixelSize:11;Layout.fillWidth:true}
                            Button {
                                id:loadModelsButton
                                text:root.loadingModels ? "Loading…" : "Load models"
                                font.pixelSize:10;Layout.preferredHeight:26
                                enabled:root.connected && !root.loadingModels && root.opencodeHasKey()
                                onClicked:root.loadModels()
                            }
                        }
                        ComboBox {
                            id:opencodeModel
                            Layout.fillWidth:true;Layout.preferredHeight:28;font.pixelSize:11
                            model:root.opencodeModels
                            editable:true
                            enabled:!root.loadingModels
                            currentIndex:root.opencodeModels.indexOf(root.opencodeSelected)
                            onActivated:index=>{root.opencodeSelected=root.opencodeModels[index]}
                            onEditTextChanged:{
                                const value=String(editText).trim()
                                if(value!==root.opencodeSelected)root.opencodeSelected=value
                            }
                        }
                    }
                    CheckBox {id:contentCapture;text:"Read attached window content";palette.windowText:root.ink;font.pixelSize:11}
                    CheckBox {id:screenCapture;text:"Use screenshots / OCR fallback";palette.windowText:root.ink;font.pixelSize:11;enabled:contentCapture.checked}
                    CheckBox {id:visionMode;text:"Model supports images";palette.windowText:root.ink;font.pixelSize:11}
                    Button {
                        id:saveConnectionButton
                        text:"Save connection"
                        Layout.fillWidth:true
                        enabled:root.connectionMode==="opencode_go" ? (root.opencodeSelected.length>0 && root.opencodeHasKey()) : (endpoint.text.trim().length>0 && modelName.text.trim().length>0)
                        onClicked:root.saveConnection()
                    }
                }
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
                    objectName:"drop-"+modelData
                    required property string modelData
                    property var blob:root.state.blobs.find(b=>b.id===modelData) || null
                    property var pos:blob ? blob.placement : ({x:0,y:0,visible:false})
                    property bool hovered:dropArea.containsMouse && !root.dragging
                    property bool replying:!!blob && !!root.responsePreviewFor(blob)
                    property bool awake:!!blob && (blob.state!=="idle" || replying || root.recentDropId===modelData || (root.selectedId===modelData && root.chatOpen) || hovered)
                    property bool waiting:!!blob && blob.state==="waiting"
                    property bool working:!!blob && ["capturing","thinking","stopping"].includes(blob.state)
                    property bool onMonitor:!!blob && pos.visible && pos.x>=monitorScope.monitor.x && pos.x<monitorScope.monitor.x+monitorScope.monitor.width && pos.y>=monitorScope.monitor.y && pos.y<monitorScope.monitor.y+monitorScope.monitor.height
                    property int badgePhase:0
                    Timer {interval:280;repeat:true;running:placed.working && !root.chatOpen;onTriggered:placed.badgePhase=(placed.badgePhase+1)%3}
                    screen:monitorScope.monitor
                    // Keep the avatar visible while its window is being read.
                    // Hiding every drop during capture looked like the drop
                    // vanished on send; the chat surface still hides so a
                    // screenshot does not contain an open conversation.
                    visible:placed.onMonitor
                    anchors {top:true;left:true}
                    margins {top:Math.max(0,pos.y-monitorScope.monitor.y-root.dropSurfaceInset);left:Math.max(0,pos.x-monitorScope.monitor.x-root.dropSurfaceInset)}
                    implicitWidth:root.dropSurfaceSize;implicitHeight:root.dropSurfaceSize;color:"transparent";exclusionMode:ExclusionMode.Ignore
                    WlrLayershell.namespace:"drops-blob";WlrLayershell.keyboardFocus:WlrKeyboardFocus.None
                    Rectangle {
                        id:ripple
                        anchors.centerIn:face
                        width:face.width;height:width;radius:width/2
                        color:"transparent";border.color:root.accent;border.width:2
                        opacity:0;scale:0.7
                        SequentialAnimation {
                            id:rippleAnim
                            ParallelAnimation {
                                NumberAnimation {target:ripple;property:"opacity";from:0.75;to:0;duration:620;easing.type:Easing.OutCubic}
                                NumberAnimation {target:ripple;property:"scale";from:0.7;to:2.05;duration:620;easing.type:Easing.OutCubic}
                            }
                        }
                        Connections {
                            target:root
                            function onRecentDropIdChanged(){if(root.recentDropId===placed.modelData)rippleAnim.restart()}
                        }
                    }
                    Item {
                        id:stateRingWrap
                        anchors.centerIn:face
                        width:face.width+9;height:width
                        visible:(placed.working || placed.waiting) && !(root.chatOpen && root.selectedId===placed.modelData)
                        Rectangle {
                            id:stateRing
                            anchors.fill:parent;radius:width/2
                            color:"transparent";border.width:2
                            border.color:placed.waiting ? "#eed29b" : root.accent
                            opacity:0.6
                            SequentialAnimation on opacity {
                                running:stateRingWrap.visible
                                loops:Animation.Infinite
                                NumberAnimation {to:0.22;duration:700}
                                NumberAnimation {to:0.8;duration:700}
                            }
                        }
                    }
                    DropFace {
                        id:face
                        anchors.centerIn:parent
                        awake:placed.awake
                        mood:placed.blob ? placed.blob.state : "idle"
                        unread:placed.replying
                        hovered:placed.hovered
                        look:placed.blob ? placed.blob.appearance || "drop" : "drop"
                        opacity:root.dragging && root.dragId===placed.modelData ? 0 : 1
                        Behavior on opacity {NumberAnimation{duration:70}}
                        width:awake ? root.blobSize : 13;height:awake ? root.blobSize-2 : 13
                        border.width:placed.waiting ? 2 : 0
                        border.color:"#eed29b"
                    }
                    Rectangle {
                        visible:placed.working && !root.chatOpen
                        anchors.right:face.right;anchors.top:face.top;anchors.rightMargin:-7;anchors.topMargin:-6
                        width:24;height:14;radius:7;color:Qt.lighter(root.background,1.15);border.color:root.accent;border.width:1
                        Row {
                            anchors.centerIn:parent;spacing:2
                            Repeater {
                                model:3
                                delegate:Rectangle {
                                    required property int index
                                    width:3;height:3;radius:1.5;color:root.accent
                                    opacity:index===placed.badgePhase ? 1 : 0.35
                                    Behavior on opacity {NumberAnimation{duration:180}}
                                }
                            }
                        }
                    }
                    Rectangle {
                        visible:placed.waiting && !root.chatOpen
                        anchors.right:face.right;anchors.top:face.top;anchors.rightMargin:-8;anchors.topMargin:-8
                        width:17;height:17;radius:8.5;color:"#eed29b"
                        Text {anchors.centerIn:parent;text:"!";color:"#2b2a1f";font.pixelSize:11;font.bold:true}
                    }
                    Rectangle {
                        objectName:"lively-cue-"+placed.modelData
                        property string cue:!placed.blob ? "" : placed.blob.state==="error" ? "×" : placed.blob.state==="capturing" ? "≡" : placed.blob.state==="stopping" ? "■" : placed.replying ? "✓" : ""
                        visible:root.lively && cue!=="" && !(root.chatOpen && root.selectedId===placed.modelData)
                        anchors.right:face.right;anchors.bottom:face.bottom;anchors.rightMargin:-5;anchors.bottomMargin:-4
                        width:17;height:17;radius:cue==="×" ? 4 : 8.5
                        color:cue==="×" ? "#f4a99a" : cue==="✓" ? "#b8dca4" : root.accent
                        Text {anchors.centerIn:parent;text:parent.cue;color:"#27322a";font.pixelSize:11;font.bold:true}
                        Accessible.role:Accessible.StaticText
                        Accessible.name:cue==="✓" ? "Reply ready" : root.stateCaption(placed.blob)
                    }
                    DragArea {id:dropArea;blobId:placed.modelData}
                    ChatBubble {blob:placed.blob;monitor:monitorScope.monitor;place:placed.pos;hostVisible:placed.onMonitor}
                }
            }
        }
    }
}
