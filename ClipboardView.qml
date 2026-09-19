pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui as Ui

ScrollView {
    id: view
    property var status: ({devices: []})
    property color foreground: Color.foreground
    property string fontFamily: Style.font.family
    property bool adding: false
    property bool advanced: false
    property string focusedDeviceSerial: ""
    onStatusChanged: {
        // Device snapshots replace Repeater delegates. Restore keyboard focus
        // by identity after the new rows exist, even when their order changes.
        if (focusedDeviceSerial) Qt.callLater(function() {
            let row = targets().find(item => item.deviceSerial === focusedDeviceSerial)
            if (row) row.forceActiveFocus(Qt.TabFocusReason)
            else focusFirst()
        })
    }
    signal command(var value)
    signal restartRequested()
    signal closeRequested()
    readonly property string stateLabel: {
        switch (status.state) {
        case "syncing": return "Syncing"
        case "connecting": return "Connecting…"
        case "waiting": return "Waiting for phone"
        case "error": case "unavailable": return "Needs attention"
        case "starting": return "Starting…"
        default: return status.selected ? "Paused" : "Choose a phone"
        }
    }
    implicitHeight: content.implicitHeight
    contentWidth: availableWidth
    clip: true
    ScrollBar.horizontal.policy: ScrollBar.AlwaysOff

    function targets() {
        let items = []
        function visit(item) {
            if (!item.visible || !item.enabled) return
            if (item.activeFocusOnTab) items.push(item)
            else for (let child of item.children) visit(child)
        }
        visit(content)
        return items
    }
    function focusFirst() {
        let items = targets()
        if (items.length) items[0].forceActiveFocus(Qt.TabFocusReason)
    }
    function moveFocus(backwards) {
        let items = targets()
        if (!items.length) return
        let current = items.findIndex(item => item.activeFocus)
        items[(current + (backwards ? -1 : 1) + items.length) % items.length].forceActiveFocus(Qt.TabFocusReason)
    }
    function reveal(item) {
        if (!contentItem || contentItem.contentY === undefined) return
        let y = item.mapToItem(content, 0, 0).y
        let offset = contentItem.contentY
        if (y < offset) offset = y
        else if (y + item.height > offset + availableHeight) offset = y + item.height - availableHeight
        contentItem.contentY = Math.max(0, Math.min(offset, content.height - availableHeight))
    }
    function back() {
        if (advanced) { advanced = false; more.forceActiveFocus() }
        else if (adding) {
            adding = false
            if (status.qrActive) command({op: "qr_cancel"})
            add.forceActiveFocus()
        } else closeRequested()
    }
    Keys.onPressed: event => {
        event.accepted = true
        if (event.key === Qt.Key_Escape) back()
        else if (event.key === Qt.Key_Tab || event.key === Qt.Key_Backtab)
            moveFocus(event.key === Qt.Key_Backtab || !!(event.modifiers & Qt.ShiftModifier))
        else if (event.key === Qt.Key_Down || event.key === Qt.Key_Up)
            moveFocus(event.key === Qt.Key_Up)
        else if (!address.activeFocus && event.modifiers === Qt.NoModifier && (event.key === Qt.Key_J || event.key === Qt.Key_K))
            moveFocus(event.key === Qt.Key_K)
        else event.accepted = false
    }

    component Copy: Text {
        width: parent.width
        textFormat: Text.PlainText
        wrapMode: Text.Wrap
        color: view.foreground
        font.family: view.fontFamily
        font.pixelSize: Style.font.body
    }
    // Ui.Button's built-in label has no width constraint. Use a bounded label
    // so device names and translations cannot push the popup off screen.
    component Action: Ui.Button {
        id: action
        property string label: ""
        property string deviceSerial: ""
        focusable: true
        foreground: view.foreground
        fontFamily: view.fontFamily
        implicitWidth: caption.implicitWidth + horizontalPadding * 2 + 4
        implicitHeight: caption.implicitHeight + verticalPadding * 2 + 4
        opacity: enabled ? 1 : 0.45
        Accessible.name: label
        Accessible.role: Accessible.Button
        Keys.forwardTo: [view]
        onActiveFocusChanged: if (activeFocus) {
            view.focusedDeviceSerial = deviceSerial
            view.reveal(this)
        }
        Text {
            id: caption
            anchors.fill: parent
            anchors.margins: action.horizontalPadding + 2
            verticalAlignment: Text.AlignVCenter
            textFormat: Text.PlainText
            text: action.label
            elide: Text.ElideRight
            color: action.foreground
            font.family: action.fontFamily
            font.pixelSize: Style.font.body
            font.bold: action.selected
        }
    }
    Column {
        id: content
        width: view.availableWidth
        spacing: Style.spacing.sm
        Copy {
            text: "OmaClip"
            font.pixelSize: Style.font.heading
            font.bold: true
        }
        Copy { text: view.stateLabel }
        Action {
            id: sync
            visible: !!view.status.selected
            width: parent.width
            label: view.status.enabled ? "Pause sync" : "Resume sync"
            bordered: true
            onClicked: view.command({op: "enable", enabled: !view.status.enabled})
        }
        Repeater {
            model: view.status.devices || []
            delegate: Action {
                required property var modelData
                deviceSerial: modelData.serial
                width: content.width
                selected: view.status.selected === modelData.serial
                label: (selected ? "✓  " : "") + (modelData.model || "Android phone") + " · " + (modelData.state === "unauthorized" ? "Allow access on phone" : modelData.state === "device" ? modelData.transport : "Offline")
                tooltipText: modelData.serial
                enabled: modelData.state === "device"
                onClicked: {
                    view.command({op: "select", serial: modelData.serial})
                    Qt.callLater(view.focusFirst)
                }
            }
        }
        Copy {
            visible: !(view.status.devices || []).length
            text: "Connect your phone by USB or add it over Wi-Fi."
        }
        Copy {
            visible: view.status.state === "error" || view.status.state === "unavailable" || view.status.state === "waiting"
            text: view.status.message || "Open More to retry."
            font.pixelSize: Style.font.caption
        }
        Flow {
            width: parent.width
            spacing: Style.spacing.sm
            Action {
                id: add
                width: Math.min(implicitWidth, parent.width)
                label: view.adding ? "−  Add phone" : "+  Add phone"
                onClicked: {
                    view.adding = !view.adding
                    if (!view.adding && view.status.qrActive) view.command({op: "qr_cancel"})
                }
            }
            Action {
                id: more
                width: Math.min(implicitWidth, parent.width)
                label: view.advanced ? "Less" : "More"
                onClicked: view.advanced = !view.advanced
            }
        }
        Column {
            width: parent.width
            visible: view.adding
            spacing: Style.spacing.sm
            Ui.PanelSeparator { width: parent.width; foreground: view.foreground }
            Copy {
                text: "On the same Wi-Fi, open your phone’s Developer options → Wireless debugging → Pair device with QR code."
                font.pixelSize: Style.font.caption
            }
            Action {
                width: parent.width
                label: view.status.qrActive ? "Cancel pairing" : "Show QR code"
                bordered: true
                onClicked: view.command({op: view.status.qrActive ? "qr_cancel" : "qr_start"})
            }
            Image {
                width: Math.min(parent.width, Style.space(220))
                height: visible ? width : 0
                anchors.horizontalCenter: parent.horizontalCenter
                visible: view.adding && !!view.status.qrActive && source.toString().length > 0
                source: view.status.qrImage || ""
                cache: false
                smooth: false
                fillMode: Image.PreserveAspectFit
            }
        }
        Column {
            width: parent.width
            visible: view.advanced
            spacing: Style.spacing.sm
            Ui.PanelSeparator { width: parent.width; foreground: view.foreground }
            Copy {
                text: "Already paired? Enter the address from Wireless debugging."
                font.pixelSize: Style.font.caption
            }
            Ui.TextField {
                id: address
                width: parent.width
                placeholderText: "IP address:port"
                Accessible.name: "Phone connection address"
                foreground: view.foreground
                Keys.forwardTo: [view]
                onActiveFocusChanged: if (activeFocus) { view.focusedDeviceSerial = ""; view.reveal(this) }
                onAccepted: if (text.trim()) view.command({op: "connect", address: text.trim()})
            }
            Flow {
                width: parent.width
                spacing: Style.spacing.sm
                Action {
                    width: Math.min(implicitWidth, parent.width)
                    label: "Connect"
                    enabled: address.text.trim().length > 0
                    onClicked: view.command({op: "connect", address: address.text.trim()})
                }
                Action {
                    width: Math.min(implicitWidth, parent.width)
                    label: "Refresh"
                    onClicked: view.command({op: "refresh"})
                }
                Action {
                    width: Math.min(implicitWidth, parent.width)
                    label: "Disconnect"
                    enabled: !!view.status.selected
                    onClicked: view.command({op: "disconnect"})
                }
                Action {
                    width: Math.min(implicitWidth, parent.width)
                    label: "Restart service"
                    onClicked: view.restartRequested()
                }
            }
            Copy {
                text: "Only new text copies are shared. Connecting or resuming keeps both clipboards."
                font.pixelSize: Style.font.caption
            }
        }
        Copy {
            visible: text.length > 0
            text: view.status.action || ""
            font.pixelSize: Style.font.caption
        }
        Copy {
            text: "↑↓ / j k  Move · Enter  Select · Esc  Back"
            opacity: 0.6
            font.pixelSize: Style.font.caption
        }
    }
}
