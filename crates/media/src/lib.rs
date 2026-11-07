//! CallXL media plane: a thin, safe wrapper over libwebrtc through the forked
//! `webrtc-sys` (see `third_party/webrtc-sys/CALLXL_CHANGES.md`).
//!
//! Rules (CLAUDE.md §1.3, §10):
//! - Audio never crosses into Rust: capture and playout run inside libwebrtc's
//!   audio device module (platform or the C++ test ADM).
//! - Every libwebrtc callback does exactly one thing: `try_send` a [`MediaEvent`]
//!   on a Tokio channel. Nothing here calls back into libwebrtc from a callback.
//! - `callxl-core` is the only caller of this crate, from one task.

mod data_channel;
mod engine;
mod peer;
pub mod policy;
pub mod sdp;
pub mod stats;

pub use data_channel::{DataChannel, DataChannelEvent, DataChannelOptions, DataState};
pub use engine::{
    AdmMode, AnaPolicy, AnaState, ApmSettings, AudioDeviceInfo, EngineOptions, MediaEngine, NetworkFilter, TestAdmStats,
};
pub use peer::{
    AudioTrack, CandidateInfo, IceConnectionState, IcePolicy, IceServer, MediaEvent, PeerConfig, PeerConnection,
    PeerConnectionState, PeerId, RemoteDataChannel, SdpKind, SignalingState, allocator_flags, dropped_events,
};

/// Errors surfaced by the media plane. Messages are plain English.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("the audio engine could not be started: {0}")]
    Engine(String),
    #[error("could not create a connection to the peer: {0}")]
    PeerConnection(String),
    #[error("the call setup message was rejected by the media engine: {0}")]
    Sdp(String),
    #[error("a network candidate was rejected: {0}")]
    Candidate(String),
    #[error("could not open a data channel: {0}")]
    DataChannel(String),
    #[error("the media engine did not answer in time")]
    Timeout,
    #[error("the media engine shut down")]
    Closed,
}

/// The field-trial string CallXL runs with.
pub fn engine_default_field_trials() -> &'static str {
    engine::DEFAULT_FIELD_TRIALS
}

pub type Result<T> = std::result::Result<T, MediaError>;

pub(crate) fn rtc_error_message(e: &cxx::Exception) -> String {
    // SAFETY: `from` no longer relies on caller-upheld invariants (see webrtc-sys).
    let err = unsafe { webrtc_sys::rtc_error::ffi::RtcError::from(e.what()) };
    if err.message.is_empty() { e.what().to_string() } else { err.message }
}
