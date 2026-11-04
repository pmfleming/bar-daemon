import QtQuick
import QtQuick.Window

// The private Rust harness drives login/exit through a file. No wallet data or
// desktop-session input is involved; both windows belong to this Qt process.
QtObject {
    id: root
    property var current: null
    property int sequence: 0
    property string lastCommand: ""
    property string commandPath: Qt.application.arguments[Qt.application.arguments.length - 1]
    property Component factory: Component {
        Window { visible: true; width: 240; height: 160; title: "Handoff fixture " + root.sequence }
    }
    Component.onCompleted: {
        console.log("handoff control:", commandPath)
        current = factory.createObject(null)
    }
    property Timer successor: Timer {
        onTriggered: {
            root.sequence += 1
            root.current = root.factory.createObject(null)
        }
    }
    property Timer commands: Timer {
        interval: 40
        repeat: true
        running: true
        onTriggered: {
            const xhr = new XMLHttpRequest()
            xhr.open("GET", "file://" + root.commandPath, false)
            xhr.send()
            const command = xhr.responseText.trim()
            if (!command || command === root.lastCommand) return
            console.log("handoff command:", command)
            root.lastCommand = command
            if (command === "exit") { Qt.quit(); return }
            if (command.startsWith("replace:")) {
                root.current.destroy()
                root.current = null
                root.successor.interval = Number(command.split(":")[1])
                root.successor.start()
            }
        }
    }
}
