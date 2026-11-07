//! Cloud signaling client: Worker token/STUN/TURN fetch and WebSocket links to the
//! Lobby and Session Durable Objects, with reconnect + exponential backoff (§4
//! "Resilience": a dropped signaling socket never touches media).

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::{Result, SignalError};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCredentials {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    #[serde(default)]
    pub ttl: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token: String,
    #[serde(default)]
    pub stun: Vec<String>,
    #[serde(default)]
    pub turn: Option<TurnCredentials>,
    /// Token lifetime in seconds.
    #[serde(default)]
    pub expires_in: u64,
}

/// Built-in STUN defaults used when the Worker is unreachable (§4).
pub const DEFAULT_STUN: [&str; 2] = ["stun:stun.cloudflare.com:3478", "stun:stun.l.google.com:19302"];

/// `wss://host/ws` → `https://host`; `ws://127.0.0.1:8787/ws` → `http://127.0.0.1:8787`.
pub fn http_base(signaling_url: &str) -> String {
    let (scheme, rest) = if let Some(r) = signaling_url.strip_prefix("wss://") {
        ("https://", r)
    } else if let Some(r) = signaling_url.strip_prefix("ws://") {
        ("http://", r)
    } else {
        ("https://", signaling_url)
    };
    let host = rest.split('/').next().unwrap_or(rest);
    format!("{scheme}{host}")
}

pub fn ws_base(signaling_url: &str) -> String {
    let http = http_base(signaling_url);
    http.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1)
}

/// `POST {base}/token {app_secret}`.
pub async fn fetch_token(signaling_url: &str, app_secret: &str) -> Result<TokenResponse> {
    let url = format!("{}/token", http_base(signaling_url));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| SignalError::Network(e.to_string()))?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "app_secret": app_secret }))
        .send()
        .await
        .map_err(|e| SignalError::Network(e.to_string()))?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(SignalError::Unauthorized);
    }
    if !resp.status().is_success() {
        return Err(SignalError::Network(format!("token request failed with HTTP {}", resp.status())));
    }
    resp.json().await.map_err(|e| SignalError::Network(e.to_string()))
}

#[derive(Debug)]
pub enum LinkEvent {
    Connected,
    Disconnected {
        reason: String,
    },
    Frame(Vec<u8>),
    /// WebSocket ping round trip.
    Latency(Duration),
}

/// Handle to a reconnecting WebSocket. Dropping it closes the link.
pub struct WsLink {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    task: tokio::task::JoinHandle<()>,
}

impl WsLink {
    /// `url_fn` is called before every (re)connect so a fresh token can be used.
    /// `on_connect` frames (e.g. a hello) are sent first on every new connection.
    pub fn spawn<F, Fut>(url_fn: F, on_connect: Vec<Vec<u8>>, events: mpsc::Sender<LinkEvent>) -> Self
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Option<String>> + Send,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let task = tokio::spawn(async move {
            let mut backoff = Duration::from_millis(250);
            loop {
                let Some(url) = url_fn().await else {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(15));
                    continue;
                };
                match tokio_tungstenite::connect_async(url.as_str()).await {
                    Ok((ws, _)) => {
                        backoff = Duration::from_millis(250);
                        let (mut sink, mut stream) = ws.split();
                        let _ = events.send(LinkEvent::Connected).await;
                        let mut ok = true;
                        for f in &on_connect {
                            if sink.send(Message::Binary(f.clone().into())).await.is_err() {
                                ok = false;
                            }
                        }
                        let mut ping = tokio::time::interval(Duration::from_secs(5));
                        let mut ping_sent: Option<Instant> = None;
                        let reason = loop {
                            if !ok {
                                break "send failed".to_string();
                            }
                            tokio::select! {
                                out = rx.recv() => match out {
                                    Some(b) => if let Err(e) = sink.send(Message::Binary(b.into())).await { break e.to_string() },
                                    None => { let _ = sink.close().await; return; }
                                },
                                msg = stream.next() => match msg {
                                    Some(Ok(Message::Binary(b))) => { let _ = events.send(LinkEvent::Frame(b.to_vec())).await; }
                                    Some(Ok(Message::Text(t))) => { let _ = events.send(LinkEvent::Frame(t.as_bytes().to_vec())).await; }
                                    Some(Ok(Message::Pong(_))) => if let Some(t) = ping_sent.take() { let _ = events.send(LinkEvent::Latency(t.elapsed())).await; },
                                    Some(Ok(Message::Close(_))) | None => break "closed by server".to_string(),
                                    Some(Ok(_)) => {}
                                    Some(Err(e)) => break e.to_string(),
                                },
                                _ = ping.tick() => {
                                    ping_sent = Some(Instant::now());
                                    if let Err(e) = sink.send(Message::Ping(Vec::new().into())).await { break e.to_string() }
                                }
                            }
                        };
                        let _ = events.send(LinkEvent::Disconnected { reason }).await;
                    }
                    Err(e) => {
                        let _ = events.send(LinkEvent::Disconnected { reason: e.to_string() }).await;
                    }
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(15));
            }
        });
        Self { tx, task }
    }

    /// Queues a frame; frames sent while disconnected are dropped by the server side
    /// semantics anyway (signaling is re-driven on reconnect).
    pub fn send(&self, frame: Vec<u8>) {
        let _ = self.tx.send(frame);
    }
}

impl Drop for WsLink {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_mapping() {
        assert_eq!(http_base("ws://127.0.0.1:8787/ws"), "http://127.0.0.1:8787");
        assert_eq!(http_base("wss://callxl.acct.workers.dev/ws"), "https://callxl.acct.workers.dev");
        assert_eq!(ws_base("wss://callxl.acct.workers.dev/ws"), "wss://callxl.acct.workers.dev");
    }
}
