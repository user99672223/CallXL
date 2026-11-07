//! Identity, signing and peer-to-peer encryption (CLAUDE.md §5).
//!
//! - Ed25519 signs every envelope (`Envelope::signed_bytes`).
//! - Peer-addressed payloads: X25519(sender, recipient) → HKDF-SHA256 (info binds the
//!   direction sender→recipient) → ChaCha20-Poly1305 with a 96-bit nonce from `seq`.
//!   `seq` starts at wall-clock µs and only grows, so a restarted sender never reuses a
//!   nonce under the same static key pair.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chacha20poly1305::aead::{Aead, KeyInit, Payload as AeadPayload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey as XPublic, StaticSecret};

use crate::schema::{Envelope, Kind, VERSION};
use crate::{Result, SignalError, now_ms};

/// Ed25519 public key, lowercase hex (64 chars). The device's identity.
pub type PeerKey = String;

/// Display name + both public keys (§5 "device card").
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceCard {
    pub name: String,
    /// Ed25519 public key, hex.
    pub sign_pk: String,
    /// X25519 public key, hex.
    pub agree_pk: String,
}

impl DeviceCard {
    pub fn key(&self) -> &str {
        &self.sign_pk
    }
    fn agree_bytes(&self) -> Result<[u8; 32]> {
        decode32(&self.agree_pk)
    }
}

fn decode32(h: &str) -> Result<[u8; 32]> {
    let v = hex::decode(h).map_err(|_| SignalError::Malformed("bad hex key".into()))?;
    v.try_into().map_err(|_| SignalError::Malformed("key must be 32 bytes".into()))
}

/// The per-install key pair. Secrets never leave this struct except via
/// [`Identity::secret_bytes`] for the OS keystore.
pub struct Identity {
    signing: SigningKey,
    agreement: StaticSecret,
}

impl Identity {
    pub fn generate() -> Self {
        Self { signing: SigningKey::generate(&mut OsRng), agreement: StaticSecret::random_from_rng(OsRng) }
    }

    /// 64 bytes: Ed25519 seed ‖ X25519 secret.
    pub fn secret_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&self.signing.to_bytes());
        out[32..].copy_from_slice(&self.agreement.to_bytes());
        out
    }

    pub fn from_secret_bytes(b: &[u8]) -> Option<Self> {
        if b.len() != 64 {
            return None;
        }
        let seed: [u8; 32] = b[..32].try_into().ok()?;
        let agree: [u8; 32] = b[32..].try_into().ok()?;
        Some(Self { signing: SigningKey::from_bytes(&seed), agreement: StaticSecret::from(agree) })
    }

    pub fn key(&self) -> PeerKey {
        hex::encode(self.signing.verifying_key().to_bytes())
    }

    pub fn card(&self, name: &str) -> DeviceCard {
        DeviceCard {
            name: name.to_string(),
            sign_pk: self.key(),
            agree_pk: hex::encode(XPublic::from(&self.agreement).to_bytes()),
        }
    }
}

/// 6-word phrase derived from both devices' keys (order-independent), shown on first
/// contact. Both sides compute the same words.
pub fn trust_phrase(a: &DeviceCard, b: &DeviceCard) -> String {
    let mut pair = [format!("{}{}", a.sign_pk, a.agree_pk), format!("{}{}", b.sign_pk, b.agree_pk)];
    pair.sort();
    let mut h = blake3::Hasher::new();
    h.update(b"callxl-trust-phrase-v1");
    h.update(pair[0].as_bytes());
    h.update(pair[1].as_bytes());
    let digest = h.finalize();
    digest.as_bytes()[..6].iter().map(|b| crate::words::WORDS[*b as usize]).collect::<Vec<_>>().join(" ")
}

/// Signs/verifies envelopes and encrypts peer-addressed payloads.
pub struct SignalCrypto {
    identity: Identity,
    key: PeerKey,
    seq: AtomicU64,
    keys: Mutex<HashMap<(String, bool), [u8; 32]>>,
    last_seq: Mutex<HashMap<String, u64>>,
}

