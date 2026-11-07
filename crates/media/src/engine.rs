use cxx::SharedPtr;
use serde::{Deserialize, Serialize};
use webrtc_sys::audio_device_controller::ffi::AudioDeviceController;
use webrtc_sys::callxl::ffi as cx;
use webrtc_sys::peer_connection_factory::ffi as pcf;

use crate::peer::{AudioTrack, PeerConfig, PeerConnection, PeerId};
use crate::{MediaError, MediaEvent, Result};

/// Which audio device module libwebrtc uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmMode {
    /// The OS audio stack via libwebrtc's platform ADM (PulseAudio/PipeWire, WASAPI, ...).
    Platform,
    /// C++ test ADM: plays `play_wav` as the microphone, records far-end playout to
    /// `record_wav` (48 kHz mono PCM16), at most `record_max_secs` seconds.
    Test { play_wav: String, record_wav: String, record_max_secs: u32 },
}

/// Audio processing (APM) settings. Defaults are CLAUDE.md §3.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApmSettings {
    pub aec: bool,
    pub noise_suppression: bool,
    /// 0 = low, 1 = moderate, 2 = high
    pub ns_level: i32,
    pub high_pass: bool,
    pub agc: bool,
}

impl Default for ApmSettings {
    fn default() -> Self {
        Self { aec: true, noise_suppression: true, ns_level: 1, high_pass: true, agc: true }
    }
}

/// "Mild" adaptive digital AGC2: cap the gain well below libwebrtc's 50 dB default.
pub const AGC2_MAX_GAIN_DB: f32 = 30.0;

impl ApmSettings {
    pub const RAW: ApmSettings =
        ApmSettings { aec: false, noise_suppression: false, ns_level: 1, high_pass: false, agc: false };

    fn to_ffi(self) -> pcf::CallXlApmSettings {
        pcf::CallXlApmSettings {
            aec: self.aec,
            ns: self.noise_suppression,
            ns_level: self.ns_level,
            high_pass: self.high_pass,
            agc2: self.agc,
            agc2_max_gain_db: AGC2_MAX_GAIN_DB,
        }
    }

    fn from_ffi(s: &pcf::CallXlApmSettings) -> Self {
        Self {
            aec: s.aec,
            noise_suppression: s.ns,
            ns_level: s.ns_level,
            high_pass: s.high_pass,
            agc: s.agc2,
        }
    }
}

/// Opus frame-length adaptation inside libwebrtc's audio network adaptor (§3).
#[derive(Clone, Copy, Debug)]
pub struct AnaPolicy {
    pub enabled: bool,
    /// Switch 10 → 20 ms when the 5 s average uplink loss exceeds this fraction.
    pub increase_loss: f32,
    /// Switch back to 10 ms after `decrease_hold_ms` below this fraction.
    pub decrease_loss: f32,
    pub window_ms: i32,
    pub decrease_hold_ms: i32,
}

impl Default for AnaPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            increase_loss: 0.05,
            decrease_loss: 0.01,
            window_ms: 5_000,
            decrease_hold_ms: 15_000,
        }
    }
}

