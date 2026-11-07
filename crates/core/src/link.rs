//! One PeerConnection per other session member (full mesh, §4): perfect negotiation,
//! fingerprint-checked SDP, paced/filtered trickle ICE, path-keyed policy, resilience.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use callxl_media::policy::{self, PathKind as MPathKind};
use callxl_media::{
    DataChannel, DataChannelEvent, DataChannelOptions, IceConnectionState, MediaEvent, PeerConfig, PeerConnection,
    PeerConnectionState, PeerId, SdpKind, SignalingState, sdp,
};
use callxl_signal::negotiation::{Negotiator, OfferAction};
use callxl_signal::schema::{CandidatePayload, SdpPayload};
use callxl_signal::{Envelope, Kind};

use crate::config;
use crate::diag::LinkView;
use crate::engine::{Core, Internal, short};
use crate::types::*;

pub const CHAT_CHANNEL_ID: i32 = 1;
pub const CTL_CHANNEL_ID: i32 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelRole {
    Chat,
    Ctl,
    File(TransferId),
}

/// Audio-first throttle for file data on internet paths (§6).
#[derive(Clone, Debug)]
pub struct Throttle {
    pub rate_bps: f64,
    pub rtt_samples: Vec<(Instant, f64)>,
    pub tokens: f64,
    pub last_refill: Instant,
}

impl Throttle {
    pub const START_BPS: f64 = 300_000.0;
    pub const MIN_BPS: f64 = 64_000.0;
    pub const MAX_BPS: f64 = 50_000_000.0;
    pub const STEP_BPS: f64 = 64_000.0;

    fn new() -> Self {
        Self { rate_bps: Self::START_BPS, rtt_samples: Vec::new(), tokens: 0.0, last_refill: Instant::now() }
    }

    /// AIMD on audio health: halve when audio loss > 2 % or RTT > 1.5× its 10 s baseline.
    pub fn update(&mut self, loss_pct: f64, rtt_ms: f64) {
        let now = Instant::now();
        self.rtt_samples.retain(|(t, _)| now.duration_since(*t) < Duration::from_secs(10));
        let baseline = self.rtt_samples.iter().map(|(_, r)| *r).fold(f64::INFINITY, f64::min);
        if rtt_ms > 0.0 {
            self.rtt_samples.push((now, rtt_ms));
        }
        let rtt_bad = baseline.is_finite() && rtt_ms > baseline * 1.5 && rtt_ms > baseline + 10.0;
        if loss_pct > 2.0 || rtt_bad {
            self.rate_bps = (self.rate_bps * 0.5).max(Self::MIN_BPS);
        } else {
            self.rate_bps = (self.rate_bps + Self::STEP_BPS).min(Self::MAX_BPS);
        }
    }

    /// Bytes that may be sent now (token bucket, ≤ 100 ms burst).
    pub fn allowance(&mut self) -> f64 {
        let now = Instant::now();
        let dt = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + self.rate_bps / 8.0 * dt).min(self.rate_bps / 8.0 * 0.1 + 16_384.0);
        self.tokens
    }
}

pub struct Link {
    pub key: PeerKey,
    pub name: String,
    pub id: PeerId,
    pub pc: Arc<PeerConnection>,
    pub negotiator: Negotiator,
    pub remote_set: bool,
    pub pending_candidates: Vec<CandidatePayload>,
    pub needs_offer: bool,
    pub force_offer: bool,
    pub chat: Option<DataChannel>,
    pub ctl: Option<DataChannel>,
    pub channels: HashMap<u64, ChannelRole>,
    pub file_channels: HashMap<TransferId, DataChannel>,
    pub path: Option<PathInfo>,
    pub path_kind: Option<MPathKind>,
    pub first_selected: Option<Instant>,
    pub switches: u32,
    pub connected: bool,
    pub ever_connected: bool,
    pub reconnecting: bool,
    pub disconnected_since: Option<Instant>,
    pub restarted_at: Option<Instant>,
    pub speaking: bool,
    pub offers_sent: u32,
    pub controller_decisions: Vec<String>,
    pub chat_queue: Vec<Vec<u8>>,
    pub ctl_queue: Vec<Vec<u8>>,
    pub created: Instant,
    pub throttle: Throttle,
    /// Host candidates the peer signaled (address, port): lets us recognise a pair whose
    /// remote side libwebrtc learned as peer-reflexive before the candidate arrived.
    pub remote_hosts: Vec<(String, u16)>,
    /// Last selected pair as reported by libwebrtc (for reclassification).
    pub selected: Option<(callxl_media::CandidateInfo, callxl_media::CandidateInfo)>,
}