impl SignalCrypto {
    pub fn new(identity: Identity) -> Self {
        let key = identity.key();
        Self {
            identity,
            key,
            seq: AtomicU64::new(now_ms().saturating_mul(1000)),
            keys: Mutex::new(HashMap::new()),
            last_seq: Mutex::new(HashMap::new()),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn card(&self, name: &str) -> DeviceCard {
        self.identity.card(name)
    }

    /// Direction-bound symmetric key: `outbound` = self→peer, else peer→self.
    fn pair_key(&self, peer: &DeviceCard, outbound: bool) -> Result<[u8; 32]> {
        let cache_key = (format!("{}{}", peer.sign_pk, peer.agree_pk), outbound);
        if let Some(k) = self.keys.lock().unwrap().get(&cache_key) {
            return Ok(*k);
        }
        let shared = self.identity.agreement.diffie_hellman(&XPublic::from(peer.agree_bytes()?));
        let (from, to) = if outbound {
            (self.key.as_str(), peer.sign_pk.as_str())
        } else {
            (peer.sign_pk.as_str(), self.key.as_str())
        };
        let hk = Hkdf::<Sha256>::new(Some(b"callxl-v1"), shared.as_bytes());
        let mut okm = [0u8; 32];
        let info = format!("callxl msg {from}->{to}");
        hk.expand(info.as_bytes(), &mut okm).map_err(|_| SignalError::Decrypt)?;
        self.keys.lock().unwrap().insert(cache_key, okm);
        Ok(okm)
    }

    fn nonce(seq: u64) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&seq.to_be_bytes());
        n
    }

    fn aad(kind: Kind, sid: Option<&str>, fp: Option<&str>) -> Vec<u8> {
        format!("{}|{}|{}", kind.as_str(), sid.unwrap_or(""), fp.unwrap_or("")).into_bytes()
    }

