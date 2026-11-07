/*
 * CallXL additions to webrtc-sys (MIT, CallXL authors). See CALLXL_CHANGES.md.
 *
 * Everything here is glue around libwebrtc's own machinery:
 *   - a string-configured FieldTrials view composed into the factory Environment
 *   - an AudioProcessing builder that keeps a handle so Rust can ApplyConfig/GetConfig
 *   - an Opus encoder factory (complexity 10) whose audio network adaptor (ANA)
 *     runs libwebrtc's BitrateController plus a loss-driven frame-length controller
 *   - a WAV-driven test AudioDeviceModule (no allocation after init)
 *   - an IceTransportFactory that installs CallXlIceController (wraps BasicIceController)
 */
#pragma once

#include <atomic>
#include <cstdint>
#include <memory>
#include <string>
#include <vector>

#include "api/audio/audio_device.h"
#include "api/audio/audio_processing.h"
#include "api/audio_codecs/audio_encoder_factory.h"
#include "api/environment/environment.h"
#include "api/field_trials_view.h"
#include "api/ice_transport_interface.h"
#include "api/scoped_refptr.h"
#include "rust/cxx.h"

namespace livekit_ffi {
struct CallXlFactoryOptions;
struct CallXlPcConfig;
struct CallXlApmSettings;
struct CallXlApmStats;
struct CallXlAnaState;
struct CallXlTestAdmStats;
struct CallXlCandidateInfo;
struct CallXlIceParams;
class PeerConnectionFactory;
class PeerConnectionObserverWrapper;
class Candidate;
}  // namespace livekit_ffi

namespace callxl {

// Field trials parsed from a "Name/Value/Name2/Value2/" string.
class StringFieldTrials final : public webrtc::FieldTrialsView {
 public:
  explicit StringFieldTrials(const std::string& s);
  std::string Lookup(absl::string_view key) const override;
  std::unique_ptr<webrtc::FieldTrialsView> CreateCopy() const override;

 private:
  std::vector<std::pair<std::string, std::string>> trials_;
};

// Shared, lock-free state written by the ANA controller (worker/encoder thread)
// and read by Rust's stats poller.
struct AnaShared {
  std::atomic<int> frame_length_ms{0};
  std::atomic<int> switches{0};
  std::atomic<int> adaptors_created{0};
  std::atomic<int> last_loss_permille{0};
  std::atomic<int> avg_loss_permille{0};
  std::atomic<int> encoder_channels{0};
  std::atomic<int> encoder_complexity{0};
  // User bitrate ceiling for the Opus target (bps); applied after BitrateController.
  std::atomic<int> max_bitrate_bps{510000};
  // Policy (set once at factory creation).
  float increase_loss = 0.05f;   // switch to 20 ms above this 5 s average
  float decrease_loss = 0.01f;   // back to 10 ms after `decrease_hold_ms` below this
  int window_ms = 5000;
  int decrease_hold_ms = 15000;
  int min_frame_ms = 10;
  int max_frame_ms = 20;
};

webrtc::scoped_refptr<webrtc::AudioEncoderFactory> CreateCallXlAudioEncoderFactory(
    std::shared_ptr<AnaShared> ana,
    int opus_complexity,
    bool ana_enabled);

// Parameters of the path-switching policy (CLAUDE.md section 4).
struct IcePolicy {
  int rtt_delta_ms = 15;
  float loss_delta = 0.05f;
  int sustain_ms = 5000;
  int cooldown_ms = 30000;
  int v6_bonus_ms = 5;
  int initial_grace_ms = 1500;
  int recheck_ms = 500;
};

// Builds ICE transports whose ICE controller is CallXlIceController.
std::unique_ptr<webrtc::IceTransportFactory> CreateCallXlIceTransportFactory(
    IcePolicy policy,
    const livekit_ffi::PeerConnectionObserverWrapper* observer);

// WAV-playing / WAV-recording AudioDeviceModule for tests.
webrtc::scoped_refptr<webrtc::AudioDeviceModule> CreateTestAudioDevice(
    const webrtc::Environment& env,
    const std::string& play_wav,
    const std::string& record_wav,
    uint32_t record_max_secs);
// `adm` must have been returned by CreateTestAudioDevice (or be null).
bool TestAudioDeviceFlush(webrtc::AudioDeviceModule* adm);
void TestAudioDeviceStats(webrtc::AudioDeviceModule* adm,
                          livekit_ffi::CallXlTestAdmStats& out);

// APM builder that remembers the APM it built.
class RememberingApmBuilder final : public webrtc::AudioProcessingBuilderInterface {
 public:
  explicit RememberingApmBuilder(
      std::shared_ptr<webrtc::scoped_refptr<webrtc::AudioProcessing>> slot,
      const webrtc::AudioProcessing::Config& initial);
  webrtc::scoped_refptr<webrtc::AudioProcessing> Build(
      const webrtc::Environment& env) override;

 private:
  std::shared_ptr<webrtc::scoped_refptr<webrtc::AudioProcessing>> slot_;
  webrtc::AudioProcessing::Config initial_;
};

webrtc::AudioProcessing::Config ToApmConfig(const livekit_ffi::CallXlApmSettings& s,
                                            webrtc::AudioProcessing::Config base);

}  // namespace callxl

namespace livekit_ffi {

// cxx-exposed free functions (declared in src/callxl.rs).
std::shared_ptr<PeerConnectionFactory> create_callxl_peer_connection_factory(
    CallXlFactoryOptions options);
CallXlCandidateInfo callxl_candidate_info(const Candidate& candidate);

}  // namespace livekit_ffi
