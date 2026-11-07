//! SQLite persistence (bundled rusqlite): settings, trusted devices, chat history
//! (kept forever, §6) and transfer state (for resume after restart).

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::types::*;

pub struct Store {
    db: Mutex<Connection>,
}

fn status_str(s: ReceiptStatus) -> &'static str {
    match s {
        ReceiptStatus::Sending => "sending",
        ReceiptStatus::Sent => "sent",
        ReceiptStatus::Delivered => "delivered",
        ReceiptStatus::Read => "read",
    }
}

fn status_from(s: &str) -> ReceiptStatus {
    match s {
        "sent" => ReceiptStatus::Sent,
        "delivered" => ReceiptStatus::Delivered,
        "read" => ReceiptStatus::Read,
        _ => ReceiptStatus::Sending,
    }
}

impl Store {
    pub fn open(dir: &Path) -> rusqlite::Result<Self> {
        let _ = std::fs::create_dir_all(dir);
        let db = Connection::open(dir.join("callxl.sqlite3"))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS trusted_devices (
                key TEXT PRIMARY KEY, name TEXT NOT NULL, agree TEXT NOT NULL, pinned_ms INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY, session TEXT NOT NULL, ts INTEGER NOT NULL, sender TEXT NOT NULL,
                sender_name TEXT NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL, own INTEGER NOT NULL,
                status TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS messages_by_session ON messages(session, ts);
             CREATE TABLE IF NOT EXISTS transfers (
                id TEXT NOT NULL, peer TEXT NOT NULL, direction TEXT NOT NULL, json TEXT NOT NULL,
                PRIMARY KEY (id, peer, direction));",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set(&self, key: &str, value: &str) {
        let _ = self.db.lock().unwrap().execute(
            "INSERT INTO kv(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        );
    }

    pub fn trusted(&self) -> Vec<(String, String, String, u64)> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare("SELECT key,name,agree,pinned_ms FROM trusted_devices ORDER BY pinned_ms").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? as u64)))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn pin(&self, key: &str, name: &str, agree: &str, ts: u64) {
        let _ = self.db.lock().unwrap().execute(
            "INSERT INTO trusted_devices(key,name,agree,pinned_ms) VALUES(?1,?2,?3,?4)
             ON CONFLICT(key) DO UPDATE SET name=excluded.name, agree=excluded.agree",
            params![key, name, agree, ts as i64],
        );
    }

    pub fn unpin(&self, key: &str) {
        let _ = self.db.lock().unwrap().execute("DELETE FROM trusted_devices WHERE key=?1", [key]);
    }

    pub fn save_message(&self, m: &ChatMessage) {
        let _ = self.db.lock().unwrap().execute(
            "INSERT INTO messages(id,session,ts,sender,sender_name,kind,body,own,status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(id) DO NOTHING",
            params![
                m.id,
                m.session,
                m.ts_ms as i64,
                m.sender_key,
                m.sender_name,
                if m.kind == ChatKind::File { "file" } else { "text" },
                m.body,
                m.own as i32,
                status_str(m.status)
            ],
        );
    }

    /// Upgrades a message status (never downgrades: read > delivered > sent > sending).
    pub fn set_status(&self, id: &str, status: ReceiptStatus) -> bool {
        let rank = |s: ReceiptStatus| s as i32;
        let cur = self.message(id).map(|m| m.status);
        match cur {
            Some(c) if rank(c) >= rank(status) => false,
            Some(_) => {
                let _ = self
                    .db
                    .lock()
                    .unwrap()
                    .execute("UPDATE messages SET status=?2 WHERE id=?1", params![id, status_str(status)]);
                true
            }
            None => false,
        }
    }

    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
        Ok(ChatMessage {
            id: r.get(0)?,
            session: r.get(1)?,
            ts_ms: r.get::<_, i64>(2)? as u64,
            sender_key: r.get(3)?,
            sender_name: r.get(4)?,
            kind: if r.get::<_, String>(5)? == "file" { ChatKind::File } else { ChatKind::Text },
            body: r.get(6)?,
            own: r.get::<_, i32>(7)? != 0,
            status: status_from(&r.get::<_, String>(8)?),
        })
    }

    pub fn message(&self, id: &str) -> Option<ChatMessage> {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT id,session,ts,sender,sender_name,kind,body,own,status FROM messages WHERE id=?1",
                [id],
                Self::row,
            )
            .optional()
            .ok()
            .flatten()
    }

    /// Newest `limit` messages of `session` older than `before` (if given), oldest first.
    pub fn history(&self, session: &str, limit: u32, before: Option<&str>) -> Vec<ChatMessage> {
        let before_ts = before.and_then(|b| self.message(b)).map(|m| m.ts_ms as i64).unwrap_or(i64::MAX);
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare(
                "SELECT id,session,ts,sender,sender_name,kind,body,own,status FROM messages
                 WHERE session=?1 AND ts<?2 ORDER BY ts DESC, id DESC LIMIT ?3",
            )
            .unwrap();
        let mut v: Vec<ChatMessage> = st
            .query_map(params![session, before_ts, limit as i64], Self::row)
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default();
        v.reverse();
        v
    }

    pub fn save_transfer(&self, t: &TransferInfo) {
        let dir = if t.direction == TransferDirection::Outgoing { "out" } else { "in" };
        let json = serde_json::to_string(t).unwrap_or_default();
        let _ = self.db.lock().unwrap().execute(
            "INSERT INTO transfers(id,peer,direction,json) VALUES(?1,?2,?3,?4)
             ON CONFLICT(id,peer,direction) DO UPDATE SET json=excluded.json",
            params![t.id, t.peer_key, dir, json],
        );
    }

    pub fn transfers(&self) -> Vec<TransferInfo> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare("SELECT json FROM transfers").unwrap();
        st.query_map([], |r| r.get::<_, String>(0))
            .map(|it| it.filter_map(Result::ok).filter_map(|j| serde_json::from_str(&j).ok()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_status() {
        let dir = std::env::temp_dir().join(format!("callxl-store-{}", uuid::Uuid::new_v4()));
        let s = Store::open(&dir).unwrap();
        let mut m = ChatMessage {
            id: "m1".into(),
            session: "s".into(),
            ts_ms: 1,
            sender_key: "k".into(),
            sender_name: "a".into(),
            kind: ChatKind::Text,
            body: "hi".into(),
            own: true,
            status: ReceiptStatus::Sent,
        };
        s.save_message(&m);
        m.id = "m2".into();
        m.ts_ms = 2;
        s.save_message(&m);
        assert_eq!(s.history("s", 10, None).len(), 2);
        assert_eq!(s.history("s", 10, Some("m2")).len(), 1);
        assert!(s.set_status("m1", ReceiptStatus::Read));
        assert!(!s.set_status("m1", ReceiptStatus::Delivered));
        assert_eq!(s.message("m1").unwrap().status, ReceiptStatus::Read);
        s.set("a", "1");
        assert_eq!(s.get("a").as_deref(), Some("1"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
