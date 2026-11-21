//! Local message store (SQLite, ~/.local/share/omessage/messages.db).

use std::os::unix::fs::PermissionsExt;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::paths;

struct ContactRow {
    pubkey: String,
    nickname: Option<String>,
    nip05: Option<String>,
    profile_name: Option<String>,
    accepted: bool,
    last_ts: i64,
    unread: i64,
}

pub struct Store {
    conn: Connection,
}

pub struct NewMessage<'a> {
    pub id: &'a str,
    pub peer: &'a str,
    pub outgoing: bool,
    pub content: &'a str,
    pub created_at: i64,
    pub status: &'a str,
}

impl Store {
    pub fn open() -> anyhow::Result<Self> {
        paths::private_dir(&paths::data_dir())?;
        let path = paths::db_path();
        let conn = Connection::open(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Self::init(conn)
    }

    /// Create or upgrade the schema on an open connection.
    pub fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS messages (
               id TEXT PRIMARY KEY,
               peer TEXT NOT NULL,
               outgoing INTEGER NOT NULL,
               content TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               received_at INTEGER NOT NULL,
               status TEXT NOT NULL DEFAULT 'received',
               read INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS messages_peer ON messages(peer, created_at);
             CREATE TABLE IF NOT EXISTS contacts (
               pubkey TEXT PRIMARY KEY,
               name TEXT,
               nip05 TEXT,
               profile_name TEXT,
               added_at INTEGER NOT NULL,
               accepted INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE IF NOT EXISTS blocked (pubkey TEXT PRIMARY KEY, blocked_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS seen_wraps (id TEXT PRIMARY KEY, seen_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS inbox_cache (
               pubkey TEXT PRIMARY KEY,
               relays TEXT NOT NULL,
               fetched_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);",
        )?;
        // v0.1 databases predate message requests. Every contact they hold
        // was added by hand or already talked to, so it counts as accepted.
        let has_accepted: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('contacts') WHERE name='accepted'")?
            .exists([])?;
        if !has_accepted {
            conn.execute_batch("ALTER TABLE contacts ADD COLUMN accepted INTEGER NOT NULL DEFAULT 1;")?;
        }
        Ok(Self { conn })
    }

    /// Whether a gift wrap was already handled.
    pub fn wrap_is_seen(&self, id: &str) -> anyhow::Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM seen_wraps WHERE id=?1", params![id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Record a gift wrap as handled. Call only once its message is stored
    /// (or deliberately dropped), so a failed write is retried next time.
    pub fn mark_wrap_seen(&self, id: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO seen_wraps(id, seen_at) VALUES (?1, strftime('%s','now'))",
            params![id],
        )?;
        Ok(())
    }

    /// Forget wraps seen more than `max_age` seconds ago. Messages are also
    /// deduplicated by rumor id, so a re-delivered old wrap is harmless.
    pub fn prune_seen(&self, max_age: i64) -> anyhow::Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM seen_wraps WHERE seen_at < strftime('%s','now') - ?1",
            params![max_age],
        )?)
    }

    /// Messages left in "sending" by a crash or restart can never finish;
    /// mark them failed so they can be retried.
    pub fn fail_pending(&self) -> anyhow::Result<usize> {
        Ok(self
            .conn
            .execute("UPDATE messages SET status='failed' WHERE status='sending'", [])?)
    }

    /// An outgoing message by id: (peer hex, content, created_at, status).
    pub fn outgoing_message(&self, id: &str) -> anyhow::Result<Option<(String, String, i64, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT peer, content, created_at, status FROM messages WHERE id=?1 AND outgoing=1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?)
    }

    /// Insert a message; returns false when it was already stored. The peer
    /// becomes a contact (`accept`) or, if new and not accepted, a request.
    pub fn insert_message(&self, m: &NewMessage, accept: bool) -> anyhow::Result<bool> {
        let read = if m.outgoing { 1 } else { 0 };
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO messages(id, peer, outgoing, content, created_at, received_at, status, read)
             VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'), ?6, ?7)",
            params![m.id, m.peer, m.outgoing as i64, m.content, m.created_at, m.status, read],
        )?;
        self.ensure_contact(m.peer, accept)?;
        Ok(n > 0)
    }

    pub fn set_status(&self, id: &str, status: &str) -> anyhow::Result<()> {
        self.conn
            .execute("UPDATE messages SET status=?2 WHERE id=?1", params![id, status])?;
        Ok(())
    }

    pub fn mark_read(&self, peer: &str) -> anyhow::Result<usize> {
        Ok(self
            .conn
            .execute("UPDATE messages SET read=1 WHERE peer=?1 AND read=0", params![peer])?)
    }

    pub fn delete_conversation(&self, peer: &str) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM messages WHERE peer=?1", params![peer])?;
        self.conn
            .execute("DELETE FROM contacts WHERE pubkey=?1", params![peer])?;
        Ok(())
    }

    /// Make sure a contact row exists. `accept` promotes a request to a
    /// contact; it never demotes one.
    pub fn ensure_contact(&self, pubkey: &str, accept: bool) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO contacts(pubkey, added_at, accepted) VALUES (?1, strftime('%s','now'), ?2)",
            params![pubkey, accept as i64],
        )?;
        if accept {
            self.conn
                .execute("UPDATE contacts SET accepted=1 WHERE pubkey=?1", params![pubkey])?;
        }
        Ok(())
    }

    /// `None`: unknown. `Some(true)`: a contact. `Some(false)`: a request.
    pub fn contact_state(&self, pubkey: &str) -> anyhow::Result<Option<bool>> {
        Ok(self
            .conn
            .query_row("SELECT accepted FROM contacts WHERE pubkey=?1", params![pubkey], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
            .map(|a| a == 1))
    }

    pub fn is_blocked(&self, pubkey: &str) -> anyhow::Result<bool> {
        Ok(self
            .conn
            .prepare("SELECT 1 FROM blocked WHERE pubkey=?1")?
            .exists(params![pubkey])?)
    }

    /// Block a sender: forget the conversation and drop their future messages.
    pub fn block(&self, pubkey: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO blocked(pubkey, blocked_at) VALUES (?1, strftime('%s','now'))",
            params![pubkey],
        )?;
        self.delete_conversation(pubkey)
    }

    pub fn unblock(&self, pubkey: &str) -> anyhow::Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM blocked WHERE pubkey=?1", params![pubkey])?
            > 0)
    }

    /// Keep requests bounded: each request keeps its newest `max_messages`,
    /// and only the `max_senders` most recently active requests are kept.
    /// Returns how many request senders were dropped.
    pub fn trim_requests(&self, peer: &str, max_messages: usize, max_senders: usize) -> anyhow::Result<usize> {
        self.conn.execute(
            "DELETE FROM messages WHERE peer=?1 AND id NOT IN
               (SELECT id FROM messages WHERE peer=?1 ORDER BY created_at DESC, received_at DESC LIMIT ?2)
             AND EXISTS (SELECT 1 FROM contacts WHERE pubkey=?1 AND accepted=0)",
            params![peer, max_messages as i64],
        )?;
        let evict: Vec<String> = self
            .conn
            .prepare(
                "SELECT c.pubkey FROM contacts c WHERE c.accepted=0
                 ORDER BY COALESCE((SELECT MAX(received_at) FROM messages m WHERE m.peer=c.pubkey), c.added_at) DESC,
                          c.added_at DESC
                 LIMIT -1 OFFSET ?1",
            )?
            .query_map(params![max_senders as i64], |r| r.get(0))?
            .filter_map(Result::ok)
            .collect();
        for pk in &evict {
            self.delete_conversation(pk)?;
        }
        Ok(evict.len())
    }

    /// Set a nickname and/or NIP-05 address. Doing either accepts the peer.
    pub fn set_contact(&self, pubkey: &str, name: Option<&str>, nip05: Option<&str>) -> anyhow::Result<()> {
        self.ensure_contact(pubkey, true)?;
        if let Some(n) = name {
            let n = n.trim();
            self.conn.execute(
                "UPDATE contacts SET name=?2 WHERE pubkey=?1",
                params![pubkey, if n.is_empty() { None } else { Some(n) }],
            )?;
        }
        if let Some(n) = nip05 {
            self.conn
                .execute("UPDATE contacts SET nip05=?2 WHERE pubkey=?1", params![pubkey, n])?;
        }
        Ok(())
    }

    pub fn set_profile_name(&self, pubkey: &str, name: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE contacts SET profile_name=?2 WHERE pubkey=?1",
            params![pubkey, name],
        )?;
        Ok(())
    }

    pub fn contacts_without_profile(&self) -> anyhow::Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare("SELECT pubkey FROM contacts WHERE profile_name IS NULL AND accepted=1")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(Result::ok).collect())
    }

    pub fn cached_inbox(&self, pubkey: &str, max_age: i64) -> anyhow::Result<Option<Vec<String>>> {
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT relays FROM inbox_cache WHERE pubkey=?1 AND fetched_at > strftime('%s','now') - ?2",
                params![pubkey, max_age],
                |r| r.get(0),
            )
            .optional()?;
        Ok(row.and_then(|s| serde_json::from_str(&s).ok()))
    }

    pub fn cache_inbox(&self, pubkey: &str, relays: &[String]) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO inbox_cache(pubkey, relays, fetched_at) VALUES (?1, ?2, strftime('%s','now'))",
            params![pubkey, serde_json::to_string(relays)?],
        )?;
        Ok(())
    }

    pub fn kv_get(&self, k: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT v FROM kv WHERE k=?1", params![k], |r| r.get(0))
            .optional()?)
    }

    pub fn kv_set(&self, k: &str, v: &str) -> anyhow::Result<()> {
        self.conn
            .execute("INSERT OR REPLACE INTO kv(k, v) VALUES (?1, ?2)", params![k, v])?;
        Ok(())
    }

    pub fn unread_total(&self) -> anyhow::Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM messages m JOIN contacts c ON c.pubkey=m.peer
                 WHERE m.read=0 AND m.outgoing=0 AND c.accepted=1",
            [],
            |r| r.get(0),
        )?)
    }

    /// Number of pending message requests.
    pub fn request_count(&self) -> anyhow::Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM contacts WHERE accepted=0", [], |r| r.get(0))?)
    }

    /// Conversations and requests, newest first, each with its latest
    /// `limit` messages. `to_npub` converts stored hex keys into npubs.
    ///
    /// A request (a sender not accepted yet) is always shown by its short
    /// npub: its self-chosen profile name is never displayed until accepted.
    /// `only` restricts the result to one peer (hex).
    pub fn conversations(
        &self,
        limit: usize,
        only: Option<&str>,
        to_npub: &dyn Fn(&str) -> String,
    ) -> anyhow::Result<Vec<Value>> {
        let mut st = self.conn.prepare(
            "SELECT c.pubkey, c.name, c.nip05, c.profile_name, c.accepted,
                    COALESCE((SELECT MAX(created_at) FROM messages m WHERE m.peer=c.pubkey), c.added_at) AS last_ts,
                    (SELECT COUNT(*) FROM messages m WHERE m.peer=c.pubkey AND m.read=0 AND m.outgoing=0) AS unread
             FROM contacts c WHERE ?1 IS NULL OR c.pubkey=?1 ORDER BY last_ts DESC",
        )?;
        let rows: Vec<ContactRow> = st
            .query_map(params![only], |r| {
                Ok(ContactRow {
                    pubkey: r.get(0)?,
                    nickname: r.get(1)?,
                    nip05: r.get(2)?,
                    profile_name: r.get(3)?,
                    accepted: r.get::<_, i64>(4)? == 1,
                    last_ts: r.get(5)?,
                    unread: r.get(6)?,
                })
            })?
            .filter_map(Result::ok)
            .collect();
        let mut out = Vec::new();
        let mut mst = self.conn.prepare(
            "SELECT id, outgoing, content, created_at, status FROM
               (SELECT * FROM messages WHERE peer=?1 ORDER BY created_at DESC, received_at DESC LIMIT ?2)
             ORDER BY created_at ASC, received_at ASC",
        )?;
        for c in rows {
            let npub = to_npub(&c.pubkey);
            let messages: Vec<Value> = mst
                .query_map(params![c.pubkey, limit as i64], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "out": r.get::<_, i64>(1)? == 1,
                        "text": r.get::<_, String>(2)?,
                        "ts": r.get::<_, i64>(3)?,
                        "status": r.get::<_, String>(4)?,
                    }))
                })?
                .filter_map(Result::ok)
                .collect();
            let non_empty = |v: &Option<String>| v.clone().filter(|s| !s.is_empty());
            let display = if c.accepted {
                non_empty(&c.nickname)
                    .or_else(|| non_empty(&c.profile_name))
                    .or_else(|| non_empty(&c.nip05))
                    .unwrap_or_else(|| short_npub(&npub))
            } else {
                short_npub(&npub)
            };
            out.push(json!({
                "peer": npub,
                "hex": c.pubkey,
                "name": display,
                "nickname": c.nickname.unwrap_or_default(),
                "nip05": c.nip05.unwrap_or_default(),
                "request": !c.accepted,
                "lastTs": c.last_ts,
                "unread": c.unread,
                "last": messages.last().cloned().unwrap_or(Value::Null),
                "messages": messages,
            }));
        }
        Ok(out)
    }
}