impl Link {
    pub fn view(&self) -> LinkView {
        LinkView {
            key: self.key.clone(),
            name: self.name.clone(),
            pc: self.pc.clone(),
            path: self.path.clone(),
            switches: self.switches,
            file_rate_limit_kbps: if self.path_kind.is_some_and(|k| k.is_internet()) {
                self.throttle.rate_bps / 1000.0
            } else {
                0.0
            },
        }
    }

    pub fn send_chat(&mut self, bytes: Vec<u8>) {
        match &self.chat {
            Some(c) if c.is_open() => {
                c.send(&bytes, false);
            }
            _ => self.chat_queue.push(bytes),
        }
    }

    pub fn send_ctl(&mut self, bytes: Vec<u8>) {
        match &self.ctl {
            Some(c) if c.is_open() => {
                c.send(&bytes, false);
            }
            _ => self.ctl_queue.push(bytes),
        }
    }
}

fn family(addr: &str) -> AddrFamily {
    if policy::is_v6(addr) { AddrFamily::V6 } else { AddrFamily::V4 }
}

fn api_kind(k: MPathKind) -> PathKind {
    match k {
        MPathKind::Lan => PathKind::Lan,
        MPathKind::Direct => PathKind::Direct,
        MPathKind::Relayed => PathKind::Relayed,
    }
}

impl Core {
    fn peer_config(&self) -> PeerConfig {
        let mut cfg = PeerConfig::default();
        let token = self.cloud.as_ref().and_then(|c| c.token.clone());
        let stun: Vec<String> = match &token {
            Some(t) if !t.stun.is_empty() => t.stun.clone(),
            _ => callxl_signal::lobby::DEFAULT_STUN.iter().map(|s| s.to_string()).collect(),
        };
        // Loopback/LAN-only test peers gather host candidates only (no internet here).
        if self.dev.bind.is_empty() {
            cfg.ice_servers.push(callxl_media::IceServer {
                urls: stun,
                username: String::new(),
                credential: String::new(),
            });
            if let Some(turn) = token.and_then(|t| t.turn) {
                cfg.ice_servers.push(callxl_media::IceServer {
                    urls: turn.urls,
                    username: turn.username,
                    credential: turn.credential,
                });
            }
        }
        if let Some(p) = self.dev.ice_policy {
            cfg.ice_policy = p;
        }
        cfg.jitter_buffer_min_delay_ms = 0;
        cfg
    }

