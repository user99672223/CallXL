/*
 * CallXL: ICE controller implementing CLAUDE.md section 4 "dynamic path
 * switching". MIT, CallXL authors.
 *
 * CallXlIceController wraps libwebrtc's BasicIceController and delegates all
 * pinging, pruning and nomination to it. Only the *switch* decisions of the
 * controlling agent are filtered once a pair is selected and stable:
 *   - an alternative must beat the selected pair by >= rtt_delta_ms RTT, or by
 *     >= loss_delta STUN-ping loss, or be IPv6 while the selected pair is IPv4
 *     and not worse (rtt within v6_bonus_ms, loss within 0.5 %),
 *   - continuously for sustain_ms,
 *   - and at least cooldown_ms after the previous switch.
 * Failover (selected pair destroyed / not writable / not receiving) is never
 * delayed. During the initial grace period a usable IPv6 pair that is not worse
 * replaces an IPv4 pick (IPv6 preference at setup). The controlled agent
 * follows nominations, except that for kControlledFlapGuardMs after a change
 * it ignores DATA_RECEIVED switches back to the pair it just left: packets still in flight on the old (slower) path
 * otherwise flip the selection on every packet (CI: 9 flips in 60 ms after one
 * controlling-side switch away from a 60 ms-delayed IPv6 path).
 * The controller runs on the network thread; it reports each switch it makes
 * through PeerConnectionObserverWrapper::on_ice_controller_decision, which on
 * the Rust side does exactly one try_send.
 */
#include <cstdlib>
#include <map>
#include <string>

#include "livekit/callxl.h"
#include "livekit/peer_connection_factory.h"
#include "p2p/base/basic_ice_controller.h"
#include "p2p/base/connection.h"
#include "p2p/base/default_ice_transport_factory.h"
#include "p2p/base/ice_controller_factory_interface.h"
#include "p2p/base/p2p_transport_channel.h"
#include "rtc_base/logging.h"
#include "rtc_base/time_utils.h"
#include "webrtc-sys/src/peer_connection_factory.rs.h"

namespace callxl {
namespace {

using webrtc::Connection;
using webrtc::IceControllerInterface;
using webrtc::IceSwitchReason;

struct ConnMetrics {
  int64_t last_sample_ms = -1;
  size_t last_sent = 0;
  size_t last_recv = 0;
  float loss = 0.f;  // EWMA of per-sample ping loss fraction
  int64_t lost = 0;  // pings known lost (cumulative)
  int64_t qualifying_since_ms = -1;
  std::string qualifying_reason;
};

constexpr int64_t kControlledFlapGuardMs = 1000;

bool IsV6(const Connection* c) {
  return c->local_candidate().address().family() == AF_INET6;
}

class CallXlIceController final : public IceControllerInterface {
 public:
  CallXlIceController(const webrtc::IceControllerFactoryArgs& args,
                      IcePolicy policy,
                      const livekit_ffi::PeerConnectionObserverWrapper* observer)
      : inner_(args),
        role_(args.ice_role_func),
        policy_(policy),
        observer_(observer),
        trace_(std::getenv("CALLXL_ICE_TRACE") != nullptr) {}

  void SetIceConfig(const webrtc::IceConfig& config) override {
    inner_.SetIceConfig(config);
  }
  void SetSelectedConnection(const Connection* c) override {
    inner_.SetSelectedConnection(c);
    if (c && c != selected_) {
      const int64_t now = webrtc::TimeMillis();
      if (first_selected_ms_ < 0) first_selected_ms_ = now;
      previous_selected_ = selected_;
      selected_changed_ms_ = now;
      selected_ = c;
      for (auto& kv : metrics_) kv.second.qualifying_since_ms = -1;
    } else if (!c) {
      selected_ = nullptr;
    }
  }
  void AddConnection(const Connection* c) override { inner_.AddConnection(c); }
  void OnConnectionDestroyed(const Connection* c) override {
    inner_.OnConnectionDestroyed(c);
    metrics_.erase(c);
    if (selected_ == c) selected_ = nullptr;
    if (previous_selected_ == c) previous_selected_ = nullptr;
  }
  std::span<const Connection* const> GetConnections() const override {
    return inner_.GetConnections();
  }
  bool HasPingableConnection() const override {
    return inner_.HasPingableConnection();
  }
  PingResult GetConnectionToPing(webrtc::Timestamp last_ping_sent) override {
    return inner_.GetConnectionToPing(last_ping_sent);
  }
  bool GetUseCandidateAttr(const Connection* c,
                           webrtc::NominationMode mode,
                           webrtc::IceMode remote_ice_mode) const override {
    return inner_.GetUseCandidateAttr(c, mode, remote_ice_mode);
  }
  const Connection* FindNextPingableConnection() override {
    return inner_.FindNextPingableConnection();
  }
  void MarkConnectionPinged(const Connection* c) override {
    inner_.MarkConnectionPinged(c);
  }
  std::vector<const Connection*> PruneConnections() override {
    return inner_.PruneConnections();
  }

