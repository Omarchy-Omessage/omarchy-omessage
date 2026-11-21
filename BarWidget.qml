import QtQuick
import Quickshell
import qs.Commons
import qs.Ui

// Omessage bar widget: a chat glyph with the unread count. Left click opens
// the chat panel, middle click opens a new chat, right click marks the open
// conversation read. The panel (Panel.qml) owns the helper connection.
BarWidget {
  id: root
  moduleName: "io.github.omarchy-omessage.omessage"

  readonly property int unread: panelLoader.item ? panelLoader.item.unread : 0
  readonly property bool online: panelLoader.item ? panelLoader.item.online : false
  readonly property int requests: panelLoader.item ? panelLoader.item.requestCount : 0

  readonly property bool opened: panelLoader.item ? panelLoader.item.opened === true : false
  readonly property bool popoutSwitchClosing: panelLoader.item
    ? panelLoader.item.popoutSwitchClosing === true
    : false

  function open() { if (panelLoader.item) panelLoader.item.open() }
  function close() { if (panelLoader.item) panelLoader.item.close() }
  function toggle() { if (panelLoader.item) panelLoader.item.toggle() }
  function closeForPopoutSwitch() { if (panelLoader.item) panelLoader.item.closeForPopoutSwitch() }

  function injectPanel() {
    if (!panelLoader.item) return
    panelLoader.item.bar = root.bar
    panelLoader.item.anchorItem = button
    panelLoader.item.hostWidget = root
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight
  onBarChanged: injectPanel()

  Loader {
    id: panelLoader
    active: true
    source: Qt.resolvedUrl("Panel.qml")
    visible: false
    onLoaded: {
      root.injectPanel()
      Qt.callLater(root.injectPanel)
    }
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.unread > 0 ? "󰍡 " + root.unread : (root.requests > 0 ? "󰍡 •" : "󰍡")
    active: root.unread > 0 || root.requests > 0
    dimmed: !root.online
    tooltipText: !root.online ? "Omessage (helper not running)"
      : "Omessage" + (root.unread > 0 ? ": " + root.unread + " unread" : "")
        + (root.requests > 0 ? (root.unread > 0 ? ", " : ": ") + root.requests
          + (root.requests === 1 ? " message request" : " message requests") : "")
    onPressed: function(buttonCode) {
      if (buttonCode === Qt.LeftButton) root.toggle()
      else if (buttonCode === Qt.MiddleButton && panelLoader.item) panelLoader.item.newChat()
      else if (buttonCode === Qt.RightButton && panelLoader.item) panelLoader.item.markAllRead()
    }
  }
}