    pub async fn create_link(&mut self, key: &str) {
        let Some(entry) = self.dir.get(key) else { return };
        let name = entry.card.name.clone();
        let id = self.next_peer_id;
        self.next_peer_id += 1;
        let cfg = self.peer_config();
        let mut pc = match self.engine.create_peer(id, &cfg, self.media_tx.clone()) {
            Ok(pc) => pc,
            Err(e) => {
                self.error(format!("could not connect to {name}: {e}"));
                return;
            }
        };
        // Bounds before the transport exists (DECISIONS.md D-0022): the conservative
        // internet start with the final max, so that the path policy issued on pair
        // selection never *raises* the max — a raised max triggers an immediate libwebrtc
        // probe straight to it (measured: BWE 600 kbps 250 ms after media start).
        let p0 = policy::policy_for(MPathKind::Direct, self.settings.bitrate_ceiling_kbps);
        if let Err(e) = pc.set_bitrate(p0.bwe_min_bps, Some(p0.bwe_start_bps), p0.bwe_max_bps) {
            tracing::warn!("set_bitrate: {e}");
        }
        if let Err(e) = pc.add_audio_track(&self.mic, "callxl") {
            self.error(format!("could not send audio to {name}: {e}"));
        }
        let chat = pc
            .create_data_channel(
                "chat",
                DataChannelOptions { ordered: true, reliable: true, negotiated_id: Some(CHAT_CHANNEL_ID) },
            )
            .ok();
        let ctl = pc
            .create_data_channel(
                "ctl",
                DataChannelOptions { ordered: true, reliable: true, negotiated_id: Some(CTL_CHANNEL_ID) },
            )
            .ok();
        let mut channels = HashMap::new();
        if let Some(c) = &chat {
            channels.insert(c.handle(), ChannelRole::Chat);
        }
        if let Some(c) = &ctl {
            channels.insert(c.handle(), ChannelRole::Ctl);
        }
        let link = Link {
            key: key.to_string(),
            name,
            id,
            pc: Arc::new(pc),
            negotiator: Negotiator::new(&self.my_key(), key),
            remote_set: false,
            pending_candidates: Vec::new(),
            needs_offer: false,
            force_offer: false,
            chat,
            ctl,
            channels,
            file_channels: HashMap::new(),
            path: None,
            path_kind: None,
            first_selected: None,
            switches: 0,
            connected: false,
            ever_connected: false,
            reconnecting: false,
            disconnected_since: None,
            restarted_at: None,
            speaking: false,
            offers_sent: 0,
            controller_decisions: Vec::new(),
            chat_queue: Vec::new(),
            ctl_queue: Vec::new(),
            created: Instant::now(),
            throttle: Throttle::new(),
            remote_hosts: Vec::new(),
            selected: None,
        };
        self.peer_ids.insert(id, key.to_string());
        let info = self.peer_info(&link);
        self.links.insert(key.to_string(), link);
        // Re-apply our APM config: the voice engine applies AudioOptions when the first
        // send stream starts and must not override AGC2 / NS level (§3).
        self.engine.apply_apm(self.settings.apm.to_media());
        self.emit(Event::PeerJoined { peer: info });
        self.publish();
    }

    pub fn drop_link(&mut self, key: &str, reason: &str) {
        if let Some(l) = self.links.remove(key) {
            self.peer_ids.remove(&l.id);
            l.pc.close();
            self.pause_transfers_for(key);
            self.emit(Event::PeerLeft { peer_key: key.to_string(), reason: reason.to_string() });
            self.publish();
        }
    }

    /// Our SDP always advertises the full §3 profile (what we can receive); the user's
    /// ceiling only limits what we send and is enforced locally (`apply_ceiling`).
    fn munge(&self, raw: &str) -> String {
        sdp::strip_candidates(&sdp::apply_opus_profile(raw, config::MAX_CEILING_KBPS * 1000))
    }

    pub fn apply_ceiling(&mut self, key: &str) {
        let ceiling = self.settings.bitrate_ceiling_kbps;
        self.engine.set_max_opus_bitrate((ceiling * 1000) as i32);
        let Some(l) = self.links.get(key) else { return };
        let Some(kind) = l.path_kind else { return };
        let p = policy::policy_for(kind, ceiling);
        if let Err(e) = l.pc.set_bitrate(p.bwe_min_bps, None, p.bwe_max_bps) {
            tracing::warn!("set_bitrate: {e}");
        }
    }

    pub async fn make_offer(&mut self, key: &str) {
        let Some(l) = self.links.get_mut(key) else { return };
        if l.pc.signaling_state() != SignalingState::Stable {
            l.needs_offer = true;
            return;
        }
        l.needs_offer = false;
        l.negotiator.making_offer = true;
        let pc = l.pc.clone();
        let res = async {
            let raw = pc.create_offer().await?;
            let munged = self.munge(&raw);
            pc.set_local_description(SdpKind::Offer, &munged).await?;
            Ok::<_, callxl_media::MediaError>(munged)
        }
        .await;
        let Some(l) = self.links.get_mut(key) else { return };
        l.negotiator.making_offer = false;
        match res {
            Ok(sdp_text) => {
                l.offers_sent += 1;
                let fp = sdp::fingerprint(&sdp_text);
                self.send_peer_fp(key, Kind::Offer, &SdpPayload { sdp: sdp_text, ice_restart: false }, fp.as_deref());
            }
            Err(e) => tracing::warn!("offer to {} failed: {e}", short(key)),
        }
    }

