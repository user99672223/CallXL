/*
 * CallXL: Opus encoder factory with libwebrtc's audio network adaptor (ANA),
 * field-trial string parsing and the APM builder. MIT, CallXL authors.
 *
 * The prebuilt libwebrtc is built with rtc_enable_protobuf=false, so the ANA
 * cannot be configured from its protobuf config string. Instead the encoder is
 * created through AudioEncoderOpusImpl::CreateForTesting with a creator that
 * assembles libwebrtc's ControllerManagerImpl directly from controllers:
 *   1. LossFrameLengthController (below): 10 <-> 20 ms on 5 s loss averages
 *   2. audio_network_adaptor::BitrateController (libwebrtc): target bitrate
 *      minus per-packet overhead for the chosen frame length.
 * The frame length is therefore changed inside the running encoder (no SDP
 * renegotiation, no encoder re-creation), which is inaudible.
 */
#include <array>
#include <map>
#include <string>
#include <vector>

#include "absl/strings/match.h"
#include "api/audio/builtin_audio_processing_builder.h"
#include "api/audio_codecs/builtin_audio_encoder_factory.h"
#include "api/audio_codecs/opus/audio_encoder_opus.h"
#include "common_audio/smoothing_filter.h"
#include "livekit/callxl.h"
#include "livekit/peer_connection_factory.h"
#include "modules/audio_coding/audio_network_adaptor/audio_network_adaptor_impl.h"
#include "modules/audio_coding/audio_network_adaptor/bitrate_controller.h"
#include "modules/audio_coding/audio_network_adaptor/controller.h"
#include "modules/audio_coding/audio_network_adaptor/controller_manager.h"
#include "modules/audio_coding/codecs/opus/audio_encoder_opus.h"
#include "rtc_base/time_utils.h"

namespace callxl {

// ---------------------------------------------------------------------------
// Field trials
// ---------------------------------------------------------------------------

StringFieldTrials::StringFieldTrials(const std::string& s) {
  // "Key1/Value1/Key2/Value2/" — the libwebrtc field-trial string format.
  std::vector<std::string> parts;
  std::string cur;
  for (char c : s) {
    if (c == '/') {
      parts.push_back(cur);
      cur.clear();
    } else {
      cur.push_back(c);
    }
  }
  if (!cur.empty()) parts.push_back(cur);
  for (size_t i = 0; i + 1 < parts.size(); i += 2) {
    if (!parts[i].empty()) trials_.emplace_back(parts[i], parts[i + 1]);
  }
}

std::string StringFieldTrials::Lookup(absl::string_view key) const {
  for (const auto& kv : trials_) {
    if (kv.first == key) return kv.second;
  }
  return "";
}

std::unique_ptr<webrtc::FieldTrialsView> StringFieldTrials::CreateCopy() const {
  auto copy = std::make_unique<StringFieldTrials>("");
  copy->trials_ = trials_;
  return copy;
}

// ---------------------------------------------------------------------------
// ANA frame-length controller
// ---------------------------------------------------------------------------

namespace {

class LossFrameLengthController final : public webrtc::Controller {
 public:
  explicit LossFrameLengthController(std::shared_ptr<AnaShared> shared)
      : shared_(std::move(shared)), frame_ms_(shared_->min_frame_ms) {
    shared_->frame_length_ms.store(frame_ms_, std::memory_order_relaxed);
  }

  void UpdateNetworkMetrics(const NetworkMetrics& m) override {
    if (!m.uplink_packet_loss_fraction) return;
    const int64_t now = webrtc::TimeMillis();
    samples_[head_] = {now, *m.uplink_packet_loss_fraction};
    head_ = (head_ + 1) % samples_.size();
    if (count_ < samples_.size()) ++count_;
    shared_->last_loss_permille.store(
        static_cast<int>(*m.uplink_packet_loss_fraction * 1000.f),
        std::memory_order_relaxed);
  }

