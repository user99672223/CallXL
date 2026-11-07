use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use cxx::{SharedPtr, UniquePtr};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use webrtc_sys::audio_track::ffi as at;
use webrtc_sys::callxl::ffi as cx;
use webrtc_sys::candidate::ffi::Candidate;
use webrtc_sys::data_channel::ffi::DataChannel as SysDataChannel;
use webrtc_sys::jsep::ffi as jsep;
use webrtc_sys::media_stream::ffi::MediaStream;
use webrtc_sys::peer_connection::PeerContext;
use webrtc_sys::peer_connection::ffi as pc;
use webrtc_sys::peer_connection_factory::{self as pcfm, ffi as pcf};
use webrtc_sys::rtc_error::ffi::RtcError;
use webrtc_sys::rtp_receiver::ffi::RtpReceiver;
use webrtc_sys::rtp_sender::ffi::RtpSender;
use webrtc_sys::rtp_transceiver::ffi::RtpTransceiver;

use crate::data_channel::{DataChannel, DataChannelEvent, DataChannelOptions};
use crate::{MediaError, Result, rtc_error_message};

pub type PeerId = u64;

pub use pc::{IceConnectionState, PeerConnectionState, SignalingState};

const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// Path-switching policy for the CallXL ICE controller (CLAUDE.md §4).
#[derive(Clone, Copy, Debug)]
pub struct IcePolicy {
    pub enabled: bool,
    pub rtt_delta_ms: i32,
    pub loss_delta: f32,
    pub sustain_ms: i32,
    pub cooldown_ms: i32,
    pub v6_bonus_ms: i32,
    pub initial_grace_ms: i32,
    pub recheck_ms: i32,
}

impl Default for IcePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            rtt_delta_ms: 15,
            loss_delta: 0.05,
            sustain_ms: 5_000,
            cooldown_ms: 30_000,
            v6_bonus_ms: 5,
            initial_grace_ms: 1_500,
            recheck_ms: 500,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
}

/// Port allocator flags (p2p/base/port_allocator.h) used by CallXL (§4).
pub mod allocator_flags {
    pub const DISABLE_TCP: u32 = 0x08;
    pub const ENABLE_IPV6: u32 = 0x40;
    pub const ENABLE_IPV6_ON_WIFI: u32 = 0x4000;
    pub const DISABLE_LINK_LOCAL_NETWORKS: u32 = 0x10000;
    /// IPv6 + IPv6-on-Wi-Fi, no unscoped link-local networks, UDP only.
    pub const CALLXL: u32 = ENABLE_IPV6 | ENABLE_IPV6_ON_WIFI | DISABLE_LINK_LOCAL_NETWORKS | DISABLE_TCP;
}