    /// Builds and signs an envelope. `to` = recipient card for peer-addressed kinds
    /// (payload is then encrypted), or `None` for broadcast/session ops (signed only).
    pub fn seal(
        &self,
        kind: Kind,
        to: Option<&DeviceCard>,
        sid: Option<&str>,
        fp: Option<&str>,
        plaintext: &[u8],
    ) -> Result<Envelope> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let payload = if kind.is_peer_addressed() {
            let peer = to.ok_or(SignalError::Malformed("peer-addressed message without recipient".into()))?;
            let key = self.pair_key(peer, true)?;
            let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
            cipher
                .encrypt(
                    Nonce::from_slice(&Self::nonce(seq)),
                    AeadPayload { msg: plaintext, aad: &Self::aad(kind, sid, fp) },
                )
                .map_err(|_| SignalError::Decrypt)?
        } else {
            plaintext.to_vec()
        };
        let mut env = Envelope {
            v: VERSION,
            from: self.key.clone(),
            to: to.map(|c| c.sign_pk.clone()),
            seq,
            ts_ms: now_ms(),
            kind,
            fp: fp.map(str::to_string),
            sid: sid.map(str::to_string),
            payload,
            sig: Vec::new(),
        };
        env.sig = self.identity.signing.sign(&env.signed_bytes()).to_bytes().to_vec();
        Ok(env)
    }

    /// Verifies the Ed25519 signature against the key named in `from`.
    pub fn verify(env: &Envelope) -> Result<()> {
        let pk = decode32(&env.from).map_err(|_| SignalError::BadSignature)?;
        let vk = VerifyingKey::from_bytes(&pk).map_err(|_| SignalError::BadSignature)?;
        let sig: [u8; 64] = env.sig.as_slice().try_into().map_err(|_| SignalError::BadSignature)?;
        vk.verify(&env.signed_bytes(), &Signature::from_bytes(&sig)).map_err(|_| SignalError::BadSignature)
    }

    /// Verifies and, for peer-addressed kinds, checks the recipient, rejects replays and
    /// decrypts. `sender` is the sender's card (from its hello); required to decrypt.
    pub fn open(&self, env: &Envelope, sender: Option<&DeviceCard>) -> Result<Vec<u8>> {
        Self::verify(env)?;
        if !env.kind.is_peer_addressed() {
            return Ok(env.payload.clone());
        }
        if env.to.as_deref() != Some(self.key.as_str()) {
            return Err(SignalError::NotForUs);
        }
        let sender = sender.ok_or(SignalError::UnknownSender)?;
        if sender.sign_pk != env.from {
            return Err(SignalError::UnknownSender);
        }
        let key = self.pair_key(sender, false)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let plain = cipher
            .decrypt(
                Nonce::from_slice(&Self::nonce(env.seq)),
                AeadPayload { msg: &env.payload, aad: &Self::aad(env.kind, env.sid.as_deref(), env.fp.as_deref()) },
            )
            .map_err(|_| SignalError::Decrypt)?;
        let mut last = self.last_seq.lock().unwrap();
        let entry = last.entry(env.from.clone()).or_insert(0);
        if env.seq <= *entry {
            return Err(SignalError::Replay);
        }
        *entry = env.seq;
        Ok(plain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (SignalCrypto, SignalCrypto) {
        (SignalCrypto::new(Identity::generate()), SignalCrypto::new(Identity::generate()))
    }

    #[test]
    fn sign_encrypt_roundtrip() {
        let (a, b) = pair();
        let (ca, cb) = (a.card("alice"), b.card("bob"));
        let env = a.seal(Kind::Offer, Some(&cb), Some("s"), Some("sha-256 AA"), b"v=0 sdp").unwrap();
        assert_ne!(env.payload, b"v=0 sdp");
        let wire = Envelope::from_bytes(&env.to_bytes()).unwrap();
        assert_eq!(b.open(&wire, Some(&ca)).unwrap(), b"v=0 sdp");
        // replay
        assert!(matches!(b.open(&wire, Some(&ca)), Err(SignalError::Replay)));
    }

    #[test]
    fn tampering_rejected() {
        let (a, b) = pair();
        let (ca, cb) = (a.card("alice"), b.card("bob"));
        let env = a.seal(Kind::Offer, Some(&cb), None, Some("sha-256 AA:BB"), b"secret").unwrap();
        let mut t = env.clone();
        t.payload[0] ^= 1;
        assert!(matches!(b.open(&t, Some(&ca)), Err(SignalError::BadSignature)));
        let mut t = env.clone();
        t.fp = Some("sha-256 AA:BC".into());
        assert!(matches!(b.open(&t, Some(&ca)), Err(SignalError::BadSignature)));
        // Re-signed by an attacker with another key: signature fine, decryption fails.
        let m = SignalCrypto::new(Identity::generate());
        let mut forged = m.seal(Kind::Offer, Some(&cb), None, None, b"x").unwrap();
        forged.from = ca.sign_pk.clone();
        assert!(b.open(&forged, Some(&ca)).is_err());
        // Wrong recipient.
        let c = SignalCrypto::new(Identity::generate());
        assert!(matches!(c.open(&env, Some(&ca)), Err(SignalError::NotForUs)));
    }

    #[test]
    fn identity_persists() {
        let id = Identity::generate();
        let again = Identity::from_secret_bytes(&id.secret_bytes()).unwrap();
        assert_eq!(id.card("x"), again.card("x"));
    }

    #[test]
    fn phrase_symmetric_six_words() {
        let (a, b) = pair();
        let (ca, cb) = (a.card("alice"), b.card("bob"));
        let p = trust_phrase(&ca, &cb);
        assert_eq!(p, trust_phrase(&cb, &ca));
        assert_eq!(p.split(' ').count(), 6);
        let c = SignalCrypto::new(Identity::generate()).card("alice");
        assert_ne!(p, trust_phrase(&c, &cb));
    }
}
