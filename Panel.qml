pragma ComponentBehavior: Bound
import QtQuick
import qs.Commons
import qs.Ui as Ui

// Omarchy panel composition adapted from Android Mirror (Ayan De, MIT).
// This plugin has no dependency on Android Mirror or its backend.
Ui.Panel {
    id: root
    moduleName: "local.omaclip.clipboard-sync"
    ipcTarget: moduleName
    readonly property color foreground: bar ? bar.foreground : Color.foreground
    readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
    readonly property var backend: bar && bar.shell ? bar.shell.serviceFor(moduleName) : null
    readonly property var bridgeStatus: backend ? backend.status : ({state: "unavailable", message: "Clipboard service unavailable. Use the built-in Omarchy bar and check that the plugin is enabled", devices: []})
    implicitWidth: icon.implicitWidth
    implicitHeight: icon.implicitHeight

    function send(value) { if (backend) backend.send(value) }
    Ui.BarIconButton {
        id: icon
        anchors.fill: parent
        bar: root.bar
        text: "󰅍"
        active: root.bridgeStatus.state === "syncing"
        useActiveColor: false
        iconComponent: Item {
            Ui.OpticalGlyph {
                anchors.fill: parent
                text: icon.text
                fontFamily: icon.fontFamily
                fontSize: icon.fontSize
                color: icon.foreground
            }
            Rectangle {
                visible: icon.active
                width: Math.round(icon.opticalSize * 0.65)
                height: width
                radius: width / 2
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.rightMargin: -width * 0.3
                anchors.topMargin: -height * 0.2
                color: root.bar ? root.bar.background : Color.bar.background
                Text {
                    anchors.centerIn: parent
                    text: "↻"
                    textFormat: Text.PlainText
                    font.family: icon.fontFamily
                    font.pixelSize: parent.width
                    font.bold: true
                    color: icon.foreground
                }
            }
        }
        tooltipText: "OmaClip · " + (root.bridgeStatus.message || root.bridgeStatus.state)
        onPressed: button => {
            if (button === Qt.RightButton && root.bridgeStatus.selected)
                root.send({ op: "enable", enabled: !root.bridgeStatus.enabled })
            else root.toggle()
        }
    }
    Ui.KeyboardPanel {
        id: panel
        owner: root
        bar: root.bar
        anchorItem: icon
        open: root.opened
        focusTarget: content
        contentWidth: panel.fittedContentWidth(Style.space(340))
        contentHeight: panel.fittedContentHeight(content.implicitHeight)
        ClipboardView {
            id: content
            anchors.fill: parent
            status: root.bridgeStatus
            foreground: root.foreground
            fontFamily: root.fontFamily
            onCommand: value => root.send(value)
            onRestartRequested: if (root.backend) root.backend.restart()
            onCloseRequested: root.close()
            onActiveFocusChanged: if (activeFocus) Qt.callLater(focusFirst)
        }
    }

    onOpenedChanged: {
        if (opened) {
            content.adding = false
            content.advanced = false
            root.send({ op: "refresh" })
        }
        else if (root.bridgeStatus.qrActive) root.send({ op: "qr_cancel" })
    }
}
