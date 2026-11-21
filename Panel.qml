import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

// Omessage chat panel. Talks to the omessaged helper (installed by
// install.sh as a systemd --user service): `omessaged watch` streams JSON
// snapshots, and actions go to `omessaged rpc` as one JSON line on stdin, so
// message text and contacts never appear in any process's arguments.
Panel {
  id: root
  moduleName: "io.github.omarchy-omessage.omessage"
  manageIpc: false

  property var anchorItem: null
  property var hostWidget: null
  readonly property var barIdentity: hostWidget || root

  // ---- helper wiring
  readonly property string dataHome: {
    var x = Quickshell.env("XDG_DATA_HOME")
    return x && x.length > 0 ? x : Quickshell.env("HOME") + "/.local/share"
  }
  readonly property string helperBin: dataHome + "/omessage/bin/omessaged"
  readonly property string pluginDir: decodeURIComponent(String(Qt.resolvedUrl(".")).replace(/^file:\/\//, "").replace(/\/$/, ""))

  property bool installed: true
  property bool online: false
  property var snap: null
  property string myNpub: ""
  readonly property var conversations: snap && snap.conversations ? snap.conversations : []
  readonly property int unread: snap ? (snap.unread || 0) : 0
  readonly property int relaysConnected: snap ? (snap.connected || 0) : 0
  readonly property int relaysTotal: snap && snap.relays ? snap.relays.length : 0

  // ---- view state
  property string selectedPeer: ""
  property string mode: "chat"          // chat | new
  property string statusText: ""
  property bool statusError: false
  property bool confirmBackup: false
  readonly property var selected: Model.findConversation(conversations, selectedPeer)
  // Message requests (senders not in your contacts) are listed apart, under
  // a "Requests" header, and can only be accepted, blocked or deleted.
  readonly property var contacts: conversations.filter(function(c) { return !c.request })
  readonly property var requests: conversations.filter(function(c) { return c.request })
  readonly property var navigable: contacts.concat(requests)
  readonly property var sidebarItems: requests.length === 0 ? contacts
    : contacts.concat([{ header: true, peer: "", label: "Requests · " + requests.length }]).concat(requests)
  readonly property int requestCount: snap ? (snap.requests || 0) : 0
  readonly property bool selectedIsRequest: !!selected && selected.request === true
  readonly property int selectedIndex: Model.indexOfPeer(sidebarItems, selectedPeer)

  readonly property color fg: Commons.Color.popups.text
  readonly property color accent: Commons.Color.accent
  readonly property color dim: Qt.rgba(fg.r, fg.g, fg.b, 0.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  function open() {
    root.controller.show()
    root.markSelectedRead()
    Qt.callLater(root.focusDefault)
  }
  function openFromHotkey() { open() }
  function close() {
    root.confirmBackup = false
    root.controller.hide()
  }
  function switchPanel(direction) {
    if (root.bar && typeof root.bar.switchPanelFrom === "function")
      return root.bar.switchPanelFrom(root.barIdentity, direction)
    return false
  }

  function focusDefault() {
    var c = panel.loadedContent
    if (!c) return
    if (root.mode === "new") c.newField.forceActiveFocus()
    else if (root.selected && !root.selectedIsRequest) c.composeField.forceActiveFocus()
    else c.forceActiveFocus()
  }

  function flash(text, isError) {
    root.statusText = text
    root.statusError = !!isError
    statusTimer.restart()
  }

  // ---- snapshot stream
  function ingest(line) {
    var msg
    try { msg = JSON.parse(line) } catch (e) { return }
    if (msg.type === "update") msg = Model.mergeUpdate(root.snap, msg)
    if (msg.type === "snapshot") {
      root.snap = msg
      root.online = true
      root.installed = true
      root.myNpub = msg.me ? msg.me.npub : root.myNpub
      var still = Model.findConversation(msg.conversations, root.selectedPeer)
      if (!still && root.mode !== "new") {
        var first = msg.conversations.filter(function(c) { return !c.request })[0] || msg.conversations[0]
        root.selectedPeer = first ? first.peer : ""
      }
      if (root.opened) root.markSelectedRead()
    } else if (msg.type === "offline") {
      root.online = false
      if (msg.npub) root.myNpub = msg.npub
    }
  }

  Process {
    id: watchProc
    command: [root.helperBin, "watch"]
    running: false
    stdout: SplitParser {
      onRead: function(data) { root.ingest(data) }
    }
    onExited: function(code, status) {
      root.online = false
      restartTimer.start()
    }
  }

  Process {
    id: probeProc
    command: ["test", "-x", root.helperBin]
    onExited: function(code, status) {
      root.installed = code === 0
      if (root.installed) watchProc.running = true
      else restartTimer.start()
    }
  }

  Timer {
    id: restartTimer
    interval: 4000
    onTriggered: probeProc.running = true
  }

  Timer {
    id: statusTimer
    interval: 6000
    onTriggered: { root.statusText = ""; root.confirmBackup = false }
  }

  Component.onCompleted: probeProc.running = true

  // ---- one-shot actions, run one at a time
  property var queue: []

  // `req` is a request object for the service ({cmd: "send", ...}); it is
  // written to `omessaged rpc` on stdin. `args` runs a local helper command
  // whose arguments carry nothing private (export-key).
  function call(req, done) { enqueue({ req: req, args: [root.helperBin, "rpc"], done: done || null }) }
  function runLocal(args, done) { enqueue({ req: null, args: [root.helperBin].concat(args), done: done || null }) }

  function enqueue(job) {
    if (!root.installed) { flash("Omessage helper is not installed", true); return }
    var q = root.queue.slice()
    q.push(job)
    root.queue = q
    if (!actionProc.running) runNext()
  }

  function runNext() {
    if (root.queue.length === 0) return
    var q = root.queue.slice()
    actionProc.current = q.shift()
    root.queue = q
    actionProc.command = actionProc.current.args
    actionProc.running = true
  }

  Process {
    id: actionProc
    property var current: null
    stdinEnabled: true
    onStarted: {
      // One line of JSON; the helper reads up to the newline.
      if (actionProc.current && actionProc.current.req)
        actionProc.write(JSON.stringify(actionProc.current.req) + "\n")
    }
    stdout: StdioCollector {
      id: actionOut
      waitForEnd: true
    }
    onExited: function(code, status) {
      var result = {}
      try { result = JSON.parse(String(actionOut.text || "").trim().split("\n").pop()) } catch (e) { result = {} }
      if (result.ok === false && result.error) root.flash(result.error, true)
      var job = actionProc.current
      actionProc.current = null
      if (job && job.done) job.done(result, code)
      Qt.callLater(root.runNext)
    }
  }

  // ---- actions
  function select(peer) {
    root.mode = "chat"
    root.selectedPeer = peer
    root.markSelectedRead()
    Qt.callLater(root.focusDefault)
  }

  function moveSelection(delta) {
    var list = root.navigable
    if (list.length === 0) return
    var cur = Model.indexOfPeer(list, root.selectedPeer)
    var i = cur < 0 ? 0 : cur + delta
    i = Math.max(0, Math.min(list.length - 1, i))
    root.select(list[i].peer)
  }

  function markSelectedRead() {
    if (root.selected && root.selected.unread > 0) call({ cmd: "read", who: root.selected.peer })
  }

  function markAllRead() {
    for (var i = 0; i < root.conversations.length; i++)
      if (root.conversations[i].unread > 0) call({ cmd: "read", who: root.conversations[i].peer })
  }

  function send(text) {
    var body = String(text || "")
    if (body.trim() === "" || !root.selected) return false
    var peer = root.selected.peer
    call({ cmd: "send", to: peer, text: body }, function(result) {
      if (result.status === "failed") root.flash("Not delivered: no relay accepted the message", true)
    })
    return true
  }

  function acceptRequest(peer) {
    call({ cmd: "accept", who: peer }, function(result) {
      if (result.ok) root.flash("Accepted. You can reply now.", false)
    })
  }

  function blockSender(peer) {
    call({ cmd: "block", who: peer }, function(result) {
      if (result.ok) root.flash("Blocked. Their messages will be dropped (unblock: omessaged unblock <npub>).", false)
    })
  }

  function deleteConversation(peer) {
    call({ cmd: "delete", who: peer })
  }

  function retry(id) {
    call({ cmd: "retry", id: id }, function(result) {
      if (result.status === "failed") root.flash("Still not delivered: no relay accepted the message", true)
    })
  }

  function newChat() {
    root.mode = "new"
    if (!root.opened) root.open()
    Qt.callLater(root.focusDefault)
  }

  function startChat(who, name) {
    var target = String(who || "").trim()
    if (target === "") return
    root.flash("Looking up " + target + "…", false)
    var req = { cmd: "add", who: target }
    if (String(name || "").trim() !== "") req.name = String(name).trim()
    call(req, function(result) {
      if (result.ok) {
        root.flash("", false)
        root.select(result.npub)
        var c = panel.loadedContent
        if (c) { c.newField.text = ""; c.nameField.text = "" }
      }
    })
  }

  function copyNpub() {
    if (!root.myNpub) return
    Quickshell.execDetached(["wl-copy", root.myNpub])
    flash("Your npub is on the clipboard. Share it so people can message you.", false)
  }

  function backupKey() {
    if (!root.confirmBackup) {
      root.confirmBackup = true
      flash("Click Back up key again to save your secret key to a private file in your home folder.", false)
      return
    }
    root.confirmBackup = false
    runLocal(["export-key"], function(result) {
      if (result.ok) flash("Secret key saved to " + result.path + " (mode 600). Anyone with it can read your messages.", false)
    })
  }

  // Single-quote a string for a POSIX shell.
  function shellQuote(s) { return "'" + String(s).replace(/'/g, "'\\''") + "'" }

  function installHelper() {
    Quickshell.execDetached(["omarchy-launch-floating-terminal-with-presentation", "bash " + shellQuote(root.pluginDir + "/install.sh")])
    flash("Installing the helper in a terminal…", false)
  }

  // ---- UI
  KeyboardPanel {
    id: panel
    anchorItem: root.anchorItem
    owner: root.barIdentity
    bar: root.bar
    open: root.opened
    focusTarget: panel.loadedContent
    contentWidth: panel.fittedContentWidth(Style.space(720))
    contentHeight: panel.fittedContentHeight(Style.space(480))

    content: PanelKeyCatcher {
      id: keys
      property alias composeField: composeField
      property alias newField: newField
      property alias nameField: nameField
      anchors.fill: parent
      implicitHeight: Style.space(480)
      blocked: composeField.activeFocus || newField.activeFocus || nameField.activeFocus
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }
      onMoveRequested: function(dx, dy) { if (dy !== 0) root.moveSelection(dy) }
      onActivateRequested: root.focusDefault()
      onTextKey: function(t, modifiers) {
        if (t === "n" || t === "N") root.newChat()
        else if (t === "i" || t === "/") root.focusDefault()
        else if (t === "c" || t === "C") root.copyNpub()
      }

      function fieldKeys(event) {
        if (event.key === Qt.Key_Escape) {
          if (root.mode === "new") root.mode = "chat"
          keys.forceActiveFocus()
          event.accepted = true
        } else if ((event.modifiers & Qt.ControlModifier) && event.key === Qt.Key_N) {
          root.newChat(); event.accepted = true
        } else if ((event.modifiers & Qt.ControlModifier) && (event.key === Qt.Key_J || event.key === Qt.Key_Down)) {
          root.moveSelection(1); event.accepted = true
        } else if ((event.modifiers & Qt.ControlModifier) && (event.key === Qt.Key_K || event.key === Qt.Key_Up)) {
          root.moveSelection(-1); event.accepted = true
        }
      }

      Column {
        anchors.fill: parent
        spacing: Style.space(8)

        // Header
        Item {
          id: header
          width: parent.width
          height: Math.max(title.implicitHeight, npubButton.implicitHeight)

          Text {
            id: title
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
            text: "󰍡  Omessage"
            color: root.fg
            font.family: root.fontFamily
            font.pixelSize: Style.font.heading
            font.bold: true
          }

          Row {
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.space(10)

            Text {
              anchors.verticalCenter: parent.verticalCenter
              text: !root.installed ? "helper not installed"
                : (!root.online ? "● offline" : "● " + root.relaysConnected + "/" + root.relaysTotal + " relays")
              color: root.online && root.relaysConnected > 0 ? root.accent : Commons.Color.urgent
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
            }

            Button {
              id: npubButton
              visible: root.myNpub !== ""
              text: Model.shortNpub(root.myNpub)
              iconText: "󰆏"
              fontFamily: root.fontFamily
              tooltipText: "Copy your npub (c)"
              onClicked: root.copyNpub()
            }
          }
        }

        Rectangle { width: parent.width; height: 1; color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.15) }

        // Body
        Row {
          id: body
          width: parent.width
          height: parent.height - header.height - footer.height - Style.space(8) * 4 - 2
          spacing: Style.space(10)

          // Conversation list
          Column {
            id: sidebar
            width: Style.space(220)
            height: parent.height
            spacing: Style.space(6)

            Button {
              id: newButton
              width: parent.width
              text: "New chat"
              iconText: "󰐕"
              leftAlign: true
              selected: root.mode === "new"
              fontFamily: root.fontFamily
              tooltipText: "Start a chat by npub or NIP-05 (n, Ctrl+N)"
              onClicked: root.newChat()
            }

            ListView {
              id: convList
              width: parent.width
              height: parent.height - newButton.height - parent.spacing
              clip: true
              model: root.sidebarItems
              spacing: Style.space(2)
              boundsBehavior: Flickable.StopAtBounds
              currentIndex: root.selectedIndex

              delegate: Rectangle {
                required property var modelData
                required property int index
                width: convList.width
                readonly property bool isHeader: modelData.header === true
                height: isHeader ? headerText.implicitHeight + Style.space(14) : convCol.implicitHeight + Style.space(10)
                radius: Style.cornerRadius
                readonly property bool isSel: !isHeader && root.mode === "chat" && modelData.peer === root.selectedPeer
                color: isSel ? Qt.rgba(root.accent.r, root.accent.g, root.accent.b, 0.18)
                  : (!isHeader && convMouse.containsMouse ? Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.06) : "transparent")

                Text {
                  id: headerText
                  visible: parent.isHeader
                  anchors.left: parent.left
                  anchors.leftMargin: Style.space(8)
                  anchors.bottom: parent.bottom
                  anchors.bottomMargin: Style.space(4)
                  text: modelData.label || ""
                  textFormat: Text.PlainText
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  font.bold: true
                }

                Column {
                  id: convCol
                  visible: !parent.isHeader
                  anchors.left: parent.left
                  anchors.right: badge.left
                  anchors.leftMargin: Style.space(8)
                  anchors.rightMargin: Style.space(6)
                  anchors.verticalCenter: parent.verticalCenter
                  spacing: Style.space(2)

                  Text {
                    width: parent.width
                    text: modelData.name
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: root.fg
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.body
                    font.bold: modelData.unread > 0
                  }
                  Text {
                    width: parent.width
                    text: modelData.last ? (modelData.last.out ? "You: " : "") + Model.oneLine(modelData.last.text) : "No messages yet"
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    color: root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                  }
                }

                Rectangle {
                  id: badge
                  anchors.right: parent.right
                  anchors.rightMargin: Style.space(8)
                  anchors.verticalCenter: parent.verticalCenter
                  visible: !parent.isHeader && modelData.unread > 0
                  width: visible ? Math.max(height, badgeText.implicitWidth + Style.space(10)) : 0
                  height: badgeText.implicitHeight + Style.space(4)
                  radius: height / 2
                  color: root.accent
                  Text {
                    id: badgeText
                    anchors.centerIn: parent
                    text: modelData.unread
                    color: Commons.Color.background
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                    font.bold: true
                  }
                }

                MouseArea {
                  id: convMouse
                  anchors.fill: parent
                  enabled: !parent.isHeader
                  hoverEnabled: true
                  cursorShape: Qt.PointingHandCursor
                  onClicked: root.select(modelData.peer)
                }
              }

              Text {
                anchors.centerIn: parent
                width: parent.width - Style.space(20)
                visible: convList.count === 0
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.WordWrap
                text: root.online ? "No conversations yet.\nPress n to start one." : ""
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
              }
            }
          }

          Rectangle { width: 1; height: parent.height; color: Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.15) }

          // Main area
          Item {
            id: main
            width: parent.width - sidebar.width - 1 - parent.spacing * 2
            height: parent.height

            // Helper missing or offline
            Column {
              anchors.centerIn: parent
              width: parent.width - Style.space(40)
              spacing: Style.space(10)
              visible: !root.online && root.mode === "chat"

              Text {
                width: parent.width
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.WordWrap
                text: !root.installed
                  ? "The Omessage helper isn't installed yet. It keeps your key, talks to Nostr relays and stores messages on this device."
                  : "Connecting to the Omessage helper… If this persists, run: systemctl --user status omessaged"
                color: root.fg
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
              }
              Button {
                anchors.horizontalCenter: parent.horizontalCenter
                visible: !root.installed
                text: "Install helper"
                iconText: "󰏗"
                bordered: true
                fontFamily: root.fontFamily
                onClicked: root.installHelper()
              }
            }

            // New chat form
            Column {
              anchors.left: parent.left
              anchors.right: parent.right
              anchors.top: parent.top
              anchors.topMargin: Style.space(20)
              spacing: Style.space(10)
              visible: root.mode === "new"

              Text {
                text: "New chat"
                color: root.fg
                font.family: root.fontFamily
                font.pixelSize: Style.font.title
                font.bold: true
              }
              Text {
                width: parent.width
                wrapMode: Text.WordWrap
                text: "Paste their npub (or a NIP-05 address like name@example.com). Messages are end-to-end encrypted with NIP-17 and travel through Nostr relays."
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
              }
              TextField {
                id: newField
                width: parent.width
                placeholderText: "npub1… or name@domain"
                foreground: root.fg
                font.family: root.fontFamily
                Keys.onPressed: function(event) { keys.fieldKeys(event) }
                onAccepted: root.startChat(newField.text, nameField.text)
              }
              TextField {
                id: nameField
                width: parent.width
                placeholderText: "Name (optional)"
                foreground: root.fg
                font.family: root.fontFamily
                Keys.onPressed: function(event) { keys.fieldKeys(event) }
                onAccepted: root.startChat(newField.text, nameField.text)
              }
              Row {
                spacing: Style.space(8)
                Button {
                  text: "Start chat"
                  iconText: "󰍩"
                  bordered: true
                  fontFamily: root.fontFamily
                  onClicked: root.startChat(newField.text, nameField.text)
                }
                Button {
                  text: "Cancel"
                  fontFamily: root.fontFamily
                  onClicked: { root.mode = "chat"; keys.forceActiveFocus() }
                }
              }
            }

            // Thread
            Item {
              anchors.fill: parent
              visible: root.online && root.mode === "chat"

              Text {
                anchors.centerIn: parent
                visible: !root.selected
                text: "Select a conversation or press n"
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
              }

              Text {
                id: threadTitle
                visible: !!root.selected
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: parent.top
                text: root.selected ? root.selected.name + "   " + Model.shortNpub(root.selected.peer) : ""
                textFormat: Text.PlainText
                elide: Text.ElideRight
                color: root.fg
                font.family: root.fontFamily
                font.pixelSize: Style.font.subtitle
                font.bold: true
              }

              ListView {
                id: thread
                visible: !!root.selected
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.top: threadTitle.bottom
                anchors.topMargin: Style.space(8)
                anchors.bottom: root.selectedIsRequest ? requestBar.top : composeField.top
                anchors.bottomMargin: Style.space(8)
                clip: true
                spacing: Style.space(6)
                boundsBehavior: Flickable.StopAtBounds
                model: root.selected ? root.selected.messages : []
                onCountChanged: Qt.callLater(thread.positionViewAtEnd)
                onVisibleChanged: if (visible) Qt.callLater(thread.positionViewAtEnd)

                delegate: Item {
                  required property var modelData
                  width: thread.width
                  height: bubble.height + stamp.implicitHeight + Style.space(2)

                  Rectangle {
                    id: bubble
                    anchors.right: modelData.out ? parent.right : undefined
                    anchors.left: modelData.out ? undefined : parent.left
                    width: Math.min(thread.width * 0.78, msgText.implicitWidth + Style.space(20))
                    height: msgText.implicitHeight + Style.space(12)
                    radius: Math.max(Style.cornerRadius, Style.space(8))
                    color: modelData.out ? Qt.rgba(root.accent.r, root.accent.g, root.accent.b, 0.85)
                                         : Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.10)
                    Text {
                      id: msgText
                      x: Style.space(10)
                      y: Style.space(6)
                      width: Math.min(implicitWidth, thread.width * 0.78 - Style.space(20))
                      text: modelData.text
                      textFormat: Text.PlainText
                      wrapMode: Text.Wrap
                      color: modelData.out ? Commons.Color.background : root.fg
                      font.family: root.fontFamily
                      font.pixelSize: Style.font.body
                    }
                  }
                  Text {
                    id: stamp
                    anchors.top: bubble.bottom
                    anchors.right: modelData.out ? parent.right : undefined
                    anchors.left: modelData.out ? undefined : parent.left
                    text: Model.stamp(modelData.ts) + (modelData.out ? "  " + Model.statusLabel(modelData.status) : "")
                    color: modelData.status === "failed" ? Commons.Color.urgent : root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                    font.underline: retryMouse.enabled && retryMouse.containsMouse

                    MouseArea {
                      id: retryMouse
                      anchors.fill: parent
                      enabled: modelData.out && modelData.status === "failed"
                      hoverEnabled: true
                      cursorShape: Qt.PointingHandCursor
                      onClicked: root.retry(modelData.id)
                    }
                  }
                }
              }

              // Shown instead of the compose field for a message request.
              Column {
                id: requestBar
                visible: root.selectedIsRequest
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                spacing: Style.space(6)

                Text {
                  width: parent.width
                  wrapMode: Text.WordWrap
                  text: "Message request from someone who isn't in your contacts. Their profile name stays hidden until you accept."
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.bodySmall
                }
                Row {
                  spacing: Style.space(8)
                  Button {
                    text: "Accept"
                    iconText: "󰄬"
                    bordered: true
                    fontFamily: root.fontFamily
                    tooltipText: "Move to your conversations and reply"
                    onClicked: if (root.selected) root.acceptRequest(root.selected.peer)
                  }
                  Button {
                    text: "Block"
                    iconText: "󰒃"
                    fontFamily: root.fontFamily
                    tooltipText: "Delete and silently drop their future messages"
                    onClicked: if (root.selected) root.blockSender(root.selected.peer)
                  }
                  Button {
                    text: "Delete"
                    iconText: "󰆴"
                    fontFamily: root.fontFamily
                    tooltipText: "Delete this request (they can write again)"
                    onClicked: if (root.selected) root.deleteConversation(root.selected.peer)
                  }
                }
              }

              TextField {
                id: composeField
                visible: !!root.selected && !root.selectedIsRequest
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.bottom: parent.bottom
                placeholderText: "Message " + (root.selected ? root.selected.name : "") + "  (Enter to send)"
                foreground: root.fg
                font.family: root.fontFamily
                Keys.onPressed: function(event) { keys.fieldKeys(event) }
                onAccepted: if (root.send(composeField.text)) composeField.text = ""
              }
            }
          }
        }

        // Footer
        Item {
          id: footer
          width: parent.width
          height: Math.max(statusLine.implicitHeight, backupButton.implicitHeight)

          Text {
            id: statusLine
            anchors.left: parent.left
            anchors.right: backupButton.left
            anchors.rightMargin: Style.space(10)
            anchors.verticalCenter: parent.verticalCenter
            text: root.statusText !== "" ? root.statusText
              : "n new · ↑↓ switch · i type · Esc close · c copy npub"
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: root.statusError ? Commons.Color.urgent : root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }

          Button {
            id: backupButton
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            visible: root.myNpub !== ""
            text: root.confirmBackup ? "Confirm backup" : "Back up key"
            iconText: "󰌋"
            active: root.confirmBackup
            fontFamily: root.fontFamily
            tooltipText: "Save your secret key (nsec) to a private file"
            onClicked: root.backupKey()
          }
        }
      }
    }
  }
}