  SwitchResult ShouldSwitchConnection(IceSwitchReason reason,
                                      const Connection* c) override {
    return Filter(reason, inner_.ShouldSwitchConnection(reason, c));
  }
  SwitchResult SortAndSwitchConnection(IceSwitchReason reason) override {
    return Filter(reason, inner_.SortAndSwitchConnection(reason));
  }

 private:
  // Ping loss per connection. A ping counts as lost only once it can no longer be
  // in flight: lost = outstanding (sent - responses) minus the pings sent within
  // the last RTT + 50 ms (at least one). Counting every unanswered ping at sample
  // time instead read pings in flight as lost (EWMA spikes to 0.3 on a loss-free
  // path), which kept the prefer-IPv6 rule from ever holding for sustain_ms.
  void Sample(int64_t now) {
    for (const Connection* c : inner_.GetConnections()) {
      ConnMetrics& m = metrics_[c];
      if (m.last_sample_ms >= 0 && now - m.last_sample_ms < 250) continue;
      webrtc::ConnectionInfo info = const_cast<Connection*>(c)->stats();
      const int64_t sent = static_cast<int64_t>(info.sent_ping_requests_total);
      const int64_t recv = static_cast<int64_t>(info.recv_ping_responses);
      if (m.last_sample_ms >= 0 && sent > static_cast<int64_t>(m.last_sent)) {
        const int64_t ds = sent - static_cast<int64_t>(m.last_sent);
        const int64_t dt = now - m.last_sample_ms;
        const int64_t rtt = c->rtt_samples() > 0 ? c->Rtt().ms() : 1000;
        int64_t in_flight = (ds * (rtt + 50) + dt - 1) / (dt > 0 ? dt : 1);
        if (in_flight < 1) in_flight = 1;
        int64_t lost = sent - recv - in_flight;
        if (lost < m.lost) lost = m.lost;  // monotone: answered pings are not "unlost"
        const float l = static_cast<float>(lost - m.lost) / static_cast<float>(ds);
        m.lost = lost;
        m.loss = 0.7f * m.loss + 0.3f * (l > 1.f ? 1.f : l);
      } else if (m.last_sample_ms < 0) {
        m.lost = sent - recv - 1 > 0 ? sent - recv - 1 : 0;
      }
      m.last_sent = static_cast<size_t>(sent);
      m.last_recv = static_cast<size_t>(recv);
      m.last_sample_ms = now;
    }
  }

  // Debug trace of the switching policy's inputs (every 2 s, controlling side).
  // Off unless CALLXL_ICE_TRACE is set (read once): it allocates on the network
  // thread, which production builds must not do (CLAUDE.md section 10).
  void Trace(int64_t now) {
    if (!trace_ || now - last_trace_ms_ < 2000) return;
    last_trace_ms_ = now;
    std::string t = "CallXL ICE eval:";
    for (const Connection* c : inner_.GetConnections()) {
      const ConnMetrics& m = metrics_[c];
      t += std::string(" [") + (c == selected_ ? "*" : "") + (IsV6(c) ? "v6" : "v4") +
           " w" + std::to_string(c->writable()) + " r" + std::to_string(c->receiving()) +
           " n" + std::to_string(c->rtt_samples()) + " rtt" + std::to_string(c->Rtt().ms()) +
           " loss" + std::to_string(static_cast<int>(m.loss * 1000)) + "/1000 q" +
           std::to_string(m.qualifying_since_ms < 0 ? -1 : now - m.qualifying_since_ms) + "]";
    }
    RTC_LOG(LS_INFO) << t;
  }

