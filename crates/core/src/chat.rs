//! Chat (§6): one ordered reliable data channel per peer, JSON messages,
//! delivered/read receipts, SQLite history.

use serde::{Deserialize, Serialize};

use crate::engine::{Core, short};
use crate::types::*;

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Wire {
    Msg { id: MessageId, ts: u64, session: SessionId, sender_name: String, kind: ChatKind, body: String },
    Receipt { id: MessageId, status: ReceiptStatus },
}

impl Core {
    pub fn send_chat(&mut self, id: MessageId, body: String) {
        let Some(session) = self.current_session().map(str::to_string) else {
            self.error("join a session before sending a message");
            return;
        };
        let msg = ChatMessage {
            id: id.clone(),
            session: session.clone(),
            ts_ms: callxl_signal::now_ms(),
            sender_key: self.my_key(),
            sender_name: self.settings.display_name.clone(),
            kind: ChatKind::Text,
            body: body.clone(),
            own: true,
            status: if self.links.is_empty() { ReceiptStatus::Sending } else { ReceiptStatus::Sent },
        };
        self.store.save_message(&msg);
        let wire = serde_json::to_vec(&Wire::Msg {
            id,
            ts: msg.ts_ms,
            session,
            sender_name: msg.sender_name.clone(),
            kind: ChatKind::Text,
            body,
        })
        .unwrap_or_default();
        for l in self.links.values_mut() {
            l.send_chat(wire.clone());
        }
    }

    pub fn on_chat_message(&mut self, key: &str, data: &[u8]) {
        let Ok(w) = serde_json::from_slice::<Wire>(data) else {
            tracing::debug!("malformed chat message from {}", short(key));
            return;
        };
        match w {
            Wire::Msg { id, ts, session, sender_name, kind, body } => {
                // The sender is the authenticated peer of this DTLS channel, never a claim.
                let msg = ChatMessage {
                    id: id.clone(),
                    session,
                    ts_ms: ts,
                    sender_key: key.to_string(),
                    sender_name,
                    kind,
                    body,
                    own: false,
                    status: ReceiptStatus::Delivered,
                };
                self.store.save_message(&msg);
                self.emit(Event::ChatReceived { message: msg });
                let receipt =
                    serde_json::to_vec(&Wire::Receipt { id, status: ReceiptStatus::Delivered }).unwrap_or_default();
                if let Some(l) = self.links.get_mut(key) {
                    l.send_chat(receipt);
                }
            }
            Wire::Receipt { id, status } => {
                let Some(m) = self.store.message(&id) else { return };
                if !m.own {
                    return;
                }
                self.store.set_status(&id, status);
                self.emit(Event::ReceiptUpdated { message_id: id, status, peer_key: key.to_string() });
            }
        }
    }

    /// Marks every received message up to and including `up_to` as read.
    pub fn mark_read(&mut self, up_to: &str) {
        let Some(target) = self.store.message(up_to) else { return };
        let history = self.store.history(&target.session, 10_000, None);
        for m in history.into_iter().filter(|m| !m.own && m.ts_ms <= target.ts_ms && m.status != ReceiptStatus::Read) {
            self.store.set_status(&m.id, ReceiptStatus::Read);
            let receipt = serde_json::to_vec(&Wire::Receipt { id: m.id.clone(), status: ReceiptStatus::Read })
                .unwrap_or_default();
            if let Some(l) = self.links.get_mut(&m.sender_key) {
                l.send_chat(receipt);
            }
        }
    }
}