#[derive(Clone, Debug)]
pub struct PeerConfig {
    pub ice_servers: Vec<IceServer>,
    pub port_allocator_flags: u32,
    pub ice_policy: IcePolicy,
    pub jitter_buffer_min_delay_ms: i32,
    pub rtcp_report_interval_ms: i32,
    /// STUN keepalive interval on the selected pair, so RTT is fresh for path switching.
    pub selected_ping_interval_ms: i32,
    /// STUN ping interval on backup pairs (libwebrtc default is 25 s).
    pub backup_ping_interval_ms: i32,
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            ice_servers: Vec::new(),
            port_allocator_flags: allocator_flags::CALLXL,
            ice_policy: IcePolicy::default(),
            jitter_buffer_min_delay_ms: 10,
            rtcp_report_interval_ms: 1_000,
            selected_ping_interval_ms: 500,
            backup_ping_interval_ms: 500,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateInfo {
    pub address: String,
    pub port: u16,
    pub family: u8,
    pub typ: String,
    pub protocol: String,
    pub priority: u32,
    pub network_name: String,
}

impl CandidateInfo {
    fn from_sys(c: &SharedPtr<Candidate>) -> Self {
        match c.as_ref() {
            Some(c) => {
                let i = cx::callxl_candidate_info(c);
                Self {
                    address: i.address,
                    port: i.port,
                    family: i.family as u8,
                    typ: i.typ,
                    protocol: i.protocol,
                    priority: i.priority,
                    network_name: i.network_name,
                }
            }
            None => Self::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdpKind {
    Offer,
    Answer,
}

/// Everything libwebrtc tells us, as data. Produced by observer callbacks with one `try_send`.
pub enum MediaEvent {
    IceCandidate { peer: PeerId, mid: String, mline_index: i32, candidate: String },
    IceConnectionState { peer: PeerId, state: IceConnectionState },
    ConnectionState { peer: PeerId, state: PeerConnectionState },
    SignalingState { peer: PeerId, state: SignalingState },
    NegotiationNeeded { peer: PeerId },
    SelectedPairChanged { peer: PeerId, local: CandidateInfo, remote: CandidateInfo, reason: String },
    IceControllerDecision { peer: PeerId, reason: String },
    DataChannelOpened { peer: PeerId, channel: RemoteDataChannel },
    Data { peer: PeerId, channel: u64, event: DataChannelEvent },
}

/// A data channel announced by the remote side; [`PeerConnection::adopt_channel`] wraps it.
pub struct RemoteDataChannel(pub(crate) SharedPtr<SysDataChannel>);
// SAFETY: libwebrtc data channels are thread-safe proxies.
unsafe impl Send for RemoteDataChannel {}

static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Number of media events dropped because the core's queue was full (diagnostics).
pub fn dropped_events() -> u64 {
    DROPPED_EVENTS.load(Ordering::Relaxed)
}

pub(crate) fn push(tx: &mpsc::Sender<MediaEvent>, ev: MediaEvent) {
    if tx.try_send(ev).is_err() {
        DROPPED_EVENTS.fetch_add(1, Ordering::Relaxed);
    }
}

struct Observer {
    id: PeerId,
    tx: mpsc::Sender<MediaEvent>,
}

impl pcfm::PeerConnectionObserver for Observer {
    fn on_signaling_change(&self, state: SignalingState) {
        push(&self.tx, MediaEvent::SignalingState { peer: self.id, state });
    }
    fn on_add_stream(&self, _: SharedPtr<MediaStream>) {}
    fn on_remove_stream(&self, _: SharedPtr<MediaStream>) {}
    fn on_data_channel(&self, dc: SharedPtr<SysDataChannel>) {
        push(&self.tx, MediaEvent::DataChannelOpened { peer: self.id, channel: RemoteDataChannel(dc) });
    }
    fn on_renegotiation_needed(&self) {}
    fn on_negotiation_needed_event(&self, _event: u32) {
        push(&self.tx, MediaEvent::NegotiationNeeded { peer: self.id });
    }
    fn on_ice_connection_change(&self, _: IceConnectionState) {}
    fn on_standardized_ice_connection_change(&self, state: IceConnectionState) {
        push(&self.tx, MediaEvent::IceConnectionState { peer: self.id, state });
    }
    fn on_connection_change(&self, state: PeerConnectionState) {
        push(&self.tx, MediaEvent::ConnectionState { peer: self.id, state });
    }
    fn on_ice_gathering_change(&self, _: pc::IceGatheringState) {}
    fn on_ice_candidate(&self, c: SharedPtr<jsep::IceCandidate>) {
        if let Some(c) = c.as_ref() {
            push(
                &self.tx,
                MediaEvent::IceCandidate {
                    peer: self.id,
                    mid: c.sdp_mid(),
                    mline_index: c.sdp_mline_index(),
                    candidate: c.candidate(),
                },
            );
        }
    }
    fn on_ice_candidate_error(&self, _: String, _: i32, _: String, _: i32, _: String) {}
    fn on_ice_candidates_removed(&self, _: Vec<SharedPtr<Candidate>>) {}
    fn on_ice_connection_receiving_change(&self, _: bool) {}
    fn on_ice_selected_candidate_pair_changed(&self, ev: pcf::CandidatePairChangeEvent) {
        push(
            &self.tx,
            MediaEvent::SelectedPairChanged {
                peer: self.id,
                local: CandidateInfo::from_sys(&ev.selected_candidate_pair.local),
                remote: CandidateInfo::from_sys(&ev.selected_candidate_pair.remote),
                reason: ev.reason,
            },
        );
    }
    fn on_add_track(&self, _: SharedPtr<RtpReceiver>, _: Vec<SharedPtr<MediaStream>>) {}
    fn on_track(&self, _: SharedPtr<RtpTransceiver>) {}
    fn on_remove_track(&self, _: SharedPtr<RtpReceiver>) {}
    fn on_interesting_usage(&self, _: i32) {}
    fn on_ice_controller_decision(&self, reason: String) {
        push(&self.tx, MediaEvent::IceControllerDecision { peer: self.id, reason });
    }
}

/// The local microphone track (captured inside libwebrtc's ADM).
#[derive(Clone)]
pub struct AudioTrack {
    track: SharedPtr<at::AudioTrack>,
}
// SAFETY: libwebrtc media tracks are thread-safe proxies.
unsafe impl Send for AudioTrack {}
unsafe impl Sync for AudioTrack {}

impl AudioTrack {
    pub(crate) fn new(track: SharedPtr<at::AudioTrack>) -> Self {
        Self { track }
    }
    /// Mute = disabled track: libwebrtc keeps sending (silence), the pipeline stays warm.
    pub fn set_enabled(&self, enabled: bool) {
        let mst = at::audio_to_media(self.track.clone());
        if let Some(t) = mst.as_ref() {
            t.set_enabled(enabled);
        }
    }
}

pub struct PeerConnection {
    id: PeerId,
    pc: SharedPtr<pc::PeerConnection>,
    tx: mpsc::Sender<MediaEvent>,
    audio_sender: Option<SharedPtr<RtpSender>>,
}
// SAFETY: libwebrtc PeerConnection is a thread-safe proxy (webrtc-sys marks it Send + Sync).
unsafe impl Send for PeerConnection {}
unsafe impl Sync for PeerConnection {}

type SdpResult = Result<String>;

fn ctx<T: Send + 'static>(tx: oneshot::Sender<T>) -> Box<PeerContext> {
    Box::new(PeerContext(Box::new(tx)))
}

fn finish<T: Send + 'static>(ctx: Box<PeerContext>, value: T) {
    let PeerContext(any) = *ctx;
    let any: Box<dyn Any + Send> = any;
    if let Ok(tx) = any.downcast::<oneshot::Sender<T>>() {
        let _ = tx.send(value);
    }
}

async fn wait<T>(rx: oneshot::Receiver<T>) -> Result<T> {
    match tokio::time::timeout(OP_TIMEOUT, rx).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err(MediaError::Closed),
        Err(_) => Err(MediaError::Timeout),
    }
}

fn err_result(e: RtcError) -> Result<()> {
    if e.ok() { Ok(()) } else { Err(MediaError::Sdp(e.message)) }
}

impl PeerConnection {
    pub(crate) fn create(
        factory: &SharedPtr<pcf::PeerConnectionFactory>,
        id: PeerId,
        cfg: &PeerConfig,
        tx: mpsc::Sender<MediaEvent>,
    ) -> Result<Self> {
        let rtc = pc::RtcConfiguration {
            ice_servers: cfg
                .ice_servers
                .iter()
                .map(|s| pc::IceServer {
                    urls: s.urls.clone(),
                    username: s.username.clone(),
                    password: s.credential.clone(),
                })
                .collect(),
            continual_gathering_policy: pc::ContinualGatheringPolicy::GatherContinually,
            ice_transport_type: pc::IceTransportsType::All,
            enable_sctp_snap: false,
        };
        let p = cfg.ice_policy;
        let ext = pcf::CallXlPcConfig {
            port_allocator_flags: cfg.port_allocator_flags,
            max_ipv6_networks: 5,
            ice_connection_receiving_timeout_ms: 1_000,
            ice_backup_ping_interval_ms: cfg.backup_ping_interval_ms,
            stable_writable_ping_interval_ms: cfg.selected_ping_interval_ms,
            ice_check_min_interval_ms: 0,
            audio_jitter_buffer_min_delay_ms: cfg.jitter_buffer_min_delay_ms,
            audio_jitter_buffer_fast_accelerate: true,
            audio_rtcp_report_interval_ms: cfg.rtcp_report_interval_ms,
            ice: pcf::CallXlIceParams {
                enabled: p.enabled,
                rtt_delta_ms: p.rtt_delta_ms,
                loss_delta: p.loss_delta,
                sustain_ms: p.sustain_ms,
                cooldown_ms: p.cooldown_ms,
                v6_bonus_ms: p.v6_bonus_ms,
                initial_grace_ms: p.initial_grace_ms,
                recheck_ms: p.recheck_ms,
            },
        };
        let observer = Arc::new(Observer { id, tx: tx.clone() });
        let wrapper = Box::new(pcfm::PeerConnectionObserverWrapper::new(observer));
        let pc = factory
            .create_peer_connection_callxl(rtc, ext, wrapper)
            .map_err(|e| MediaError::PeerConnection(rtc_error_message(&e)))?;
        Ok(Self { id, pc, tx, audio_sender: None })
    }