/// Restricts which local networks libwebrtc gathers candidates on.
#[derive(Clone, Debug, Default)]
pub struct NetworkFilter {
    /// `network_ignore_mask`; `None` keeps libwebrtc's default (ignore loopback).
    pub ignore_mask: Option<i32>,
    /// Interface names to ignore.
    pub ignore_interfaces: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct EngineOptions {
    pub field_trials: String,
    pub adm: AdmMode,
    pub network: NetworkFilter,
    pub apm: ApmSettings,
    pub ana: AnaPolicy,
    pub opus_complexity: i32,
}

/// Field trials from CLAUDE.md §3, with names checked against the pinned libwebrtc
/// (`registered_field_trials.h`). `WebRTC-Audio-SendSideBwe` and
/// `WebRTC-SendSideBwe-WithOverhead` no longer exist there: audio send-side BWE is on
/// whenever transport-cc is negotiated and overhead accounting is the default
/// (`WebRTC-Audio-LegacyOverhead` would disable it). `WebRTC-Audio-AlrProbing` lets the
/// app-limited audio stream trigger ALR probes; without it the audio allocation stays
/// at the start rate (measured: 83.2 kbps target forever vs 510 kbps within ~1 s).
/// See DECISIONS.md.
pub const DEFAULT_FIELD_TRIALS: &str =
    "WebRTC-Audio-Allocation/min:32kbps,max:600kbps/WebRTC-Audio-AlrProbing/Enabled/";

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            field_trials: DEFAULT_FIELD_TRIALS.to_string(),
            adm: AdmMode::Platform,
            network: NetworkFilter::default(),
            apm: ApmSettings::default(),
            ana: AnaPolicy::default(),
            opus_complexity: 10,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnaState {
    pub frame_length_ms: i32,
    pub switches: i32,
    pub adaptors_created: i32,
    pub last_loss_permille: i32,
    pub avg_loss_permille: i32,
    pub encoder_channels: i32,
    pub encoder_complexity: i32,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct TestAdmStats {
    pub active: bool,
    pub recorded_frames: u64,
    pub captured_frames: u64,
    pub max_tick_late_us: i64,
    pub late_ticks: u64,
    pub record_capacity_frames: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub input: bool,
    pub index: u16,
    pub id: String,
    pub name: String,
}

/// One libwebrtc PeerConnectionFactory (threads, ADM, APM, codecs). One per process.
pub struct MediaEngine {
    factory: SharedPtr<pcf::PeerConnectionFactory>,
    adm: AdmMode,
    ana_enabled: bool,
    platform_acquired: std::sync::atomic::AtomicBool,
}

// SAFETY: PeerConnectionFactory is thread-safe (webrtc-sys marks it Send + Sync).
unsafe impl Send for MediaEngine {}
unsafe impl Sync for MediaEngine {}

impl MediaEngine {
    pub fn new(opts: EngineOptions) -> Result<Self> {
        let (use_test, play, record, secs) = match &opts.adm {
            AdmMode::Platform => (false, String::new(), String::new(), 0),
            AdmMode::Test { play_wav, record_wav, record_max_secs } => {
                (true, play_wav.clone(), record_wav.clone(), *record_max_secs)
            }
        };
        let ffi_opts = pcf::CallXlFactoryOptions {
            field_trials: opts.field_trials.clone(),
            network_ignore_mask: opts.network.ignore_mask.unwrap_or(-1),
            network_ignore_list: opts.network.ignore_interfaces.clone(),
            use_test_adm: use_test,
            test_play_wav: play,
            test_record_wav: record,
            test_record_max_secs: secs,
            opus_complexity: opts.opus_complexity,
            ana_enabled: opts.ana.enabled,
            ana_increase_loss: opts.ana.increase_loss,
            ana_decrease_loss: opts.ana.decrease_loss,
            ana_window_ms: opts.ana.window_ms,
            ana_decrease_hold_ms: opts.ana.decrease_hold_ms,
            apm: opts.apm.to_ffi(),
        };
        let factory = cx::create_callxl_peer_connection_factory(ffi_opts);
        if factory.is_null() {
            return Err(MediaError::Engine("libwebrtc refused to create the factory".into()));
        }
        Ok(Self {
            factory,
            adm: opts.adm,
            ana_enabled: opts.ana.enabled,
            platform_acquired: Default::default(),
        })
    }

    pub fn adm_mode(&self) -> &AdmMode {
        &self.adm
    }

    /// Reads a field trial back from the factory's Environment (proves it is applied).
    pub fn field_trial(&self, name: &str) -> String {
        self.factory.field_trial_lookup(name.to_string())
    }

    /// Microphone track captured by the ADM (platform or test). Audio stays in C++.
    pub fn create_audio_track(&self, apm: ApmSettings) -> Result<AudioTrack> {
        let track =
            self.factory.create_callxl_audio_track("callxl-mic".into(), apm.to_ffi(), self.ana_enabled);
        if track.is_null() {
            return Err(MediaError::Engine("could not create the microphone track".into()));
        }
        if self.adm == AdmMode::Platform {
            self.ensure_platform_audio();
        }
        Ok(AudioTrack::new(track))
    }

    fn controller(&self) -> Option<SharedPtr<AudioDeviceController>> {
        if self.adm != AdmMode::Platform {
            return None;
        }
        let c = self.factory.audio_device();
        (!c.is_null()).then_some(c)
    }

    fn ensure_platform_audio(&self) {
        use std::sync::atomic::Ordering;
        if self.platform_acquired.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(c) = self.controller() {
            if !c.acquire_platform_adm() {
                tracing::warn!("platform audio device could not be opened");
            }
            c.set_adm_recording_enabled(true);
            c.set_adm_playout_enabled(true);
        }
    }

    pub fn create_peer(
        &self,
        id: PeerId,
        cfg: &PeerConfig,
        events: tokio::sync::mpsc::Sender<MediaEvent>,
    ) -> Result<PeerConnection> {
        PeerConnection::create(&self.factory, id, cfg, events)
    }

    pub fn apply_apm(&self, apm: ApmSettings) -> bool {
        self.factory.callxl_apply_apm(apm.to_ffi())
    }

    /// The live APM configuration read back from libwebrtc (`AudioProcessing::GetConfig`).
    pub fn apm(&self) -> ApmSettings {
        ApmSettings::from_ffi(&self.factory.callxl_get_apm())
    }

    /// AEC3 statistics: (ERL dB, ERLE dB, divergent filter fraction, delay ms, residual
    /// echo likelihood); -1 = not available yet.
    pub fn apm_stats(&self) -> (f64, f64, f64, i32, f64) {
        let s = self.factory.callxl_apm_stats();
        (s.echo_return_loss_db, s.echo_return_loss_enhancement_db, s.divergent_filter_fraction, s.delay_ms, s.residual_echo_likelihood)
    }

    pub fn ana_state(&self) -> AnaState {
        let s = self.factory.callxl_ana_state();
        AnaState {
            frame_length_ms: s.frame_length_ms,
            switches: s.switches,
            adaptors_created: s.adaptors_created,
            last_loss_permille: s.last_loss_permille,
            avg_loss_permille: s.avg_loss_permille,
            encoder_channels: s.encoder_channels,
            encoder_complexity: s.encoder_complexity,
        }
    }

    /// Caps the Opus target bitrate (user ceiling, §3) inside the audio network adaptor.
    pub fn set_max_opus_bitrate(&self, bps: i32) {
        self.factory.callxl_set_max_bitrate(bps);
    }

    pub fn test_adm_flush(&self) -> bool {
        self.factory.callxl_test_adm_flush()
    }

    pub fn test_adm_stats(&self) -> Option<TestAdmStats> {
        if !self.factory.callxl_uses_test_adm() {
            return None;
        }
        let s = self.factory.callxl_test_adm_stats();
        Some(TestAdmStats {
            active: s.active,
            recorded_frames: s.recorded_frames,
            captured_frames: s.played_frames,
            max_tick_late_us: s.max_tick_late_us,
            late_ticks: s.late_ticks,
            record_capacity_frames: s.record_capacity_frames,
        })
    }

    pub fn audio_devices(&self) -> Vec<AudioDeviceInfo> {
        let mut out = Vec::new();
        match self.controller() {
            Some(c) => {
                for i in 0..c.recording_devices().max(0) as u16 {
                    out.push(AudioDeviceInfo {
                        input: true,
                        index: i,
                        id: c.recording_device_guid(i),
                        name: c.recording_device_name(i),
                    });
                }
                for i in 0..c.playout_devices().max(0) as u16 {
                    out.push(AudioDeviceInfo {
                        input: false,
                        index: i,
                        id: c.playout_device_guid(i),
                        name: c.playout_device_name(i),
                    });
                }
            }
            None => {
                out.push(AudioDeviceInfo {
                    input: true,
                    index: 0,
                    id: "callxl-test-in".into(),
                    name: "CallXL test capture (WAV)".into(),
                });
                out.push(AudioDeviceInfo {
                    input: false,
                    index: 0,
                    id: "callxl-test-out".into(),
                    name: "CallXL test playout (WAV)".into(),
                });
            }
        }
        out
    }

    pub fn set_input_device(&self, id: Option<&str>) -> bool {
        match self.controller() {
            Some(c) => match id {
                Some(id) => c.set_recording_device_by_guid(id.to_string()),
                None => c.set_recording_device(0),
            },
            None => true,
        }
    }

    pub fn set_output_device(&self, id: Option<&str>) -> bool {
        match self.controller() {
            Some(c) => match id {
                Some(id) => c.set_playout_device_by_guid(id.to_string()),
                None => c.set_playout_device(0),
            },
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_trials_applied() {
        let engine = MediaEngine::new(EngineOptions {
            adm: AdmMode::Test { play_wav: String::new(), record_wav: String::new(), record_max_secs: 0 },
            ..Default::default()
        })
        .expect("engine");
        assert_eq!(engine.field_trial("WebRTC-Audio-Allocation"), "min:32kbps,max:600kbps");
        assert_eq!(engine.field_trial("WebRTC-Audio-AlrProbing"), "Enabled");
        assert_eq!(engine.field_trial("WebRTC-Does-Not-Exist"), "");
    }

    #[test]
    fn apm_config_applied_and_read_back() {
        let engine = MediaEngine::new(EngineOptions {
            adm: AdmMode::Test { play_wav: String::new(), record_wav: String::new(), record_max_secs: 0 },
            ..Default::default()
        })
        .expect("engine");
        // The APM is built lazily by the voice engine on factory creation.
        assert_eq!(engine.apm(), ApmSettings::default());
        assert!(engine.apply_apm(ApmSettings::RAW));
        assert_eq!(engine.apm(), ApmSettings::RAW);
        assert!(engine.apply_apm(ApmSettings::default()));
        assert_eq!(engine.apm(), ApmSettings::default());
    }
}