impl Store {
    /// Every conversation's npub, in the same order as `conversations`.
    pub fn conversation_order(&self, to_npub: &dyn Fn(&str) -> String) -> anyhow::Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT c.pubkey,
                    COALESCE((SELECT MAX(created_at) FROM messages m WHERE m.peer=c.pubkey), c.added_at) AS last_ts
             FROM contacts c ORDER BY last_ts DESC",
        )?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(Result::ok).map(|pk| to_npub(&pk)).collect())
    }
}

pub fn short_npub(npub: &str) -> String {
    if npub.len() > 16 {
        format!("{}…{}", &npub[..10], &npub[npub.len() - 4..])
    } else {
        npub.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn mem() -> Store {
        Store::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    pub fn msg<'a>(id: &'a str, peer: &'a str, outgoing: bool, status: &'a str) -> NewMessage<'a> {
        NewMessage {
            id,
            peer,
            outgoing,
            content: "hi",
            created_at: 1_700_000_000,
            status,
        }
    }

    #[test]
    fn wraps_are_seen_only_once_marked_and_pruned_by_age() {
        let st = mem();
        assert!(!st.wrap_is_seen("w1").unwrap());
        st.mark_wrap_seen("w1").unwrap();
        assert!(st.wrap_is_seen("w1").unwrap());
        st.conn
            .execute("UPDATE seen_wraps SET seen_at = seen_at - 40*86400", [])
            .unwrap();
        st.mark_wrap_seen("w2").unwrap();
        assert_eq!(st.prune_seen(30 * 86_400).unwrap(), 1);
        assert!(!st.wrap_is_seen("w1").unwrap());
        assert!(st.wrap_is_seen("w2").unwrap());
    }

    #[test]
    fn pending_sends_become_failed_and_can_be_looked_up() {
        let st = mem();
        st.insert_message(&msg("m1", "p1", true, "sending"), true).unwrap();
        st.insert_message(&msg("m2", "p1", true, "sent"), true).unwrap();
        assert_eq!(st.fail_pending().unwrap(), 1);
        let (peer, _, _, status) = st.outgoing_message("m1").unwrap().unwrap();
        assert_eq!((peer.as_str(), status.as_str()), ("p1", "failed"));
        assert_eq!(st.outgoing_message("m2").unwrap().unwrap().3, "sent");
    }

    fn convs(st: &Store) -> Vec<Value> {
        st.conversations(200, None, &|h| format!("npub1{h}xxxxxxxxxxxxxxxx"))
            .unwrap()
    }

    #[test]
    fn strangers_are_requests_shown_by_npub_until_accepted() {
        let st = mem();
        st.insert_message(&msg("m1", "aa", false, "received"), false).unwrap();
        st.set_profile_name("aa", "Mom").unwrap();
        assert_eq!(st.contact_state("aa").unwrap(), Some(false));
        assert_eq!(st.request_count().unwrap(), 1);
        assert_eq!(st.unread_total().unwrap(), 0);
        assert!(st.contacts_without_profile().unwrap().is_empty());
        let c = &convs(&st)[0];
        assert_eq!(c["request"], true);
        assert!(!c["name"].as_str().unwrap().contains("Mom"));

        st.ensure_contact("aa", true).unwrap();
        let c = &convs(&st)[0];
        assert_eq!((c["request"].as_bool(), c["name"].as_str()), (Some(false), Some("Mom")));
        assert_eq!(st.unread_total().unwrap(), 1);
        // A later message from a stranger path never demotes a contact.
        st.insert_message(&msg("m2", "aa", false, "received"), false).unwrap();
        assert_eq!(st.contact_state("aa").unwrap(), Some(true));
    }

    #[test]
    fn block_forgets_and_remembers() {
        let st = mem();
        st.insert_message(&msg("m1", "bb", false, "received"), false).unwrap();
        st.block("bb").unwrap();
        assert!(st.is_blocked("bb").unwrap());
        assert_eq!(st.contact_state("bb").unwrap(), None);
        assert!(convs(&st).is_empty());
        assert!(st.unblock("bb").unwrap());
        assert!(!st.is_blocked("bb").unwrap());
    }

    #[test]
    fn requests_are_capped_per_sender_and_in_total() {
        let st = mem();
        for i in 0..30 {
            let id = format!("x{i}");
            let m = NewMessage {
                created_at: 1_700_000_000 + i,
                ..msg(&id, "cc", false, "received")
            };
            st.insert_message(&m, false).unwrap();
            st.trim_requests("cc", 20, 50).unwrap();
        }
        assert_eq!(convs(&st)[0]["messages"].as_array().unwrap().len(), 20);
        assert_eq!(convs(&st)[0]["messages"][0]["id"], "x10");

        // Accepted contacts are never trimmed.
        st.ensure_contact("dd", true).unwrap();
        for i in 0..25 {
            st.insert_message(&msg(&format!("d{i}"), "dd", false, "received"), true)
                .unwrap();
            st.trim_requests("dd", 20, 50).unwrap();
        }
        let n: i64 = st
            .conn
            .query_row("SELECT COUNT(*) FROM messages WHERE peer='dd'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 25);

        st.conn
            .execute("UPDATE messages SET received_at=1 WHERE peer='cc'", [])
            .unwrap();
        for i in 0..55 {
            let peer = format!("s{i:02}");
            st.insert_message(&msg(&format!("q{i}"), &peer, false, "received"), false)
                .unwrap();
            st.conn
                .execute(
                    "UPDATE messages SET received_at=?1 WHERE peer=?2",
                    params![1_000 + i, peer],
                )
                .unwrap();
            st.trim_requests(&peer, 20, 50).unwrap();
        }
        assert_eq!(st.request_count().unwrap(), 50);
        assert_eq!(st.contact_state("s54").unwrap(), Some(false));
        assert_eq!(st.contact_state("cc").unwrap(), None, "oldest request evicted");
        assert_eq!(st.contact_state("dd").unwrap(), Some(true));
    }

    #[test]
    fn v01_contacts_migrate_as_accepted() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE contacts (pubkey TEXT PRIMARY KEY, name TEXT, nip05 TEXT, profile_name TEXT,
               added_at INTEGER NOT NULL);
             INSERT INTO contacts(pubkey, added_at) VALUES ('old', 1);",
        )
        .unwrap();
        let st = Store::init(conn).unwrap();
        assert_eq!(st.contact_state("old").unwrap(), Some(true));
        st.insert_message(&msg("m1", "new", false, "received"), false).unwrap();
        assert_eq!(st.contact_state("new").unwrap(), Some(false));
    }

    #[test]
    fn single_conversation_and_order_agree_with_full_list() {
        let st = mem();
        for (i, peer) in ["p1", "p2", "p3"].iter().enumerate() {
            let id = format!("m{i}");
            let m = NewMessage {
                created_at: 1_700_000_000 + i as i64,
                ..msg(&id, peer, false, "received")
            };
            st.insert_message(&m, true).unwrap();
        }
        let npub = |h: &str| format!("npub1{h}");
        let all = st.conversations(10, None, &npub).unwrap();
        let order = st.conversation_order(&npub).unwrap();
        let peers: Vec<&str> = all.iter().map(|c| c["peer"].as_str().unwrap()).collect();
        assert_eq!(peers, order);
        assert_eq!(order, ["npub1p3", "npub1p2", "npub1p1"]);
        let one = st.conversations(10, Some("p2"), &npub).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0], all[1]);
        assert!(st.conversations(10, Some("gone"), &npub).unwrap().is_empty());
    }
}
