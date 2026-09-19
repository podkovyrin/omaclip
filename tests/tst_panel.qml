import QtQuick
import QtTest
import ".." as Plugin
import QtQuick.Window

Window {
    visible: true
    width: 520
    height: 700
    Plugin.ClipboardView {
        id: view
        width: 340
        height: Math.min(600, implicitHeight)
    }
    Timer {
        interval: 500; running: true
        onTriggered: {
            try {
                for (let test of ["test_keyboard", "test_snapshot_focus", "test_pairing_back", "test_compact_and_narrow"]) {
                    checks.init()
                    checks[test]()
                    console.log("PASS", test)
                }
            } catch (e) { console.error("FAIL", e, e.stack) }
            Qt.quit()
        }
    }
    SignalSpy { id: commands; target: view; signalName: "command" }
    SignalSpy { id: closed; target: view; signalName: "closeRequested" }
    TestCase {
        id: checks
        name: "ClipboardPanel"
        when: windowShown
        function init() {
            view.width = 340
            view.adding = false
            view.advanced = false
            view.status = {state: "syncing", selected: "phone", enabled: true, devices: [
                {serial: "phone", model: "Pixel 10 Pro XL", transport: "Wi-Fi", state: "device"}
            ]}
            commands.clear()
            closed.clear()
            wait(30)
            view.focusFirst()
        }
        function focusedAction() { return view.targets().find(item => item.activeFocus) }
        function test_keyboard() {
            compare(focusedAction().label, "Pause sync")
            keyClick(Qt.Key_Return)
            compare(commands.signalArguments[0][0].op, "enable")
            compare(commands.signalArguments[0][0].enabled, false)
            keyClick(Qt.Key_Down)
            verify(focusedAction().label.indexOf("Pixel") >= 0)
            keyClick(Qt.Key_Return)
            compare(commands.signalArguments[1][0].serial, "phone")
            wait(20)
            keyClick(Qt.Key_K)
            compare(focusedAction().label, "More")
            keyClick(Qt.Key_Tab)
            compare(focusedAction().label, "Pause sync")
            keyClick(Qt.Key_Tab, Qt.ShiftModifier)
            compare(focusedAction().label, "More")
            keyClick(Qt.Key_Return)
            verify(view.advanced)
            keyClick(Qt.Key_Tab)
            compare(focusedAction().placeholderText, "IP address:port")
            keyClick(Qt.Key_J)
            compare(focusedAction().text, "j")
            keyClick(Qt.Key_Escape)
            verify(!view.advanced)
            compare(focusedAction().label, "More")
            keyClick(Qt.Key_Escape)
            compare(closed.count, 1)
        }
        function test_snapshot_focus() {
            keyClick(Qt.Key_Down)
            compare(focusedAction().deviceSerial, "phone")
            view.status = {state: "syncing", selected: "phone", enabled: true, devices: [
                {serial: "new", model: "Another phone", transport: "USB", state: "device"},
                {serial: "phone", model: "Pixel", transport: "Wi-Fi", state: "device"}
            ]}
            wait(30)
            compare(focusedAction().deviceSerial, "phone")
        }
        function test_pairing_back() {
            view.adding = true
            view.status = {selected: "phone", qrActive: true, devices: []}
            keyClick(Qt.Key_Escape)
            verify(!view.adding)
            compare(commands.signalArguments[0][0].op, "qr_cancel")
        }
        function test_compact_and_narrow() {
            verify(view.implicitHeight < 300)
            grabImage(view).save("/tmp/omaclip-ui-check/compact.png")
            view.width = 220
            view.advanced = true
            view.adding = true
            view.status = {state: "waiting", message: "A very long status message that must wrap within the available space", devices: [
                {serial: "adb-57301FDCQ007X1-QnYeb7._adb-tls-connect._tcp", model: "A very very very very very long phone name", transport: "Wi-Fi", state: "device"},
                {serial: "usb", model: "Other phone", state: "unauthorized"}
            ]}
            wait(50)
            for (let item of view.targets()) {
                verify(item.width <= view.availableWidth)
                verify(item.width > 0)
            }
            view.focusFirst()
            for (let i = 0; i < view.targets().length; i++) keyClick(Qt.Key_Tab)
            verify(focusedAction() !== undefined)
            grabImage(view).save("/tmp/omaclip-ui-check/narrow.png")
        }
    }
}
