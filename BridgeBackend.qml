pragma ComponentBehavior: Bound
import QtQuick
import Quickshell.Io

// Only lifecycle and status IPC. Clipboard data never enters the QML engine.
Item {
    id: root
    visible: false
    property string adbPath: ""
    property bool active: true
    property var status: ({ state: "starting", message: "Starting clipboard bridge", devices: [], selected: "", enabled: false })
    readonly property string executable: decodeURIComponent(Qt.resolvedUrl("bin/omaclip").toString().replace(/^file:\/\//, ""))

    function send(value) {
        if (bridge.running) bridge.write(JSON.stringify(value) + "\n")
    }
    function restart() {
        if (bridge.running) bridge.signal(15)
        else retry.restart()
    }
    Process {
        id: bridge
        command: [root.executable, "--adb", root.adbPath]
        stdinEnabled: true
        running: root.active
        stdout: SplitParser {
            onRead: data => {
                try { root.status = JSON.parse(data) }
                catch (_) { root.status = { state: "error", message: "Invalid bridge status; restart the bridge", devices: [] } }
            }
        }
        onExited: {
            if (root.status.state !== "error")
                root.status = { state: "error", message: "Bridge stopped; retrying in 5 seconds", devices: [] }
            if (root.active) retry.restart()
        }
    }
    Timer {
        id: retry
        interval: 5000
        onTriggered: if (root.active) bridge.running = true
    }
    Component.onDestruction: {
        active = false
        if (bridge.running) bridge.signal(15)
    }
}
