//! The core task: the only owner of call state and the only caller into `callxl-media`
//! (CLAUDE.md §10). Everything arrives here as a message — UI commands, media events,
//! LAN/cloud signaling frames, discovery results, timers — and is handled in order.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use callxl_lan::{ConnId, Discovery, DiscoveryEvent, LanAdvert, LanEvent, LanSignaling};
use callxl_media::{AudioTrack, MediaEngine, MediaEvent, PeerId};
use callxl_signal::lobby::{LinkEvent, TokenResponse, WsLink};
use callxl_signal::schema::{self, HelloPayload, MemberPayload, SessionRef, SessionsPayload};
use callxl_signal::{DeviceCard as Card, Envelope, Kind, SessionInfo, SignalCrypto, now_ms, trust_phrase};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use crate::api::Shared;
use crate::config;
use crate::diag::DiagInputs;
use crate::link::Link;
use crate::store::Store;
use crate::types::*;

/// Rust-only options used by `callxl-cli` and tests (not part of the UniFFI API).
#[derive(Clone, Debug, Default)]
pub struct DevOptions {
    /// `None` = platform ADM. `Some((play, record))` = C++ test ADM.
    pub test_adm: Option<(String, String)>,
    /// Restrict ICE candidates and LAN listening to these local addresses.
    pub bind: Vec<IpAddr>,
    pub lan_port: u16,
    /// LAN peers to connect to directly (no mDNS needed).
    pub lan_peers: Vec<SocketAddr>,
    pub mdns: bool,
    pub lan: bool,
    pub cloud: bool,
    /// Classify host↔host pairs as internet (`Direct`) paths.
    pub force_internet: bool,
    /// Userspace impairment relay control address (D-0003 proxy backend).
    pub candidate_rewrite: Option<SocketAddr>,
    pub reset_identity: bool,
    pub ice_policy: Option<callxl_media::IcePolicy>,
}

impl DevOptions {
    pub fn production() -> Self {
        Self { mdns: true, lan: true, cloud: true, ..Default::default() }
    }
}

pub(crate) enum Command {
    SetDisplayName(String),
    Trust(PeerKey),
    Untrust(PeerKey),
    CreateSession { id: SessionId, name: Option<String> },
    Join(SessionId),
    Leave,
    SetMuted(bool),
    SetInputDevice(Option<String>),
    SetOutputDevice(Option<String>),
    SetApm(ApmConfig),
    SetRawMic(bool),
    SetBitrate(u32),
    SendChat { id: MessageId, body: String },
    MarkRead(MessageId),
    OfferFile { id: TransferId, path: String, recipients: Vec<PeerKey> },
    RespondFile { id: TransferId, accept: bool },
    CancelTransfer(TransferId),
    SetSignalingUrl(Option<String>),
    SetDownloadDir(String),
    Debug { name: String, args: Value, reply: oneshot::Sender<Result<Value, String>> },
    Stop(oneshot::Sender<()>),
}

/// Messages the core sends itself (timers, background results).
pub(crate) enum Internal {
    SendCandidate {
        key: PeerKey,
        peer: PeerId,
        mid: String,
        mline: i32,
        candidate: String,
    },
    Stats(DiagnosticsSnapshot),
    TurnBytes(u64),
    Token(Option<TokenResponse>),
    HashDone {
        id: TransferId,
        path: String,
        name: String,
        size: u64,
        hash: Result<String, String>,
        recipients: Vec<PeerKey>,
    },
}

pub(crate) struct PeerEntry {
    pub card: Card,
    pub instance: String,
    pub lan_conn: Option<ConnId>,
    pub lan_sessions: Vec<SessionInfo>,
    pub cloud_sessions: HashSet<SessionId>,
    pub last_hello_sent: Option<Instant>,
    pub lan_latency_ms: Option<f64>,
}

pub(crate) struct Cloud {
    pub lobby: WsLink,
    pub session: Option<(SessionId, WsLink)>,
    pub connected: bool,
    pub session_connected: bool,
    pub latency_ms: Option<f64>,
    pub token: Option<TokenResponse>,
}

pub(crate) struct MySession {
    pub id: SessionId,
    pub name: Option<String>,
    pub created_ms: u64,
}

pub(crate) struct Core {
    pub shared: Arc<Shared>,
    pub store: Arc<Store>,
    pub crypto: Arc<SignalCrypto>,
    pub instance: String,
    pub dev: DevOptions,
    pub settings: Settings,
    pub engine: Arc<MediaEngine>,
    pub mic: AudioTrack,
    pub muted: bool,
    pub media_tx: mpsc::Sender<MediaEvent>,
    pub internal_tx: mpsc::UnboundedSender<Internal>,
    pub lan: Option<LanSignaling>,
    pub discovery: Option<Discovery>,
    pub lan_conn_keys: HashMap<ConnId, PeerKey>,
    pub cloud: Option<Cloud>,
    pub cloud_tx: mpsc::Sender<(bool, LinkEvent)>,
    pub dir: HashMap<PeerKey, PeerEntry>,
    pub cloud_sessions: Vec<SessionInfo>,
    pub my_session: Option<MySession>,
    pub call_state: CallState,
    pub links: HashMap<PeerKey, Link>,
    pub peer_ids: HashMap<PeerId, PeerKey>,
    pub next_peer_id: PeerId,
    pub prompted: HashSet<PeerKey>,
    pub transfers: crate::files::Transfers,
    pub rejected_signaling: u64,
    /// Test hook (`debug forge_fingerprint`): sign offers/answers with a DTLS fingerprint
    /// that does not match the SDP — a validly signed but lying peer.
    pub debug_forge_fingerprint: bool,
    pub glare_events: u64,
    pub link_views: watch::Sender<DiagInputs>,
    pub last_stats: Option<DiagnosticsSnapshot>,
    pub stopping: bool,
    pub cloud_url_fn: Option<UrlFn>,
    pub dialing: HashMap<PeerKey, Instant>,
}

