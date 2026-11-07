/*
 * CallXL: factory construction and accessors. MIT, CallXL authors.
 * See CALLXL_CHANGES.md for the list of additions and why each is needed.
 */
#include "livekit/callxl.h"

#include <memory>
#include <utility>

#include "api/audio_codecs/builtin_audio_decoder_factory.h"
#include "api/audio_options.h"
#include "api/create_modular_peer_connection_factory.h"
#include "api/enable_media.h"
#include "api/environment/environment_factory.h"
#include "api/peer_connection_interface.h"
#include "api/rtc_event_log/rtc_event_log_factory.h"
#include "livekit/adm_proxy.h"
#include "livekit/audio_track.h"
#include "livekit/candidate.h"
#include "livekit/peer_connection.h"
#include "livekit/peer_connection_factory.h"
#include "livekit/rtc_error.h"
#include "livekit/video_decoder_factory.h"
#include "livekit/video_encoder_factory.h"
#include "livekit/webrtc.h"
#include "rtc_base/network.h"
#include "rtc_base/thread.h"
#include "webrtc-sys/src/callxl.rs.h"
#include "webrtc-sys/src/peer_connection.rs.h"
#include "webrtc-sys/src/peer_connection_factory.rs.h"

namespace livekit_ffi {

PeerConnectionFactory::PeerConnectionFactory(
    std::shared_ptr<RtcRuntime> rtc_runtime,
    const CallXlFactoryOptions& o)
    : rtc_runtime_(rtc_runtime),
      env_(webrtc::CreateEnvironment(std::make_unique<callxl::StringFieldTrials>(
          std::string(o.field_trials)))) {
  callxl_ana_ = std::make_shared<callxl::AnaShared>();
  callxl_ana_->increase_loss = o.ana_increase_loss;
  callxl_ana_->decrease_loss = o.ana_decrease_loss;
  callxl_ana_->window_ms = o.ana_window_ms;
  callxl_ana_->decrease_hold_ms = o.ana_decrease_hold_ms;
  callxl_apm_ = std::make_shared<webrtc::scoped_refptr<webrtc::AudioProcessing>>();

  webrtc::PeerConnectionFactoryDependencies dependencies;
  dependencies.network_thread = rtc_runtime_->network_thread();
  dependencies.worker_thread = rtc_runtime_->worker_thread();
  dependencies.signaling_thread = rtc_runtime_->signaling_thread();
  dependencies.socket_factory = rtc_runtime_->network_thread()->socketserver();
  dependencies.event_log_factory = std::make_unique<webrtc::RtcEventLogFactory>();
  dependencies.env = env_;

  if (o.network_ignore_list.size() > 0) {
    auto nm = std::make_unique<webrtc::BasicNetworkManager>(
        env_, rtc_runtime_->network_thread()->socketserver());
    std::vector<std::string> ignore;
    for (const auto& n : o.network_ignore_list) ignore.emplace_back(std::string(n));
    nm->set_network_ignore_list(ignore);
    dependencies.network_manager = std::move(nm);
  }

  if (o.use_test_adm) {
    callxl_test_adm_ = callxl::CreateTestAudioDevice(
        env_, std::string(o.test_play_wav), std::string(o.test_record_wav),
        o.test_record_max_secs);
    dependencies.adm = callxl_test_adm_;
  } else {
    adm_proxy_ = rtc_runtime_->worker_thread()->BlockingCall([&] {
      return webrtc::make_ref_counted<livekit_ffi::AdmProxy>(
          env_, rtc_runtime_->worker_thread());
    });
    audio_device_ = std::make_shared<AudioDeviceController>(rtc_runtime_, adm_proxy_);
    dependencies.adm = adm_proxy_;
  }

  auto video_encoder_factory = std::make_unique<livekit_ffi::VideoEncoderFactory>();
  video_encoder_factory_ = video_encoder_factory.get();
  dependencies.video_encoder_factory = std::move(video_encoder_factory);
  dependencies.video_decoder_factory =
      std::make_unique<livekit_ffi::VideoDecoderFactory>();
  dependencies.audio_encoder_factory = callxl::CreateCallXlAudioEncoderFactory(
      callxl_ana_, o.opus_complexity, o.ana_enabled);
  dependencies.audio_decoder_factory = webrtc::CreateBuiltinAudioDecoderFactory();
  dependencies.audio_processing_builder = std::make_unique<callxl::RememberingApmBuilder>(
      callxl_apm_, callxl::ToApmConfig(o.apm, webrtc::AudioProcessing::Config()));

  webrtc::EnableMedia(dependencies);
  peer_factory_ = webrtc::CreateModularPeerConnectionFactory(std::move(dependencies));
  if (!peer_factory_) {
    RTC_LOG_ERR(LS_ERROR) << "Failed to create CallXL PeerConnectionFactory";
    return;
  }
  if (o.network_ignore_mask >= 0) {
    webrtc::PeerConnectionFactoryInterface::Options opts;
    opts.network_ignore_mask = o.network_ignore_mask;
    peer_factory_->SetOptions(opts);
  }
}

std::shared_ptr<PeerConnection> PeerConnectionFactory::create_peer_connection_callxl(
    RtcConfiguration config,
    CallXlPcConfig ext,
    rust::Box<PeerConnectionObserverWrapper> observer) const {
  webrtc::PeerConnectionInterface::RTCConfiguration native =
      to_native_rtc_configuration(config);
  native.set_port_allocator_flags(ext.port_allocator_flags);
  if (ext.max_ipv6_networks > 0) native.max_ipv6_networks = ext.max_ipv6_networks;
  if (ext.ice_connection_receiving_timeout_ms > 0)
    native.ice_connection_receiving_timeout = ext.ice_connection_receiving_timeout_ms;
  if (ext.ice_backup_ping_interval_ms > 0)
    native.ice_backup_candidate_pair_ping_interval = ext.ice_backup_ping_interval_ms;
  if (ext.stable_writable_ping_interval_ms > 0)
    native.stable_writable_connection_ping_interval_ms =
        ext.stable_writable_ping_interval_ms;
  if (ext.ice_check_min_interval_ms > 0)
    native.ice_check_min_interval = ext.ice_check_min_interval_ms;
  native.audio_jitter_buffer_min_delay_ms = ext.audio_jitter_buffer_min_delay_ms;
  native.audio_jitter_buffer_fast_accelerate = ext.audio_jitter_buffer_fast_accelerate;
  if (ext.audio_rtcp_report_interval_ms > 0)
    native.set_audio_rtcp_report_interval_ms(ext.audio_rtcp_report_interval_ms);

  callxl::IcePolicy policy;
  policy.rtt_delta_ms = ext.ice.rtt_delta_ms;
  policy.loss_delta = ext.ice.loss_delta;
  policy.sustain_ms = ext.ice.sustain_ms;
  policy.cooldown_ms = ext.ice.cooldown_ms;
  policy.v6_bonus_ms = ext.ice.v6_bonus_ms;
  policy.initial_grace_ms = ext.ice.initial_grace_ms;
  policy.recheck_ms = ext.ice.recheck_ms > 0 ? ext.ice.recheck_ms : 500;

  std::shared_ptr<PeerConnection> pc =
      std::make_shared<PeerConnection>(rtc_runtime_, peer_factory_, std::move(observer));
  if (!pc->InitializeCallXl(native, ext.ice.enabled, policy)) {
    throw std::runtime_error(serialize_error(to_error(webrtc::RTCError(
        webrtc::RTCErrorType::INTERNAL_ERROR, "failed to initialize pc"))));
  }
  return pc;
}

std::shared_ptr<AudioTrack> PeerConnectionFactory::create_callxl_audio_track(
    rust::String label,
    CallXlApmSettings apm,
    bool ana) const {
  webrtc::AudioOptions options;
  options.echo_cancellation = apm.aec;
  // AGC1 stays off: AGC2 is configured directly on the APM (callxl_apply_apm).
  options.auto_gain_control = false;
  options.noise_suppression = apm.ns;
  options.highpass_filter = apm.high_pass;
  options.audio_jitter_buffer_fast_accelerate = true;
  if (ana) {
    // Any non-empty config enables the ANA; our encoder factory ignores its content.
    options.audio_network_adaptor = true;
    options.audio_network_adaptor_config = std::string("callxl");
  }
  webrtc::scoped_refptr<webrtc::AudioSourceInterface> source =
      peer_factory_->CreateAudioSource(options);
  if (!source) return nullptr;
  return std::static_pointer_cast<AudioTrack>(
      rtc_runtime_->get_or_create_media_stream_track(
          peer_factory_->CreateAudioTrack(label.c_str(), source.get())));
}

rust::String PeerConnectionFactory::field_trial_lookup(rust::String name) const {
  return rust::String(env_.field_trials().Lookup(std::string(name)));
}

bool PeerConnectionFactory::callxl_apply_apm(CallXlApmSettings settings) const {
  if (!callxl_apm_ || !*callxl_apm_) return false;
  webrtc::AudioProcessing* apm = callxl_apm_->get();
  apm->ApplyConfig(callxl::ToApmConfig(settings, apm->GetConfig()));
  return true;
}

CallXlApmSettings PeerConnectionFactory::callxl_get_apm() const {
  CallXlApmSettings s{};
  s.ns_level = 1;
  if (!callxl_apm_ || !*callxl_apm_) return s;
  const webrtc::AudioProcessing::Config c = (*callxl_apm_)->GetConfig();
  s.aec = c.echo_canceller.enabled;
  s.ns = c.noise_suppression.enabled;
  switch (c.noise_suppression.level) {
    case webrtc::AudioProcessing::Config::NoiseSuppression::kLow: s.ns_level = 0; break;
    case webrtc::AudioProcessing::Config::NoiseSuppression::kModerate: s.ns_level = 1; break;
    default: s.ns_level = 2; break;
  }
  s.high_pass = c.high_pass_filter.enabled;
  s.agc2 = c.gain_controller2.enabled && c.gain_controller2.adaptive_digital.enabled &&
           !c.gain_controller1.enabled;
  s.agc2_max_gain_db = c.gain_controller2.adaptive_digital.max_gain_db;
  return s;
}

CallXlApmStats PeerConnectionFactory::callxl_apm_stats() const {
  CallXlApmStats s{-1, -1, -1, -1, -1};
  if (!callxl_apm_ || !*callxl_apm_) return s;
  const webrtc::AudioProcessingStats st = (*callxl_apm_)->GetStatistics();
  s.echo_return_loss_db = st.echo_return_loss.value_or(-1);
  s.echo_return_loss_enhancement_db = st.echo_return_loss_enhancement.value_or(-1);
  s.divergent_filter_fraction = st.divergent_filter_fraction.value_or(-1);
  s.delay_ms = st.delay_ms.value_or(-1);
  s.residual_echo_likelihood = st.residual_echo_likelihood.value_or(-1);
  return s;
}

CallXlAnaState PeerConnectionFactory::callxl_ana_state() const {
  CallXlAnaState s{};
  if (!callxl_ana_) return s;
  s.frame_length_ms = callxl_ana_->frame_length_ms.load();
  s.switches = callxl_ana_->switches.load();
  s.adaptors_created = callxl_ana_->adaptors_created.load();
  s.last_loss_permille = callxl_ana_->last_loss_permille.load();
  s.avg_loss_permille = callxl_ana_->avg_loss_permille.load();
  s.encoder_channels = callxl_ana_->encoder_channels.load();
  s.encoder_complexity = callxl_ana_->encoder_complexity.load();
  return s;
}

void PeerConnectionFactory::callxl_set_max_bitrate(int32_t bps) const {
  if (callxl_ana_) callxl_ana_->max_bitrate_bps.store(bps, std::memory_order_relaxed);
}

bool PeerConnectionFactory::callxl_uses_test_adm() const {
  return callxl_test_adm_.get() != nullptr;
}

bool PeerConnectionFactory::callxl_test_adm_flush() const {
  return callxl::TestAudioDeviceFlush(callxl_test_adm_.get());
}

CallXlTestAdmStats PeerConnectionFactory::callxl_test_adm_stats() const {
  CallXlTestAdmStats s{};
  callxl::TestAudioDeviceStats(callxl_test_adm_.get(), s);
  return s;
}

std::shared_ptr<PeerConnectionFactory> create_callxl_peer_connection_factory(
    CallXlFactoryOptions options) {
  return std::make_shared<PeerConnectionFactory>(RtcRuntime::create(), options);
}

CallXlCandidateInfo callxl_candidate_info(const Candidate& candidate) {
  const webrtc::Candidate& c = candidate.get();
  CallXlCandidateInfo info{};
  info.address = c.address().ipaddr().ToString();
  info.port = static_cast<uint16_t>(c.address().port());
  info.family = c.address().family() == AF_INET6 ? 6 : 4;
  info.typ = std::string(c.is_local()   ? "host"
                         : c.is_stun()  ? "srflx"
                         : c.is_relay() ? "relay"
                                        : "prflx");
  info.protocol = c.protocol();
  info.priority = c.priority();
  info.network_name = c.network_name();
  info.foundation = c.foundation();
  return info;
}

}  // namespace livekit_ffi