    pub fn id(&self) -> PeerId {
        self.id
    }

    /// Adds the microphone track; returns after libwebrtc created the sender.
    pub fn add_audio_track(&mut self, track: &AudioTrack, stream_id: &str) -> Result<()> {
        let mst = at::audio_to_media(track.track.clone());
        let sender = self
            .pc
            .add_track(mst, &vec![stream_id.to_string()])
            .map_err(|e| MediaError::PeerConnection(rtc_error_message(&e)))?;
        self.audio_sender = Some(sender);
        Ok(())
    }

    /// Caps the Opus encoding at `max_bps` / floors it at `min_bps` via RtpSender parameters.
    pub fn set_audio_encoding_limits(&self, min_bps: i32, max_bps: i32) -> Result<()> {
        let Some(sender) = &self.audio_sender else { return Ok(()) };
        let mut params = sender.get_parameters();
        for e in params.encodings.iter_mut() {
            e.has_max_bitrate_bps = true;
            e.max_bitrate_bps = max_bps;
            e.has_min_bitrate_bps = true;
            e.min_bitrate_bps = min_bps;
            e.network_priority = webrtc_sys::webrtc::ffi::Priority::High;
            e.bitrate_priority = 4.0;
        }
        sender
            .set_parameters(params)
            .map_err(|e| MediaError::PeerConnection(rtc_error_message(&e)))
    }