  void MakeDecision(webrtc::AudioEncoderRuntimeConfig* config) override {
    const int64_t now = webrtc::TimeMillis();
    float sum = 0.f;
    int n = 0;
    for (size_t i = 0; i < count_; ++i) {
      const Sample& s = samples_[i];
      if (now - s.t_ms <= shared_->window_ms) {
        sum += s.loss;
        ++n;
      }
    }
    if (n > 0) {
      const float avg = sum / static_cast<float>(n);
      shared_->avg_loss_permille.store(static_cast<int>(avg * 1000.f),
                                       std::memory_order_relaxed);
      if (frame_ms_ == shared_->min_frame_ms) {
        if (avg > shared_->increase_loss) {
          frame_ms_ = shared_->max_frame_ms;
          increased_ = true;
          low_since_ms_ = -1;
          shared_->switches.fetch_add(1, std::memory_order_relaxed);
        }
      } else if (avg < shared_->decrease_loss) {
        if (low_since_ms_ < 0) low_since_ms_ = now;
        if (now - low_since_ms_ >= shared_->decrease_hold_ms) {
          frame_ms_ = shared_->min_frame_ms;
          increased_ = false;
          low_since_ms_ = -1;
          shared_->switches.fetch_add(1, std::memory_order_relaxed);
        }
      } else {
        low_since_ms_ = -1;
      }
    } else if (frame_ms_ != shared_->min_frame_ms) {
      // No fresh loss reports for a whole window: treat as clean.
      if (low_since_ms_ < 0) low_since_ms_ = now;
      if (now - low_since_ms_ >= shared_->decrease_hold_ms) {
        frame_ms_ = shared_->min_frame_ms;
        increased_ = false;
        low_since_ms_ = -1;
        shared_->switches.fetch_add(1, std::memory_order_relaxed);
      }
    }
    config->frame_length_ms = frame_ms_;
    config->last_fl_change_increase = increased_;
    shared_->frame_length_ms.store(frame_ms_, std::memory_order_relaxed);
  }

 private:
  struct Sample {
    int64_t t_ms = 0;
    float loss = 0.f;
  };
  std::shared_ptr<AnaShared> shared_;
  std::array<Sample, 64> samples_{};
  size_t head_ = 0;
  size_t count_ = 0;
  int frame_ms_;
  bool increased_ = false;
  int64_t low_since_ms_ = -1;
};

// Runs last: caps the ANA's bitrate decision at the user's ceiling, so BWE and the
// RTP encoding limit can include packet overhead while the Opus target stays exact.
class CeilingController final : public webrtc::Controller {
 public:
  explicit CeilingController(std::shared_ptr<AnaShared> shared) : shared_(std::move(shared)) {}
  void UpdateNetworkMetrics(const NetworkMetrics&) override {}
  void MakeDecision(webrtc::AudioEncoderRuntimeConfig* config) override {
    const int cap = shared_->max_bitrate_bps.load(std::memory_order_relaxed);
    if (config->bitrate_bps && *config->bitrate_bps > cap) config->bitrate_bps = cap;
  }

 private:
  std::shared_ptr<AnaShared> shared_;
};

class CallXlAudioEncoderFactory : public webrtc::AudioEncoderFactory {
 public:
  CallXlAudioEncoderFactory(std::shared_ptr<AnaShared> ana,
                            int complexity,
                            bool ana_enabled)
      : builtin_(webrtc::CreateBuiltinAudioEncoderFactory()),
        ana_(std::move(ana)),
        complexity_(complexity),
        ana_enabled_(ana_enabled) {}

  std::vector<webrtc::AudioCodecSpec> GetSupportedEncoders() override {
    return builtin_->GetSupportedEncoders();
  }

  std::optional<webrtc::AudioCodecInfo> QueryAudioEncoder(
      const webrtc::SdpAudioFormat& format) override {
    return builtin_->QueryAudioEncoder(format);
  }