pub(crate) type UrlFn =
    Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> + Send + Sync>;

impl Core {
    pub fn emit(&self, e: Event) {
        self.shared.emit(e);
    }

    pub fn error(&self, reason: impl Into<String>) {
        let reason = reason.into();
        tracing::warn!("{reason}");
        self.emit(Event::Error { reason });
    }

    pub fn my_key(&self) -> String {
        self.crypto.key().to_string()
    }

    pub fn my_card(&self) -> Card {
        self.crypto.card(&self.settings.display_name)
    }

    pub fn current_session(&self) -> Option<&str> {
        self.my_session.as_ref().map(|s| s.id.as_str())
    }

    // ------------------------------------------------------------------ run loop

    pub async fn run(
        mut self,
        mut cmd_rx: mpsc::UnboundedReceiver<Command>,
        mut media_rx: mpsc::Receiver<MediaEvent>,
        mut internal_rx: mpsc::UnboundedReceiver<Internal>,
        mut lan_rx: mpsc::Receiver<LanEvent>,
        mut disc_rx: mpsc::Receiver<DiscoveryEvent>,
        mut cloud_rx: mpsc::Receiver<(bool, LinkEvent)>,
    ) {
        for addr in self.dev.lan_peers.clone() {
            if let Some(lan) = &self.lan {
                lan.connect(addr);
            }
        }
        self.start_cloud();
        self.publish();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Paces throttled (internet-path) file sends: the token bucket refills continuously
        // but is only drained when chunks are pumped. Armed only while such a send runs.
        let mut pace = tokio::time::interval(Duration::from_millis(20));
        pace.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let pacing = self.throttled_sends();
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    if let Command::Stop(reply) = cmd {
                        self.shutdown().await;
                        let _ = reply.send(());
                        return;
                    }
                    self.on_command(cmd).await;
                }
                Some(ev) = media_rx.recv() => self.on_media(ev).await,
                Some(i) = internal_rx.recv() => self.on_internal(i).await,
                Some(ev) = lan_rx.recv() => self.on_lan(ev).await,
                Some(ev) = disc_rx.recv() => self.on_discovery(ev),
                Some((session, ev)) = cloud_rx.recv() => self.on_cloud(session, ev).await,
                _ = tick.tick() => self.on_tick().await,
                _ = pace.tick(), if pacing => {
                    for key in self.links.keys().cloned().collect::<Vec<_>>() {
                        self.pump_file(&key, "");
                    }
                }
                else => return,
            }
        }
    }

    async fn shutdown(&mut self) {
        self.stopping = true;
        if self.my_session.is_some() {
            self.leave("you left the session").await;
        }
        self.links.clear();
        self.cloud = None;
        self.discovery = None;
        self.engine.test_adm_flush();
    }

    // ------------------------------------------------------------------ commands

    async fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::SetDisplayName(n) => {
                self.settings.display_name = n;
                self.save_settings();
                self.broadcast_hello();
                self.publish();
            }
            Command::Trust(key) => self.trust(&key, true),
            Command::Untrust(key) => self.trust(&key, false),
            Command::CreateSession { id, name } => {
                if let Some(c) = &self.cloud
                    && let Ok(env) = self.crypto.seal(
                        Kind::SessionCreate,
                        None,
                        Some(&id),
                        None,
                        &schema::encode(&SessionRef { id: id.clone(), name: name.clone() }),
                    )
                {
                    c.lobby.send(env.to_bytes());
                }
                self.join(id, name).await;
            }
            Command::Join(id) => {
                let name = self.session_name(&id);
                self.join(id, name).await;
            }
            Command::Leave => self.leave("you left the session").await,
            Command::SetMuted(m) => {
                self.muted = m;
                self.mic.set_enabled(!m);
                self.publish();
            }
            Command::SetInputDevice(id) => {
                if !self.engine.set_input_device(id.as_deref()) {
                    self.error("that microphone could not be selected");
                }
                self.settings.input_device = id;
                self.save_settings();
                self.emit(Event::DevicesChanged);
            }
            Command::SetOutputDevice(id) => {
                if !self.engine.set_output_device(id.as_deref()) {
                    self.error("that speaker could not be selected");
                }
                self.settings.output_device = id;
                self.save_settings();
                self.emit(Event::DevicesChanged);
            }
            Command::SetApm(cfg) => {
                self.settings.apm = cfg;
                self.settings.raw_mic = cfg == ApmConfig::RAW;
                self.apply_apm();
            }
            Command::SetRawMic(raw) => {
                self.settings.raw_mic = raw;
                self.settings.apm = if raw { ApmConfig::RAW } else { ApmConfig::default() };
                self.apply_apm();
            }
            Command::SetBitrate(kbps) => {
                self.settings.bitrate_ceiling_kbps = kbps.clamp(config::MIN_CEILING_KBPS, config::MAX_CEILING_KBPS);
                self.save_settings();
                // Applied locally and live: the ANA caps the Opus target, BWE max follows.
                // No renegotiation (it would re-create the encoder) and no new start rate
                // (it would reset the estimate): both caused audible gaps (DECISIONS.md).
                let keys: Vec<_> = self.links.keys().cloned().collect();
                for k in keys {
                    self.apply_ceiling(&k);
                }
                self.publish();
            }
            Command::SendChat { id, body } => self.send_chat(id, body),
            Command::MarkRead(id) => self.mark_read(&id),
            Command::OfferFile { id, path, recipients } => self.offer_file(id, path, recipients),
            Command::RespondFile { id, accept } => self.respond_file(&id, accept),
            Command::CancelTransfer(id) => self.cancel_transfer(&id),
            Command::SetSignalingUrl(url) => {
                self.settings.signaling_url = url.clone();
                self.settings.effective_signaling_url = url.unwrap_or_else(|| config::DEFAULT_SIGNALING_URL.into());
                self.save_settings();
                self.cloud = None;
                self.start_cloud();
                self.publish();
            }
            Command::SetDownloadDir(d) => {
                self.settings.download_dir = d;
                self.save_settings();
                self.publish();
            }
            Command::Debug { name, args, reply } => {
                let r = self.debug(&name, args).await;
                let _ = reply.send(r);
            }
            Command::Stop(_) => {}
        }
    }

    fn apply_apm(&mut self) {
        let want = self.settings.apm.to_media();
        if !self.engine.apply_apm(want) {
            self.error("audio processing settings could not be applied");
        }
        self.save_settings();
        self.publish();
    }

    pub fn save_settings(&self) {
        if let Ok(j) = serde_json::to_string(&self.settings) {
            self.store.set("settings", &j);
        }
    }

    async fn debug(&mut self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "renegotiate" => {
                let keys: Vec<_> = self.links.keys().cloned().collect();
                for k in &keys {
                    if let Some(l) = self.links.get_mut(k) {
                        l.force_offer = true;
                    }
                    self.make_offer(k).await;
                }
                Ok(json!({ "links": keys.len() }))
            }
            "test_adm_flush" => Ok(json!(self.engine.test_adm_flush())),
            "test_adm_stats" => Ok(serde_json::to_value(self.engine.test_adm_stats()).unwrap_or(Value::Null)),
            "field_trial" => {
                let n = args.get("name").and_then(Value::as_str).unwrap_or_default();
                Ok(json!(self.engine.field_trial(n)))
            }
            "link_info" => {
                let v: Vec<Value> = self
                    .links
                    .values()
                    .map(|l| {
                        json!({
                            "key": l.key,
                            "local_sdp": l.pc.local_description(),
                            "signaling_state": l.pc.signaling_state().repr,
                            "connected": l.connected,
                            "polite": l.negotiator.polite,
                            "offers_sent": l.offers_sent,
                            "controller_decisions": l.controller_decisions,
                        })
                    })
                    .collect();
                Ok(Value::Array(v))
            }
            // The build-time secret (already in the binary): lets the e2e harness start a
            // local Worker that accepts this build (signaling_roundtrip_wrangler).
            "app_secret" => Ok(json!(config::APP_SECRET)),
            "forge_fingerprint" => {
                self.debug_forge_fingerprint = args.get("on").and_then(Value::as_bool).unwrap_or(true);
                Ok(json!(self.debug_forge_fingerprint))
            }
            "restart_ice" => {
                let keys: Vec<_> = self.links.keys().cloned().collect();
                for k in &keys {
                    if let Some(l) = self.links.get(k) {
                        l.pc.restart_ice();
                    }
                }
                Ok(json!(keys.len()))
            }
            other => Err(format!("unknown debug command '{other}'")),
        }
    }

    // ------------------------------------------------------------------ trust

    fn trust(&mut self, key: &str, pin: bool) {
        if pin {
            let (name, agree) = match self.dir.get(key) {
                Some(e) => (e.card.name.clone(), e.card.agree_pk.clone()),
                None => match self.store.trusted().into_iter().find(|t| t.0 == key) {
                    Some(t) => (t.1, t.2),
                    None => {
                        self.error("that device is not known yet");
                        return;
                    }
                },
            };
            // A re-trusted device replaces any old key pinned under the same name.
            for (k, n, _, _) in self.store.trusted() {
                if n == name && k != key {
                    self.store.unpin(&k);
                }
            }
            self.store.pin(key, &name, &agree, now_ms());
        } else {
            self.store.unpin(key);
        }
        self.emit(Event::TrustedDevicesChanged { devices: self.trusted_devices() });
        self.publish();
    }

    pub fn trusted_devices(&self) -> Vec<TrustedDevice> {
        self.store
            .trusted()
            .into_iter()
            .map(|(key, name, _, ts)| TrustedDevice {
                online: self.dir.contains_key(&key),
                key,
                display_name: name,
                pinned_ms: ts,
            })
            .collect()
    }

    pub fn is_trusted(&self, key: &str) -> bool {
        self.store.trusted().iter().any(|t| t.0 == key)
    }

    /// First contact → 6-word phrase; a pinned name with a new key → "key changed".
    fn maybe_prompt(&mut self, card: &Card) {
        if self.is_trusted(&card.sign_pk) || !self.prompted.insert(card.sign_pk.clone()) {
            return;
        }
        let key_changed = self.store.trusted().iter().any(|t| t.1 == card.name && t.0 != card.sign_pk);
        self.emit(Event::TrustPrompt {
            peer_key: card.sign_pk.clone(),
            display_name: card.name.clone(),
            phrase: trust_phrase(&self.my_card(), card),
            key_changed,
        });
    }

    // ------------------------------------------------------------------ sessions

    fn session_name(&self, id: &str) -> Option<String> {
        self.sessions().into_iter().find(|s| s.id == id).and_then(|s| s.name)
    }

    pub fn my_sessions_info(&self) -> Vec<SessionInfo> {
        match &self.my_session {
            Some(s) => {
                let mut members: BTreeSet<String> = self.members_of(&s.id);
                members.insert(self.my_key());
                vec![SessionInfo {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    members: members.into_iter().collect(),
                    created_ms: s.created_ms,
                }]
            }
            None => Vec::new(),
        }
    }

    /// Other devices that are members of `sid` (LAN reports ∪ cloud session).
    pub fn members_of(&self, sid: &str) -> BTreeSet<String> {
        let me = self.my_key();
        self.dir
            .iter()
            .filter(|(k, e)| {
                **k != me && (e.lan_sessions.iter().any(|s| s.id == sid) || e.cloud_sessions.contains(sid))
            })
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn sessions(&self) -> Vec<SessionSummary> {
        let mut map: HashMap<String, SessionSummary> = HashMap::new();
        let name_of = |k: &str, me: &Core| -> String {
            if k == me.my_key() {
                return me.settings.display_name.clone();
            }
            me.dir.get(k).map(|e| e.card.name.clone()).unwrap_or_else(|| k[..k.len().min(8)].to_string())
        };
        let add = |info: &SessionInfo, lan: bool, cloud: bool, me: &Core, map: &mut HashMap<String, SessionSummary>| {
            let entry = map.entry(info.id.clone()).or_insert_with(|| SessionSummary {
                id: info.id.clone(),
                name: info.name.clone(),
                members: Vec::new(),
                lan: false,
                cloud: false,
                joined: me.current_session() == Some(info.id.as_str()),
            });
            entry.lan |= lan;
            entry.cloud |= cloud;
            if entry.name.is_none() {
                entry.name = info.name.clone();
            }
            for m in &info.members {
                if !entry.members.iter().any(|x| &x.key == m) {
                    entry.members.push(SessionMember { key: m.clone(), display_name: name_of(m, me) });
                }
            }
        };
        for e in self.dir.values() {
            for s in &e.lan_sessions {
                add(s, e.lan_conn.is_some(), false, self, &mut map);
            }
        }
        for s in &self.cloud_sessions {
            add(s, false, true, self, &mut map);
        }
        for s in self.my_sessions_info() {
            add(&s, self.lan.is_some(), self.cloud.as_ref().is_some_and(|c| c.session_connected), self, &mut map);
        }
        let mut v: Vec<_> = map.into_values().filter(|s| !s.members.is_empty()).collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    async fn join(&mut self, id: SessionId, name: Option<String>) {
        if self.current_session() == Some(id.as_str()) {
            return;
        }
        if self.my_session.is_some() {
            self.leave("you joined another session").await;
        }
        self.my_session = Some(MySession { id: id.clone(), name, created_ms: now_ms() });
        self.set_call_state(CallState::Joining);
        self.broadcast_hello();
        self.join_cloud_session(&id);
        self.reconcile().await;
        self.publish_sessions();
    }

    pub async fn leave(&mut self, reason: &str) {
        let Some(s) = self.my_session.take() else { return };
        let keys: Vec<_> = self.links.keys().cloned().collect();
        for k in keys {
            self.send_peer(&k, Kind::Bye, &schema::TextPayload { reason: reason.to_string() });
            self.drop_link(&k, reason);
        }
        if let Some(c) = &mut self.cloud {
            if let Ok(env) = self.crypto.seal(
                Kind::SessionLeave,
                None,
                Some(&s.id),
                None,
                &schema::encode(&SessionRef { id: s.id.clone(), name: None }),
            ) {
                c.lobby.send(env.to_bytes());
            }
            c.session = None;
            c.session_connected = false;
        }
        for e in self.dir.values_mut() {
            e.cloud_sessions.clear();
        }
        self.broadcast_hello();
        self.set_call_state(CallState::Left { reason: reason.to_string() });
        self.publish_sessions();
    }

    pub fn set_call_state(&mut self, s: CallState) {
        if self.call_state != s {
            self.call_state = s.clone();
            self.emit(Event::CallStateChanged { state: s });
            self.publish();
        }
    }

    /// Brings the mesh in line with the current session's membership.
    pub async fn reconcile(&mut self) {
        let Some(sid) = self.current_session().map(str::to_string) else { return };
        let members = self.members_of(&sid);
        for k in &members {
            if !self.links.contains_key(k) {
                self.create_link(k).await;
            }
        }
        let gone: Vec<_> = self.links.keys().filter(|k| !members.contains(*k)).cloned().collect();
        for k in gone {
            self.drop_link(&k, "left the session");
        }
        self.update_call_state();
    }

    pub fn update_call_state(&mut self) {
        if self.my_session.is_none() {
            return;
        }
        let total = self.links.len();
        let connected = self.links.values().filter(|l| l.connected).count();
        let reconnecting = self.links.values().find(|l| l.reconnecting);
        let next = if let Some(l) = reconnecting {
            CallState::Reconnecting { reason: format!("lost the connection to {}; reconnecting", l.name) }
        } else if total == 0 || connected > 0 {
            CallState::InCall
        } else {
            CallState::Joining
        };
        self.set_call_state(next);
    }

    // ------------------------------------------------------------------ signaling out

    fn hello_payload(&self, reply: bool) -> HelloPayload {
        HelloPayload {
            card: self.my_card(),
            sessions: self.my_sessions_info(),
            lan_port: self.lan.as_ref().map(|l| l.local_addr().port()),
            reply,
            instance: self.instance.clone(),
        }
    }

    pub fn hello_bytes(&self, reply: bool, sid: Option<&str>) -> Option<Vec<u8>> {
        self.crypto
            .seal(Kind::Hello, None, sid, None, &schema::encode(&self.hello_payload(reply)))
            .ok()
            .map(|e| e.to_bytes())
    }

    /// Re-announces our card and session membership to every LAN peer (and the cloud session).
    pub fn broadcast_hello(&mut self) {
        if let (Some(lan), Some(b)) = (&self.lan, self.hello_bytes(false, None)) {
            for conn in self.lan_conn_keys.keys() {
                lan.send(*conn, b.clone());
            }
        }
        if let Some(c) = &self.cloud
            && let Some((sid, link)) = &c.session
            && let Some(b) = self.hello_bytes(false, Some(sid))
        {
            link.send(b);
        }
    }

    /// Seals `payload` to `key` and sends it over LAN if possible, else the cloud session.
    pub fn send_peer<T: serde::Serialize>(&mut self, key: &str, kind: Kind, payload: &T) -> bool {
        self.send_peer_fp(key, kind, payload, None)
    }

    pub fn send_peer_fp<T: serde::Serialize>(&mut self, key: &str, kind: Kind, payload: &T, fp: Option<&str>) -> bool {
        let Some(entry) = self.dir.get(key) else { return false };
        let sid = self.current_session().map(str::to_string);
        let forged = fp.filter(|_| self.debug_forge_fingerprint).map(|f| {
            let mut b = f.to_string().into_bytes();
            let digest = b.iter().rposition(|c| *c == b' ').map_or(0, |i| i + 1);
            if let Some(c) = b[digest..].iter_mut().find(|c| c.is_ascii_hexdigit()) {
                *c = if *c == b'0' { b'1' } else { b'0' };
            }
            String::from_utf8(b).unwrap_or_default()
        });
        let fp = forged.as_deref().or(fp);
        let env = match self.crypto.seal(kind, Some(&entry.card), sid.as_deref(), fp, &schema::encode(payload)) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("could not seal {kind:?}: {e}");
                return false;
            }
        };
        let bytes = env.to_bytes();
        if let (Some(conn), Some(lan)) = (entry.lan_conn, &self.lan)
            && lan.send(conn, bytes.clone())
        {
            return true;
        }
        if let Some(c) = &self.cloud
            && let Some((_, link)) = &c.session
        {
            link.send(bytes);
            return true;
        }
        tracing::debug!("no route to {} for {kind:?}", &key[..8]);
        false
    }

    // ------------------------------------------------------------------ signaling in

    /// Verifies, decrypts and dispatches one envelope. `lan_conn` = arrived over LAN TCP.
    pub async fn on_envelope(&mut self, bytes: &[u8], lan_conn: Option<ConnId>, via_session: bool) {
        let env = match Envelope::from_bytes(bytes) {
            Ok(e) => e,
            Err(e) => {
                self.reject(&format!("a signaling message was malformed: {e}"));
                return;
            }
        };
        if env.from == schema::SERVER {
            self.on_server_message(env).await;
            return;
        }
        if env.from == self.my_key() {
            return;
        }
        let sender = self.dir.get(&env.from).map(|e| e.card.clone());
        let plain = match self.crypto.open(&env, sender.as_ref()) {
            Ok(p) => p,
            Err(callxl_signal::SignalError::NotForUs) => return,
            Err(e) => {
                self.reject(&format!("a signaling message from {} was rejected: {e}", short(&env.from)));
                return;
            }
        };
        match env.kind {
            Kind::Hello => match schema::decode::<HelloPayload>(&plain) {
                Ok(h) => self.on_hello(&env, h, lan_conn, via_session).await,
                Err(e) => self.reject(&format!("bad hello: {e}")),
            },
            Kind::Offer | Kind::Answer => match schema::decode::<schema::SdpPayload>(&plain) {
                Ok(p) => self.on_sdp(&env, p).await,
                Err(e) => self.reject(&format!("bad session description: {e}")),
            },
            Kind::Candidate => {
                if let Ok(c) = schema::decode::<schema::CandidatePayload>(&plain) {
                    self.on_remote_candidate(&env.from, c).await;
                }
            }
            Kind::Renegotiate => {
                let k = env.from.clone();
                self.make_offer(&k).await;
            }
            Kind::Bye => {
                let reason = schema::decode::<schema::TextPayload>(&plain).map(|t| t.reason).unwrap_or_default();
                let k = env.from.clone();
                if let Some(e) = self.dir.get_mut(&k) {
                    e.lan_sessions.clear();
                    e.cloud_sessions.clear();
                }
                let name = self.dir.get(&k).map(|e| e.card.name.clone()).unwrap_or_default();
                let why = if reason.is_empty() { format!("{name} left") } else { format!("{name} left: {reason}") };
                self.drop_link(&k, &why);
                self.update_call_state();
                self.publish_sessions();
            }
            Kind::MemberLeft => {
                if let Ok(m) = schema::decode::<MemberPayload>(&plain) {
                    self.on_member_left(&m).await;
                }
            }
            _ => {}
        }
    }

    pub fn reject(&mut self, reason: &str) {
        self.rejected_signaling += 1;
        self.error(reason.to_string());
    }

    async fn on_hello(&mut self, env: &Envelope, h: HelloPayload, lan_conn: Option<ConnId>, via_session: bool) {
        if h.card.sign_pk != env.from {
            self.reject("a hello carried a device card that does not match its signature");
            return;
        }
        let key = env.from.clone();
        let restarted = self.dir.get(&key).is_some_and(|e| e.instance != h.instance);
        if restarted && self.links.contains_key(&key) {
            // Same device, new process (crash/restart): its old connection is gone.
            self.drop_link(&key, "reconnected");
        }
        let entry = self.dir.entry(key.clone()).or_insert_with(|| PeerEntry {
            card: h.card.clone(),
            instance: h.instance.clone(),
            lan_conn: None,
            lan_sessions: Vec::new(),
            cloud_sessions: HashSet::new(),
            last_hello_sent: None,
            lan_latency_ms: None,
        });
        entry.card = h.card.clone();
        entry.instance = h.instance.clone();
        if let Some(conn) = lan_conn {
            entry.lan_conn = Some(conn);
            entry.lan_sessions = h.sessions.clone();
            if !h.reply
                && let Some(t) = entry.last_hello_sent.take()
            {
                entry.lan_latency_ms = Some(t.elapsed().as_secs_f64() * 1000.0);
            }
            self.lan_conn_keys.insert(conn, key.clone());
        }
        if via_session && let Some(sid) = env.sid.clone() {
            entry.cloud_sessions.insert(sid);
        }
        if h.reply
            && let (Some(conn), Some(lan), Some(b)) = (lan_conn, &self.lan, self.hello_bytes(false, None))
        {
            lan.send(conn, b);
        }
        self.maybe_prompt(&h.card);
        self.reconcile().await;
        self.resume_transfers_for(&key);
        self.publish_sessions();
        self.emit(Event::TrustedDevicesChanged { devices: self.trusted_devices() });
    }

    async fn on_member_left(&mut self, m: &MemberPayload) {
        if let Some(e) = self.dir.get_mut(&m.key) {
            e.cloud_sessions.remove(&m.session);
        }
        self.reconcile().await;
        self.publish_sessions();
    }

    async fn on_server_message(&mut self, env: Envelope) {
        match env.kind {
            Kind::Sessions => {
                if let Ok(p) = schema::decode::<SessionsPayload>(&env.payload) {
                    self.cloud_sessions = p.list;
                    self.publish_sessions();
                }
            }
            Kind::MemberLeft => {
                if let Ok(m) = schema::decode::<MemberPayload>(&env.payload) {
                    self.on_member_left(&m).await;
                }
            }
            Kind::Error => {
                if let Ok(t) = schema::decode::<schema::TextPayload>(&env.payload) {
                    self.error(format!("signaling server: {}", t.reason));
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ LAN

    async fn on_lan(&mut self, ev: LanEvent) {
        match ev {
            LanEvent::Connected { conn, .. } => {
                if let (Some(lan), Some(b)) = (&self.lan, self.hello_bytes(true, None)) {
                    lan.send(conn, b);
                }
            }
            LanEvent::Frame { conn, data } => self.on_envelope(&data, Some(conn), false).await,
            LanEvent::Closed { conn, .. } => {
                if let Some(key) = self.lan_conn_keys.remove(&conn) {
                    let mut gone = false;
                    if let Some(e) = self.dir.get_mut(&key)
                        && e.lan_conn == Some(conn)
                    {
                        e.lan_conn = None;
                        e.lan_sessions.clear();
                        gone = e.cloud_sessions.is_empty();
                    }
                    if gone {
                        // Keep the link if media is still up (signaling loss never touches media);
                        // membership is re-learned when the peer reconnects.
                        let media_up = self.links.get(&key).is_some_and(|l| l.connected);
                        if !media_up {
                            self.reconcile().await;
                        }
                    }
                    self.publish_sessions();
                }
            }
        }
    }

    fn on_discovery(&mut self, ev: DiscoveryEvent) {
        match ev {
            DiscoveryEvent::Found { key, addrs, port, .. } => {
                let have = self.dir.get(&key).is_some_and(|e| e.lan_conn.is_some());
                let dialing = self.dialing.get(&key).is_some_and(|t| t.elapsed() < Duration::from_secs(5));
                if have || dialing || key == self.my_key() {
                    return;
                }
                // Prefer an address of a family we gather candidates on.
                let pick = addrs
                    .iter()
                    .find(|a| self.dev.bind.is_empty() || self.dev.bind.iter().any(|b| b.is_ipv4() == a.is_ipv4()))
                    .copied();
                if let (Some(a), Some(lan)) = (pick, &self.lan) {
                    self.dialing.insert(key, Instant::now());
                    lan.connect(SocketAddr::new(a, port));
                }
            }
            DiscoveryEvent::Lost { .. } => {}
        }
    }

    // ------------------------------------------------------------------ cloud

    pub fn start_cloud(&mut self) {
        if !self.dev.cloud || self.cloud.is_some() {
            return;
        }
        let url = self.settings.effective_signaling_url.clone();
        if config::is_placeholder_url(&url) {
            tracing::info!("cloud signaling not configured (placeholder URL); LAN only");
            return;
        }
        if config::app_secret_is_dev() {
            tracing::warn!("APP_SECRET not set at build time: cloud sessions only visible to this build");
        }
        let token_cache: Arc<tokio::sync::Mutex<Option<(TokenResponse, Instant)>>> = Default::default();
        let mk_url = {
            let url = url.clone();
            let cache = token_cache.clone();
            let internal = self.internal_tx.clone();
            move |path: String| {
                let url = url.clone();
                let cache = cache.clone();
                let internal = internal.clone();
                async move {
                    let mut c = cache.lock().await;
                    let fresh = c.as_ref().is_some_and(|(_, t)| t.elapsed() < Duration::from_secs(50 * 60));
                    if !fresh {
                        match callxl_signal::lobby::fetch_token(&url, config::APP_SECRET).await {
                            Ok(t) => {
                                let _ = internal.send(Internal::Token(Some(t.clone())));
                                *c = Some((t, Instant::now()));
                            }
                            Err(e) => {
                                tracing::warn!("cloud token: {e}");
                                let _ = internal.send(Internal::Token(None));
                                return None;
                            }
                        }
                    }
                    let token = c.as_ref().map(|(t, _)| t.token.clone())?;
                    Some(format!("{}{path}?token={token}", callxl_signal::lobby::ws_base(&url)))
                }
            }
        };
        let (tx, mut rx) = mpsc::channel(256);
        let cloud_tx = self.cloud_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if cloud_tx.send((false, ev)).await.is_err() {
                    break;
                }
            }
        });
        let list = self
            .crypto
            .seal(Kind::SessionList, None, None, None, b"{}")
            .map(|e| e.to_bytes())
            .ok()
            .into_iter()
            .collect();
        let f = mk_url.clone();
        let lobby = WsLink::spawn(move || f("/ws".to_string()), list, tx);
        self.cloud = Some(Cloud {
            lobby,
            session: None,
            connected: false,
            session_connected: false,
            latency_ms: None,
            token: None,
        });
        let url_fn: UrlFn = Arc::new(move |p: String| Box::pin(mk_url(p)));
        self.cloud_url_fn = Some(url_fn);
    }

    fn join_cloud_session(&mut self, sid: &str) {
        let Some(f) = self.cloud_url_fn.clone() else { return };
        let Some(hello) = self.hello_bytes(false, Some(sid)) else { return };
        let (tx, mut rx) = mpsc::channel(256);
        let cloud_tx = self.cloud_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if cloud_tx.send((true, ev)).await.is_err() {
                    break;
                }
            }
        });
        let path = format!("/session/{sid}/ws");
        let link = WsLink::spawn(move || f(path.clone()), vec![hello], tx);
        if let Some(c) = &mut self.cloud {
            if let Ok(env) = self.crypto.seal(
                Kind::SessionJoin,
                None,
                Some(sid),
                None,
                &schema::encode(&SessionRef { id: sid.to_string(), name: None }),
            ) {
                c.lobby.send(env.to_bytes());
            }
            c.session = Some((sid.to_string(), link));
        }
    }

    async fn on_cloud(&mut self, session: bool, ev: LinkEvent) {
        match ev {
            LinkEvent::Connected => {
                if let Some(c) = &mut self.cloud {
                    if session {
                        c.session_connected = true;
                    } else {
                        c.connected = true;
                    }
                }
                self.publish_sessions();
            }
            LinkEvent::Disconnected { reason } => {
                tracing::info!("cloud {} link down: {reason}", if session { "session" } else { "lobby" });
                if let Some(c) = &mut self.cloud {
                    if session {
                        c.session_connected = false;
                    } else {
                        c.connected = false;
                    }
                }
                self.publish();
            }
            LinkEvent::Frame(b) => self.on_envelope(&b, None, session).await,
            LinkEvent::Latency(d) => {
                if let Some(c) = &mut self.cloud {
                    c.latency_ms = Some(d.as_secs_f64() * 1000.0);
                }
            }
        }
    }

    // ------------------------------------------------------------------ internal

    async fn on_internal(&mut self, i: Internal) {
        match i {
            Internal::SendCandidate { key, peer, mid, mline, candidate } => {
                if self.links.get(&key).is_some_and(|l| l.id == peer) {
                    self.send_peer(
                        &key,
                        Kind::Candidate,
                        &schema::CandidatePayload { mid, mline_index: mline, candidate },
                    );
                }
            }
            Internal::Stats(s) => self.on_stats(s),
            Internal::TurnBytes(n) => {
                let k = crate::diag::turn_key();
                let cur: u64 = self.store.get(&k).and_then(|v| v.parse().ok()).unwrap_or(0);
                self.store.set(&k, &(cur + n).to_string());
            }
            Internal::Token(t) => {
                if let Some(c) = &mut self.cloud
                    && t.is_some()
                {
                    c.token = t;
                }
            }
            Internal::HashDone { id, path, name, size, hash, recipients } => {
                self.on_hash_done(id, path, name, size, hash, recipients)
            }
        }
    }

    async fn on_tick(&mut self) {
        let now = Instant::now();
        // Periodic LAN hello (keeps membership fresh, measures LAN signaling latency).
        let conns: Vec<(ConnId, PeerKey)> = self.lan_conn_keys.iter().map(|(c, k)| (*c, k.clone())).collect();
        let hello = self.hello_bytes(true, None);
        for (conn, key) in conns {
            if let Some(e) = self.dir.get_mut(&key) {
                let due = e.last_hello_sent.is_none_or(|t| now.duration_since(t) > Duration::from_secs(5));
                if due && let (Some(lan), Some(b)) = (&self.lan, &hello) {
                    e.last_hello_sent = Some(now);
                    lan.send(conn, b.clone());
                }
            }
        }
        self.check_links(now).await;
        for key in self.links.keys().cloned().collect::<Vec<_>>() {
            self.pump_file(&key, "");
        }
    }

    // ------------------------------------------------------------------ snapshot

    pub fn peers(&self) -> Vec<PeerInfo> {
        self.links.values().map(|l| self.peer_info(l)).collect()
    }

    pub fn peer_info(&self, l: &Link) -> PeerInfo {
        PeerInfo {
            key: l.key.clone(),
            display_name: l.name.clone(),
            trusted: self.is_trusted(&l.key),
            connected: l.connected,
            speaking: l.speaking,
            lan: self.dir.get(&l.key).is_some_and(|e| e.lan_conn.is_some()),
            path: l.path.clone(),
        }
    }

    pub fn publish_sessions(&mut self) {
        let s = self.sessions();
        self.emit(Event::SessionsChanged { sessions: s });
        self.publish();
    }

    pub fn publish(&self) {
        let snap = Snapshot {
            device: crate::api::card_to_api(&self.my_card()),
            sessions: self.sessions(),
            current_session: self.current_session().map(str::to_string),
            call_state: self.call_state.clone(),
            peers: self.peers(),
            muted: self.muted,
            trusted_devices: self.trusted_devices(),
            transfers: self.transfers.list(),
            settings: self.settings.clone(),
            cloud_connected: self.cloud.as_ref().is_some_and(|c| c.connected),
        };
        self.shared.set_snapshot(snap);
        let _ = self.link_views.send(self.diag_inputs());
    }

    pub fn diag_inputs(&self) -> DiagInputs {
        let lan_lat: Vec<f64> = self
            .links
            .keys()
            .filter_map(|k| self.dir.get(k))
            .filter(|e| e.lan_conn.is_some())
            .filter_map(|e| e.lan_latency_ms)
            .collect();
        let any_lan = self.links.keys().any(|k| self.dir.get(k).is_some_and(|e| e.lan_conn.is_some()));
        let cloud_up = self.cloud.as_ref().is_some_and(|c| c.session_connected || c.connected);
        let (transport, latency) = if any_lan {
            (
                SignalingTransport::Lan,
                if lan_lat.is_empty() { 0.0 } else { lan_lat.iter().sum::<f64>() / lan_lat.len() as f64 },
            )
        } else if cloud_up {
            (SignalingTransport::Cloud, self.cloud.as_ref().and_then(|c| c.latency_ms).unwrap_or(0.0))
        } else {
            (SignalingTransport::None, 0.0)
        };
        DiagInputs {
            links: self.links.values().map(|l| l.view()).collect(),
            signaling: Some(transport),
            signaling_latency_ms: latency,
            rejected_signaling: self.rejected_signaling,
            glare_events: self.glare_events,
            turn_bytes_month: self.store.get(&crate::diag::turn_key()).and_then(|v| v.parse().ok()).unwrap_or(0),
        }
    }

    pub fn download_dir(&self) -> PathBuf {
        PathBuf::from(&self.settings.download_dir)
    }
}