    pub async fn on_sdp(&mut self, env: &Envelope, p: SdpPayload) {
        let key = env.from.clone();
        // §5: the DTLS fingerprint inside the SDP must match the signed envelope field.
        let in_sdp = sdp::fingerprint(&p.sdp);
        if in_sdp.is_none() || in_sdp != env.fp {
            self.reject(&format!(
                "refused a call setup from {}: its DTLS fingerprint does not match the signed message",
                short(&key)
            ));
            return;
        }
        if !self.links.contains_key(&key) {
            let ours = self.current_session().map(str::to_string);
            if ours.is_none() || env.sid != ours {
                return;
            }
            self.create_link(&key).await;
        }
        let remote = sdp::apply_opus_profile(&p.sdp, config::MAX_CEILING_KBPS * 1000);
        let Some(l) = self.links.get_mut(&key) else { return };
        let pc = l.pc.clone();
        if env.kind == Kind::Offer {
            let stable = pc.signaling_state() == SignalingState::Stable;
            let action = l.negotiator.on_remote_offer(stable);
            match action {
                OfferAction::Ignore => {
                    self.glare_events += 1;
                    tracing::info!("glare with {}: ignoring their offer (impolite)", short(&key));
                    return;
                }
                OfferAction::RollbackThenAccept => {
                    self.glare_events += 1;
                    tracing::info!("glare with {}: rolling back our offer (polite)", short(&key));
                    if let Err(e) = pc.rollback().await {
                        tracing::warn!("rollback failed: {e}");
                    }
                }
                OfferAction::Accept => {}
            }
            if let Err(e) = pc.set_remote_description(SdpKind::Offer, &remote).await {
                self.error(format!("could not apply the call offer from {}: {e}", short(&key)));
                return;
            }
            self.after_remote_description(&key).await;
            let res = async {
                let raw = pc.create_answer().await?;
                let munged = self.munge(&raw);
                pc.set_local_description(SdpKind::Answer, &munged).await?;
                Ok::<_, callxl_media::MediaError>(munged)
            }
            .await;
            match res {
                Ok(ans) => {
                    let fp = sdp::fingerprint(&ans);
                    self.send_peer_fp(&key, Kind::Answer, &SdpPayload { sdp: ans, ice_restart: false }, fp.as_deref());
                }
                Err(e) => self.error(format!("could not answer {}: {e}", short(&key))),
            }
        } else {
            if pc.signaling_state() != SignalingState::HaveLocalOffer {
                tracing::debug!("stale answer from {}", short(&key));
                return;
            }
            if let Err(e) = pc.set_remote_description(SdpKind::Answer, &remote).await {
                self.error(format!("could not apply the call answer from {}: {e}", short(&key)));
                return;
            }
            self.after_remote_description(&key).await;
        }
        let pending = self.links.get(&key).is_some_and(|l| l.needs_offer || l.force_offer);
        if let Some(l) = self.links.get_mut(&key) {
            l.force_offer = false;
        }
        if pending
            && self.links.get(&key).is_some_and(|l| l.pc.signaling_state() == SignalingState::Stable)
            && self.links.get(&key).is_some_and(|l| l.needs_offer)
        {
            self.make_offer(&key).await;
        }
    }

    async fn after_remote_description(&mut self, key: &str) {
        let Some(l) = self.links.get_mut(key) else { return };
        l.remote_set = true;
        let pending = std::mem::take(&mut l.pending_candidates);
        let pc = l.pc.clone();
        for c in pending {
            if let Err(e) = pc.add_ice_candidate(&c.mid, c.mline_index, &c.candidate).await {
                tracing::debug!("buffered candidate rejected: {e}");
            }
        }
    }

    pub async fn on_remote_candidate(&mut self, key: &str, c: CandidatePayload) {
        if let Some(p) = policy::parse_candidate(&c.candidate)
            && policy::is_discarded_address(&p.address)
        {
            return;
        }
        let Some(l) = self.links.get_mut(key) else { return };
        let mut reclassify = false;
        if let Some(p) = policy::parse_candidate(&c.candidate)
            && p.typ == "host"
        {
            reclassify = l.selected.as_ref().is_some_and(|(_, r)| r.typ == "prflx" && r.port == p.port);
            l.remote_hosts.push((p.address, p.port));
        }
        if reclassify && let Some((loc, rem)) = l.selected.clone() {
            self.on_selected_pair(key, loc, rem, "remote host candidate arrived", false);
        }
        let Some(l) = self.links.get_mut(key) else { return };
        if !l.remote_set {
            l.pending_candidates.push(c);
            return;
        }
        let ignoring = l.negotiator.ignore_offer;
        let pc = l.pc.clone();
        if let Err(e) = pc.add_ice_candidate(&c.mid, c.mline_index, &c.candidate).await
            && !ignoring
        {
            tracing::debug!("candidate from {} rejected: {e}", short(key));
        }
    }

