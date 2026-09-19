pragma ComponentBehavior: Bound
import QtQuick
import Quickshell.Io

// Loaded once by Omarchy, independent of the number of monitors/panels.
BridgeBackend {
    id: root
    property var shell: null
    property var manifest: null
    active: shell !== null
    adbPath: {
        const layout = shell && shell.barConfig ? shell.barConfig.layout : null
        if (layout) {
            for (const section of ["left", "center", "right"]) {
                for (const entry of (layout[section] || [])) {
                    if (entry.id === "local.omaclip.clipboard-sync") return String(entry.adbPath || "")
                }
            }
        }
        return ""
    }
    IpcHandler {
        target: "local.omaclip.clipboard-sync.bridge"
        function pause(): void { root.send({ op: "enable", enabled: false }) }
        function resume(): void { root.send({ op: "enable", enabled: true }) }
        function disconnect(): void { root.send({ op: "disconnect" }) }
        function restart(): void { root.restart() }
        function pairQr(): void { root.send({ op: "qr_start" }) }
        function cancelPairing(): void { root.send({ op: "qr_cancel" }) }
        function status(): string {
            let summary = Object.assign({}, root.status)
            delete summary.qrImage // Pairing credential must not enter diagnostic output.
            return JSON.stringify(summary)
        }
    }
}