  std::unique_ptr<webrtc::AudioEncoder> Create(
      const webrtc::Environment& env,
      const webrtc::SdpAudioFormat& format,
      Options options) override {
    if (!absl::EqualsIgnoreCase(format.name, "opus")) {
      return builtin_->Create(env, format, options);
    }
    std::optional<webrtc::AudioEncoderOpusConfig> cfg =
        webrtc::AudioEncoderOpus::SdpToConfig(format);
    if (!cfg) return nullptr;
    cfg->complexity = complexity_;
    cfg->low_rate_complexity = complexity_;
    if (!cfg->IsOk()) return nullptr;

    std::shared_ptr<AnaShared> ana = ana_;
    ana->encoder_channels.store(static_cast<int>(cfg->num_channels), std::memory_order_relaxed);
    ana->encoder_complexity.store(cfg->complexity, std::memory_order_relaxed);
    const int initial_frame = cfg->frame_size_ms;
    const int initial_bitrate = cfg->bitrate_bps.value_or(64000);
    auto creator = [env, ana, initial_frame, initial_bitrate](absl::string_view)
        -> std::unique_ptr<webrtc::AudioNetworkAdaptor> {
      std::vector<std::unique_ptr<webrtc::Controller>> controllers;
      controllers.push_back(std::make_unique<LossFrameLengthController>(ana));
      controllers.push_back(
          std::make_unique<webrtc::audio_network_adaptor::BitrateController>(
              webrtc::audio_network_adaptor::BitrateController::Config(
                  initial_bitrate, initial_frame, 0, 0)));
      controllers.push_back(std::make_unique<CeilingController>(ana));
      std::map<const webrtc::Controller*, std::pair<int, float>> scoring;
      auto manager = std::make_unique<webrtc::ControllerManagerImpl>(
          env, webrtc::ControllerManagerImpl::Config(0, 0.f),
          std::move(controllers), scoring);
      ana->adaptors_created.fetch_add(1, std::memory_order_relaxed);
      return std::make_unique<webrtc::AudioNetworkAdaptorImpl>(
          env, std::move(manager));
    };
    if (!ana_enabled_) {
      ana->frame_length_ms.store(initial_frame, std::memory_order_relaxed);
      return std::make_unique<webrtc::AudioEncoderOpusImpl>(env, *cfg,
                                                            options.payload_type);
    }
    ana->frame_length_ms.store(initial_frame, std::memory_order_relaxed);
    return webrtc::AudioEncoderOpusImpl::CreateForTesting(
        env, *cfg, options.payload_type, creator,
        std::make_unique<webrtc::SmoothingFilterImpl>(5000));
  }

 private:
  webrtc::scoped_refptr<webrtc::AudioEncoderFactory> builtin_;
  std::shared_ptr<AnaShared> ana_;
  int complexity_;
  bool ana_enabled_;
};

}  // namespace

webrtc::scoped_refptr<webrtc::AudioEncoderFactory> CreateCallXlAudioEncoderFactory(
    std::shared_ptr<AnaShared> ana,
    int opus_complexity,
    bool ana_enabled) {
  return webrtc::make_ref_counted<CallXlAudioEncoderFactory>(
      std::move(ana), opus_complexity, ana_enabled);
}

// ---------------------------------------------------------------------------
// APM
// ---------------------------------------------------------------------------

RememberingApmBuilder::RememberingApmBuilder(
    std::shared_ptr<webrtc::scoped_refptr<webrtc::AudioProcessing>> slot,
    const webrtc::AudioProcessing::Config& initial)
    : slot_(std::move(slot)), initial_(initial) {}

webrtc::scoped_refptr<webrtc::AudioProcessing> RememberingApmBuilder::Build(
    const webrtc::Environment& env) {
  webrtc::BuiltinAudioProcessingBuilder builder(initial_);
  webrtc::scoped_refptr<webrtc::AudioProcessing> apm = builder.Build(env);
  *slot_ = apm;
  return apm;
}

webrtc::AudioProcessing::Config ToApmConfig(
    const livekit_ffi::CallXlApmSettings& s,
    webrtc::AudioProcessing::Config c) {
  c.echo_canceller.enabled = s.aec;
  c.echo_canceller.enforce_high_pass_filtering = s.aec && s.high_pass;
  c.noise_suppression.enabled = s.ns;
  switch (s.ns_level) {
    case 0:
      c.noise_suppression.level = webrtc::AudioProcessing::Config::NoiseSuppression::kLow;
      break;
    case 2:
      c.noise_suppression.level = webrtc::AudioProcessing::Config::NoiseSuppression::kHigh;
      break;
    default:
      c.noise_suppression.level =
          webrtc::AudioProcessing::Config::NoiseSuppression::kModerate;
      break;
  }
  c.high_pass_filter.enabled = s.high_pass;
  // AGC1 is never used; AGC2 adaptive digital is the only gain controller.
  c.gain_controller1.enabled = false;
  c.gain_controller2.enabled = s.agc2;
  c.gain_controller2.adaptive_digital.enabled = s.agc2;
  c.gain_controller2.adaptive_digital.max_gain_db = s.agc2_max_gain_db;
  c.gain_controller2.input_volume_controller.enabled = false;
  return c;
}

}  // namespace callxl