    async fn on_local_candidate(&mut self, peer: PeerId, mid: String, mline: i32, mut candidate: String) {
        let Some(key) = self.peer_ids.get(&peer).cloned() else { return };
        let Some(parsed) = policy::parse_candidate(&candidate) else { return };
        if policy::is_discarded_address(&parsed.address) {
            return;
        }
        if !self.dev.bind.is_empty() {
            let ok = parsed.address.parse::<std::net::IpAddr>().is_ok_and(|a| self.dev.bind.contains(&a));
            if !ok {
                return;
            }
        }
        if let (Some(relay), "host", "udp") =
            (self.dev.candidate_rewrite, parsed.typ.as_str(), parsed.protocol.as_str())
        {
            match crate::relay_client::register(relay, &parsed.address, parsed.port).await {
                Ok(addr) => {
                    if let Some(r) = policy::rewrite_candidate(&candidate, &addr.ip().to_string(), addr.port()) {
                        candidate = r;
                    }
                }
                Err(e) => tracing::warn!("impairment relay registration failed: {e}"),
            }
        }
        let delay = policy::trickle_delay(&parsed);
        if delay.is_zero() {
            self.send_peer(&key, Kind::Candidate, &CandidatePayload { mid, mline_index: mline, candidate });
        } else {
            let tx = self.internal_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = tx.send(Internal::SendCandidate { key, peer, mid, mline, candidate });
            });
        }
    }

    /// Classifies the selected pair, updates `PathInfo`, re-applies the path policy.
    /// `is_switch` = a real selected-pair change (counted), not a reclassification.
    fn on_selected_pair(
        &mut self,
        key: &str,
        local: callxl_media::CandidateInfo,
        mut remote: callxl_media::CandidateInfo,
        reason: &str,
        is_switch: bool,
    ) {
        let force = self.dev.force_internet;
        let Some(l) = self.links.get_mut(key) else { return };
        l.selected = Some((local.clone(), remote.clone()));
        if remote.typ == "prflx" {
            // Learned from a connectivity check before the signaled candidate arrived.
            let fam_ok = |a: &str| remote.address.is_empty() || a == remote.address;
            if let Some((a, _)) = l.remote_hosts.iter().find(|(a, p)| *p == remote.port && fam_ok(a)) {
                remote.address = a.clone();
                remote.typ = "host".into();
            }
        }
        let kind = policy::classify(&local.typ, &remote.typ, force);
        let now = Instant::now();
        if is_switch {
            match l.first_selected {
                None => l.first_selected = Some(now),
                Some(t) if now.duration_since(t) > Duration::from_millis(1_500) => l.switches += 1,
                Some(_) => {}
            }
        }
        let info = PathInfo {
            kind: api_kind(kind),
            local_addr: format!("{}:{}", local.address, local.port),
            local_family: family(&local.address),
            remote_addr: format!("{}:{}", remote.address, remote.port),
            remote_family: if remote.address.is_empty() { family(&local.address) } else { family(&remote.address) },
            local_candidate_type: local.typ.clone(),
            remote_candidate_type: remote.typ.clone(),
            switches: l.switches,
        };
        tracing::info!(
            "selected pair {} {} -> {} ({kind:?}, switches {}, reason {reason})",
            short(key),
            info.local_addr,
            info.remote_addr,
            l.switches
        );
        let kind_changed = l.path_kind != Some(kind);
        l.path_kind = Some(kind);
        l.path = Some(info.clone());
        self.emit(Event::PeerPathChanged { peer_key: key.to_string(), path: info });
        if kind_changed {
            self.apply_path_policy(key);
        }
        self.publish();
    }

    pub fn apply_path_policy(&mut self, key: &str) {
        let ceiling = self.settings.bitrate_ceiling_kbps;
        let Some(l) = self.links.get(key) else { return };
        let Some(kind) = l.path_kind else { return };
        let p = policy::policy_for(kind, ceiling);
        self.engine.set_max_opus_bitrate((ceiling * 1000) as i32);
        if let Err(e) = l.pc.set_bitrate(p.bwe_min_bps, Some(p.bwe_start_bps), p.bwe_max_bps) {
            tracing::warn!("set_bitrate: {e}");
        }
        // No RtpSender encoding limits: SetParameters racing the send-stream setup left
        // the audio allocation stuck at the start rate in 3 of 3 runs (465/83 kbps);
        // WebRTC-Audio-Allocation already bounds the allocation (DECISIONS.md).
        l.pc.set_jitter_buffer_min_delay(p.jitter_buffer_min_delay);
        tracing::info!(
            "path policy {} -> {:?}: start {} kbps, max {} kbps, jitter min {:?}",
            short(key),
            kind,
            p.bwe_start_bps / 1000,
            p.bwe_max_bps / 1000,
            p.jitter_buffer_min_delay
        );
    }

    pub async fn on_media(&mut self, ev: MediaEvent) {
        match ev {
            MediaEvent::IceCandidate { peer, mid, mline_index, candidate } => {
                self.on_local_candidate(peer, mid, mline_index, candidate).await
            }
            MediaEvent::NegotiationNeeded { peer } => {
                if let Some(key) = self.peer_ids.get(&peer).cloned() {
                    self.make_offer(&key).await;
                }
            }
            MediaEvent::SignalingState { peer, state } => {
                if state == SignalingState::Stable
                    && let Some(key) = self.peer_ids.get(&peer).cloned()
                    && self.links.get(&key).is_some_and(|l| l.needs_offer)
                {
                    self.make_offer(&key).await;
                }
            }
            MediaEvent::IceConnectionState { peer, state } => self.on_ice_state(peer, state),
            MediaEvent::ConnectionState { peer, state } => {
                if state == PeerConnectionState::Failed {
                    self.on_ice_state(peer, IceConnectionState::IceConnectionFailed);
                }
            }
            MediaEvent::SelectedPairChanged { peer, local, remote, reason } => {
                let Some(key) = self.peer_ids.get(&peer).cloned() else { return };
                self.on_selected_pair(&key, local, remote, &reason, true);
            }
            MediaEvent::IceControllerDecision { peer, reason } => {
                if let Some(key) = self.peer_ids.get(&peer).cloned() {
                    tracing::info!("ICE controller switched {}: {reason}", short(&key));
                    if let Some(l) = self.links.get_mut(&key) {
                        l.controller_decisions.push(reason);
                    }
                }
            }
            MediaEvent::DataChannelOpened { peer, channel } => {
                let Some(key) = self.peer_ids.get(&peer).cloned() else { return };
                let Some(l) = self.links.get_mut(&key) else { return };
                let dc = l.pc.adopt_channel(channel);
                let label = dc.label();
                if let Some(id) = label.strip_prefix("file:") {
                    l.channels.insert(dc.handle(), ChannelRole::File(id.to_string()));
                    l.file_channels.insert(id.to_string(), dc);
                }
            }
            MediaEvent::Data { peer, channel, event } => {
                let Some(key) = self.peer_ids.get(&peer).cloned() else { return };
                let Some(role) = self.links.get(&key).and_then(|l| l.channels.get(&channel).cloned()) else { return };
                match event {
                    DataChannelEvent::State(s) => {
                        if s == callxl_media::DataState::Open {
                            self.on_channel_open(&key, &role);
                        }
                    }
                    DataChannelEvent::Message { data, .. } => match role {
                        ChannelRole::Chat => self.on_chat_message(&key, &data),
                        ChannelRole::Ctl => self.on_ctl_message(&key, &data),
                        ChannelRole::File(id) => self.on_file_data(&key, &id, &data),
                    },
                    DataChannelEvent::BufferedAmount(_) => {}
                }
            }
        }
    }

    fn on_channel_open(&mut self, key: &str, role: &ChannelRole) {
        let Some(l) = self.links.get_mut(key) else { return };
        match role {
            ChannelRole::Chat => {
                for m in std::mem::take(&mut l.chat_queue) {
                    if let Some(c) = &l.chat {
                        c.send(&m, false);
                    }
                }
            }
            ChannelRole::Ctl => {
                for m in std::mem::take(&mut l.ctl_queue) {
                    if let Some(c) = &l.ctl {
                        c.send(&m, false);
                    }
                }
                self.resume_transfers_for(key);
            }
            ChannelRole::File(id) => {
                let id = id.clone();
                self.pump_file(key, &id);
            }
        }
    }

    fn on_ice_state(&mut self, peer: PeerId, state: IceConnectionState) {
        let Some(key) = self.peer_ids.get(&peer).cloned() else { return };
        let Some(l) = self.links.get_mut(&key) else { return };
        match state {
            IceConnectionState::IceConnectionConnected | IceConnectionState::IceConnectionCompleted => {
                let first = !l.ever_connected;
                l.connected = true;
                l.ever_connected = true;
                l.reconnecting = false;
                l.disconnected_since = None;
                if first {
                    tracing::info!("connected to {} in {} ms", short(&key), l.created.elapsed().as_millis());
                }
            }
            IceConnectionState::IceConnectionDisconnected | IceConnectionState::IceConnectionFailed => {
                l.connected = false;
                if l.disconnected_since.is_none() {
                    l.disconnected_since = Some(Instant::now());
                }
            }
            _ => {}
        }
        self.update_call_state();
        self.publish();
    }

    /// ICE restart within 1 s of `disconnected`, reconnecting after 15 s, leave after 30 s (§4).
    pub async fn check_links(&mut self, now: Instant) {
        let mut restart = Vec::new();
        let mut drop = Vec::new();
        let mut changed = false;
        for l in self.links.values_mut() {
            let since = match l.disconnected_since {
                Some(t) => t,
                None if !l.ever_connected => l.created,
                None => continue,
            };
            let down = now.duration_since(since);
            if l.ever_connected {
                let wait = if l.negotiator.polite {
                    Duration::from_millis(2_000)
                } else {
                    Duration::from_millis(config::ICE_RESTART_AFTER_MS)
                };
                let again = l.restarted_at.is_none_or(|t| now.duration_since(t) > Duration::from_secs(5));
                if down > wait && again {
                    l.restarted_at = Some(now);
                    restart.push(l.key.clone());
                }
            }
            if down > Duration::from_millis(config::RECONNECTING_AFTER_MS) && !l.reconnecting {
                l.reconnecting = true;
                changed = true;
            }
            if down > Duration::from_millis(config::LEAVE_AFTER_MS) {
                drop.push((l.key.clone(), l.name.clone()));
            }
        }
        for k in restart {
            if let Some(l) = self.links.get(&k) {
                tracing::info!("ICE restart towards {}", short(&k));
                l.pc.restart_ice();
            }
            self.make_offer(&k).await;
        }
        let had = !self.links.is_empty();
        for (k, name) in &drop {
            self.drop_link(k, &format!("the connection to {name} was lost for 30 seconds"));
        }
        if had && self.links.is_empty() && !drop.is_empty() && self.my_session.is_some() {
            let reason = format!("the connection to {} was lost for 30 seconds", drop[0].1);
            self.leave(&reason).await;
            return;
        }
        if changed || !drop.is_empty() {
            self.update_call_state();
        }
    }

    pub fn on_stats(&mut self, s: DiagnosticsSnapshot) {
        // The voice engine re-applies AudioOptions when send streams start; keep the
        // live APM equal to the user's settings (§3) and prove it via the readback.
        if s.global.apm != self.settings.apm && !self.links.is_empty() {
            tracing::info!("APM drifted to {:?}; re-applying {:?}", s.global.apm, self.settings.apm);
            self.engine.apply_apm(self.settings.apm.to_media());
        }
        for p in &s.peers {
            let Some(l) = self.links.get_mut(&p.peer_key) else { continue };
            let speaking = p.audio_level > 0.02;
            if speaking != l.speaking {
                l.speaking = speaking;
                self.shared.emit(Event::SpeakingChanged { peer_key: p.peer_key.clone(), speaking });
            }
            if l.path_kind.is_some_and(|k| k.is_internet()) {
                l.throttle.update(p.loss_pct.max(p.remote_loss_pct), p.rtt_ms);
            }
        }
        self.shared.set_diagnostics(s.clone());
        self.emit(Event::StatsTick { snapshot: s.clone() });
        self.last_stats = Some(s);
        let _ = self.link_views.send(self.diag_inputs());
    }
}