pub fn short(k: &str) -> &str {
    &k[..k.len().min(8)]
}

/// Builds the core's LAN side: TCP listener and mDNS advert.
pub(crate) async fn start_lan(
    dev: &DevOptions,
    crypto: &SignalCrypto,
    name: &str,
    lan_tx: mpsc::Sender<LanEvent>,
    disc_tx: mpsc::Sender<DiscoveryEvent>,
) -> (Option<LanSignaling>, Option<Discovery>) {
    if !dev.lan {
        return (None, None);
    }
    let bind_ip: IpAddr = dev.bind.first().copied().unwrap_or(IpAddr::from([0, 0, 0, 0]));
    let lan = match LanSignaling::bind(SocketAddr::new(bind_ip, dev.lan_port), lan_tx).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("LAN signaling unavailable: {e}");
            return (None, None);
        }
    };
    let disc = if dev.mdns {
        let card = crypto.card(name);
        let advert = LanAdvert {
            key: card.sign_pk,
            agree_pk: card.agree_pk,
            name: name.to_string(),
            port: lan.local_addr().port(),
            addrs: dev.bind.clone(),
        };
        let loopback = dev.bind.iter().any(|a| a.is_loopback());
        match Discovery::start(&advert, loopback, disc_tx) {
            Ok(d) => Some(d),
            Err(e) => {
                tracing::warn!("{e}");
                None
            }
        }
    } else {
        None
    };
    (Some(lan), disc)
}