  bool Usable(const Connection* c) const {
    return c && c->writable() && c->receiving() && c->rtt_samples() > 0;
  }

  SwitchResult Recheck(int64_t now) {
    SwitchResult r;
    if (recheck_pending_until_ms_ < now) {
      recheck_pending_until_ms_ = now + policy_.recheck_ms;
      r.recheck_event = webrtc::IceRecheckEvent(
          IceSwitchReason::ICE_CONTROLLER_RECHECK, policy_.recheck_ms);
    }
    return r;
  }

  void Report(const std::string& what) {
    if (observer_) observer_->on_ice_controller_decision(rust::String(what));
  }

  SwitchResult Filter(IceSwitchReason reason, SwitchResult inner) {
    const int64_t now = webrtc::TimeMillis();
    if (reason == IceSwitchReason::ICE_CONTROLLER_RECHECK &&
        recheck_pending_until_ms_ <= now + 1) {
      recheck_pending_until_ms_ = -1;
    }
    Sample(now);

    // Controlled side follows the controlling agent's nominations, without
    // flapping back on late packets from the path it just left.
    if (role_ && role_() != webrtc::ICEROLE_CONTROLLING) {
      if (reason == IceSwitchReason::DATA_RECEIVED && inner.connection &&
          *inner.connection && *inner.connection == previous_selected_ &&
          selected_ && Usable(selected_) &&
          now - selected_changed_ms_ < kControlledFlapGuardMs) {
        return SwitchResult{};
      }
      return inner;
    }

    // No usable selection yet, failover, or initial settling: libwebrtc decides.
    const bool selected_ok = selected_ && Usable(selected_);
    const bool in_grace = first_selected_ms_ < 0 ||
                          now - first_selected_ms_ < policy_.initial_grace_ms;
    if (!selected_ok || in_grace) {
      if (inner.connection && *inner.connection && *inner.connection != selected_ &&
          selected_ && !in_grace) {
        last_switch_ms_ = now;
        Report("failover");
      }
      // IPv6 preference at setup (section 4): libwebrtc selects whichever pair
      // became writable first; while settling, take a usable IPv6 pair that is
      // not worse than the IPv4 pick (RTT within v6_bonus_ms). Inside the grace
      // period this is the initial choice, not a counted switch.
      if (in_grace) {
        const Connection* pick =
            inner.connection && *inner.connection ? *inner.connection : selected_;
        if (pick && !IsV6(pick) && Usable(pick)) {
          const Connection* v6 = nullptr;
          for (const Connection* c : inner_.GetConnections()) {
            if (!IsV6(c) || !Usable(c) ||
                c->local_candidate().component() != pick->local_candidate().component() ||
                c->Rtt().ms() > pick->Rtt().ms() + policy_.v6_bonus_ms)
              continue;
            if (!v6 || c->Rtt() < v6->Rtt()) v6 = c;
          }
          if (v6 && v6 != selected_) inner.connection = v6;
        }
      }
      // Keep a recheck chain alive: in a stable call libwebrtc requests no sorts at
      // all, so without it the policy below would never be evaluated (observed: no
      // decision in 12 s with the selected IPv6 pair at 120 ms RTT vs ~0 ms IPv4).
      if (!inner.recheck_event) inner.recheck_event = Recheck(now).recheck_event;
      return inner;
    }

    // Stable selection: apply the CallXL policy.
    Trace(now);
    const ConnMetrics& sm = metrics_[selected_];
    const int64_t sel_rtt = selected_->Rtt().ms();
    const Connection* best = nullptr;
    int64_t best_since = INT64_MAX;
    std::string best_reason;
    for (const Connection* c : inner_.GetConnections()) {
      if (c == selected_) continue;
      ConnMetrics& m = metrics_[c];
      bool qualifies = false;
      std::string why;
      if (Usable(c) && c->local_candidate().component() ==
                           selected_->local_candidate().component()) {
        const int64_t rtt = c->Rtt().ms();
        if (sel_rtt - rtt >= policy_.rtt_delta_ms) {
          qualifies = true;
          why = "rtt " + std::to_string(sel_rtt) + "->" + std::to_string(rtt) + " ms";
        } else if (sm.loss - m.loss >= policy_.loss_delta) {
          qualifies = true;
          why = "loss " + std::to_string(static_cast<int>(sm.loss * 100)) + "->" +
                std::to_string(static_cast<int>(m.loss * 100)) + " %";
        } else if (IsV6(c) && !IsV6(selected_) &&
                   rtt <= sel_rtt + policy_.v6_bonus_ms &&
                   m.loss <= sm.loss + 0.005f) {
          qualifies = true;
          why = "prefer-ipv6";
        }
      }
      if (!qualifies) {
        m.qualifying_since_ms = -1;
        continue;
      }
      if (m.qualifying_since_ms < 0) m.qualifying_since_ms = now;
      if (m.qualifying_since_ms < best_since) {
        best_since = m.qualifying_since_ms;
        best = c;
        best_reason = why;
      }
    }

    const bool cooled = last_switch_ms_ < 0 || now - last_switch_ms_ >= policy_.cooldown_ms;
    if (best && cooled && now - best_since >= policy_.sustain_ms) {
      last_switch_ms_ = now;
      SwitchResult r;
      r.connection = best;
      recheck_pending_until_ms_ = now + policy_.recheck_ms;
      r.recheck_event = webrtc::IceRecheckEvent(
          IceSwitchReason::ICE_CONTROLLER_RECHECK, policy_.recheck_ms);
      Report(std::string(IsV6(selected_) ? "v6" : "v4") + "->" +
             (IsV6(best) ? "v6" : "v4") + ": " + best_reason);
      return r;
    }
    // Suppress libwebrtc's own priority-based re-sorting; re-evaluate soon.
    return Recheck(now);
  }

