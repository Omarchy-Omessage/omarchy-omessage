//! The background service: keeps relay connections, unwraps NIP-17 gift
//! wraps addressed to this device, stores messages, sends notifications and
//! answers the QML panel over a unix socket.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nostr::nips::{nip17, nip65};
use nostr_sdk::prelude::*;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::config::Config;
use crate::store::{NewMessage, Store};
use crate::{identity, paths};

const SNAPSHOT_MESSAGES: usize = 200;
/// Largest message we send or store, in bytes. Relays commonly cap whole
/// events at 64-128 KiB and a gift wrap roughly doubles the size, so this
/// keeps us well inside every relay's limit and bounds what a sender can
/// make us store per message.
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
/// Largest request line accepted on the control socket.
const MAX_REQUEST_BYTES: u64 = 256 * 1024;
/// Longest profile name we keep for a contact.
const MAX_NAME_CHARS: usize = 64;
const INBOX_CACHE_SECS: i64 = 3600;
/// How long a handled gift wrap id is remembered.
const SEEN_WRAP_MAX_AGE_SECS: i64 = 30 * 86_400;

/// Message requests kept at once, and messages kept per request.
const MAX_REQUESTS: usize = 50;
const MAX_REQUEST_MESSAGES: usize = 20;

/// A newly stored message: whose conversation, and the notification to show.
struct Incoming {
    peer: PublicKey,
    notice: Option<(String, String)>,
}

/// What a watcher needs to refresh.
#[derive(Clone, Debug)]
enum Change {
    Status,
    Peer(String),
}

pub struct App {
    keys: Keys,
    me: PublicKey,
    client: Client,
    store: Mutex<Store>,
    cfg: Config,
    changed: broadcast::Sender<Change>,
    started: Timestamp,
}

pub async fn run() -> anyhow::Result<()> {
    let cfg = Config::load()?;
    let (keys, created) = identity::load_or_create()?;
    let me = keys.public_key();
    let store = Store::open()?;
    let _ = store.fail_pending();

    // Control socket first, so a second instance fails fast.
    paths::private_dir(&paths::runtime_dir())?;
    let sock = paths::socket_path();
    if sock.exists() {
        if UnixStream::connect(&sock).await.is_ok() {
            anyhow::bail!("omessaged is already running ({})", sock.display());
        }
        let _ = std::fs::remove_file(&sock);
    }
    let listener = UnixListener::bind(&sock)?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;

    let client = Client::builder()
        .authenticator(SignerAuthenticator::new(keys.clone()))
        .build();
    for url in cfg.all_relays() {
        if let Err(e) = client.add_relay(url.as_str()).await {
            eprintln!("omessaged: skipping relay {url}: {e}");
        }
    }

    let (changed, _) = broadcast::channel(64);
    let app = Arc::new(App {
        keys,
        me,
        client,
        store: Mutex::new(store),
        cfg,
        changed,
        started: Timestamp::now(),
    });
    eprintln!(
        "omessaged: {} identity {}",
        if created { "created" } else { "loaded" },
        identity::npub(&me)
    );

    // Notifications must be subscribed before the REQ goes out.
    let mut notifications = app.client.notifications();
    {
        let app = app.clone();
        tokio::spawn(async move {
            while let Some(n) = notifications.next().await {
                if let ClientNotification::Event { event, .. } = n {
                    // One task per event: a panic loses that event, not the
                    // whole receive loop.
                    let app = app.clone();
                    let _ = tokio::spawn(async move { app.handle_event(&event).await }).await;
                }
            }
        });
    }

    app.client.connect().and_wait(Duration::from_secs(8)).await;
    app.subscribe().await;
    {
        let app = app.clone();
        tokio::spawn(async move { app.publish_inbox_list().await });
    }
    {
        let app = app.clone();
        tokio::spawn(app.housekeeping());
    }

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    let app = app.clone();
                    tokio::spawn(async move { let _ = app.serve(stream).await; });
                }
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = term.recv() => break,
        }
    }
    let _ = std::fs::remove_file(&sock);
    app.client.shutdown().await;
    Ok(())
}