    /// `PeerConnection::SetBitrate` (BWE min/start/max). `start` = `None` keeps the
    /// running estimate.
    pub fn set_bitrate(&self, min_bps: i32, start_bps: Option<i32>, max_bps: i32) -> Result<()> {
        self.pc
            .set_bitrate(pc::BitrateSettings {
                has_min_bitrate_bps: true,
                min_bitrate_bps: min_bps,
                has_start_bitrate_bps: start_bps.is_some(),
                start_bitrate_bps: start_bps.unwrap_or(0),
                has_max_bitrate_bps: true,
                max_bitrate_bps: max_bps,
            })
            .map_err(|e| MediaError::PeerConnection(rtc_error_message(&e)))
    }

    /// Minimum jitter-buffer delay on every audio receiver (`None` = automatic).
    pub fn set_jitter_buffer_min_delay(&self, delay: Option<Duration>) {
        for r in self.pc.get_receivers() {
            if let Some(r) = r.ptr.as_ref() {
                r.set_jitter_buffer_minimum_delay(delay.is_some(), delay.map_or(0.0, |d| d.as_secs_f64()));
            }
        }
    }

    pub async fn create_offer(&self) -> SdpResult {
        let (tx, rx) = oneshot::channel::<SdpResult>();
        self.pc.create_offer(
            Default::default(),
            ctx(tx),
            |c, sdp: UniquePtr<jsep::SessionDescription>| finish::<SdpResult>(c, Ok(sdp.stringify())),
            |c, e: RtcError| finish::<SdpResult>(c, Err(MediaError::Sdp(e.message))),
        );
        wait(rx).await?
    }

