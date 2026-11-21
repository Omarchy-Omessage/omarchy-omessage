# Omessage

An iMessage-style private messenger for [Omarchy](https://omarchy.org). Your
Omarchy install *is* your account: the first time it runs, Omessage creates a
Nostr identity for this device, and you chat with other people by their npub.
Messages are end-to-end encrypted Nostr private DMs
([NIP-17](https://github.com/nostr-protocol/nips/blob/master/17.md): NIP-44
encryption inside NIP-59 gift wraps), so relays can't read them or see who is
talking to whom. Because it is plain Nostr, you can also message people who use
Nostr apps that support NIP-17 (0xchat, Amethyst, and others).

- Bar widget with an unread count (left click: open, middle click: new chat,
  right click: mark all read)
- Chat panel with conversation list, threads, compose, and new chat by npub or
  NIP-05 address (`name@example.com`)
- Desktop notifications for new messages
- Message requests: messages from people who aren't in your contacts wait in
  a separate **Requests** section (see below)
- Keyboard first: `n` new chat · `↑/↓` or `j/k` switch conversation · `i` or
  `Enter` type · `Ctrl+N` new chat while typing · `Ctrl+J/K` switch while typing
  · `c` copy your npub · `Esc` leave the field / close

## How it works

```
 Omarchy shell (QML plugin)  ──omessaged watch / send──▶  omessaged (systemd --user)
   BarWidget.qml, Panel.qml        unix socket in            keys · relays · NIP-17
                                   $XDG_RUNTIME_DIR           SQLite · notify-send
                                                                   │
                                                          wss:// Nostr relays
```

- **Plugin** (`BarWidget.qml`, `Panel.qml`, `Model.js`): a `bar-widget` that
  loads its chat panel internally, following the built-in clock plugin's
  structure. It runs no network code itself. It starts `omessaged watch` to receive
  JSON state, and runs `omessaged <command>` to act.
- **Helper** (`helper/`, Rust, [nostr-sdk](https://crates.io/crates/nostr-sdk)):
  a small user service that owns the identity, keeps relay connections,
  unwraps incoming gift wraps, stores messages in SQLite, publishes your kind
  10050 DM inbox relay list, and sends notifications through `notify-send`.

### Files it creates

| Path | What |
|------|------|
| `~/.local/share/omessage/identity.key` | This device's secret key (hex), mode `0600` |
| `~/.local/share/omessage/messages.db` | Local message store, mode `0600` |
| `~/.local/share/omessage/bin/omessaged` | The helper binary |
| `~/.config/omessage/config.json` | Relay settings |
| `~/.config/systemd/user/omessaged.service` | The user service |
| `$XDG_RUNTIME_DIR/omessage/omessaged.sock` | Control socket (owner only) |

It doesn't change any Omarchy configuration.

## Dependencies

- Omarchy with the Quattro shell (plugin API `schemaVersion` 1)
- systemd user session, `notify-send` (libnotify), `curl` (NIP-05 lookups),
  `wl-copy` (copy npub). All of these ship with Omarchy.
- To build the helper: Rust 1.89 or newer (Arch package `rust`).
  Cargo downloads the crates pinned in `helper/Cargo.lock`
  (nostr-sdk, rusqlite with bundled SQLite, tokio, serde).

No sudo or pkexec is required. Everything installs in your home directory.

## Install

```bash
omarchy plugin add https://github.com/Omarchy-Omessage/omarchy-omessage --enable
~/.config/omarchy/plugins/io.github.omarchy-omessage.omessage/install.sh
```

`install.sh` builds the helper from `helper/` (the first build takes a few
minutes), installs it, and enables the `omessaged` user service. It then prints
your npub. You can also click **Install helper** in the panel, which runs the same
script in a terminal. To install a binary you built elsewhere:
`OMESSAGE_HELPER_BIN=/path/to/omessaged ./install.sh`.

Re-run `install.sh` after `omarchy plugin update` to rebuild the helper.

## Use

1. Click the 󰍡 icon in the bar.
2. Click your npub in the header (or press `c`) to copy it, and share it.
3. Press `n`, paste a friend's npub or NIP-05 address, and press Enter.

**Back up your key.** Your identity lives only on this device. **Back up key**
(click twice to confirm) writes your `nsec` to
`~/omessage-key-backup-<id>.txt` with mode `0600`. You can also run
`omessaged export-key --out <path>`. Keep it somewhere safe: anyone with it can
read your messages and send as you. To restore it on a fresh install, stop the
service and run `omessaged import-key < backup.txt` before you start it again.

### Message requests

Anyone who knows your npub can write to you, so messages from people who
aren't in your contacts land under **Requests** instead of your conversations:

- They show only the sender's short npub. A self-chosen profile name is never
  shown (or fetched) until you accept, so nobody can pose as "Mom".
- A new request raises one generic "New message request" notification, with
  none of the sender's text, and doesn't count toward the unread badge (the
  bar shows a dot instead).
- **Accept** moves the sender to your conversations, **Block** deletes the
  request and silently drops their future messages, and **Delete** removes it
  (they can write again).
- Requests are capped at 50 senders with 20 messages each; the oldest go first.

Starting a chat with someone, or replying, accepts them.

### Command line

```
omessaged id                     your npub
omessaged send <npub|nip05> <text>
omessaged add <npub|nip05> [name]
omessaged accept|block|unblock|delete <npub>
omessaged retry <message-id>     resend a failed message
omessaged snapshot               JSON state
```

A message that no relay accepted shows "not delivered · retry"; click it to
send it again.

## Relays

Defaults, from `~/.config/omessage/config.json`:

```json
{
  "relays": ["wss://relay.damus.io", "wss://nos.lol", "wss://relay.primal.net"],
  "inbox_relays": ["wss://relay.damus.io", "wss://nos.lol"],
  "discovery_relays": ["wss://purplepag.es", "wss://relay.nos.social"],
  "notifications": true
}
```

`inbox_relays` is published as your NIP-17 DM relay list (kind 10050), so other
clients know where to deliver to you. Outgoing messages go to the recipient's
kind 10050 relays, then their NIP-65 read relays, then your own relays. A copy
of each message is also wrapped to you and stored on your inbox relays.

**Planned: a self-hosted, always-on relay.** Public relays can be slow or
prune old events, so a dedicated Omessage relay is planned. You can already
run your own: put its `wss://` URL first in `inbox_relays` and restart with
`systemctl --user restart omessaged`.

Omessage publishes only your kind 10050 inbox list and gift-wrapped DMs
(kind 1059). It never posts public notes.

## Uninstall

```bash
~/.config/omarchy/plugins/io.github.omarchy-omessage.omessage/uninstall.sh   # add --purge to delete key + messages
omarchy plugin remove io.github.omarchy-omessage.omessage
```

`uninstall.sh` stops and disables the service, and removes the unit file and the
helper binary. Your identity and messages stay in `~/.local/share/omessage`
unless you pass `--purge`, which asks before deleting.

## Limitations (v0.1)

- One-to-one chats only. Group DMs aren't supported yet.
- Text only: no attachments, reactions, or read receipts.
- Messages are limited to 16 KiB of text.
- One device per identity. Importing the same key on a second machine works,
  but there's no device linking UI.
- "Omarchy-only" is a matter of distribution, not enforcement. The plugin
  runs only inside the Omarchy shell, but the protocol is open Nostr.

## License

MIT © Bryan Hayes
