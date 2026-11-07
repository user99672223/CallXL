//! Every type crossing the UniFFI boundary (and the CLI JSON protocol, via serde).

use serde::{Deserialize, Serialize};

pub type SessionId = String;
pub type MessageId = String;
pub type TransferId = String;
/// Ed25519 public key, lowercase hex.
pub type PeerKey = String;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AppConfig {
    pub data_dir: String,
    pub download_dir: String,
    pub device_name: String,
    pub signaling_url: Option<String>,
    pub headless: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct DeviceCard {
    pub display_name: String,
    /// Ed25519 public key, hex.
    pub signing_key: String,
    /// X25519 public key, hex.
    pub agreement_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum NsLevel {
    Low,
    Moderate,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct ApmConfig {
    pub aec: bool,
    pub noise_suppression: bool,
    pub ns_level: NsLevel,
    pub high_pass: bool,
    pub agc: bool,
}

impl Default for ApmConfig {
    /// CLAUDE.md §3: AEC3 on, NS on (moderate), high-pass on, AGC2 on.
    fn default() -> Self {
        Self { aec: true, noise_suppression: true, ns_level: NsLevel::Moderate, high_pass: true, agc: true }
    }
}

impl ApmConfig {
    pub const RAW: ApmConfig =
        ApmConfig { aec: false, noise_suppression: false, ns_level: NsLevel::Moderate, high_pass: false, agc: false };

    pub(crate) fn to_media(self) -> callxl_media::ApmSettings {
        callxl_media::ApmSettings {
            aec: self.aec,
            noise_suppression: self.noise_suppression,
            ns_level: match self.ns_level {
                NsLevel::Low => 0,
                NsLevel::Moderate => 1,
                NsLevel::High => 2,
            },
            high_pass: self.high_pass,
            agc: self.agc,
        }
    }

    pub(crate) fn from_media(s: callxl_media::ApmSettings) -> Self {
        Self {
            aec: s.aec,
            noise_suppression: s.noise_suppression,
            ns_level: match s.ns_level {
                0 => NsLevel::Low,
                2 => NsLevel::High,
                _ => NsLevel::Moderate,
            },
            high_pass: s.high_pass,
            agc: s.agc,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum CallState {
    Idle,
    Joining,
    InCall,
    Reconnecting { reason: String },
    Left { reason: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum PathKind {
    Lan,
    Direct,
    Relayed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum AddrFamily {
    V4,
    V6,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct PathInfo {
    pub kind: PathKind,
    pub local_addr: String,
    pub local_family: AddrFamily,
    pub remote_addr: String,
    pub remote_family: AddrFamily,
    pub local_candidate_type: String,
    pub remote_candidate_type: String,
    pub switches: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct PeerInfo {
    pub key: PeerKey,
    pub display_name: String,
    pub trusted: bool,
    pub connected: bool,
    pub speaking: bool,
    /// Signaling with this peer runs over the LAN (mDNS/TCP) rather than the cloud.
    pub lan: bool,
    pub path: Option<PathInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct SessionMember {
    pub key: PeerKey,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct SessionSummary {
    pub id: SessionId,
    pub name: Option<String>,
    pub members: Vec<SessionMember>,
    /// Seen on the LAN (UI shows the LAN glyph).
    pub lan: bool,
    /// Seen via the cloud lobby.
    pub cloud: bool,
    pub joined: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct TrustedDevice {
    pub key: PeerKey,
    pub display_name: String,
    pub online: bool,
    pub pinned_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum ChatKind {
    Text,
    File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum ReceiptStatus {
    Sending,
    Sent,
    Delivered,
    Read,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct ChatMessage {
    pub id: MessageId,
    pub session: SessionId,
    pub ts_ms: u64,
    pub sender_key: PeerKey,
    pub sender_name: String,
    pub kind: ChatKind,
    pub body: String,
    pub own: bool,
    pub status: ReceiptStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, uniffi::Enum)]
pub enum TransferDirection {
    Outgoing,
    Incoming,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum TransferState {
    Offered,
    Accepted,
    Transferring,
    Paused,
    Completed,
    Declined,
    Cancelled,
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct TransferInfo {
    pub id: TransferId,
    pub peer_key: PeerKey,
    pub direction: TransferDirection,
    pub name: String,
    pub size: u64,
    /// BLAKE3, hex.
    pub hash: String,
    pub bytes_done: u64,
    pub state: TransferState,
    /// Local file: source (outgoing) or destination (incoming).
    pub path: String,
    pub session: SessionId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum AudioDeviceKind {
    Input,
    Output,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AudioDevice {
    pub kind: AudioDeviceKind,
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Settings {
    pub display_name: String,
    /// None = DEFAULT_SIGNALING_URL.
    pub signaling_url: Option<String>,
    pub effective_signaling_url: String,
    pub download_dir: String,
    pub bitrate_ceiling_kbps: u32,
    pub apm: ApmConfig,
    pub raw_mic: bool,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum SignalingTransport {
    Lan,
    Cloud,
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct PeerStats {
    pub peer_key: PeerKey,
    pub display_name: String,
    pub path: Option<PathInfo>,
    pub rtt_ms: f64,
    pub jitter_ms: f64,
    pub loss_pct: f64,
    /// Loss of our outgoing stream as reported by the far end (RTCP), percent.
    pub remote_loss_pct: f64,
    pub send_kbps: f64,
    pub recv_kbps: f64,
    pub opus_frame_ms: u32,
    pub jitter_buffer_ms: f64,
    pub concealment_events: u64,
    pub pair_switches: u32,
    pub opus_bitrate_kbps: f64,
    /// Opus payload bytes actually produced over the last second.
    pub opus_payload_kbps: f64,
    pub fec_enabled: bool,
    pub dtx_enabled: bool,
    pub codec: String,
    pub clock_rate: u32,
    pub channels: u32,
    pub opus_complexity: u32,
    pub available_outgoing_kbps: f64,
    pub audio_level: f64,
    /// Data-channel (file) send rate allowed by the audio-first throttle; 0 = unthrottled.
    pub file_rate_limit_kbps: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct GlobalStats {
    pub uplink_kbps: f64,
    pub downlink_kbps: f64,
    pub media_thread_cpu_pct: f64,
    pub signaling_transport: SignalingTransport,
    pub signaling_latency_ms: f64,
    pub turn_bytes_month: u64,
    pub apm: ApmConfig,
    /// How Opus frame size adapts (always libwebrtc's audio network adaptor here).
    pub frame_adaptation_mechanism: String,
    pub ana_frame_ms: u32,
    pub ana_switches: u32,
    /// Uplink loss the frame-length controller currently averages (5 s window), percent.
    pub ana_uplink_loss_pct: f64,
    pub rejected_signaling: u64,
    pub glare_events: u64,
    /// AEC3: echo return loss / enhancement (dB), estimated delay (ms); -1 = n/a.
    pub aec_erl_db: f64,
    pub aec_erle_db: f64,
    pub aec_delay_ms: i32,
    pub dropped_media_events: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Sparkline {
    pub metric: String,
    pub values: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct DiagnosticsSnapshot {
    pub ts_ms: u64,
    pub peers: Vec<PeerStats>,
    pub global: GlobalStats,
    pub series: Vec<Sparkline>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct Snapshot {
    pub device: DeviceCard,
    pub sessions: Vec<SessionSummary>,
    pub current_session: Option<SessionId>,
    pub call_state: CallState,
    pub peers: Vec<PeerInfo>,
    pub muted: bool,
    pub trusted_devices: Vec<TrustedDevice>,
    pub transfers: Vec<TransferInfo>,
    pub settings: Settings,
    pub cloud_connected: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, uniffi::Enum)]
pub enum Event {
    SessionsChanged { sessions: Vec<SessionSummary> },
    TrustedDevicesChanged { devices: Vec<TrustedDevice> },
    CallStateChanged { state: CallState },
    PeerJoined { peer: PeerInfo },
    PeerLeft { peer_key: PeerKey, reason: String },
    PeerPathChanged { peer_key: PeerKey, path: PathInfo },
    SpeakingChanged { peer_key: PeerKey, speaking: bool },
    ChatReceived { message: ChatMessage },
    ReceiptUpdated { message_id: MessageId, status: ReceiptStatus, peer_key: PeerKey },
    FileOffered { transfer: TransferInfo },
    TransferProgress { transfer: TransferInfo },
    TrustPrompt { peer_key: PeerKey, display_name: String, phrase: String, key_changed: bool },
    StatsTick { snapshot: DiagnosticsSnapshot },
    DevicesChanged,
    Error { reason: String },
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Event::SessionsChanged { .. } => "SessionsChanged",
            Event::TrustedDevicesChanged { .. } => "TrustedDevicesChanged",
            Event::CallStateChanged { .. } => "CallStateChanged",
            Event::PeerJoined { .. } => "PeerJoined",
            Event::PeerLeft { .. } => "PeerLeft",
            Event::PeerPathChanged { .. } => "PeerPathChanged",
            Event::SpeakingChanged { .. } => "SpeakingChanged",
            Event::ChatReceived { .. } => "ChatReceived",
            Event::ReceiptUpdated { .. } => "ReceiptUpdated",
            Event::FileOffered { .. } => "FileOffered",
            Event::TransferProgress { .. } => "TransferProgress",
            Event::TrustPrompt { .. } => "TrustPrompt",
            Event::StatsTick { .. } => "StatsTick",
            Event::DevicesChanged => "DevicesChanged",
            Event::Error { .. } => "Error",
        }
    }

    /// The variant's fields as JSON (CLI protocol: `{"event": name, "data": ...}`).
    pub fn data(&self) -> serde_json::Value {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(m)) => m.into_iter().next().map(|(_, v)| v).unwrap_or(serde_json::Value::Null),
            _ => serde_json::Value::Null,
        }
    }
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    #[error("CallXL is already running")]
    AlreadyStarted,
    #[error("CallXL could not start: {reason}")]
    StartFailed { reason: String },
}
