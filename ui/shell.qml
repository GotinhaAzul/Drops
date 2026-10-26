pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs as Dialogs
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
    readonly property bool sudo: state.sudo === true
    property bool capturing: state.blobs.some(b=>b.state==="capturing")
    property var inspectedContext: null
    // Close the readable panels before another one opens or the shell hides,
    // so a modal popup can never outlive its panel or block the UI.
    function dismissOverlays() {
        let dismissed=false
        if(contextView.opened){contextView.close();dismissed=true}
        if(directoryView.opened){directoryView.close();dismissed=true}
        if(folderPicker.visible){folderPicker.close();dismissed=true}
        return dismissed
    }
    function inspectContext() {
        inspectedContext=selected ? selected.context : null
        directoryView.close()
        folderPicker.close()
        if(!state.panel_open)send({type:"toggle"})
        contextView.open()
        if(selected)send({type:"context",id:selectedId})
    }
    property string directoryDropId: ""
    function openDirectory() {
        if (!selected) return
        directoryDropId=selected.id
        directoryPath.text=selected.directory || (selected.context && selected.context.tool_workspace && selected.context.tool_workspace.root) || ""
        failure=""
        contextView.close()
        folderPicker.close()
        if (!state.panel_open) send({type:"toggle"})
        directoryView.open()
    }
    function directoryLabel(blob) {
        if (!blob) return "Choose a Drop first"
        if (blob.directory) return blob.directory
        const workspace=blob.context && blob.context.tool_workspace
        if (workspace && workspace.source==="private") return "Automatic · no folder detected"
        if (workspace && workspace.source==="configured") return workspace.root
        return workspace && workspace.root ? "Automatic · "+workspace.root : "Automatic · follows the anchored app"
    }
    function directoryHint(blob) {
        if (blob && blob.directory) return "Selected directory · file edits need approval"
        const workspace=blob && blob.context && blob.context.tool_workspace
        if (workspace && workspace.source==="private") return "Private folder · choose a directory to work on your files"
        if (workspace && workspace.source==="configured") return "Configured directory · file edits need approval"
        return "Follows the anchored app · choose a folder if detection fails"
    }
    function contextMetadata(value) {
        const copy=JSON.parse(JSON.stringify(value || {}))
        if(copy.content && copy.content.image)delete copy.content.image.data_url
        return JSON.stringify(copy,null,2)
    }
    function appDetails(blob) { return blob && blob.context && blob.context.app_context || ({}) }
    function hasTargetSelection(blob) {
        const app=appDetails(blob)
        return !!((app.selections || []).length || (app.selected_items || []).length)
    }
    function appSummary(blob) {
        const app=appDetails(blob)
        const pieces=[]
        if(app.page)pieces.push(app.page.title || app.page.uri)
        else if(app.document)pieces.push(app.document.title || app.document.uri)
        const selections=app.selections || []
        if(selections.length)pieces.push("Highlighted text · "+selections.reduce((n,s)=>n+String(s.text || "").length,0)+" characters")
        const items=app.selected_items || []
        if(items.length)pieces.push(items.length+(app.items_truncated ? "+" : "")+" selected item"+(items.length===1 ? "" : "s"))
        if(app.partial)pieces.push("Partial context")
        return pieces.join("\n")
    }
    function inkTint(alpha) { return Qt.rgba(root.ink.r,root.ink.g,root.ink.b,alpha) }
    function accentTint(alpha) { return Qt.rgba(root.accent.r,root.accent.g,root.accent.b,alpha) }
    function lookTint(look) {
        return look==="bunny" ? "#e9bfd5" : look==="robot" ? "#b6c5ee" : look==="cloud" ? "#d7e6ef" : look==="cat" ? "#f2c6a0" : look==="sprout" ? "#b8dca4" : root.accent
    }
    function lookName(look) {
        const value=String(look || "drop")
        return value.charAt(0).toUpperCase()+value.slice(1)
    }
    function dropLook(blob) { return blob ? blob.appearance || "drop" : root.pendingLook }
    function dropNumber(blob) {
        const index=blob ? root.blobIds.indexOf(blob.id) : -1
        return index<0 ? 1 : index+1
    }
    function modelLabel() {
        const model=String(root.state.connection && root.state.connection.model || "").trim()
        if(!root.connected)return "reconnecting"
        if(!model)return "no model yet"
        return model.split("/").pop()
    }
    function connectionColor() {
        if(!root.connected || !root.hasModel)return "#e5b96f"
        if(root.capturing)return root.accent
        if(root.selected && root.selected.state==="error")return "#f4a99a"
        return "#9fbf8f"
    }
    function heroStatus(blob) {
        if(!root.connected)return "Reconnecting to buddyd"
        if(!blob)return "Create a drop to begin"
        const caption=root.stateCaption(blob)
        if(caption)return caption
        if(blob.placement && blob.placement.anchor.kind==="panel")return "Not placed yet"
        return "Idle · ready when you are"
    }
    function heroHint(blob) {
        if(!blob)return "Up to three drops can live on your desktop"
        if(!blob.placement || blob.placement.anchor.kind==="panel")return "Drag the face onto a window or workspace"
        return "Click the face to chat · drag to move"
    }
    function chooseLook(index) {
        if(index<0 || index>=root.looks.length)return
        const look=root.looks[index]
        root.pendingLook=look
        if(root.currentDrop)root.send({type:"style",id:root.currentDrop.id,appearance:look})
    }
    function openSettings() {
        dismissOverlays();loadSettings();settingsOpen=true;failure=""
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
        if(blob && blob.directory)return String(blob.directory).split("/").pop()+" · selected directory"
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
        if(context.app_kind && context.app_kind!=="window")return context.app_kind
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
        if (kind==="browser") return "I'm on this browser window. What would you like help with?"
        if (kind==="editor") return "I'm on " + label + ". Want me to take a look?"
        if (kind==="file_manager") return "I'm on this folder. What would you like to work on?"
        if (kind==="document") return "I'm here with your document. What would you like help with?"
        if (kind==="workspace") return "This workspace is all mine. What should we do?"
        return "I'm on " + label + ". What do you need?"
    }
    function suggestionsFor(blob) {
        const kind=contextKind(blob)
        const app=appDetails(blob)
        if((app.selections || []).length)return [
            {label:"Explain selection",prompt:"Explain the highlighted text in app_context.selections in plain language. Use only the selection actually captured; if it is no longer available, tell me."},
            {label:"Rewrite selection",prompt:"Draft a clearer version of the highlighted text in app_context.selections here in chat, preserving its meaning. If no selection was captured, tell me."}
        ]
        if(kind==="file_manager")return (app.selected_items || []).length ? [
            {label:"Review selected files",prompt:"Review the selected explorer items in app_context. Use the available read tools only for verified paths within the tool workspace. Explain any items you cannot access."},
            {label:"Suggest organization",prompt:"Suggest how to organize the selected explorer items in app_context. Do not change files yet; explain which paths are verified and any missing information."}
        ] : [
            {label:"What's in this folder?",prompt:"Use the file tools to explain what is in the detected folder. If no folder was detected, tell me instead of describing the private Drop directory as my folder."},
            {label:"Help organize it",prompt:"Inspect the detected folder and suggest an organization plan. Do not change anything yet. If the folder could not be detected, tell me."}
        ]
        if(kind==="document")return [
            {label:"Summarize document",prompt:"Summarize the current document using captured content and the available read tools. Explain any missing content or unsupported format."},
            {label:"Explain this",prompt:"Explain what I am looking at in this document. Use captured evidence and tell me what you cannot read."}
        ]
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
        if (!root.selectedBusy) send({type:"context",id:id})
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
        property color tint:root.lookTint(look)
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
    component SectionLabel: Label {
        color:root.muted;font.pixelSize:9;font.letterSpacing:1.3
        font.capitalization:Font.AllUppercase;font.bold:true
    }
    component PanelCard: Rectangle {
        id:card
        property int pad:10
        default property alias content:cardColumn.data
        implicitHeight:cardColumn.implicitHeight+card.pad*2
        radius:14
        color:root.inkTint(0.035)
        border.width:1
        border.color:root.inkTint(0.10)
        ColumnLayout {
            id:cardColumn
            anchors.fill:parent;anchors.margins:card.pad
            spacing:7
        }
    }
    component IconButton: ToolButton {
        id:icon
        implicitWidth:28;implicitHeight:28
        hoverEnabled:true
        background:Rectangle {
            radius:9
            color:icon.hovered ? root.inkTint(0.08) : "transparent"
            border.width:icon.hovered ? 1 : 0
            border.color:root.inkTint(0.10)
            Behavior on color {ColorAnimation{duration:100}}
        }
        contentItem:Text {
            text:icon.text;color:icon.hovered ? root.ink : root.muted
            font.pixelSize:13;horizontalAlignment:Text.AlignHCenter;verticalAlignment:Text.AlignVCenter
        }
    }
    component ActionButton: Button {
        id:action
        property bool primary:false
        property bool danger:false
        implicitHeight:30
        hoverEnabled:true
        leftPadding:10;rightPadding:10
        opacity:enabled ? 1 : 0.38
        scale:pressed ? 0.98 : 1
        Behavior on scale {NumberAnimation{duration:80}}
        background:Rectangle {
            radius:9
            color:action.primary
                ? root.accentTint(action.hovered ? 0.26 : 0.16)
                : action.danger
                    ? Qt.rgba(0.95,0.45,0.38,action.hovered ? 0.16 : 0.08)
                    : root.inkTint(action.hovered ? 0.075 : 0.035)
            border.width:1
            border.color:action.primary
                ? root.accentTint(action.hovered ? 0.75 : 0.48)
                : action.danger
                    ? Qt.rgba(0.95,0.45,0.38,action.hovered ? 0.55 : 0.30)
                    : root.inkTint(action.hovered ? 0.20 : 0.10)
            Behavior on color {ColorAnimation{duration:110}}
        }
        contentItem:Text {
            text:action.text
            color:action.primary ? root.accent : action.danger ? "#f0a79c" : root.ink
            font.pixelSize:11;font.bold:action.primary
            horizontalAlignment:Text.AlignHCenter;verticalAlignment:Text.AlignVCenter
            elide:Text.ElideRight
        }
    }
    component SegmentButton: Button {
        id:segment
        property bool selected:false
        implicitHeight:28
        hoverEnabled:true
        leftPadding:8;rightPadding:8
        background:Rectangle {
            radius:7
            color:segment.selected ? root.accentTint(0.20) : segment.hovered ? root.inkTint(0.06) : "transparent"
            border.width:1
            border.color:segment.selected ? root.accentTint(0.55) : "transparent"
            Behavior on color {ColorAnimation{duration:100}}
        }
        contentItem:Text {
            text:segment.text
            color:segment.selected ? root.accent : root.muted
            font.pixelSize:10;font.bold:segment.selected
            horizontalAlignment:Text.AlignHCenter;verticalAlignment:Text.AlignVCenter
            elide:Text.ElideRight
        }
    }
    component ThemeSwitch: Switch {
        id:sw
        implicitWidth:36;implicitHeight:22
        indicator:Rectangle {
            implicitWidth:36;implicitHeight:20;radius:10
            x:0;y:(sw.height-height)/2
            color:sw.checked ? root.accentTint(0.72) : root.inkTint(0.12)
            border.width:1
            border.color:sw.checked ? root.accentTint(0.85) : root.inkTint(0.16)
            Behavior on color {ColorAnimation{duration:130}}
            Rectangle {
                width:16;height:16;radius:8;y:2
                x:sw.checked ? parent.width-width-2 : 2
                color:sw.checked ? root.background : root.muted
                Behavior on x {NumberAnimation{duration:130;easing.type:Easing.OutCubic}}
            }
        }
    }
    component ThemedCheck: CheckBox {
        id:check
        spacing:8
        font.pixelSize:11
        indicator:Rectangle {
            implicitWidth:16;implicitHeight:16;radius:5
            x:check.leftPadding
            y:check.topPadding+(check.availableHeight-height)/2
            color:check.checked ? root.accentTint(0.75) : root.inkTint(0.05)
            border.width:1
            border.color:check.checked ? root.accentTint(0.9) : root.inkTint(0.18)
            Text {
                anchors.centerIn:parent;text:"✓";visible:check.checked
                color:root.background;font.pixelSize:10;font.bold:true
            }
            Behavior on color {ColorAnimation{duration:100}}
        }
        contentItem:Text {
            leftPadding:check.indicator ? check.indicator.width+check.spacing : 0
            text:check.text;color:root.ink;font.pixelSize:11
            verticalAlignment:Text.AlignVCenter;wrapMode:Text.Wrap
        }
    }
    component FramedField: TextField {
        id:field
        implicitHeight:34
        font.pixelSize:11
        color:root.ink
        placeholderTextColor:root.muted
        selectByMouse:true
        leftPadding:10;rightPadding:10
        background:Rectangle {
            radius:8
            color:root.inkTint(0.045)
            border.width:1
            border.color:field.activeFocus ? root.accentTint(0.65) : root.inkTint(0.12)
            Behavior on border.color {ColorAnimation{duration:110}}
        }
    }
    component ContextBlock: TextArea {
        id:block
        readOnly:true
        wrapMode:TextEdit.WrapAnywhere
        textFormat:TextEdit.PlainText
        color:root.ink;font.pixelSize:10
        leftPadding:9;rightPadding:9;topPadding:7;bottomPadding:7
        background:Rectangle {
            radius:8
            color:root.inkTint(0.04)
            border.width:1
            border.color:root.inkTint(0.08)
        }
    }
    component LookChip: Item {
        id:chip
        property string look:"drop"
        property bool selected:false
        property bool hovered:false
        signal picked()
        implicitWidth:38;implicitHeight:38
        function activate() { picked() }
        Rectangle {
            anchors.fill:parent;radius:12
            color:chip.selected ? root.accentTint(0.14) : chip.hovered ? root.inkTint(0.06) : "transparent"
            border.width:1
            border.color:chip.selected ? root.accentTint(0.55) : root.inkTint(0.08)
            Behavior on color {ColorAnimation{duration:100}}
        }
        DropFace {
            width:24;height:23;anchors.centerIn:parent
            look:chip.look;awake:true;mood:"idle";hovered:chip.hovered
        }
        Rectangle {
            visible:chip.selected
            width:4;height:4;radius:2;anchors.bottom:parent.bottom;anchors.bottomMargin:3;anchors.horizontalCenter:parent.horizontalCenter
            color:root.accent
        }
        MouseArea {
            anchors.fill:parent;hoverEnabled:true;cursorShape:Qt.PointingHandCursor
            onEntered:chip.hovered=true;onExited:chip.hovered=false
            onClicked:chip.activate()
        }
        Accessible.role:Accessible.Button
        Accessible.name:root.lookName(chip.look)+" look"
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
        Shortcut {sequence:"Escape";enabled:bubble.active;onActivated:{if(!root.dismissOverlays())root.closeChat()}}
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
                Rectangle {
                    width:parent.width;height:visible ? appSummaryText.implicitHeight+16 : 0
                    visible:root.appSummary(bubble.blob).length>0
                    radius:8;color:Qt.rgba(root.accent.r,root.accent.g,root.accent.b,0.08)
                    Text {
                        id:appSummaryText;x:8;y:8;width:parent.width-38
                        text:root.appSummary(bubble.blob);textFormat:Text.PlainText;wrapMode:Text.Wrap
                        maximumLineCount:3;elide:Text.ElideRight;color:root.accent;font.pixelSize:10
                    }
                    MouseArea {anchors.fill:parent;cursorShape:Qt.PointingHandCursor;onClicked:{root.selectedId=bubble.blob.id;root.inspectContext()}}
                    ToolButton {
                        anchors.right:parent.right;anchors.top:parent.top;width:28;height:28;text:"↻"
                        enabled:!bubble.busy;Accessible.name:"Refresh app context"
                        ToolTip.visible:hovered;ToolTip.text:"Refresh what this Drop sees"
                        onClicked:root.send({type:"context",id:bubble.blob.id})
                    }
                }
                Flow {
                    id:suggestionFlow
                    visible:!bubble.hasHistory || root.hasTargetSelection(bubble.blob)
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
        implicitWidth:344
        implicitHeight:root.settingsOpen ? 620 : panelContent.implicitHeight+44
        color:"transparent";exclusionMode:ExclusionMode.Ignore
        WlrLayershell.namespace:"drops-panel"
        WlrLayershell.keyboardFocus:WlrKeyboardFocus.OnDemand
        Shortcut {
            id:escapeShortcut
            sequence:"Escape";enabled:panel.visible && !folderPicker.visible
            onActivated:{
                if(root.dismissOverlays())return
                if(root.chatOpen){root.closeChat();return}
                root.send({type:"toggle"})
            }
        }
        onVisibleChanged: if(!visible) root.dismissOverlays()
        Rectangle {
            anchors.fill:parent;radius:20
            gradient:Gradient {
                GradientStop {position:0;color:Qt.lighter(root.background,1.09)}
                GradientStop {position:0.42;color:root.background}
                GradientStop {position:1;color:Qt.darker(root.background,0.94)}
            }
            border.color:root.accentTint(0.18);border.width:1
            Rectangle {
                anchors.top:parent.top;anchors.horizontalCenter:parent.horizontalCenter
                width:parent.width-56;height:1;color:root.accentTint(0.32)
            }
            ColumnLayout {
                id:panelContent
                visible:!root.settingsOpen
                anchors.fill:parent;anchors.margins:12;spacing:6
                RowLayout {
                    Layout.fillWidth:true;spacing:8
                    Item {
                        Layout.preferredWidth:10;Layout.preferredHeight:10;Layout.alignment:Qt.AlignVCenter
                        Rectangle {anchors.centerIn:parent;width:16;height:16;radius:8;color:root.connectionColor();opacity:0.16}
                        Rectangle {anchors.centerIn:parent;width:7;height:7;radius:3.5;color:root.connectionColor()}
                    }
                    ColumnLayout {
                        Layout.fillWidth:true;spacing:0
                        Label {text:"DROPS";color:root.ink;font.pixelSize:10;font.bold:true;font.letterSpacing:2.4}
                        Label {
                            Layout.fillWidth:true;elide:Text.ElideRight;font.pixelSize:9
                            color:root.failure ? "#f4a99a" : root.muted
                            text:root.failure ? root.failure : !root.connected ? "Reconnecting to buddyd" : root.hasModel ? "Connected · "+root.modelLabel() : "No model configured"
                            MouseArea {id:statusHover;anchors.fill:parent;hoverEnabled:true;acceptedButtons:Qt.NoButton}
                            ToolTip.visible:statusHover.containsMouse && !!root.failure;ToolTip.text:root.failure
                        }
                    }
                    IconButton {
                        text:"⚙";Accessible.name:"Model connection"
                        ToolTip.visible:hovered;ToolTip.text:"Model connection"
                        onClicked:root.openSettings()
                    }
                    IconButton {
                        text:root.selectedBusy ? "■" : "×"
                        Accessible.name:root.selectedBusy ? "Stop this turn" : "Close panel"
                        ToolTip.visible:hovered;ToolTip.text:Accessible.name
                        onClicked:root.selectedBusy ? root.send({type:"cancel",id:root.selectedId}) : root.send({type:"toggle"})
                    }
                }
                RowLayout {
                    Layout.fillWidth:true;spacing:5
                    visible:root.blobIds.length>1
                    Repeater {
                        model:root.blobIds
                        delegate: Rectangle {
                            id:dropPill
                            required property string modelData
                            required property int index
                            property var blob:root.state.blobs.find(b=>b.id===modelData) || null
                            Layout.fillWidth:true;implicitHeight:28;radius:8
                            color:root.selectedId===modelData ? root.accentTint(0.15) : root.inkTint(0.03)
                            border.width:1
                            border.color:root.selectedId===modelData ? root.accentTint(0.45) : root.inkTint(0.08)
                            Behavior on color {ColorAnimation{duration:110}}
                            Row {
                                anchors.centerIn:parent;spacing:5
                                Rectangle {width:8;height:8;radius:4;color:root.lookTint(root.dropLook(dropPill.blob));anchors.verticalCenter:parent.verticalCenter}
                                Text {
                                    text:"Drop "+(index+1)
                                    color:root.selectedId===dropPill.modelData ? root.accent : root.muted
                                    font.pixelSize:9;font.bold:root.selectedId===dropPill.modelData
                                    anchors.verticalCenter:parent.verticalCenter
                                }
                            }
                            Accessible.role:Accessible.Button
                            Accessible.name:"Select Drop "+(index+1)
                            MouseArea {anchors.fill:parent;cursorShape:Qt.PointingHandCursor;onClicked:{root.selectedId=dropPill.modelData;root.chatOpen=false}}
                        }
                    }
                }
                PanelCard {
                    Layout.fillWidth:true
                    RowLayout {
                        Layout.fillWidth:true;spacing:13
                        Item {
                            Layout.preferredWidth:76;Layout.preferredHeight:76
                            Rectangle {anchors.centerIn:parent;width:76;height:76;radius:38;color:"transparent";border.width:1;border.color:root.accentTint(0.13)}
                            Rectangle {anchors.centerIn:parent;width:66;height:66;radius:33;color:root.accentTint(0.05)}
                            DropFace {
                                anchors.centerIn:parent;width:58;height:56
                                look:root.currentDrop ? root.currentDrop.appearance || "drop" : root.pendingLook
                                mood:root.currentDrop ? root.currentDrop.state : "idle"
                                hovered:panelAvatar.containsMouse
                                opacity:!root.currentDrop ? 0.45 : root.dragging && root.dragId===root.currentDrop.id ? 0 : 1
                                Behavior on opacity {NumberAnimation{duration:70}}
                                DragArea {id:panelAvatar;enabled:!!root.currentDrop;blobId:root.currentDrop ? root.currentDrop.id : ""}
                            }
                        }
                        ColumnLayout {
                            Layout.fillWidth:true;spacing:3
                            SectionLabel {
                                text:root.currentDrop ? "Drop "+root.dropNumber(root.currentDrop)+" · "+root.lookName(root.dropLook(root.currentDrop)) : "No drop yet"
                            }
                            Label {
                                Layout.fillWidth:true;elide:Text.ElideRight
                                text:root.currentDrop ? root.shortLabel(root.currentDrop) : "Make your first drop"
                                color:root.ink;font.pixelSize:14;font.bold:true
                                MouseArea {anchors.fill:parent;enabled:!!root.selected;cursorShape:Qt.PointingHandCursor;onClicked:root.inspectContext()}
                            }
                            RowLayout {
                                Layout.fillWidth:true;spacing:5
                                Rectangle {width:5;height:5;radius:2.5;color:root.connectionColor();Layout.alignment:Qt.AlignVCenter}
                                Label {Layout.fillWidth:true;elide:Text.ElideRight;text:root.heroStatus(root.currentDrop);color:root.muted;font.pixelSize:10}
                            }
                            Label {
                                Layout.fillWidth:true;wrapMode:Text.Wrap;maximumLineCount:2;elide:Text.ElideRight
                                text:root.heroHint(root.currentDrop);color:root.muted;font.pixelSize:9;opacity:0.8
                            }
                        }
                    }
                    ActionButton {
                        id:chatButton
                        Layout.fillWidth:true;primary:true
                        enabled:!!root.currentDrop && root.currentDrop.placement.anchor.kind!=="panel"
                        text:!root.currentDrop ? "Make a drop to start" : enabled ? "Chat with this drop  →" : "Drag me onto a window"
                        onClicked:root.openChat(root.currentDrop.id,true)
                    }
                }
                PanelCard {
                    Layout.fillWidth:true
                    RowLayout {
                        Layout.fillWidth:true
                        SectionLabel {text:"File access";Layout.fillWidth:true}
                        ActionButton {
                            id:directoryButton
                            text:"Change";implicitHeight:24
                            enabled:!!root.selected && !root.selectedBusy && root.connected
                            Accessible.name:"Choose working directory"
                            ToolTip.visible:hovered;ToolTip.text:"Choose this Drop's working directory"
                            onClicked:root.openDirectory()
                        }
                    }
                    RowLayout {
                        Layout.fillWidth:true;spacing:9
                        Item {
                            Layout.preferredWidth:24;Layout.preferredHeight:24;Layout.alignment:Qt.AlignTop
                            Rectangle {x:1;y:8;width:22;height:14;radius:4;color:root.accentTint(0.13);border.width:1;border.color:root.accentTint(0.35)}
                            Rectangle {x:3;y:5;width:9;height:5;radius:2;color:root.accentTint(0.20)}
                        }
                        ColumnLayout {
                            Layout.fillWidth:true;spacing:2
                            Label {
                                Layout.fillWidth:true;elide:Text.ElideMiddle
                                text:root.directoryLabel(root.selected)
                                color:root.accent;font.pixelSize:11
                                ToolTip.visible:directoryHover.containsMouse;ToolTip.text:text
                                MouseArea {id:directoryHover;anchors.fill:parent;hoverEnabled:true;cursorShape:Qt.PointingHandCursor;onClicked:root.inspectContext()}
                            }
                            Label {
                                Layout.fillWidth:true;wrapMode:Text.Wrap;maximumLineCount:2;elide:Text.ElideRight
                                text:root.sudo ? "Edits run without prompts in this directory" : root.directoryHint(root.selected)
                                color:root.muted;font.pixelSize:9
                            }
                        }
                    }
                }
                Rectangle {
                    Layout.fillWidth:true
                    implicitHeight:evidenceRow.implicitHeight+12
                    visible:root.appSummary(root.currentDrop).length>0
                    radius:10
                    color:root.accentTint(0.06)
                    border.width:1;border.color:root.accentTint(0.14)
                    RowLayout {
                        id:evidenceRow
                        anchors.fill:parent;anchors.leftMargin:10;anchors.rightMargin:6;spacing:8
                        Rectangle {width:6;height:6;radius:3;color:root.accent;Layout.alignment:Qt.AlignVCenter}
                        Label {
                            Layout.fillWidth:true;elide:Text.ElideRight;textFormat:Text.PlainText
                            text:root.appSummary(root.currentDrop).replace(/\n/g,"   ·   ")
                            color:root.ink;font.pixelSize:10
                            MouseArea {anchors.fill:parent;cursorShape:Qt.PointingHandCursor;onClicked:root.inspectContext()}
                        }
                        ActionButton {text:"Inspect";implicitHeight:24;onClicked:root.inspectContext()}
                    }
                }
                PanelCard {
                    Layout.fillWidth:true
                    RowLayout {
                        Layout.fillWidth:true
                        SectionLabel {text:"Appearance";Layout.fillWidth:true}
                        Label {
                            text:root.lookName(root.currentDrop ? root.dropLook(root.currentDrop) : root.pendingLook)
                            color:root.accent;font.pixelSize:9;font.letterSpacing:1
                            font.capitalization:Font.AllUppercase
                        }
                    }
                    Row {
                        id:looksRow
                        Layout.alignment:Qt.AlignHCenter
                        spacing:3
                        Repeater {
                            id:lookRepeater
                            model:root.looks
                            delegate: LookChip {
                                required property string modelData
                                required property int index
                                look:modelData
                                selected:(root.currentDrop ? root.dropLook(root.currentDrop) : root.pendingLook)===modelData
                                onPicked:root.chooseLook(index)
                            }
                        }
                    }
                }
                PanelCard {
                    Layout.fillWidth:true
                    RowLayout {
                        Layout.fillWidth:true;spacing:10
                        ColumnLayout {
                            Layout.fillWidth:true;spacing:1
                            Label {text:"Lively drops";color:root.ink;font.pixelSize:11;font.bold:true}
                            Label {
                                Layout.fillWidth:true;wrapMode:Text.Wrap;maximumLineCount:2;elide:Text.ElideRight
                                text:"Gentle motion and expressive faces."
                                color:root.muted;font.pixelSize:9
                            }
                        }
                        ThemeSwitch {
                            id:livelyToggle
                            Accessible.name:"Lively Drops"
                            checked:root.lively;enabled:root.connected
                            onToggled:root.send({type:"lively",enabled:checked})
                            ToolTip.visible:hovered;ToolTip.text:"Gentle motion, expressive faces and status badges"
                        }
                    }
                    Rectangle {Layout.fillWidth:true;height:1;color:root.inkTint(0.07)}
                    RowLayout {
                        Layout.fillWidth:true;spacing:10
                        ColumnLayout {
                            Layout.fillWidth:true;spacing:1
                            Label {text:"Sudo mode";color:root.sudo ? "#e5b96f" : root.ink;font.pixelSize:11;font.bold:true}
                            Label {
                                Layout.fillWidth:true;wrapMode:Text.Wrap;maximumLineCount:2;elide:Text.ElideRight
                                text:root.sudo ? "No prompts · boundaries and backups stay on." : "Ask before edits, shell commands and web requests."
                                color:root.muted;font.pixelSize:9
                            }
                        }
                        ThemeSwitch {
                            id:sudoToggle
                            Accessible.name:"Sudo mode"
                            checked:root.sudo;enabled:root.connected
                            onToggled:root.send({type:"sudo",enabled:checked})
                            ToolTip.visible:hovered;ToolTip.text:"Validation, workspace limits and backups still apply"
                        }
                    }
                }
                RowLayout {
                    Layout.fillWidth:true;spacing:6
                    ActionButton {
                        id:newDropButton
                        Layout.fillWidth:true
                        text:"+  New drop"
                        Accessible.name:"New drop"
                        enabled:root.blobIds.length<3
                        ToolTip.visible:hovered;ToolTip.text:root.blobIds.length>=3 ? "Three drops is the limit" : "Create another drop"
                        onClicked:{root.chatOpen=false;root.send({type:"create",appearance:root.currentDrop ? root.currentDrop.appearance || "drop" : root.pendingLook})}
                    }
                    IconButton {
                        id:deleteDropButton
                        text:"−"
                        Accessible.name:"Delete drop"
                        enabled:!!root.currentDrop
                        ToolTip.visible:hovered;ToolTip.text:"Delete selected drop and conversation"
                        onClicked:root.send({type:"remove",id:root.currentDrop.id})
                    }
                }
            }
            Dialog {
                id:directoryView;objectName:"directory-view"
                x:8;y:8;width:parent.width-16;modal:true;padding:14
                closePolicy:Popup.CloseOnPressOutside
                background:Rectangle {radius:16;color:root.background;border.width:1;border.color:root.accentTint(0.38)}
                ColumnLayout {
                    width:parent.width;spacing:9
                    RowLayout {
                        Layout.fillWidth:true
                        ColumnLayout {
                            Layout.fillWidth:true;spacing:1
                            SectionLabel {text:"Working directory";Layout.fillWidth:true}
                            Label {text:"File access";color:root.ink;font.pixelSize:14;font.bold:true}
                        }
                        IconButton {
                            text:"×";Accessible.name:"Close working directory"
                            onClicked:directoryView.close()
                        }
                    }
                    Label {
                        Layout.fillWidth:true;wrapMode:Text.Wrap;color:root.muted;font.pixelSize:10
                        text:"This Drop can read and edit files inside one directory. File changes keep their approval and backup protections."
                    }
                    RowLayout {
                        Layout.fillWidth:true;spacing:6
                        FramedField {
                            id:directoryPath;Layout.fillWidth:true
                            placeholderText:"/absolute/path/to/folder"
                            Accessible.name:"Working directory path"
                        }
                        ActionButton {text:"Browse…";onClicked:folderPicker.open()}
                    }
                    Label {
                        Layout.fillWidth:true;wrapMode:Text.Wrap;color:root.muted;font.pixelSize:9
                        text:"Automatic follows the anchored app when its directory can be detected."
                    }
                    RowLayout {
                        Layout.fillWidth:true;spacing:6
                        ActionButton {
                            id:automaticDirectoryButton
                            Layout.fillWidth:true
                            text:"Automatic"
                            onClicked:{root.send({type:"directory",id:root.directoryDropId,path:null});directoryView.close()}
                        }
                        ActionButton {
                            id:useDirectoryButton
                            Layout.fillWidth:true;primary:true
                            text:"Use folder"
                            enabled:directoryPath.text.trim().length>0
                            onClicked:{root.send({type:"directory",id:root.directoryDropId,path:directoryPath.text.trim()});directoryView.close()}
                        }
                    }
                    ActionButton {
                        Layout.fillWidth:true
                        text:"Cancel"
                        onClicked:directoryView.close()
                    }
                }
            }
            Dialogs.FolderDialog {
                id:folderPicker
                title:"Choose this Drop's working directory"
                onAccepted:directoryPath.text=decodeURIComponent(String(selectedFolder).replace(/^file:\/\//,""))
            }
            ScrollView {
                id:settingsScroll
                visible:root.settingsOpen
                anchors.top:parent.top;anchors.left:parent.left;anchors.right:parent.right;anchors.bottom:settingsFooter.top
                anchors.margins:14;anchors.bottomMargin:8
                contentWidth:availableWidth;clip:true
                ScrollBar.vertical.policy:ScrollBar.AsNeeded
                palette.window:root.background
                palette.mid:root.inkTint(0.20)
                ColumnLayout {
                    width:settingsScroll.availableWidth;spacing:9
                    Item {
                        Layout.fillWidth:true;implicitHeight:38
                        Column {
                            anchors.left:parent.left;anchors.verticalCenter:parent.verticalCenter
                            spacing:0
                            Label {text:"Model";color:root.ink;font.pixelSize:16;font.bold:true}
                            Label {text:"Connection & capture";color:root.muted;font.pixelSize:10}
                        }
                        IconButton {
                            anchors.right:parent.right;anchors.verticalCenter:parent.verticalCenter
                            text:"×";Accessible.name:"Close settings"
                            ToolTip.visible:hovered;ToolTip.text:"Close settings"
                            onClicked:root.settingsOpen=false
                        }
                    }
                    Label {visible:!!root.failure;Layout.fillWidth:true;text:root.failure;color:"#f4a99a";font.pixelSize:10;wrapMode:Text.Wrap}
                    PanelCard {
                        Layout.fillWidth:true
                        SectionLabel {text:"Provider";Layout.fillWidth:true}
                        Rectangle {
                            Layout.fillWidth:true;implicitHeight:34;radius:10
                            color:root.inkTint(0.04);border.width:1;border.color:root.inkTint(0.10)
                            RowLayout {
                                anchors.fill:parent;anchors.margins:3;spacing:2
                                SegmentButton {
                                    id:customModeButton
                                    Layout.fillWidth:true;text:"Custom endpoint"
                                    selected:root.connectionMode==="custom"
                                    onClicked:root.selectConnectionMode(0)
                                }
                                SegmentButton {
                                    id:goModeButton
                                    Layout.fillWidth:true;text:"Opencode Go"
                                    selected:root.connectionMode==="opencode_go"
                                    onClicked:root.selectConnectionMode(1)
                                }
                            }
                        }
                        Label {
                            Layout.fillWidth:true;wrapMode:Text.Wrap;color:root.muted;font.pixelSize:9
                            text:root.connectionMode==="opencode_go" ? "Use an Opencode Go key and pick from the gateway's model list." : "Point Drops at any OpenAI-compatible endpoint."
                        }
                    }
                    PanelCard {
                        Layout.fillWidth:true
                        visible:root.connectionMode==="custom"
                        SectionLabel {text:"Custom endpoint";Layout.fillWidth:true}
                        Label {text:"Endpoint URL";color:root.muted;font.pixelSize:10}
                        FramedField {id:endpoint;Layout.fillWidth:true;placeholderText:"http://localhost:11434/v1"}
                        Label {text:"Model name";color:root.muted;font.pixelSize:10}
                        FramedField {id:modelName;Layout.fillWidth:true;placeholderText:"Model served by your endpoint"}
                        Label {text:"API-key environment variable · optional";color:root.muted;font.pixelSize:10}
                        FramedField {id:keyEnv;Layout.fillWidth:true;placeholderText:"e.g. DROPS_API_KEY"}
                    }
                    PanelCard {
                        Layout.fillWidth:true
                        visible:root.connectionMode==="opencode_go"
                        SectionLabel {text:"Opencode Go";Layout.fillWidth:true}
                        Label {text:"API key";color:root.muted;font.pixelSize:10}
                        FramedField {
                            id:opencodeKey
                            Layout.fillWidth:true
                            echoMode:TextInput.Password
                            placeholderText:"Paste your Opencode Go API key"
                        }
                        Label {
                            Layout.fillWidth:true;wrapMode:Text.Wrap;color:root.muted;font.pixelSize:9
                            text:"Stored only in Drops' config file (mode 0600). Set OPENCODE_API_KEY instead if you prefer the environment."
                        }
                        RowLayout {
                            Layout.fillWidth:true;spacing:6
                            Label {text:"Model";color:root.muted;font.pixelSize:10;Layout.fillWidth:true}
                            ActionButton {
                                id:loadModelsButton
                                text:root.loadingModels ? "Loading…" : "Load models"
                                implicitHeight:26
                                enabled:root.connected && !root.loadingModels && root.opencodeHasKey()
                                onClicked:root.loadModels()
                            }
                        }
                        ComboBox {
                            id:opencodeModel
                            Layout.fillWidth:true;Layout.preferredHeight:34;font.pixelSize:11
                            model:root.opencodeModels
                            editable:true
                            enabled:!root.loadingModels
                            currentIndex:root.opencodeModels.indexOf(root.opencodeSelected)
                            onActivated:index=>{root.opencodeSelected=root.opencodeModels[index]}
                            onEditTextChanged:{
                                const value=String(editText).trim()
                                if(value!==root.opencodeSelected)root.opencodeSelected=value
                            }
                            palette.text:root.ink
                            palette.buttonText:root.ink
                            palette.button:Qt.lighter(root.background,1.08)
                            palette.base:Qt.lighter(root.background,1.08)
                            palette.window:root.background
                            palette.dark:root.muted
                            palette.highlight:root.accentTint(0.25)
                            palette.highlightedText:root.ink
                            background:Rectangle {
                                implicitHeight:34;radius:8
                                color:root.inkTint(0.045)
                                border.width:1
                                border.color:opencodeModel.visualFocus ? root.accentTint(0.65) : root.inkTint(0.12)
                            }
                        }
                    }
                    PanelCard {
                        Layout.fillWidth:true
                        SectionLabel {text:"Capture";Layout.fillWidth:true}
                        ThemedCheck {
                            id:contentCapture
                            Layout.fillWidth:true
                            text:"Read attached window content"
                        }
                        Label {
                            Layout.fillWidth:true;wrapMode:Text.Wrap;color:root.muted;font.pixelSize:9
                            text:"Accessibility text and selections from the anchored window."
                        }
                        ThemedCheck {
                            id:screenCapture
                            Layout.fillWidth:true
                            text:"Screenshots / OCR fallback"
                            enabled:contentCapture.checked
                            opacity:enabled ? 1 : 0.45
                        }
                        ThemedCheck {
                            id:visionMode
                            Layout.fillWidth:true
                            text:"Model supports images"
                        }
                    }
                }
            }
            ColumnLayout {
                id:settingsFooter
                visible:root.settingsOpen
                anchors.left:parent.left;anchors.right:parent.right;anchors.bottom:parent.bottom
                anchors.margins:14
                spacing:6
                Rectangle {Layout.fillWidth:true;height:1;color:root.inkTint(0.08)}
                ActionButton {
                    id:saveConnectionButton
                    Layout.fillWidth:true;primary:true
                    text:"Save connection"
                    enabled:root.connectionMode==="opencode_go" ? (root.opencodeSelected.length>0 && root.opencodeHasKey()) : (endpoint.text.trim().length>0 && modelName.text.trim().length>0)
                    onClicked:root.saveConnection()
                }
                Label {
                    Layout.fillWidth:true;wrapMode:Text.Wrap;color:root.muted;font.pixelSize:9
                    horizontalAlignment:Text.AlignHCenter
                    text:"Keys stay local · file edits still ask for approval"
                }
            }
            Popup {
                id:contextView;objectName:"context-view";x:8;y:8;width:parent.width-16;height:parent.height-16;modal:true;padding:0
                closePolicy:Popup.CloseOnPressOutside
                background:Rectangle {color:root.background;radius:16;border.width:1;border.color:root.accentTint(0.40)}
                contentItem:ColumnLayout {
                    spacing:0
                    RowLayout {
                        Layout.fillWidth:true;Layout.margins:14;Layout.bottomMargin:6;spacing:8
                        ColumnLayout {
                            Layout.fillWidth:true;spacing:0
                            SectionLabel {text:"Context";Layout.fillWidth:true}
                            Label {text:"What this Drop sees";font.pixelSize:14;font.bold:true;color:root.ink}
                        }
                        IconButton {
                            id:contextCloseButton;objectName:"context-close"
                            text:"×"
                            Accessible.name:"Close context details"
                            ToolTip.visible:hovered;ToolTip.text:"Close"
                            onClicked:contextView.close()
                        }
                    }
                    Rectangle {Layout.fillWidth:true;height:1;Layout.leftMargin:14;Layout.rightMargin:14;color:root.inkTint(0.08)}
                    ScrollView {
                        id:contextScroll
                        Layout.fillWidth:true;Layout.fillHeight:true
                        Layout.leftMargin:14;Layout.rightMargin:14;Layout.bottomMargin:14;Layout.topMargin:10
                        contentWidth:availableWidth;clip:true
                        ScrollBar.vertical.policy:ScrollBar.AsNeeded
                        palette.window:root.background
                        palette.mid:root.inkTint(0.20)
                        Column {
                            width:contextScroll.availableWidth;spacing:9
                            Label {
                                width:parent.width;wrapMode:Text.Wrap;color:root.muted;font.pixelSize:9
                                text:root.inspectedContext && root.inspectedContext.app_context && root.inspectedContext.app_context.status==="disabled"
                                    ? "Window content capture is off."
                                    : "Refreshed when opened or sent. Missing selections and paths are never guessed."
                            }
                            PanelCard {
                                width:parent.width
                                SectionLabel {text:"File access";Layout.fillWidth:true}
                                ContextBlock {
                                    Layout.fillWidth:true
                                    color:root.accent
                                    text:root.inspectedContext && root.inspectedContext.tool_workspace ? root.inspectedContext.tool_workspace.root : "Not detected"
                                }
                            }
                            PanelCard {
                                width:parent.width
                                visible:root.inspectedContext && root.inspectedContext.app_context && (root.inspectedContext.app_context.documents || []).length>0
                                SectionLabel {text:"Page & documents";Layout.fillWidth:true}
                                Column {
                                    Layout.fillWidth:true
                                    spacing:6
                                    Repeater {
                                        model:root.inspectedContext && root.inspectedContext.app_context ? root.inspectedContext.app_context.documents || [] : []
                                        delegate: ContextBlock {
                                            required property var modelData
                                            width:parent.width
                                            text:(modelData.title || "Page / document")+"\n"+modelData.uri
                                        }
                                    }
                                }
                            }
                            PanelCard {
                                width:parent.width
                                visible:root.inspectedContext && root.inspectedContext.app_context && (root.inspectedContext.app_context.selections || []).length>0
                                SectionLabel {text:"Highlighted text";Layout.fillWidth:true}
                                Column {
                                    Layout.fillWidth:true
                                    spacing:6
                                    Repeater {
                                        model:root.inspectedContext && root.inspectedContext.app_context ? root.inspectedContext.app_context.selections || [] : []
                                        delegate: Rectangle {
                                            required property var modelData
                                            width:parent.width;implicitHeight:selectionBody.implicitHeight+18
                                            radius:9;color:root.accentTint(0.07)
                                            border.width:1;border.color:root.accentTint(0.16)
                                            Rectangle {x:0;y:8;width:2;height:parent.height-16;radius:1;color:root.accent}
                                            Column {
                                                id:selectionBody
                                                x:10;y:8;width:parent.width-20;spacing:3
                                                Label {
                                                    visible:modelData.truncated
                                                    text:"Partial selection"
                                                    color:root.accent;font.pixelSize:9;font.bold:true
                                                }
                                                Label {
                                                    width:parent.width;wrapMode:Text.Wrap;textFormat:Text.PlainText
                                                    text:modelData.text;color:root.ink;font.pixelSize:10
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            PanelCard {
                                width:parent.width
                                visible:root.inspectedContext && root.inspectedContext.app_context && (root.inspectedContext.app_context.selected_items || []).length>0
                                SectionLabel {text:"Selected files";Layout.fillWidth:true}
                                Column {
                                    Layout.fillWidth:true
                                    spacing:6
                                    Repeater {
                                        model:root.inspectedContext && root.inspectedContext.app_context ? root.inspectedContext.app_context.selected_items || [] : []
                                        delegate: ContextBlock {
                                            required property var modelData
                                            width:parent.width
                                            text:modelData.name+"\n"+(modelData.path || (modelData.path_verified ? "Outside this Drop's file access" : "File path not verified"))
                                        }
                                    }
                                }
                            }
                            Rectangle {
                                id:imageWrap
                                property string source:root.inspectedContext && root.inspectedContext.content && root.inspectedContext.content.image ? root.inspectedContext.content.image.data_url || "" : ""
                                width:parent.width
                                height:source.length>0 ? Math.min(220,contextImage.sourceSize.height*(width/Math.max(1,contextImage.sourceSize.width)))+2 : 0
                                visible:source.length>0
                                radius:10;clip:true
                                color:root.inkTint(0.04)
                                border.width:1;border.color:root.inkTint(0.08)
                                Image {
                                    id:contextImage
                                    anchors.fill:parent;anchors.margins:1
                                    fillMode:Image.PreserveAspectFit
                                    source:imageWrap.source
                                }
                            }
                            PanelCard {
                                width:parent.width
                                ThemedCheck {
                                    id:technicalContext
                                    Layout.fillWidth:true
                                    text:"Show technical details"
                                }
                                ContextBlock {
                                    visible:technicalContext.checked
                                    Layout.fillWidth:true
                                    text:root.contextMetadata(root.inspectedContext)
                                }
                            }
                        }
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
