.pragma library

function findConversation(list, peer) {
  if (!list || !peer) return null
  for (var i = 0; i < list.length; i++) if (list[i].peer === peer) return list[i]
  return null
}

function indexOfPeer(list, peer) {
  if (!list || !peer) return -1
  for (var i = 0; i < list.length; i++) if (list[i].peer === peer) return i
  return -1
}

function shortNpub(npub) {
  var s = String(npub || "")
  return s.length > 16 ? s.slice(0, 10) + "…" + s.slice(-4) : s
}

function oneLine(text) {
  return String(text || "").replace(/\s+/g, " ").trim()
}

function stamp(ts) {
  if (!ts) return ""
  var d = new Date(ts * 1000)
  var now = new Date()
  var sameDay = d.toDateString() === now.toDateString()
  return sameDay ? Qt.formatTime(d, "HH:mm") : Qt.formatDateTime(d, "d MMM HH:mm")
}

function statusLabel(status) {
  if (status === "sending") return "sending…"
  if (status === "failed") return "not delivered · retry"
  return "sent"
}

// Apply an incremental "update" from omessaged to the last snapshot: replace
// the conversations it carries, then order (and drop) by `update.order`.
function mergeUpdate(snap, update) {
  var byPeer = {}
  var old = snap && snap.conversations ? snap.conversations : []
  for (var i = 0; i < old.length; i++) byPeer[old[i].peer] = old[i]
  var changed = update.conversations || []
  for (var j = 0; j < changed.length; j++) byPeer[changed[j].peer] = changed[j]
  var order = update.order || []
  var list = []
  for (var k = 0; k < order.length; k++) if (byPeer[order[k]]) list.push(byPeer[order[k]])
  var out = {}
  for (var key in update) out[key] = update[key]
  out.type = "snapshot"
  out.conversations = list
  delete out.order
  return out
}
