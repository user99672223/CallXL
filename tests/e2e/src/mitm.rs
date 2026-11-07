//! Man-in-the-middle for LAN TCP signaling (tampered-SDP tests, §5): relays the
//! length-prefixed envelope frames between a peer and `target`, optionally corrupting
//! offers/answers on the way.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use callxl_signal::framing::{read_frame, write_frame};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tamper {
    /// Relay unchanged.
    None,
    /// Flip one hex digit of the signed `fp` (DTLS fingerprint) field.
    Fingerprint,
    /// Flip one byte of the encrypted payload (ChaCha20-Poly1305 ciphertext).
    Ciphertext,
}

pub struct Mitm {
    pub addr: SocketAddr,
    /// Offers/answers corrupted so far.
    pub tampered: Arc<AtomicU64>,
    /// Offers/answers seen so far.
    pub sdp_frames: Arc<AtomicU64>,
}

fn corrupt(frame: &[u8], mode: Tamper, tampered: &AtomicU64, seen: &AtomicU64) -> Vec<u8> {
    let Ok(mut v) = serde_json::from_slice::<Value>(frame) else { return frame.to_vec() };
    let kind = v["kind"].as_str().unwrap_or_default();
    if kind != "offer" && kind != "answer" {
        return frame.to_vec();
    }
    seen.fetch_add(1, Ordering::Relaxed);
    match mode {
        Tamper::None => return frame.to_vec(),
        Tamper::Fingerprint => {
            let Some(fp) = v["fp"].as_str() else { return frame.to_vec() };
            let mut b = fp.as_bytes().to_vec();
            let digest = b.iter().rposition(|c| *c == b' ').map_or(0, |i| i + 1);
            if let Some(c) = b[digest..].iter_mut().find(|c| c.is_ascii_hexdigit()) {
                *c = if *c == b'0' { b'1' } else { b'0' };
            }
            v["fp"] = Value::String(String::from_utf8(b).unwrap_or_default());
        }
        Tamper::Ciphertext => {
            let Some(p) = v["payload"].as_str() else { return frame.to_vec() };
            let Ok(mut b) = B64.decode(p) else { return frame.to_vec() };
            let i = b.len() / 2;
            b[i] ^= 0x01;
            v["payload"] = Value::String(B64.encode(b));
        }
    }
    tampered.fetch_add(1, Ordering::Relaxed);
    serde_json::to_vec(&v).unwrap_or_else(|_| frame.to_vec())
}

async fn pump(
    mut r: tokio::net::tcp::OwnedReadHalf,
    mut w: tokio::net::tcp::OwnedWriteHalf,
    mode: Tamper,
    tampered: Arc<AtomicU64>,
    seen: Arc<AtomicU64>,
) {
    while let Ok(f) = read_frame(&mut r).await {
        let out = corrupt(&f, mode, &tampered, &seen);
        if write_frame(&mut w, &out).await.is_err() {
            break;
        }
    }
}

impl Mitm {
    pub async fn start(target: SocketAddr, mode: Tamper) -> Result<Self> {
        let listener = TcpListener::bind(SocketAddr::new(target.ip(), 0)).await?;
        let addr = listener.local_addr()?;
        let tampered = Arc::new(AtomicU64::new(0));
        let sdp_frames = Arc::new(AtomicU64::new(0));
        let (t, s) = (tampered.clone(), sdp_frames.clone());
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let Ok(server) = TcpStream::connect(target).await else { continue };
                let (cr, cw) = client.into_split();
                let (sr, sw) = server.into_split();
                tokio::spawn(pump(cr, sw, mode, t.clone(), s.clone()));
                tokio::spawn(pump(sr, cw, mode, t.clone(), s.clone()));
            }
        });
        Ok(Self { addr, tampered, sdp_frames })
    }

    pub fn tampered(&self) -> u64 {
        self.tampered.load(Ordering::Relaxed)
    }

    pub fn sdp_frames(&self) -> u64 {
        self.sdp_frames.load(Ordering::Relaxed)
    }
}
