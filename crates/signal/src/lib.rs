//! CallXL signaling (CLAUDE.md §4, §5).
//!
//! The same [`Envelope`] bytes travel over LAN TCP (length-prefixed frames, see
//! [`framing`]) and the cloud WebSocket (binary frames, see [`lobby`]). Payloads
//! addressed to a peer are encrypted to that peer; every envelope is signed.

pub mod crypto;
pub mod framing;
pub mod lobby;
pub mod negotiation;
pub mod schema;
mod words;

pub use crypto::{DeviceCard, Identity, PeerKey, SignalCrypto, trust_phrase};
pub use schema::{Envelope, Kind, Payload, SessionInfo};

#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    #[error("the message signature is invalid")]
    BadSignature,
    #[error("the message could not be decrypted (wrong key or tampered)")]
    Decrypt,
    #[error("the message was replayed or arrived out of order")]
    Replay,
    #[error("the message is malformed: {0}")]
    Malformed(String),
    #[error("the message is not addressed to this device")]
    NotForUs,
    #[error("the sender's keys are unknown (no hello received yet)")]
    UnknownSender,
    #[error("the signaling server is unreachable: {0}")]
    Network(String),
    #[error("the signaling server rejected the app secret")]
    Unauthorized,
}

pub type Result<T> = std::result::Result<T, SignalError>;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
