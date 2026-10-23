import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui

BarWidget {
    id: root
    moduleName: "local.drops"
    property string statusText: "Drops · click to open"
    property string activity: "offline"
    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight
    Process {
        id: status
        command: ["buddyctl", "status", "--waybar"]
        running: true
        stdout: StdioCollector {
            onStreamFinished: {
                try {const state=JSON.parse(text); root.statusText=state.class==="offline" ? "Drops · click to create a companion" : state.tooltip;root.activity=state.class}
                catch(e) {root.activity="offline"}
            }
        }
    }
    Timer { interval:2000;repeat:true;running:true;onTriggered:if(!status.running)status.running=true }
    WidgetButton {
        id: button
        anchors.fill: parent
        bar:root.bar
        text:"●"; labelVisible:false;fixedWidth:28
        tooltipText:root.statusText
        onPressed: function(button) {if(root.bar && button===Qt.LeftButton)root.bar.run("drops toggle")}
        Rectangle {
            anchors.centerIn:parent;width:12;height:14;radius:6
            color: root.activity==="waiting" ? "#eed29b" : root.activity==="error" ? "#f4a99a" : "#a7d8f5"
            opacity:root.activity==="offline" ? 0.55 : 1
            Row {anchors.centerIn:parent;spacing:3
                Rectangle{width:2;height:3;radius:1;color:"#27322a"}
                Rectangle{width:2;height:3;radius:1;color:"#27322a"}
            }
        }
    }
}