    pub async fn create_answer(&self) -> SdpResult {
        let (tx, rx) = oneshot::channel::<SdpResult>();
        self.pc.create_answer(
            Default::default(),
            ctx(tx),
            |c, sdp: UniquePtr<jsep::SessionDescription>| finish::<SdpResult>(c, Ok(sdp.stringify())),
            |c, e: RtcError| finish::<SdpResult>(c, Err(MediaError::Sdp(e.message))),
        );
        wait(rx).await?
    }

    fn description(kind: SdpKind, sdp: &str) -> Result<UniquePtr<jsep::SessionDescription>> {
        let t = match kind {
            SdpKind::Offer => jsep::SdpType::Offer,
            SdpKind::Answer => jsep::SdpType::Answer,
        };
        jsep::create_session_description(t, sdp.to_string())
            .map_err(|e| MediaError::Sdp(e.what().to_string()))
    }

    pub async fn set_local_description(&self, kind: SdpKind, sdp: &str) -> Result<()> {
        let desc = Self::description(kind, sdp)?;
        let (tx, rx) = oneshot::channel::<RtcError>();
        self.pc.set_local_description(desc, ctx(tx), |c, e| finish::<RtcError>(c, e));
        err_result(wait(rx).await?)
    }

    pub async fn set_remote_description(&self, kind: SdpKind, sdp: &str) -> Result<()> {
        let desc = Self::description(kind, sdp)?;
        let (tx, rx) = oneshot::channel::<RtcError>();
        self.pc.set_remote_description(desc, ctx(tx), |c, e| finish::<RtcError>(c, e));
        err_result(wait(rx).await?)
    }

    /// Rolls back a local offer (perfect negotiation, polite side on glare).
    pub async fn rollback(&self) -> Result<()> {
        let desc = jsep::create_session_description(jsep::SdpType::Rollback, String::new())
            .map_err(|e| MediaError::Sdp(e.what().to_string()))?;
        let (tx, rx) = oneshot::channel::<RtcError>();
        self.pc.set_local_description(desc, ctx(tx), |c, e| finish::<RtcError>(c, e));
        err_result(wait(rx).await?)
    }

    pub async fn add_ice_candidate(&self, mid: &str, mline_index: i32, candidate: &str) -> Result<()> {
        let c = jsep::create_ice_candidate(mid.to_string(), mline_index, candidate.to_string())
            .map_err(|e| MediaError::Candidate(e.what().to_string()))?;
        let (tx, rx) = oneshot::channel::<RtcError>();
        self.pc.add_ice_candidate(c, ctx(tx), |c, e| finish::<RtcError>(c, e));
        let e = wait(rx).await?;
        if e.ok() { Ok(()) } else { Err(MediaError::Candidate(e.message)) }
    }

    /// Raw `RTCStatsReport` JSON from libwebrtc's `GetStats`.
    pub async fn stats_json(&self) -> Result<String> {
        let (tx, rx) = oneshot::channel::<String>();
        self.pc.get_stats(ctx(tx), |c, json| finish::<String>(c, json));
        wait(rx).await
    }

    pub fn restart_ice(&self) {
        self.pc.restart_ice();
    }

    pub fn signaling_state(&self) -> SignalingState {
        self.pc.signaling_state()
    }

    pub fn connection_state(&self) -> PeerConnectionState {
        self.pc.connection_state()
    }

    pub fn local_description(&self) -> Option<String> {
        let d = self.pc.current_local_description();
        d.as_ref().map(|d| d.stringify())
    }

    pub fn create_data_channel(&self, label: &str, opts: DataChannelOptions) -> Result<DataChannel> {
        let init = opts.to_init();
        let dc = self
            .pc
            .create_data_channel(label.to_string(), init)
            .map_err(|e| MediaError::DataChannel(rtc_error_message(&e)))?;
        Ok(DataChannel::attach(dc, self.id, self.tx.clone()))
    }

    /// Wraps a channel opened by the remote side and starts delivering its events.
    pub fn adopt_channel(&self, ch: RemoteDataChannel) -> DataChannel {
        DataChannel::attach(ch.0, self.id, self.tx.clone())
    }

    pub fn close(&self) {
        self.pc.close();
    }
}

impl Drop for PeerConnection {
    fn drop(&mut self) {
        self.pc.close();
    }
}
