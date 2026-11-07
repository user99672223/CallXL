//! Direct TCP signaling between LAN peers. One task per connection; frames are
//! delivered to the core as [`LanEvent::Frame`].

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use callxl_signal::framing::{read_frame, write_frame};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::LanError;

pub type ConnId = u64;

#[derive(Debug)]
pub enum LanEvent {
    Connected { conn: ConnId, addr: SocketAddr, outbound: bool },
    Frame { conn: ConnId, data: Vec<u8> },
    Closed { conn: ConnId, reason: String },
}

#[derive(Clone)]
pub struct LanSignaling {
    inner: Arc<Inner>,
}

struct Inner {
    next: AtomicU64,
    conns: Mutex<HashMap<ConnId, mpsc::UnboundedSender<Vec<u8>>>>,
    events: mpsc::Sender<LanEvent>,
    local: SocketAddr,
}

impl LanSignaling {
    /// Binds the signaling listener (`port` 0 = ephemeral) and starts accepting.
    pub async fn bind(addr: SocketAddr, events: mpsc::Sender<LanEvent>) -> Result<Self, LanError> {
        let listener = TcpListener::bind(addr).await.map_err(LanError::Bind)?;
        let local = listener.local_addr().map_err(LanError::Bind)?;
        let me = Self {
            inner: Arc::new(Inner { next: AtomicU64::new(1), conns: Mutex::new(HashMap::new()), events, local }),
        };
        let acceptor = me.clone();
        tokio::spawn(async move {
            while let Ok((stream, peer)) = listener.accept().await {
                acceptor.adopt(stream, peer, false);
            }
        });
        Ok(me)
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.inner.local
    }

    /// Connects in the background; success is reported as `LanEvent::Connected`.
    pub fn connect(&self, addr: SocketAddr) -> ConnId {
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let me = self.clone();
        tokio::spawn(async move {
            match tokio::time::timeout(std::time::Duration::from_secs(3), TcpStream::connect(addr)).await {
                Ok(Ok(stream)) => me.run(id, stream, addr, true),
                Ok(Err(e)) => {
                    let _ = me.inner.events.send(LanEvent::Closed { conn: id, reason: e.to_string() }).await;
                }
                Err(_) => {
                    let _ = me.inner.events.send(LanEvent::Closed { conn: id, reason: "connect timed out".into() }).await;
                }
            }
        });
        id
    }

    fn adopt(&self, stream: TcpStream, addr: SocketAddr, outbound: bool) {
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        self.run(id, stream, addr, outbound);
    }

    fn run(&self, id: ConnId, stream: TcpStream, addr: SocketAddr, outbound: bool) {
        let _ = stream.set_nodelay(true);
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        self.inner.conns.lock().unwrap().insert(id, tx);
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let (mut r, mut w) = stream.into_split();
            let _ = inner.events.send(LanEvent::Connected { conn: id, addr, outbound }).await;
            let writer = tokio::spawn(async move {
                while let Some(frame) = rx.recv().await {
                    if write_frame(&mut w, &frame).await.is_err() {
                        break;
                    }
                }
            });
            let reason = loop {
                match read_frame(&mut r).await {
                    Ok(data) => {
                        if inner.events.send(LanEvent::Frame { conn: id, data }).await.is_err() {
                            break "core stopped".to_string();
                        }
                    }
                    Err(e) => break e.to_string(),
                }
            };
            writer.abort();
            inner.conns.lock().unwrap().remove(&id);
            let _ = inner.events.send(LanEvent::Closed { conn: id, reason }).await;
        });
    }

    pub fn send(&self, conn: ConnId, frame: Vec<u8>) -> bool {
        match self.inner.conns.lock().unwrap().get(&conn) {
            Some(tx) => tx.send(frame).is_ok(),
            None => false,
        }
    }

    pub fn close(&self, conn: ConnId) {
        self.inner.conns.lock().unwrap().remove(&conn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_flow_both_ways() {
        let (ta, mut ra) = mpsc::channel(16);
        let (tb, mut rb) = mpsc::channel(16);
        let a = LanSignaling::bind("127.0.0.1:0".parse().unwrap(), ta).await.unwrap();
        let b = LanSignaling::bind("127.0.0.1:0".parse().unwrap(), tb).await.unwrap();
        let c = a.connect(b.local_addr());
        assert!(matches!(ra.recv().await, Some(LanEvent::Connected { outbound: true, .. })));
        let bc = match rb.recv().await {
            Some(LanEvent::Connected { conn, outbound: false, .. }) => conn,
            other => panic!("{other:?}"),
        };
        assert!(a.send(c, b"ping".to_vec()));
        assert!(matches!(rb.recv().await, Some(LanEvent::Frame { data, .. }) if data == b"ping"));
        assert!(b.send(bc, b"pong".to_vec()));
        assert!(matches!(ra.recv().await, Some(LanEvent::Frame { data, .. }) if data == b"pong"));
    }
}