impl App {
    /// The store, even if an earlier panic poisoned the lock: every write is
    /// a single SQLite statement, so the data is still consistent.
    fn db(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Relay status or other top-level state changed.
    fn notify_changed(&self) {
        let _ = self.changed.send(Change::Status);
    }

    /// One conversation changed (or was removed).
    fn notify_peer(&self, pk: &PublicKey) {
        let _ = self.changed.send(Change::Peer(pk.to_hex()));
    }

    async fn subscribe(&self) {
        // Gift wraps carry randomized timestamps up to two days in the past,
        // so look back three days from the last time we were running.
        let last_seen: u64 = {
            let st = self.db();
            st.kv_get("last_seen").ok().flatten().and_then(|v| v.parse().ok())
        }
        .unwrap_or_else(|| Timestamp::now().as_secs());
        let since = Timestamp::from(last_seen.saturating_sub(3 * 86_400));
        let filter = Filter::new().kind(Kind::GiftWrap).pubkey(self.me).since(since);
        match self.client.subscribe(filter).await {
            Ok(out) => eprintln!("omessaged: subscribed on {} relays", out.success.len()),
            Err(e) => eprintln!("omessaged: subscribe failed: {e}"),
        }
    }

    async fn publish_inbox_list(&self) {
        let relays: Vec<RelayUrl> = self
            .cfg
            .inbox_relays
            .iter()
            .filter_map(|r| RelayUrl::parse(r).ok())
            .collect();
        if relays.is_empty() {
            return;
        }
        let event = match nip17::InboxRelayList::new(relays)
            .into_event_builder()
            .finalize(&self.keys)
        {
            Ok(e) => e,
            Err(e) => return eprintln!("omessaged: could not sign inbox list: {e}"),
        };
        let targets = self.cfg.all_relays();
        match self
            .client
            .send_event(&event)
            .to(targets.iter().map(|s| s.as_str()))
            .await
        {
            Ok(out) => eprintln!(
                "omessaged: published kind 10050 inbox list to {} relays",
                out.success.len()
            ),
            Err(e) => eprintln!("omessaged: inbox list publish failed: {e}"),
        }
    }

    /// Every 5 s: watch relay status. Every minute: remember we were
    /// running and fetch missing profiles. Every hour: prune seen wraps. Each
    /// round runs in its own task, so a panic skips one round and no more.
    async fn housekeeping(self: Arc<Self>) {
        let mut last_sig = String::new();
        let mut tick: u64 = 0;
        loop {
            let app = self.clone();
            let prev = last_sig.clone();
            if let Ok(sig) = tokio::spawn(async move { app.housekeeping_round(tick, prev).await }).await {
                last_sig = sig;
            }
            tick += 1;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn housekeeping_round(&self, tick: u64, last_sig: String) -> String {
        let sig = self.relay_signature().await;
        if sig != last_sig {
            self.notify_changed();
        }
        if tick.is_multiple_of(12) {
            let _ = self.db().kv_set("last_seen", &Timestamp::now().as_secs().to_string());
            self.fetch_profiles().await;
        }
        if tick.is_multiple_of(720) {
            let _ = self.db().prune_seen(SEEN_WRAP_MAX_AGE_SECS);
        }
        sig
    }

    async fn relay_signature(&self) -> String {
        let relays = self.client.relays().await;
        let mut v: Vec<String> = relays.iter().map(|(u, r)| format!("{u}={}", r.status())).collect();
        v.sort();
        v.join(",")
    }

    async fn fetch_profiles(&self) {
        let missing: Vec<PublicKey> = {
            let st = self.db();
            st.contacts_without_profile().unwrap_or_default()
        }
        .iter()
        .filter_map(|h| PublicKey::from_hex(h).ok())
        .collect();
        if missing.is_empty() {
            return;
        }
        let filter = Filter::new().kind(Kind::Metadata).authors(missing.clone());
        let events = match self.client.fetch_events(filter).timeout(Duration::from_secs(6)).await {
            Ok(e) => e,
            Err(_) => return,
        };
        let st = self.db();
        let mut found = HashSet::new();
        let mut sorted: Vec<&Event> = events.iter().collect();
        sorted.sort_by_key(|e| e.created_at);
        for ev in sorted {
            if let Ok(meta) = Metadata::from_json(&ev.content) {
                let name = meta
                    .display_name
                    .filter(|s| !s.is_empty())
                    .or(meta.name)
                    .unwrap_or_default();
                let name = clean_name(&name);
                let _ = st.set_profile_name(&ev.pubkey.to_hex(), &name);
                found.insert(ev.pubkey);
            }
        }
        // Remember the miss so we do not ask again every minute.
        for pk in missing.iter().filter(|p| !found.contains(*p)) {
            let _ = st.set_profile_name(&pk.to_hex(), "");
        }
        drop(st);
        if !found.is_empty() {
            for pk in &found {
                self.notify_peer(pk);
            }
        }
    }

    async fn handle_event(&self, event: &Event) {
        if event.kind != Kind::GiftWrap {
            return;
        }
        let wrap_id = event.id.to_hex();
        if self.db().wrap_is_seen(&wrap_id).unwrap_or(true) {
            return;
        }
        match self.store_wrap(event) {
            Ok(stored) => {
                // Only now is the wrap done with; a failed write above leaves
                // it unseen so the next delivery retries it.
                let _ = self.db().mark_wrap_seen(&wrap_id);
                if let Some(incoming) = stored {
                    if let Some((title, body)) = incoming.notice {
                        notify(&title, &body);
                    }
                    self.notify_peer(&incoming.peer);
                }
            }
            Err(e) => eprintln!("omessaged: could not store message from {}: {e}", event.id),
        }
    }

    /// Unwrap and store one gift wrap. `Ok(None)`: nothing to store (not
    /// for us, invalid, blocked, duplicate). `Ok(Some(_))`: a new message.
    /// `Err`: the database failed and the wrap should be retried.
    fn store_wrap(&self, event: &Event) -> anyhow::Result<Option<Incoming>> {
        let gift = match UnwrappedGift::from_gift_wrap(&self.keys, event) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("omessaged: could not unwrap {}: {e}", event.id);
                return Ok(None);
            }
        };
        let mut rumor = gift.rumor;
        if rumor.kind != Kind::PrivateDirectMessage || rumor.pubkey != gift.sender {
            return Ok(None);
        }
        if rumor.content.len() > MAX_MESSAGE_BYTES {
            eprintln!(
                "omessaged: dropped an oversized message ({} bytes)",
                rumor.content.len()
            );
            return Ok(None);
        }
        // The sender picks created_at; never store it in the future, where it
        // would pin a conversation to the top of the list. (The rumor itself
        // stays untouched so its id, our dedupe key, is stable.)
        let created_at = rumor.created_at.min(Timestamp::now());
        let outgoing = gift.sender == self.me;
        let peer = if outgoing {
            match rumor.tags.public_keys().find(|p| *p != self.me) {
                Some(p) => p,
                None => self.me, // note to self
            }
        } else {
            gift.sender
        };
        rumor.ensure_id();
        let id = rumor.id.map(|i| i.to_hex()).unwrap_or_else(|| event.id.to_hex());
        let peer_hex = peer.to_hex();
        // Strangers land in message requests; blocked senders are dropped.
        // Anything we sent ourselves makes the peer a contact.
        let (known, is_request) = {
            let st = self.db();
            if !outgoing && st.is_blocked(&peer_hex)? {
                return Ok(None);
            }
            let known = st.contact_state(&peer_hex)?;
            (known, !outgoing && known != Some(true))
        };
        let inserted = self.db().insert_message(
            &NewMessage {
                id: &id,
                peer: &peer_hex,
                outgoing,
                content: &rumor.content,
                created_at: created_at.as_secs() as i64,
                status: if outgoing { "sent" } else { "received" },
            },
            !is_request,
        )?;
        if !inserted {
            return Ok(None);
        }
        if is_request {
            self.db().trim_requests(&peer_hex, MAX_REQUEST_MESSAGES, MAX_REQUESTS)?;
        }
        let recent = created_at.as_secs() + 600 >= self.started.as_secs();
        let notice = if outgoing || !recent || !self.cfg.notifications {
            None
        } else if is_request {
            // One generic notice per new sender, with nothing they control.
            known.is_none().then(|| {
                (
                    "New message request".to_string(),
                    "Open Omessage to review it.".to_string(),
                )
            })
        } else {
            Some((self.display_name(&peer), rumor.content.clone()))
        };
        Ok(Some(Incoming { peer, notice }))
    }

    fn display_name(&self, pk: &PublicKey) -> String {
        self.db()
            .conversations(1, Some(&pk.to_hex()), &npub_of)
            .ok()
            .and_then(|cs| cs.into_iter().next())
            .and_then(|c| c["name"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| crate::store::short_npub(&identity::npub(pk)))
    }

    async fn inbox_relays_for(&self, pk: &PublicKey) -> Vec<String> {
        let hex = pk.to_hex();
        if let Ok(Some(cached)) = self.db().cached_inbox(&hex, INBOX_CACHE_SECS)
            && !cached.is_empty()
        {
            return cached;
        }
        let mut relays: Vec<String> = Vec::new();
        let filter = Filter::new().kind(Kind::InboxRelays).author(*pk).limit(5);
        if let Ok(events) = self.client.fetch_events(filter).timeout(Duration::from_secs(6)).await
            && let Some(ev) = events.iter().max_by_key(|e| e.created_at)
        {
            relays = nip17::extract_relay_list(ev).map(|u| u.to_string()).collect();
        }
        if relays.is_empty() {
            // Fall back to the NIP-65 read relays, then to our own relays.
            let filter = Filter::new().kind(Kind::RelayList).author(*pk).limit(5);
            if let Ok(events) = self.client.fetch_events(filter).timeout(Duration::from_secs(6)).await
                && let Some(ev) = events.iter().max_by_key(|e| e.created_at)
            {
                relays = nip65::extract_relay_list(ev)
                    .filter(|(_, m)| !matches!(m, Some(RelayMetadata::Write)))
                    .map(|(u, _)| u.to_string())
                    .collect();
            }
        }
        relays.retain(|r| r.starts_with("wss://"));
        relays.truncate(5);
        if relays.is_empty() {
            return self.cfg.relays.clone();
        }
        let _ = self.db().cache_inbox(&hex, &relays);
        relays
    }

    async fn ensure_relays(&self, urls: &[String]) -> Vec<String> {
        let mut ok = Vec::new();
        for url in urls {
            if self.client.add_relay(url.as_str()).await.is_err() {
                continue;
            }
            let _ = self
                .client
                .try_connect_relay(url.as_str(), Duration::from_secs(5))
                .await;
            ok.push(url.clone());
        }
        ok
    }

    pub async fn send(&self, to: &str, text: &str) -> anyhow::Result<Value> {
        let text = text.trim_end();
        if text.trim().is_empty() {
            anyhow::bail!("message is empty");
        }
        if text.len() > MAX_MESSAGE_BYTES {
            anyhow::bail!(
                "message is too long ({} bytes; the limit is {MAX_MESSAGE_BYTES})",
                text.len()
            );
        }
        let (pk, nip05) = resolve(to).await?;
        // Messaging someone is the clearest way to say you want to hear back.
        self.db().unblock(&pk.to_hex())?;
        if let Some(n) = nip05 {
            self.db().set_contact(&pk.to_hex(), None, Some(&n))?;
        }

        let mut rumor = EventBuilder::new(Kind::PrivateDirectMessage, text)
            .tag(Tag::public_key(pk))
            .finalize_unsigned(self.me);
        rumor.ensure_id();
        let id = rumor.id.map(|i| i.to_hex()).unwrap_or_default();
        {
            let st = self.db();
            st.insert_message(
                &NewMessage {
                    id: &id,
                    peer: &pk.to_hex(),
                    outgoing: true,
                    content: text,
                    created_at: rumor.created_at.as_secs() as i64,
                    status: "sending",
                },
                true,
            )?;
            st.mark_read(&pk.to_hex())?;
        }
        self.notify_peer(&pk);
        self.deliver(pk, rumor, id).await
    }

    /// Send a failed outgoing message again. The rumor is rebuilt from the
    /// stored text and timestamp, so it keeps the same id.
    pub async fn retry(&self, id: &str) -> anyhow::Result<Value> {
        let (peer, content, created_at, status) = self
            .db()
            .outgoing_message(id)?
            .ok_or_else(|| anyhow::anyhow!("no outgoing message {id}"))?;
        if status != "failed" {
            anyhow::bail!("message is {status}, not failed");
        }
        let pk = PublicKey::from_hex(&peer)?;
        let mut rumor = EventBuilder::new(Kind::PrivateDirectMessage, content)
            .tag(Tag::public_key(pk))
            .custom_created_at(Timestamp::from(created_at.max(0) as u64))
            .finalize_unsigned(self.me);
        rumor.ensure_id();
        let new_id = rumor.id.map(|i| i.to_hex()).unwrap_or_default();
        if new_id != id {
            anyhow::bail!("could not rebuild message {id}");
        }
        self.db().set_status(id, "sending")?;
        self.notify_peer(&pk);
        self.deliver(pk, rumor, new_id).await
    }

    /// Gift-wrap and publish a stored outgoing message. Whatever happens, the
    /// message ends up "sent" or "failed", never stuck in "sending".
    async fn deliver(&self, pk: PublicKey, rumor: UnsignedEvent, id: String) -> anyhow::Result<Value> {
        let result = self.publish(pk, rumor).await;
        let status = match &result {
            Ok((accepted, _, _)) if !accepted.is_empty() => "sent",
            _ => "failed",
        };
        self.db().set_status(&id, status)?;
        self.notify_peer(&pk);
        let (accepted, errors) = match result {
            Ok((accepted, errors, self_wrap)) => {
                // Copy for our own other devices / restore; best effort.
                let mine: Vec<String> = self.ensure_relays(&self.cfg.inbox_relays).await;
                let _ = self
                    .client
                    .send_event(&self_wrap)
                    .to(mine.iter().map(|s| s.as_str()))
                    .await;
                (accepted, errors)
            }
            Err(e) => (vec![], vec![e.to_string()]),
        };
        Ok(json!({
            "ok": status == "sent",
            "id": id,
            "to": identity::npub(&pk),
            "status": status,
            "relays": accepted,
            "errors": errors,
        }))
    }

    /// Publish the recipient's wrap. Returns (relays that accepted, per-relay
    /// errors, our own copy to store on our inbox relays).
    async fn publish(&self, pk: PublicKey, rumor: UnsignedEvent) -> anyhow::Result<(Vec<String>, Vec<String>, Event)> {
        let to_wrap = GiftWrapBuilder::new(pk, rumor.clone()).finalize(&self.keys)?;
        let self_wrap = GiftWrapBuilder::new(self.me, rumor).finalize(&self.keys)?;
        let _ = self.db().mark_wrap_seen(&self_wrap.id.to_hex());

        let targets = self.inbox_relays_for(&pk).await;
        let targets = self.ensure_relays(&targets).await;
        let out = self
            .client
            .send_event(&to_wrap)
            .to(targets.iter().map(|s| s.as_str()))
            .await?;
        let accepted: Vec<String> = out.success.keys().map(|u| u.to_string()).collect();
        let errors: Vec<String> = out.failed.iter().map(|(u, e)| format!("{u}: {e}")).collect();
        Ok((accepted, errors, self_wrap))
    }

    /// Everything the panel shows: top-level state plus every conversation.
    pub async fn snapshot(&self) -> Value {
        let mut v = self.state("snapshot").await;
        v["conversations"] = Value::from(
            self.db()
                .conversations(SNAPSHOT_MESSAGES, None, &npub_of)
                .unwrap_or_default(),
        );
        v
    }

    /// An incremental update: top-level state, the full conversation order
    /// (npubs only, so removals show up), and only the conversations in
    /// `peers` (hex keys) that still exist.
    async fn update(&self, peers: &HashSet<String>) -> Value {
        let mut v = self.state("update").await;
        let st = self.db();
        let mut changed = Vec::new();
        for hex in peers {
            changed.extend(
                st.conversations(SNAPSHOT_MESSAGES, Some(hex), &npub_of)
                    .unwrap_or_default(),
            );
        }
        v["order"] = Value::from(st.conversation_order(&npub_of).unwrap_or_default());
        v["conversations"] = Value::from(changed);
        v
    }

    async fn state(&self, kind: &str) -> Value {
        let relays = self.client.relays().await;
        let mut list: Vec<Value> = relays
            .iter()
            .map(|(u, r)| json!({"url": u.to_string(), "status": r.status().to_string()}))
            .collect();
        list.sort_by(|a, b| a["url"].as_str().cmp(&b["url"].as_str()));
        let connected = relays.values().filter(|r| r.status() == RelayStatus::Connected).count();
        let st = self.db();
        json!({
            "type": kind,
            "version": env!("CARGO_PKG_VERSION"),
            "me": { "npub": identity::npub(&self.me), "hex": self.me.to_hex() },
            "relays": list,
            "connected": connected,
            "inboxRelays": self.cfg.inbox_relays,
            "unread": st.unread_total().unwrap_or(0),
            "requests": st.request_count().unwrap_or(0),
        })
    }

    async fn dispatch(&self, req: &Value) -> Value {
        let cmd = req["cmd"].as_str().unwrap_or("");
        let s = |k: &str| req[k].as_str().unwrap_or("").to_string();
        let res: anyhow::Result<Value> = async {
            match cmd {
                "snapshot" => Ok(self.snapshot().await),
                "send" => self.send(&s("to"), &s("text")).await,
                "retry" => {
                    let id = s("id");
                    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                        anyhow::bail!("not a message id: {id}");
                    }
                    self.retry(&id.to_lowercase()).await
                }
                "accept" => {
                    let (pk, _) = resolve(&s("who")).await?;
                    self.db().unblock(&pk.to_hex())?;
                    self.db().ensure_contact(&pk.to_hex(), true)?;
                    self.notify_peer(&pk);
                    Ok(json!({"ok": true}))
                }
                "block" => {
                    let (pk, _) = resolve(&s("who")).await?;
                    if pk == self.me {
                        anyhow::bail!("you cannot block yourself");
                    }
                    self.db().block(&pk.to_hex())?;
                    self.notify_peer(&pk);
                    Ok(json!({"ok": true}))
                }
                "unblock" => {
                    let (pk, _) = resolve(&s("who")).await?;
                    let n = self.db().unblock(&pk.to_hex())?;
                    Ok(json!({"ok": true, "unblocked": n}))
                }
                "add" => {
                    let (pk, nip05) = resolve(&s("who")).await?;
                    self.db().unblock(&pk.to_hex())?;
                    let name = req["name"].as_str().map(clean_name);
                    self.db().set_contact(&pk.to_hex(), name.as_deref(), nip05.as_deref())?;
                    self.notify_peer(&pk);
                    Ok(json!({"ok": true, "npub": identity::npub(&pk)}))
                }
                "rename" => {
                    let (pk, _) = resolve(&s("who")).await?;
                    self.db()
                        .set_contact(&pk.to_hex(), Some(&clean_name(&s("name"))), None)?;
                    self.notify_peer(&pk);
                    Ok(json!({"ok": true}))
                }
                "read" => {
                    let (pk, _) = resolve(&s("who")).await?;
                    let n = self.db().mark_read(&pk.to_hex())?;
                    if n > 0 {
                        self.notify_peer(&pk);
                    }
                    Ok(json!({"ok": true, "marked": n}))
                }
                "delete" => {
                    let (pk, _) = resolve(&s("who")).await?;
                    self.db().delete_conversation(&pk.to_hex())?;
                    self.notify_peer(&pk);
                    Ok(json!({"ok": true}))
                }
                other => anyhow::bail!("unknown command '{other}'"),
            }
        }
        .await;
        res.unwrap_or_else(|e| json!({"ok": false, "error": e.to_string()}))
    }

    async fn serve(self: Arc<Self>, stream: UnixStream) -> anyhow::Result<()> {
        let (r, mut w) = stream.into_split();
        let mut lines = BufReader::new(r.take(MAX_REQUEST_BYTES)).lines();
        while let Some(line) = lines.next_line().await? {
            let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            if req["cmd"] == "watch" {
                // One full snapshot, then incremental updates that carry only
                // the conversations that changed. Falls back to a snapshot if
                // this watcher fell behind and missed changes.
                let mut rx = self.changed.subscribe();
                let mut payload = self.snapshot().await;
                loop {
                    w.write_all(format!("{payload}\n").as_bytes()).await?;
                    w.flush().await?;
                    let mut peers = HashSet::new();
                    let mut full = false;
                    match rx.recv().await {
                        Ok(change) => collect_change(change, &mut peers),
                        Err(broadcast::error::RecvError::Lagged(_)) => full = true,
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                    // Coalesce bursts (initial sync can deliver many wraps).
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    loop {
                        match rx.try_recv() {
                            Ok(change) => collect_change(change, &mut peers),
                            Err(broadcast::error::TryRecvError::Lagged(_)) => full = true,
                            Err(_) => break,
                        }
                    }
                    payload = if full {
                        self.snapshot().await
                    } else {
                        self.update(&peers).await
                    };
                }
            }
            let resp = self.dispatch(&req).await;
            w.write_all(format!("{resp}\n").as_bytes()).await?;
            w.flush().await?;
        }
        Ok(())
    }
}

fn collect_change(change: Change, peers: &mut HashSet<String>) {
    if let Change::Peer(hex) = change {
        peers.insert(hex);
    }
}

fn npub_of(hex: &str) -> String {
    PublicKey::from_hex(hex).map(|p| identity::npub(&p)).unwrap_or_default()
}

/// Trim a profile name or nickname to one short line.
fn clean_name(s: &str) -> String {
    let one_line: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    one_line
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_NAME_CHARS)
        .collect()
}

fn notify(title: &str, body: &str) {
    let title = notify_text(title, 64);
    let body = notify_text(body, 240);
    // `--` ends option parsing: a sender-chosen name or message that starts
    // with `-` must never be read as a notify-send flag.
    let _ = std::process::Command::new("notify-send")
        .args([
            "-a",
            "Omessage",
            "-i",
            "mail-message-new",
            "-c",
            "im.received",
            "--",
            &title,
            &body,
        ])
        .spawn()
        .map(|mut c| std::thread::spawn(move || c.wait()));
}

/// Make untrusted text safe for a notification: drop control characters,
/// cap the length and escape the Pango/HTML markup that mako and other
/// notification daemons interpret.
fn notify_text(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for c in s.chars().filter(|c| !c.is_control() || *c == '\n').take(max_chars) {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Accept npub, nprofile, hex, `nostr:` URIs and NIP-05 identifiers.
pub async fn resolve(who: &str) -> anyhow::Result<(PublicKey, Option<String>)> {
    let who = who.trim().trim_start_matches("nostr:");
    if who.is_empty() {
        anyhow::bail!("enter an npub or a NIP-05 address");
    }
    if who.starts_with("nprofile1") {
        return Ok((Nip19Profile::from_bech32(who)?.public_key, None));
    }
    if let Ok(pk) = PublicKey::parse(who) {
        return Ok((pk, None));
    }
    if who.contains('.') {
        let (name, domain) = match who.split_once('@') {
            Some((n, d)) => (n.to_lowercase(), d.to_lowercase()),
            None => ("_".to_string(), who.to_lowercase()),
        };
        if !domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        {
            anyhow::bail!("not a valid NIP-05 address: {who}");
        }
        let url = format!("https://{domain}/.well-known/nostr.json?name={name}");
        let out = tokio::process::Command::new("curl")
            // NIP-05: fetchers must ignore redirects, so no -L. Cap the reply size.
            .args([
                "-fsS",
                "--proto",
                "=https",
                "--max-time",
                "8",
                "--max-filesize",
                "65536",
                "--",
                &url,
            ])
            .output()
            .await?;
        if !out.status.success() {
            anyhow::bail!("NIP-05 lookup failed for {who}");
        }
        let v: Value = serde_json::from_slice(&out.stdout)?;
        let hex = v["names"][&name]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("{who} is not listed at {domain}"))?;
        let pk = PublicKey::from_hex(hex)?;
        return Ok((
            pk,
            Some(if name == "_" {
                domain
            } else {
                format!("{name}@{domain}")
            }),
        ));
    }
    anyhow::bail!("not an npub, hex key or NIP-05 address: {who}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_text_escapes_markup_and_strips_controls() {
        assert_eq!(
            notify_text("<b>hi</b> & \u{1b}[31mx", 100),
            "&lt;b&gt;hi&lt;/b&gt; &amp; [31mx"
        );
        assert_eq!(notify_text("line1\nline2", 100), "line1\nline2");
        assert_eq!(notify_text("abcdef", 3), "abc");
    }

    #[test]
    fn clean_name_is_one_short_line() {
        assert_eq!(clean_name("  Mom\n\t <3  "), "Mom <3");
        assert_eq!(clean_name(&"x".repeat(500)).chars().count(), MAX_NAME_CHARS);
    }
}