  webrtc::BasicIceController inner_;
  std::function<webrtc::IceRole()> role_;
  IcePolicy policy_;
  const livekit_ffi::PeerConnectionObserverWrapper* observer_;
  std::map<const Connection*, ConnMetrics> metrics_;
  const Connection* selected_ = nullptr;
  const Connection* previous_selected_ = nullptr;
  int64_t selected_changed_ms_ = -1;
  int64_t first_selected_ms_ = -1;
  int64_t last_switch_ms_ = -1;
  int64_t recheck_pending_until_ms_ = -1;
  int64_t last_trace_ms_ = -1;
  const bool trace_;
};

class CallXlIceControllerFactory final
    : public webrtc::IceControllerFactoryInterface {
 public:
  CallXlIceControllerFactory(IcePolicy policy,
                             const livekit_ffi::PeerConnectionObserverWrapper* obs)
      : policy_(policy), observer_(obs) {}
  std::unique_ptr<IceControllerInterface> Create(
      const webrtc::IceControllerFactoryArgs& args) override {
    return std::make_unique<CallXlIceController>(args, policy_, observer_);
  }

 private:
  IcePolicy policy_;
  const livekit_ffi::PeerConnectionObserverWrapper* observer_;
};

class CallXlIceTransportFactory final : public webrtc::IceTransportFactory {
 public:
  CallXlIceTransportFactory(IcePolicy policy,
                            const livekit_ffi::PeerConnectionObserverWrapper* obs)
      : controller_factory_(policy, obs) {}

  webrtc::scoped_refptr<webrtc::IceTransportInterface> CreateIceTransport(
      const std::string& transport_name,
      int component,
      webrtc::IceTransportInit init) override {
    init.set_ice_controller_factory(&controller_factory_);
    return webrtc::make_ref_counted<webrtc::DefaultIceTransport>(
        webrtc::P2PTransportChannel::Create(transport_name, component,
                                            std::move(init)));
  }

 private:
  CallXlIceControllerFactory controller_factory_;
};

}  // namespace

std::unique_ptr<webrtc::IceTransportFactory> CreateCallXlIceTransportFactory(
    IcePolicy policy,
    const livekit_ffi::PeerConnectionObserverWrapper* observer) {
  return std::make_unique<CallXlIceTransportFactory>(policy, observer);
}

}  // namespace callxl
