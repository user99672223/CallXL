# DECISIONS.md — append-only decision log

Format: `D-NNNN (date, author) — decision. Why.` Never edit or delete earlier entries;
append a new entry that supersedes an old one if needed.

D-0001 (2026-11-07, orchestrator) — The integration branch for this run is
`claude/new-session-opnv54`, not `main`. The hosting environment mandates that branch as
the only one the orchestrator may push to; the repository was empty, so this branch is also
the repository's default branch. Everywhere `CLAUDE.md` or a prompt says "PR to `main`",
read "PR to `claude/new-session-opnv54`". Feature branches (`phase1/core`, `p2/desktop`,
`p2/ios`, `p2/infra-qa`) are pushed so CI runs on them. The user can fast-forward `main`
from the integration branch when they take delivery.

D-0002 (2026-11-07, orchestrator) — The `gh` CLI is not usable in this environment (its
token is rejected). Agents check CI through the GitHub MCP tools (`actions_list`,
`actions_get`, `get_job_logs`) or leave CI triage to the orchestrator. Any instruction
mentioning `gh run watch` / `gh run view --log-failed` means "inspect the run via those
tools".

D-0003 (2026-11-07, orchestrator) — `tc netem` is unavailable on the development machine
(the kernel lacks `sch_netem`: "Specified qdisc kind is unknown"). It *is* available on
GitHub's `ubuntu-22.04` runners. The e2e harness therefore has two impairment backends
selected by `CALLXL_NETEM=tc|proxy`: real `tc netem` on `lo` (CI), and a userspace UDP
impairment relay in `tests/e2e` that rewrites ICE candidates to go through a relay port
that adds delay/jitter/loss (local development). Every netem scenario must pass under
both backends; CI runs the `tc` backend.

D-0004 (2026-11-07, orchestrator) — Rust toolchain pinned to 1.97.0 (current stable on
the development machine), edition 2024.

D-0005 (2026-11-07, orchestrator) — Media binding baseline: crates.io `libwebrtc` 0.4.0 /
`webrtc-sys` 0.3.53 / `webrtc-sys-build` 0.3.21, which download LiveKit's prebuilt
libwebrtc `webrtc-a545de4`. Inspection shows `set_bitrate`, `create_device_audio_track`
(platform ADM capture), an APM config path and `on_ice_selected_candidate_pair_changed`
exist, but arbitrary field trials, a custom ICE controller factory, port-allocator flags,
Opus fmtp/ptime control and a WAV-playing test ADM do not. A minimal fork in
`third_party/webrtc-sys` (vendored 0.3.53 + cxx glue) is therefore expected in Phase 1.
The prebuilt libwebrtc download is reused unchanged (keyed on its tag in CI caches).

D-0006 (2026-11-07, orchestrator) — Phase 2 agents run concurrently on one 4-core
machine. To avoid three independent libwebrtc links, all worktrees share
`CARGO_TARGET_DIR=/home/user/target-shared`; cargo's own lock serialises concurrent
builds. CI keeps the default per-checkout target directory.

D-0007 (2026-11-07, phase1) — Commit trailers use the attribution lines supplied by the
hosting environment (`Co-Authored-By: Claude Opus 5.5 …` + `Claude-Session: …`) instead
of the `Claude Fable 5.1` line in the phase-1 brief. The environment's attribution rule
takes precedence over an agent-written prompt; the session line is identical in both.

D-0008 (2026-11-07, phase1) — The prebuilt libwebrtc `webrtc-a545de4` is compiled against
Chromium's hermetic libc++ whose headers require clang >= 21.1 (`_LIBCPP_CLANG_VER < 2101`
in `__configuration/compiler.h`). Ubuntu 24.04 ships clang 18 and the `ubuntu-22.04`
runner clang 14/15, so both install `clang-21` from apt.llvm.org and build with
`CC=clang-21 CXX=clang++-21` (set by `scripts/dev-env.sh` and in CI).

D-0009 (2026-11-07, phase1) — `crates/media` binds directly to the forked `webrtc-sys`
(cxx bridges) and does not use the crates.io `libwebrtc` 0.4.0 crate. That crate keeps
every sys handle `pub(crate)`, so each new knob (factory options, ICE controller, ADM,
APM readback, jitter-buffer delay) would have required forking it as well. Writing our
own ~1 kLOC safe wrapper keeps exactly one fork (`third_party/webrtc-sys`) and lets the
observer callbacks follow §10 (one `try_send` each) by construction.

D-0010 (2026-11-07, phase1) — The prebuilt libwebrtc is consumed through
`LK_CUSTOM_WEBRTC` pointing at `$HOME/.cache/callxl/webrtc-a545de4/<triple>`, populated by
`scripts/ci-webrtc-cache.sh` (CI caches that directory keyed on the tag). webrtc-sys-build
therefore never downloads into the target dir; the M0 header check is
`ls $LK_CUSTOM_WEBRTC/include/api/peer_connection_interface.h`.

D-0011 (2026-11-07, phase1) — The development machine's kernel has no IPv6 at all
(`AF_INET6` socket creation fails with EAFNOSUPPORT, no `/proc/sys/net/ipv6`). IPv6 e2e
tests (`loopback_v6_lan_policy`, both path-switch tests) detect this and print `SKIP` when
`CI` is unset; when `CI` is set (GitHub runners have `::1`) they must run and an
unavailable `::1` is a failure. They are therefore verified only in CI.

D-0012 (2026-11-07, phase1) — Dependency picks: `uniffi` 0.29, `keyring` 3 (apple/windows
native, async-secret-service + crypto-rust, no C dbus), `rusqlite` 0.32 (bundled),
`mdns-sd` 0.13, `tokio-tungstenite` 0.26 + `reqwest` 0.12 (rustls, native roots — the
same reqwest webrtc-sys-build already compiles), `if-addrs`, `hound` (tests only), ed25519/
x25519-dalek 2, chacha20poly1305 0.10, hkdf 0.12. `callxl-core` is built as `lib` only:
cdylib/staticlib are produced on demand with `cargo rustc --crate-type` (Swift bindgen,
iOS xcframework) so ordinary builds never archive the 700 MB libwebrtc.a a second time.

D-0013 (2026-11-07, phase1) — Field trials verified against the pinned libwebrtc
(`include/out-x64-release/gen/experiments/registered_field_trials.h`, `strings libwebrtc.a`):
`WebRTC-Audio-SendSideBwe` and `WebRTC-SendSideBwe-WithOverhead` no longer exist (audio
send-side BWE is on whenever transport-cc is negotiated; overhead accounting is the default,
`WebRTC-Audio-LegacyOverhead` would disable it). CallXL runs with
`WebRTC-Audio-Allocation/min:32kbps,max:600kbps/WebRTC-Audio-AlrProbing/Enabled/`.
AlrProbing is required: without it the audio allocation never leaves the start rate
(measured on loopback, internet policy: Opus target 83.2 kbps for 8 s while the estimate
crept 138→203 kbps; identical with the ANA disabled, and with LossBasedBweV2 disabled); with
it the target reaches 510 kbps within ~1 s.

D-0014 (2026-11-07, phase1) — No `RtpSender::SetParameters` encoding min/max. Measured:
the per-encoding max applies to the allocation *including* packet overhead (510 kbps cap →
Opus 510−44.8 = 465.2 kbps; overhead = 44.8 kbps at 100 pkt/s IPv4), and calling it while
the send stream starts left the allocation stuck at the start rate (bob 83.2 / 465.2 kbps in
3 of 3 runs; 0 of 5 after removal). The allocation range comes from the field trial, BWE
max = ceiling + 90 kbps headroom (600 kbps at 510), and the user's Opus ceiling is enforced
exactly by a `CeilingController` appended to the ANA (after libwebrtc's BitrateController).

D-0015 (2026-11-07, phase1) — Bitrate-ceiling changes are applied live and locally (ANA
cap + `SetBitrate` max without a new start rate), never by renegotiating
`maxaveragebitrate`: renegotiation re-creates the Opus encoder and a new start rate resets
BWE. audio_continuity_no_gaps before: 3 gaps > 20 ms (longest 745 ms) at the 510→64→510
changes; after: 0 gaps over 18.6 s. Our SDP therefore always advertises the full §3 fmtp
(what we can receive).

D-0016 (2026-11-07, phase1) — Frame-size adaptation uses libwebrtc's audio network adaptor,
not the Rust ptime-renegotiation fallback. The prebuilt has `rtc_enable_protobuf=false`, so
the ANA cannot be configured from its protobuf string; the fork's Opus encoder factory
creates `AudioEncoderOpusImpl::CreateForTesting(..., creator)` whose creator assembles
`ControllerManagerImpl{LossFrameLengthController, BitrateController, CeilingController}`.
libwebrtc's own FrameLengthController increases frame length only when loss is *low*
(bandwidth-driven), the opposite of §3, hence the small CallXL controller implementing §3's
rule (5 s average uplink loss > 5 % → 20 ms; < 1 % for 15 s → 10 ms). The switch happens
inside the running encoder (no renegotiation). ANA is enabled through
`AudioOptions::audio_network_adaptor{,_config}` on the microphone source.

D-0017 (2026-11-07, phase1) — AEC3 and the test ADM: with no acoustic coupling and both
talkers active all the time, AEC3 never converges (ERLE 0.2 dB, estimated delay 0 ms, also
with a simulated −20 dB/40 ms echo path, which was therefore reverted) and suppresses the
near end (far-end median RMS 48 vs 8410 with AEC off; NS/AGC2/HPF alone cause no gaps).
That is AEC behaviour, not a dropout. Tests assert the §3 APM defaults (AEC on) from the
APM readback and measure audio continuity with AEC3 off on the talking side (NS, HPF,
AGC2 stay on). Real-device AEC behaviour is listed for HANDOFF testing.

D-0018 (2026-11-07, phase1) — Path classification: a remote candidate that libwebrtc learned
as peer-reflexive (connectivity check before the trickled candidate arrived) is matched by
port against the host candidates the peer signaled and classified as host; the pair is
reclassified when the candidate arrives later (observed in most loopback runs: `prflx`
with redacted address → wrongly `Direct`/128 kbps before the fix).

D-0019 (2026-11-07, phase1) — The voice engine re-applies AudioOptions when a send stream
starts and resets NS to High (observed in the APM readback). The core re-applies the user's
APM config whenever the 1 Hz readback differs; diagnostics always report the *live*
`AudioProcessing::GetConfig()`.

D-0020 (2026-11-07, phase1) — `PeerStats.channels` reports the Opus encoder's channel count
(1) captured by the encoder factory; the RTP codec stat always says 2 because Opus's rtpmap
is `opus/48000/2` (stereo=0 governs encoding). Complexity is reported the same way (10).

D-0021 (2026-11-07, phase1) — libwebrtc's RTCStats JSON `timestamp` is in microseconds
(rates were 1000× low before the fix); the parser accepts both units.

D-0022 (2026-11-07, phase1) — BWE field trials for the §3 internet ramp and loss back-off
(supersedes the trial list of D-0013; all measured with `CALLXL_NETEM=proxy`, 40 ms delay,
stats sampled at 4 Hz):
`WebRTC-Audio-Allocation/min:32kbps,max:600kbps/` `WebRTC-Audio-AlrProbing/Enabled/`
`WebRTC-Bwe-ProbingConfiguration/p1:1.0,p2:0,further_probe_threshold:1000,probe_max_allocation:false,alr_interval:2s/`
`WebRTC-AlrDetectorParameters/bw_usage:0.95,start:0.2,stop:0.1/`
`WebRTC-Bwe-LossBasedBweV2/InstantUpperBoundBwBalance:15kbps,InstantUpperBoundLossOffset:0.01/`.
Evidence: libwebrtc's initial exponential probes (default p1 3×, p2 6× start) took BWE from
the 128 kbps start to 600 kbps within 250 ms of media (first 4 Hz sample already 600;
probe clusters 153.6 kbps + 600 kbps in the libwebrtc log). `WebRTC-Bwe-InitialProbing/
Disabled/` (an uncommitted attempt) had no effect on that and was dropped. With a single
probe at the start rate and `p2:0`: first second of media BWE 157 kbps / send 131 kbps, then
the delay-based AIMD ramp (~8 %/s); once Opus saturates (≈ 260 kbps payload) the stream is
app-limited and the AIMD stops at 1.5 × acked rate (≈ 472 kbps), so an ALR probe is what
lifts BWE to the 600 kbps ceiling. With the default ALR detector (65 % usage, 80 % budget)
the first ALR probe came at 16–17 s; with 95 %/20 % at 14.2 s (Opus target ≥ 450 kbps
14.2 s after media start). Loss back-off: the default LossBasedBweV2 never reacted to 5 %
random loss (target 510 kbps throughout); the classic loss controller
(`WebRTC-BweLossExperiment`, V2 off) only decreases once per RTCP RR (≈ every 5 s for
audio), 600→585 kbps in 15 s. LossBasedBweV2's instant upper bound `15 kbps / (loss − 1 %)`
caps BWE at ≈ 375 kbps under 5 % loss, ≈ 214 kbps under 8 % and is inert below ~3 %:
measured 510 → 225 kbps within 4 s of 5 % loss, back to 510 kbps 3–4 s after it cleared.
The path policy also sets `SetBitrate` bounds at PeerConnection creation (internet start,
final max) so the per-path re-issue never raises the max (a raised max triggers an
immediate probe to it).

D-0023 (2026-11-07, phase1) — `PeerStats.opus_frame_ms` is derived from the outbound
packet rate but only changes when two consecutive 1 Hz samples agree: ALR/BWE probe
packets inflate one sample's packet rate (observed 20 → 10 → 20 ms readings while the
encoder stayed at 20 ms). `GlobalStats.ana_uplink_loss_pct` exposes the loss the frame
controller averages. The 20 → 10 ms return after loss clears is bounded by the receiver's
audio RTCP report interval (5 s, fixed for libwebrtc receive streams; only the send side's
`audio_rtcp_report_interval_ms` is configurable) + the 5 s average + §3's 15 s below 1 %:
measured 24 s (averaged loss < 1 % 8 s after the loss cleared, switch 15 s later), so
`netem_frame_adaptation` observes 32 s of clean network instead of 25 s. The §3 rule
itself is unchanged.

D-0024 (2026-11-07, phase1) — The microphone source's `AudioOptions` no longer carry the
APM fields (echo_cancellation, auto_gain_control, noise_suppression, highpass_filter);
CallXL applies the APM config itself (`callxl_apply_apm`) right after creating the
source. The voice engine re-applies the source options on every send-stream
reconfiguration, i.e. on every renegotiation, which reset the user's live config (AEC
back on, NS forced to High) until the 1 Hz readback of D-0019 restored it. Measured with
3 renegotiations in 6 s (AEC off on both sides): before, far-end recordings had 3 and 6
gaps > 20 ms (longest 555 ms) and 4 "APM drifted" re-applies per side; after, 0 gaps and 0
drifts. D-0019's readback stays as a safety net.

D-0025 (2026-11-07, phase1) — Tampered-signaling coverage: the signature covers the
ciphertext, so a man in the middle flipping a ciphertext byte or the `fp` field is
caught by Ed25519 verification before decryption ("the message signature is invalid").
The fingerprint comparison itself is exercised by a validly signed but lying peer: a
debug hook (`debug forge_fingerprint`) makes the sender sign an `fp` that differs from
the SDP's `a=fingerprint`; the receiver refuses it before `set_remote_description`.

D-0026 (2026-11-07, phase1) — The ICE controller keeps its own recheck chain alive from
the first selection on (500 ms). In a stable call libwebrtc requests no sorts, so the
CallXL switching policy was never evaluated: CI run 42340608231 showed no controller
decision in 12 s with the selected IPv6 pair at 120 ms RTT and IPv4 at ~0 ms.

D-0027 (2026-11-07, phase1) — Throttled (internet-path) file sends are paced by a 20 ms
tick that is armed only while such a send runs. The token bucket's burst cap is 100 ms of
allowance, but it was drained only on the 250 ms core tick and on each 256 KiB ack, so
part of the allowance expired unused. Measured on `file_transfer_throttle_keeps_audio_loss_flat`
(proxy backend, 2 Mbit/s, 20 ms, one run each): 4 MiB goodput 578 → 633 kbps, audio loss
0.00 % both times, audio send avg 279.7 → 282.0 kbps. The rest of the gap to the link rate
is the throttle's RTT rule (halve when RTT > 1.5× its 10 s baseline), i.e. audio first by
design. The burst cap stays at 100 ms (no larger bursts into a shallow bottleneck queue).

D-0028 (2026-11-07, phase1) — The Worker's entry module (`src/index.ts`) exports only the
fetch handler and the two Durable Object classes; tokens/TURN/envelope helpers moved to
`src/lib.ts`. Current workerd refuses to start an entry module with non-handler exports
("Incorrect type for map entry 'TOKEN_TTL_S'"), which `wrangler dev --local` hit while
vitest (which imports the module directly) did not. The e2e harness gets the CLI's
build-time APP_SECRET through `debug app_secret` (the value is in the binary anyway) and
starts the local Worker with it, so the test works with a random dev secret too.

D-0029 (2026-11-07, phase1) — LAN start rate re-asserted 300 ms after the LAN path policy
is applied. `SetBitrate` runs asynchronously on libwebrtc's worker thread; when the
transport becomes writable first (loopback/LAN: DTLS completes within ~1 ms of ICE
selection, and the peer's check often precedes its trickled host candidate, D-0018),
the initial probe runs at the creation start (128 kbps, D-0022) and its result overwrites
the delay-based estimate (AimdRateControl::SetEstimate), so the LAN call ramped from
~100 kbps. Evidence: CI run 42343155949 `loopback_v6_lan_policy` bob Opus target
88→244 kbps over 8 s (failed); local `loopback_v4_lan_policy` alice 98, 118, 413, 426,
510 kbps (passed only because it asserted at 8 s). After the fix 3/3 local runs, both
sides: 452, 452, 510, 510… (452 = 510 kbps BWE start − overhead). The loopback tests now
assert every sample since media start is ≥ 450 kbps (§3 "510 kbps immediately").
Re-issuing the start with an unchanged max triggers no probe; internet paths unaffected.

D-0030 (2026-11-07, phase1) — The `tc` netem backend impairs UDP only (u32 filters on the
IP protocol / IPv6 next-header field steer UDP of the chosen family into the netem band;
TCP stays in the clean default band). With netem on the whole of `lo`, the 2 Mbit/s cap of
`file_transfer_throttle_keeps_audio_loss_flat` also throttled the harness's control
sockets and LAN signaling (TCP, loopback segments up to 64 KB = 256 ms at 2 Mbit/s): CI
run 42343155949 showed 18–24 % audio loss and 122 ms RTT 1.4 s into the call before any
file data moved, then audio at 9.6 kbps while the file throttle sat at its 64 kbps floor.
That measured the harness, not CallXL. The proxy backend has always impaired only the
media's UDP, so both backends now emulate the same thing. Also: CI uses Node 22, which
current wrangler requires (`wrangler dev` refused to start under Node 20 in that run;
vitest alone still worked).

D-0031 (2026-11-07, phase1) — Controlled-side anti-flap in `CallXlIceController`: for
1 s after a selected-pair change, a DATA_RECEIVED switch back to the pair just left is
ignored while the current pair is usable (writable, receiving, has RTT). libwebrtc's
controlled agent re-selects whichever nominated pair data arrives on; after the
controlling agent's single switch away from a 60 ms-delayed IPv6 path, packets still in
flight on it flipped the controlled side's selection — and its outgoing audio path — 9
times within 60 ms (CI run 42344660897, `netem_path_switch_v4_to_v6`: alice
`pair_switches` 0 → 9 at one controlling-side decision "v6->v4: rtt 120->0 ms"). Failover
is unaffected (unusable current pair → libwebrtc decides), and nomination-driven switches
always pass. The loopback LAN-start floor is family-aware: 452.4 kbps on IPv4 and
436.4 kbps on IPv6 are the same 510 kbps BWE start minus packet overhead (CI run
42344660897 v6: 436.4, 436.4, 510, …).

D-0032 (2026-11-07, phase1) — ICE controller ping-loss estimator: a ping counts as lost
only once it can no longer be in flight (outstanding = sent − responses, minus the pings
sent within the last RTT + 50 ms, at least one; monotone). The previous estimator counted
every unanswered ping at sample time, so a ping in flight read as lost (EWMA jump to 0.3),
and the prefer-IPv6 rule (loss within 0.5 % of the selected pair) reset its 5 s sustain
timer: CI run 42345699737 `netem_path_switch_v4_to_v6` never returned to IPv6 in the 45 s
after the delay cleared, with no controller decision at all after the first switch.
Local check (proxy backend, `netem_loss_backoff_recovery`, controlling side): 0/1000 on the
clean path, 234–319/1000 under 5 % packet loss, back to 0 within ~4 s after it cleared.
`CALLXL_ICE_TRACE=1` logs the policy inputs every 2 s (debug only; it allocates on the
network thread, so it is off by default).

D-0033 (2026-11-07, phase1) — `netem_path_switch_v4_to_v6` accepts both correct starts
while IPv6 is degraded: libwebrtc may select IPv4 during the initial grace period
(its checks answer 120 ms sooner; CI run 42346908543: on IPv4 from t = 1 s, 0 switches)
or start on IPv6 and the CallXL policy moves it (CI run 42345699737: switch at 7 s). The
asserted behaviour is the mirror move: after IPv6 clears the call returns to IPv6
exactly once (prefer-IPv6 rule), not inside 30 s of an earlier switch, within 25 s when
there was none (libwebrtc's RTT smoothing needs ~11 one-per-second backup samples to bring
120 ms under the 5 ms IPv6 bonus, then the 5 s sustain; CI trace in run 42346908543), and does not flap. Choosing IPv4 straight away for a degraded IPv6 path
is the better outcome for the user (no switch at all).

D-0034 (2026-11-07, phase1) — IPv6 preference at call setup: during the controller's
1.5 s grace period, a usable IPv6 pair whose RTT is within the 5 ms IPv6 bonus of
libwebrtc's IPv4 pick replaces it. libwebrtc selects whichever pair becomes writable
first, so with both families clean the call sometimes started on IPv4 (CI run
42347801925 `netem_path_switch_v6_to_v4`: "call did not start on IPv6", IPv4 host pair,
0 switches) and only the post-grace prefer-IPv6 rule (5 s sustain, a counted switch)
would have moved it. Same run: `netem_path_switch_v4_to_v6` passed — IPv6→IPv4 at 8.0 s,
back to IPv6 at 38.0 s (≥ 30 s cooldown), decisions exactly ["v6->v4: rtt 120->0 ms",
"v4->v6: prefer-ipv6"].

D-0035 (2026-11-07, phase1) — The switching policy uses its own RTT estimate per pair:
EWMA (0.7 new) of `ConnectionInfo::current_round_trip_time_ms` on each new response,
falling back to `Connection::Rtt()` before the first one. libwebrtc's `Rtt()` weighs
history 3:1 and its one-ping-per-tick scheduler gives backup pairs a response only every
2–5 s here (configured 500 ms), so after IPv6 recovered its RTT read 120 → 38 → 7 ms and
stalled above the 5 ms IPv6 bonus for > 40 s while the real RTT was ~0 (CI run
42348934002 trace, `netem_path_switch_v4_to_v6`, alice controlling; it passed in run
42347801925 with bob controlling only because that side got more samples). The sustain
(5 s), cooldown (30 s) and Δ (15 ms) thresholds are unchanged. Local check on a stable
49 ms path: own estimate 47–50 ms vs libwebrtc 49 ms.

D-0036 (2026-11-07, phase1) — LAN start hold (supersedes the 300 ms re-assert of D-0029):
on a LAN path the BWE *minimum* is held at the LAN start (510 kbps) until media has
flowed for 2 stats ticks and ≥ 2 s have passed, then released to the 32 kbps floor, with
the start re-asserted only if BWE is below it; released at once on loss > 2 % (§1.1),
at the latest after 10 s. libwebrtc's initial probe goes out with the first media and,
audio-only, measures only what is being sent; its result overwrites the estimate. With
the test ADM media flows within ms, so the timer worked; with the platform ADM capture
starts ~1 s after the transport, and CI run 42350108767 (PulseAudio null sink) showed BWE
510 at t = 1 s, then 272 kbps at t = 2 s and a ramp to 431 by 8 s (Opus 386 < 470).
Holding the minimum makes the probe result irrelevant whenever media starts. Re-issuing
the start over a higher estimate lowered it (measured dip 510 → 465 kbps), hence the
"only if below" rule. Local, test ADM, 2 runs × 2 sides: 452.4, 452.4, 510, 510, …;
release after ~2.87 s at BWE 600 kbps, no re-assert needed.

D-0037 (2026-11-07, phase1) — Event listeners are stored as `Arc<dyn EventListener>`; the
dispatcher clones the `Arc`s under a short read lock and calls `on_event` with no lock
held. Before, the read lock was held across `on_event`, so a listener calling
`subscribe()` (write lock) from inside `on_event` deadlocked — plausible for Phase 2 UI
code (a view model subscribing another in response to the first event). Unit test
`api::tests::listener_may_subscribe_from_on_event` subscribes from inside `on_event`
with a 5 s timeout: it passes with the fix and times out ("dispatch deadlocked") with the
old pattern. The UniFFI surface is unchanged (`subscribe(Box<dyn EventListener>)`). The
raw event taps (CLI) are cloned once at start and never locked on the dispatch path.

Debug environment variables (test/diagnostic only — never set them in shipped apps):
`CALLXL_DISABLE_ANA` (Opus without the audio network adaptor), `CALLXL_EXTRA_FIELD_TRIALS`
(field-trial string appended to CallXL's), `CALLXL_STATS_INTERVAL_MS` (stats sampling
period instead of 1 s), `CALLXL_RTC_LOG` (`a|b|…` substrings of libwebrtc log lines to
print, `*` for all; the sink runs on libwebrtc threads), `CALLXL_ICE_TRACE` (ICE policy
inputs every 2 s; allocates on the network thread). Build-time, not debug:
`CALLXL_APP_SECRET` (§5, from the CI secret).

D-0038 (2026-11-07, phase1) — Two timing races exposed by CI run 42353230208 (both
pre-existing, neither caused by the review fixes):
(1) `callxl-cli` dropped events emitted before the first control client connected:
`trust_flow_and_key_change` lost alice's TrustPrompt (the LAN hello to bob happens
within ms of start). The CLI now keeps those events (≤ 4096) and replays them to the
first client; one lock makes "append to backlog" vs "broadcast" and "subscribe + take
backlog" atomic, so nothing is lost or duplicated. Check: a client connecting 2 s after
the LAN hello receives exactly 1 TrustPrompt.
(2) The ICE controller's recheck chain (D-0026) accepted a firing only ≤ 1 ms before its
due time; a timer firing ≥ 2 ms early ended the chain and the switching policy stopped
for the rest of the call (no evaluation at all on either side of
`netem_path_switch_v4_to_v6`, which therefore never returned to IPv6). Any recheck in the
second half of the pending window now counts.
D-0070 (2026-11-07, p2/infra-qa) — `worker.yml`: every run (any branch touching
`infra/worker/**`) runs the vitest suite and `wrangler deploy --dry-run` (bundles and
validates bindings without credentials); it deploys only on the integration branch, `main`
or a manual dispatch, and only when `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`
exist (checked through step `env:`, since `if:` cannot read secrets). Otherwise it prints
the manual set-up steps (also to the job summary) and is green. `TOKEN_SIGNING_KEY`
persistence: generated with `openssl rand -hex 32` only when `wrangler secret list` does
not show it, so it lives solely in Cloudflare's secret store and survives every redeploy;
CI never rotates it (rotation would invalidate all live 1-hour tokens), and a failing
`secret list` fails the job rather than silently generating a new key. `APP_SECRET` is
re-put on each deploy (`wrangler secret put` overwrites idempotently). Optional
`TURN_KEY_ID`/`TURN_API_TOKEN` repository secrets are forwarded when present.

D-0071 (2026-11-07, p2/infra-qa) — Diagnostics export schema v1
(`docs/diagnostics-export.md`): top-level `{schema, app_version, platform, ts, call,
peers, global, series, log_tail}`, `series` as an object metric → ≤ 60 numbers, ≤ 1000 log
lines. Writers may only add keys within v1; anything else bumps `schema`. Redaction:
peers are public key + 8-hex tag (no display names except the user's own), no session
names, file names, paths, device names or trusted-device list; in log lines known peer
names → `peer:<tag>`, transfer file names → `<file>` (`files.rs` logs them on resume),
the home directory → `~`. The validator (`crates/core/src/diagnostics/schema_v1.rs`)
depends only on `serde_json` and is included by the e2e crate with `#[path]`: one rule
set for unit and e2e tests without making the e2e test binaries link the core and
libwebrtc. The export file is now `CallXL-diagnostics-<ts>.json` (brief's interface; was
`callxl-diagnostics-`). `CallXl.config` (only read by the old export for `data_dir`,
which the new report leaves out as a local path) was removed. Evidence: `export_schema_v1`
on a real recorded snapshot (fixture captured from `diagnostics_complete`, proxy backend)
and `diagnostics_complete` validating the live export (local: "export: schema v1 valid,
6343 bytes, 16 series, 4 log lines").

D-0072 (2026-11-07, p2/infra-qa) — Release versions: `scripts/check-version.sh` compares
the tag (without `v`) to `apps/desktop/Cargo.toml` (or the workspace version it
inherits). A final tag `vX.Y.Z` must match exactly and fails early otherwise. A
prerelease tag `vX.Y.Z-<pre>` (e.g. the `v0.0.1-rc` dry-run while the code says 0.1.0)
is *stamped*: the workflow writes the tag's version into the manifest before building,
then checks, so every artifact and `CARGO_PKG_VERSION` carry the prerelease version. The
`.deb` asset is published as `callxl_<tag version>_amd64.deb` (cargo-deb spells
prereleases Debian-style, `0.0.1~rc`, inside the package; GitHub would mangle `~` in an
asset name). Prerelease = tag contains `-`.

D-0073 (2026-11-07, p2/infra-qa) — CI job graph: `lint` (fmt + clippy), `swift-bindgen`,
`worker` (vitest), `linux` (`_build-linux.yml`: build + unit tests + .deb, uploads the
debug `callxl-cli` and the five e2e test executables as `e2e-bins`), `windows`, `macos`
start together; `e2e` (4 suites) and `netem` (`_netem-matrix.yml`, 6 scenarios) fan out
from `e2e-bins` without recompiling. Test executables run from another job work because
GitHub's checkout path is identical on every ubuntu runner (`CARGO_MANIFEST_DIR` paths to
`scripts/netem.sh` and `infra/worker` stay valid); the netem legs sparse-check-out only
`scripts` and `tests/e2e`. `APP_SECRET` is baked only into release packages
(`bake_app_secret`), never into the CI test binaries uploaded as artifacts.

D-0074 (2026-11-07, p2/infra-qa) — e2e samples carry `t_stats`, the time the returned
stats were measured (`t` minus the snapshot's age from `DiagnosticsSnapshot.ts_ms`), and
`netem_loss_backoff_recovery` attributes values to its windows by `t_stats` (thresholds
unchanged: ≥ 30 % drop within 10 s of loss onset, ≥ 450 kbps within 30 s after it
clears). The collector ticks at 1 Hz in its own phase, so the poll time overstated a
value's age by up to 1 s. CI run 42352890429 failed "only dropped 510 -> 364.088": loss
onset at 17.08 s, BWE falling from 24.1 s, target 301.9 kbps (≤ 357 = 70 %) in the sample
polled at 27.1 s, i.e. 10.02 s after onset by poll time while its stats were taken
before 27.1 s. The reaction itself varies: 4 s after onset in the green run 42351643296
(510 → 201 kbps), 7 s in 42352890429. Local check (proxy backend) after the change: drop
to 299.1 kbps at t_stats 19.7 s (onset 17.0 s), recovered 3.6 s after the loss cleared.
If a CI run still misses the window by stats time, the next step is the product side
(LossBasedBweV2 onset speed), not the window.

D-0075 (2026-11-07, p2/infra-qa) — CI timing baseline (M0), the Phase 1 single serial
`linux` job: warm caches (run 42351643296, phase1/core) 10 m 27 s wall-clock — clippy
15 s, build 43 s, Swift bindgen 33 s, serial e2e 6 m 52 s, PulseAudio 21 s; cold caches
on the new branch (run 42352890429; GitHub caches are per branch with fallback to the
default branch, which had none) 22 m 02 s — clippy 5 m 11 s, build 4 m 17 s, bindgen
1 m 56 s, e2e 8 m 59 s. sccache in the cold run: Rust 0 % hits, 67 cache write errors and
a rust-cache save refused by the cache-service rate limit ("You've hit a rate limit").

D-0076 (2026-11-07, p2/infra-qa) — The repository already has `CLOUDFLARE_API_TOKEN`,
`CLOUDFLARE_ACCOUNT_ID` and `APP_SECRET`, so the M1 `workflow_dispatch` of `worker.yml`
(run 42355101376) took the deploy path: Worker `callxl` deployed to
`https://callxl.hbadgujar44.workers.dev` (version 559cf8b3-5154-4d0a-9df5-8ede6075b121),
`/health` = ok, `APP_SECRET` uploaded, `TOKEN_SIGNING_KEY` generated and stored (none
existed), no TURN secrets. The no-secrets path is proven with a dispatch input
`simulate_missing_secrets: true`, which blanks the two secrets in the decision step only
(same branch of the workflow as a repository without them). `DEFAULT_SIGNALING_URL` is
left as the placeholder: changing a shipped default belongs to the orchestrator (the
URL is `wss://callxl.hbadgujar44.workers.dev/ws`).

D-0077 (2026-11-07, p2/infra-qa) — `netem_path_switch_v6_to_v4` asserts continuity
(no gap > 20 ms, unchanged) from 1 s after the IPv6 delay step to the end of the call,
instead of the last 14 s of a ~16 s recording (which included the step). CI run
42355062935 failed with one 25 ms gap in bob's recording at 4.19 s = 08:26:49.7, the
moment `tc` added 60 ms to the active IPv6 path (08:26:49.58); the switch came 5.8 s later
(08:26:55.49, "v6->v4: rtt 120->0 ms") and left no gap; alice's recording had none. A
sudden +60 ms one-way step starves the playout buffer for up to 60 ms whatever the
policy (NetEq expand fades out after ~20 ms); real paths degrading is the scenario's
injected event, and what §4 requires to be inaudible is the switch. The new window still
contains the switch and the 6+ s after it. The same scenario passed in runs 42351643296
and 42352890429 with the step inside the window (step landed in a quieter part of the
test signal).

D-0078 (2026-11-07, p2/infra-qa) — Correction to D-0077: why the delay step left no
> 20 ms gap in runs 42351643296 and 42352890429 is not established (the failing gap was
25 ms, just over the bound); the clause "step landed in a quieter part of the test
signal" is a guess, not a finding.

D-0079 (2026-11-07, p2/infra-qa) — Release mechanics. (1) The git proxy of this
environment refuses tag pushes (HTTP 403 on `refs/tags/*`; branch pushes work), and the
GitHub tools available here cannot create tags, so `v0.0.1-rc` cannot be pushed from the
branch; the dry-run uses `workflow_dispatch` and `softprops/action-gh-release` creates the
tag through the API at the dispatched commit. (2) The API answers 404 to a dispatch of a
workflow that has never run and is not on the default branch: `release.yml` was registered
by one commit (71fb38f) with a temporary branch-push trigger whose run skipped every job
(run 42355852994), removed in the next commit. (3) The first dispatch (run 42355891400)
passed every gate (meta, the three builds, all six netem legs) and failed in `release`
because no platform package exists before the desktop/iOS branches merge (`dl/` was never
created); `release` now tolerates that and, with `allow_partial`, publishes a Release whose
`SHA256SUMS.txt` lists whatever exists.

D-0080 (2026-11-07, p2/infra-qa) — Reusable Windows/macOS builds and the Linux package
job copy the steps of the desktop and iOS agents' own CI jobs (branches `p2/desktop`
93fd367, `p2/ios` 3b65790; Inno Setup defines, MSVC + static CRT, simulator pick,
swiftformat pin, `--headless-smoke` < 1 s), each guarded by the presence of its app, so
the merge replaces their inline jobs with calls without changing what is tested.
`_build-linux.yml` has two jobs selected by inputs: `build` (debug, unit tests,
`e2e-bins`, now including the debug `callxl` for `gui_join`) and `package` (fat-LTO
release, `.deb`, install, launch < 1 s); `ci.yml` and `release.yml` call it twice so the
LTO build never delays the e2e/netem fan-out (a caller's `needs:` waits for every job of a
called workflow). `gui_join` runs in the `e2e loopback` leg, which already starts
PulseAudio. `scripts/ci-webrtc-cache.sh` is taken verbatim from `p2/desktop` (same
`webrtc.lib` check plus `cygpath`; Git Bash on windows-latest has `unzip`, so my 7-Zip
fallback was dropped).

D-0081 (2026-11-07, p2/infra-qa) — CI speed, before/after (M5). Wall-clock of `ci.yml`
on Linux (Windows/macOS jobs still no-ops on this branch):
warm caches 10 m 27 s (run 42351643296, one serial job) → 3 m 41 s (run 42356464865:
lint ∥ swift-bindgen ∥ worker ∥ build 1 m 57 s, then 4 e2e suites ∥ 6 netem scenarios,
longest leg e2e loopback 1 m 36 s); cold caches 22 m 02 s (run 42352890429) → 7 m 48 s
(run 42355062935, every per-job cache key new). Release dry-run end to end: 3 m 57 s
(run 42356469404). Cost: runner time grows with the fan-out, 627 → 895 runner-seconds
warm (17 jobs, each ~30–60 s of set-up), accepted for a 2.8× shorter feedback loop.
What made it: (1) clippy and the build no longer run back to back (cold: 5 m 11 s +
4 m 17 s serial); (2) the e2e/netem legs reuse one debug build via the `e2e-bins`
artifact instead of compiling (upload ~10 s at compression level 1, download ~5 s);
(3) the 6 m 52 s serial e2e became ≤ 1 m 36 s per leg; (4) sccache warm: Rust hit rate
100 % (lint), 87.5 % (build), 85.7 % (bindgen). `CARGO_INCREMENTAL=0`,
`CARGO_NET_RETRY=10` everywhere. `cargo nextest` rejected: the unit tests take 1 s in
total (run 42356464865), so there is nothing for it to win and it would add an install
step. Caches are keyed per job kind (`lint`, `build`, `package`, `bindgen`, `windows`,
`macos`) and on the libwebrtc tag; tag builds read the default branch's caches, so the
release runs warm once CI has run on the integration branch after the merge. The
libwebrtc archive is fetched once per OS and cache key (8 s cold, 0 s warm).

D-0082 (2026-11-07, p2/infra-qa) — After rebasing onto the integration branch (merge
d293752 of PR #1, which added phase1's D-0037/D-0038), this branch's entries were
renumbered from D-0037…D-0048 to D-0070…D-0081 (cross-references inside them updated).
D-0037…D-0049 are taken by phase1 and p2/ios, D-0050…D-0069 by p2/desktop; p2/infra-qa uses D-0070+.

D-0083 (2026-11-07, p2/infra-qa) — Diagnostics export v1 also carries `app: "CallXL"`
(additive; validated). The p2/ios test `BindingsTests.testCoreLifecycleThroughBindings`
asserts `json["app"] == "CallXL"` and `json["diagnostics"] != nil`, i.e. the pre-v1 shape;
with my branch and p2/ios merged it failed on exactly those lines (integration check,
release run 42358935791, macOS job: "XCTAssertEqual failed: ("nil") is not equal to
("Optional("CallXL")")"). `app` now passes; the second assertion must change on the iOS
side to the v1 shape (`json["schema"] as? Int == 1`, `json["global"] != nil`, see
docs/diagnostics-export.md). Reported to the orchestrator.

D-0084 (2026-11-07, p2/infra-qa) — Integration check (throwaway branch
`p2/infra-qa-integration-check` = this branch + `p2/desktop` + `p2/ios`, `ci.yml` resolved
to the reusable calls): release run 42358935791 with `allow_partial: false` and tag
`v0.0.2-rc` passed meta, linux build, linux-package (fat-LTO desktop, `.deb` stamped
0.0.2-rc, installed, launched) and all six netem legs, then refused to publish:
"missing release artifacts: CallXL-Setup.exe CallXL-unsigned.ipa (windows job: failure,
macos job: failure)", the loud failure the brief requires. Windows failed in the app's own
build set-up (MSVC under Git Bash: `/usr/bin/link` shadows `link.exe` under vcvars), which
p2/desktop was still fixing (its own run 42357923913 also fails); macOS failed on the iOS
test of D-0083. Neither is a workflow defect; the reusable builds follow those jobs and
will be re-synced from the merged versions.

D-0085 (2026-11-07, p2/infra-qa) — Additional baseline (D-0081 table): the integration
branch's first `ci.yml` run after the PR #1 merge (run 42357082797, d293752, old single
serial `linux` job, no warm cache on that branch) took 19 m 51 s; the same code on the
fanned-out layout ran cold in 7 m 48 s and warm in 3 m 41 s (D-0081).

D-0086 (2026-11-07, p2/infra-qa) — Releases are cut by `workflow_dispatch` of
`release.yml` with `tag` (Actions UI or API) as the primary path, since tag pushes are
refused here (D-0079): `meta` takes the tag from the input, `softprops/action-gh-release`
creates the tag at `github.sha` (the dispatched commit) together with the Release. Proven
by v0.0.1-rc (run 42356469404, tag created at 5834802 with no local tag). Tags created
with the workflow's `GITHUB_TOKEN` do not trigger other workflows, so the `push: tags`
path does not run a second time. Documented in HANDOFF.md §1.

D-0087 (2026-11-07, p2/infra-qa) — `Swatinem/rust-cache` saves only on the integration
branch and `main` (`save-if`); every other ref (feature branches, PRs, tags) restores
from those entries through GitHub's default-branch fallback. Observation that led to it:
run 42360575065 (518d120, after the rebase) found no rust-cache entry at all ("No cache
found") and no libwebrtc cache on a branch that had saved both an hour earlier, its build
took 7 m 21 s instead of 1 m 57 s (sccache Rust hits 53.9 % vs 87.5 %). Saves per entry
are 0.1–0.5 GB (rust-cache, from the job logs) per job kind and branch, plus libwebrtc
per OS and the sccache objects, across six active branches; GitHub evicts least recently
used entries beyond 10 GB per repository. Eviction is the inferred cause: the cache-usage
API is not reachable through this environment's proxy, so the usage figure itself is not
verified. Effect is measurable only after the merge (integration-branch runs warm the
shared entries); until then feature-branch builds rely on sccache. Rejected: dropping the
libwebrtc cache (download is only 8 s on Linux, but the brief requires it and Windows/iOS
archives are larger).

D-0088 (2026-11-07, p2/infra-qa) — `DEFAULT_SIGNALING_URL` is now
`wss://callxl.hbadgujar44.workers.dev/ws`, the Worker deployed by `worker.yml` (run
42355101376, D-0076), replacing the `<account>` placeholder (orchestrator decision after
the PR #2 review). The Settings override (`set_signaling_url`) is unchanged; HANDOFF.md §4
says how to change the default after a redeploy under another Cloudflare account. The
e2e peers keep running with `--no-cloud` unless a test passes a local Worker URL, so CI
does not contact the deployed Worker.


D-0050 (2026-11-07, p2/desktop) — Numbering: p2/desktop uses D-0050…D-0069 so entries do
not collide with the concurrent branches (phase1 D-0037+, p2/ios D-0039+, p2/infra-qa
D-0042+ at the time of writing). Gaps in the sequence are therefore expected.

D-0051 (2026-11-07, p2/desktop) — Desktop UI toolkit pin: `slint = "=1.18.1"` with
`backend-winit` + `renderer-femtovg` (OpenGL, the on-screen renderer) + `renderer-software`
(offscreen screenshot tests, and Slint's fallback when no GL context can be created) +
`system-tray` + `unstable-winit-030` (winit window events for drag-and-drop/focus; pinned
exactly because that module may change in minor releases). Rejected: Skia (pulls a
prebuilt Skia download and a much larger binary for no visible gain in this flat UI);
`accessibility` off for now (accesskit opens an AT-SPI D-Bus connection at startup) — a
v0.2 candidate. The UI is compiled by `slint-build` (never interpreted, §10).

D-0052 (2026-11-07, p2/desktop) — Design tokens: `design/tokens.json` is turned into the
Slint library `@tokens` by `apps/desktop/build.rs` in `OUT_DIR` (not a committed
`ui/tokens.slint`, which would go stale and dirty the tree on every token edit). Fonts:
Inter 4.1 Regular + SemiBold, Latin subset via `pyftsubset`, embedded with Slint's font
import: 101 832 + 103 688 bytes vs 411 640 + 418 540 for the full TTFs. Identical text on
Windows and Linux; other scripts fall back to system fonts. Icons: `design/icons/
generate.py` (Pillow 12.3, present on the dev machine) renders SVG, PNG 16…1024 and a
multi-size `.ico` from one geometry; outputs are committed so builds never need Python.

D-0053 (2026-11-07, p2/desktop) — Tray: Slint 1.18's native `SystemTrayIcon` (ksni
StatusNotifierItem over zbus on Linux, `Shell_NotifyIconW` on Windows) instead of the
`tray-icon` crate, whose Linux backend needs GTK3 + libayatana-appindicator and a GTK main
loop. Menu: Show, Mute/Unmute, Leave call, Quit, bound reactively to the call state. A
missing StatusNotifier host is non-fatal (observed under Xvfb without a session bus:
"Failed to create system tray icon … D-Bus connection error"; the app runs on, and closing
the window then quits instead of hiding).

D-0054 (2026-11-07, p2/desktop) — Single instance: a local socket from `interprocess` 2.4
(abstract Unix socket on Linux — no stale file after a crash — and a named pipe on
Windows), named after the user and the profile directory. A second launch connects, sends
`show`, and exits; the first brings its window forward. `single-instance` was rejected
because it can only detect, not signal, the running instance. Unit test
`instance::tests::second_launch_focuses_the_first`.

D-0055 (2026-11-07, p2/desktop) — Drag-and-drop: winit `HoveredFile`/`DroppedFile` through
`slint::winit_030::WinitWindowAccessor::on_winit_window_event`. Slint 1.18's `DropArea`
only receives drags started by a Slint `DragArea`: the winit backend has no
`DroppedFile` handling (`grep -rn DroppedFile i-slint-backend-winit-1.18.1` → nothing).
Drops arriving within 60 ms are batched, then the recipient picker opens (§6: sender
chooses recipients). A "Send a file" button uses `rfd` 0.17 (xdg-desktop-portal / Win32
dialog) on a helper thread. Notifications: `notify-rust` on its own thread (the D-Bus call
blocks for a round trip); Windows AppUserModelID `com.callxl.app` is set by the process
(`SetCurrentProcessExplicitAppUserModelID`) and on the installer's shortcuts.

D-0056 (2026-11-07, p2/desktop) — Core ⇄ UI threading: `CallXl::new` and `start` run on a
dedicated `callxl-ui-bridge` thread (keystore lookup, SQLite open and media-engine creation
never touch the UI thread, so the window appears at once). `EventListener::on_event` only
does `Sender::send` into that thread's channel; the bridge diffs a fresh `snapshot()` after
every event (and at least once a second) against the previous one and posts patches with
`slint::invoke_from_event_loop`; patches update `VecModel` rows in place (`set_row_data`
only when a row changed). Conversions, labels and sparkline paths are computed on the
bridge thread. Own chat messages are shown optimistically with the id `send_chat` returns.
Audio devices: the core emits `DevicesChanged` only after a user selection, so the bridge
polls `list_audio_devices` every 3 s; a chosen device that disappears falls back to the
system default with a toast (hot-switch).

D-0057 (2026-11-07, p2/desktop) — Window close: during a call (tray available) the window
hides and the call continues in the tray (a one-time notification says so); otherwise the
app quits. The uninstaller removes the program folder and shortcuts but keeps
`%APPDATA%\CallXL` (identity keys, trusted devices, chat history), like any per-user app;
the Linux profile is `$XDG_DATA_HOME/CallXL`. A theme preference lives in
`<profile>/desktop.json` (the only desktop-specific setting; the core owns the rest).

D-0058 (2026-11-07, p2/desktop) — Minimal bug fixes outside `apps/` so the workspace
builds on Windows (no API change): `callxl-cli` gates its `unix://` control socket with
`#[cfg(unix)]` (TCP control, which the harness uses, is unchanged); the e2e Worker helper
gates `process_group(0)`; the harness looks for `callxl-cli{EXE_SUFFIX}`;
`scripts/ci-webrtc-cache.sh` accepts `lib/webrtc.lib` and exports a native path
(`cygpath -m`) under Git Bash.

D-0059 (2026-11-07, p2/desktop) — GUI e2e (`apps/desktop/tests/gui_join.sh`): the desktop
app runs under Xvfb with its production configuration (platform audio device, real mDNS,
no bind restriction); a `callxl-cli` peer with the test ADM creates a session; the GUI
joins it through its own UI command (`CALLXL_DEBUG_AUTOJOIN=1` invokes the
`App.join-session` callback for the first listed session — the same path as a click;
pixel-coordinate clicking with xdotool was rejected as fragile). Pass = the CLI's GetStats
show a connected path to the GUI with media flowing (and, in CI with PulseAudio, audio
received from the GUI). Local run (dev machine, no PulseAudio, eth0 192.0.2.2): joined and
connected 2.1 s after launch, "Lan path, 192.0.2.2:52267 -> 192.0.2.2:37082 (host/host),
send 284 kbps", CLI call state InCall.

D-0060 (2026-11-07, p2/desktop) — `callxl --headless-smoke` (§14 "launches under 1 s"):
`startup_ms` is measured by the app from the first line of `main` until both the window is
live (first event-loop turn after `show()`) and `CallXl::start` has returned (media engine,
LAN listener, mDNS). It uses a throw-away profile so CI never creates a real identity.
Local debug build under `xvfb-run`: `startup_ms=494`.

D-0061 (2026-11-07, p2/desktop) — Windows CI findings. (1) `CC=cl.exe` without a developer
prompt fails ("failed to find tool cl.exe"); with `ilammy/msvc-dev-cmd` (vcvars) Git
Bash's `/usr/bin/link` shadows MSVC's `link.exe` ("/usr/bin/link: extra operand"). The
windows job therefore sets no CC/CXX and no vcvars: cc-rs and rustc locate MSVC through
the VS installation (runs 42355869460, 42356690342). The workflow-level `CC=clang-21` moved
into the linux job. (2) The fork's test ADM used POSIX `clock_gettime`/`clock_nanosleep`
(MSVC: "C2065 CLOCK_MONOTONIC undeclared", run 42357923913); on Windows it now uses
QueryPerformanceCounter and a high-resolution waitable timer (≈0.5 ms wake-up; plain
`Sleep` rounds to the 15.6 ms quantum, which would break 10 ms ticks). Linux keeps the
`clock_nanosleep` path unchanged (third_party/webrtc-sys/CALLXL_CHANGES.md).

D-0130 (2026-11-07, p2/desktop) — Numbering and CI layout after PR #2. New p2/desktop
entries use D-0130 upwards (coordinator: infra/QA D-0070+, iOS D-0100+); D-0050…D-0061
keep their numbers (no collision). The desktop CI steps now live in the reusable
workflows from PR #2: `_build-windows.yml` drops `CC=cl.exe` and the vcvars step (D-0061)
and adds metrics; `_build-linux.yml` (job `package`) and `_build-windows.yml` write
startup times, binary/installer sizes and release-build times to `target/desktop-metrics`
and upload them as `desktop-metrics-{linux,windows}` artifacts, so the numbers can be read
without the full job log; the ci.yml `e2e loopback` job keeps `gui_join` and saves its
output as `e2e-artifacts-gui/gui_join.txt`. Artifact names `CallXL-Setup.exe` (in
`windows-installer`) and `callxl_<ver>_amd64.deb` (in `linux-deb`) are unchanged.

D-0131 (2026-11-07, p2/desktop) — Startup measurement and the deferred tray. `--headless-
smoke` now reports `ui_ms` (first frame actually rendered, via Slint's rendering notifier)
and `core_ms` (`CallXl::start` returned) separately; `startup_ms` is when both are done.
The tray icon is created on the first event-loop turn after `show()` instead of before
the window: with a session bus but no StatusNotifier host (CI after the .deb pulled in
xdg-desktop-portal/PipeWire as Recommends) the second launch measured `startup_ms=2394`
(run 42362385739, first launch 337); after the change the three launches measured
474 / 141 / 142 ms with `ui_ms` 474 / 136 / 137 and `core_ms` 130 / 141 / 142 (run
42364027461). The cause of the 2.4 s stall was not isolated beyond "UI thread before the
first frame"; the tray registration was the only blocking D-Bus call there and no longer
is. Earlier runs measured a looser "window shown" point (first event-loop turn after
`show()`): 40 / 166 / 164 ms (run 42357197946), 33 / 145 / 144 ms (run 42361333414).

D-0038 (2026-11-07, p2/ios) — iOS core packaging: `CallXLCore.xcframework` holds a

D-0039 (2026-11-07, p2/ios) — iOS core packaging: `CallXLCore.xcframework` holds a

D-0100 (2026-11-07, p2/ios) — iOS core packaging: `CallXLCore.xcframework` holds a
*dynamic* `CallXLCore.framework` per slice, linked from `cargo rustc -p callxl-core
--crate-type cdylib --release` (`-install_name @rpath/CallXLCore.framework/CallXLCore`,
`-ObjC` so libwebrtc's Objective-C categories used by its iOS ADM/`RTCAudioSession` are
loaded, `-dead_strip`), not a static library: the prebuilt iOS `libwebrtc.a` is 494 MB per
slice, which a Rust staticlib would re-archive into every slice and every app link; the
dylib is resolved and dead-stripped once. Slices: `ios-arm64` (device) and
`ios-arm64-simulator` only — LiveKit publishes `webrtc-ios-device-arm64-release.zip` and
`webrtc-ios-simulator-arm64-release.zip` for `webrtc-a545de4` (229 MB each) but
`webrtc-ios-simulator-x64-release.zip` is a 404, so there is no x86_64 simulator slice and
no lipo step (Apple-silicon Macs, including `macos-latest`, run the arm64 simulator). The
iOS prebuilt uses Apple's libc++ (`use_custom_libcxx = false` in its args.gn), so Xcode's
clang builds webrtc-sys (no clang-21 on macOS). Swift bindings come from a standalone
host crate `packaging/ios/uniffi-bindgen` (uniffi =0.29.5 `cli`, own lockfile), because
the in-crate `uniffi-bindgen` bin would need a host (macOS) build of callxl-core and its
libwebrtc; it reads the metadata from the unstripped device dylib, which is then
`strip -x`'d. `uniffi.toml` `ffi_module_name` = `CallXLCore` (was `CallXLCoreFFI`) so the
generated Swift imports the framework module; header/modulemap file names are unchanged.
Sizes (run 42373009146): CallXLCore.xcframework.zip 15.3 MB; device dylib 15.6 MB;
CallXL-unsigned.ipa 8.3 MB (app binary 2.0 MB).

D-0101 (2026-11-07, p2/ios) — LAN discovery on Apple targets uses the system DNS-SD API
(`dns_sd.h`: DNSServiceRegister/Browse/Resolve/GetAddrInfo, `crates/lan/src/dnssd.rs`)
instead of mdns-sd. Since iOS 14, sending/receiving multicast UDP on an app's own BSD
sockets requires Apple's restricted `com.apple.developer.networking.multicast`
entitlement (Apple TN3179), which a sideloaded free-account build cannot carry; Bonjour
through the system API needs only `NSLocalNetworkUsageDescription` + `NSBonjourServices`.
Same service type and TXT keys (`k`, `x`, `n`, `v`) and instance name as mdns-sd, so
desktop and iPhone interoperate. Found addresses are listed IPv4 first (the LAN signaling
listener binds `0.0.0.0`), link-local IPv6 dropped. One thread owns every DNSServiceRef
(poll on their sockets), so no locks. Proven in CI on macOS (`two_peers_discover_each_other`);
on a real iPhone it is a HANDOFF item (the Simulator does not enforce the entitlement).

D-0102 (2026-11-07, p2/ios) — iOS keychain entry for the identity is the fixed name
`identity` (service `com.callxl.app`), not a hash of the data directory: an iOS app's
container path contains a UUID that may change on update/re-signing, which would have
generated a new identity (and a "Key changed" prompt at every peer) after a weekly
re-sideload. One app sandbox = one profile, so nothing collides.

D-0103 (2026-11-07, p2/ios) — iOS audio session and CallKit: libwebrtc's iOS ADM configures
the session itself from `RTCAudioSessionConfiguration.webRTCConfiguration` (which lacks
`.defaultToSpeaker`), and WebRTC's CallKit integration is `RTCAudioSession.useManualAudio`
+ `isAudioEnabled` toggled in `provider(_:didActivate:)`. The prebuilt is built with
`rtc_enable_objc_symbol_export=false`, so those classes cannot be linked against; the
app reaches them through the Objective-C runtime (`NSClassFromString`, KVC) — they are
registered because the framework links libwebrtc with `-ObjC`. At launch the app sets
WebRTC's configuration to §8's (`.playAndRecord`, `.voiceChat`, `.allowBluetooth`,
`.allowBluetoothA2DP`, `.defaultToSpeaker`) and enables manual audio; a CallKit
`CXStartCallAction` configures the session and joins, CallKit activates it, the app then
enables WebRTC audio. Hold (a phone call) mutes the microphone until resume. If CallKit
refuses the transaction the app joins anyway and activates the session itself. File
sending uses the system document picker (Files, iCloud, other providers) with a recipient
picker, not a Share Extension (a second process with its own IPC to the running core);
picked files stay security-scoped until the transfer ends, so nothing is copied.

D-0104 (2026-11-07, p2/ios) — CallXLCore.framework link-time trimming, each change with its
measured effect: (1) `-Wl,-dead_strip_dylibs`: dependent libraries 23 → 18 (OpenGLES, GLKit,
CoreAudio, libiconv, … were linked by webrtc-sys's iOS list but unused). (2) No `-ObjC`:
it force-loaded all 147 Objective-C classes of libwebrtc (every video codec, Metal renderer,
camera capture). Only the 7 category objects whose classes are linked are linked
explicitly (`OBJC_CATEGORY_OBJECTS` in build-xcframework.sh); a missing one is caught by
BindingsTests (run 42366443719 crashed in start() with `-[RTCVideoCodecInfo
initWithNativeSdpVideoFormat:]: unrecognized selector` until the video-codec categories
were added). (3) iOS-only empty video codec factories in the CallXL factory (audio-only
app, §15): dependent libraries 18 → 13 (Metal, MetalKit, QuartzCore, CoreGraphics, Network
gone), device dylib 16 154 504 → 15 570 568 bytes. Launch effect, measured in one job
(run 42373009146, simctl, Release, warm medians): bare SwiftUI app 410 ms; + CallXLCore
linked but never called 729 ms; + Charts/CallKit/AVKit 620 ms; CallXL 951 ms (gate
< 1000 ms passed). Across runs the same bare app measured 410–746 ms and CallXL
951–1583 ms, so the gate is sensitive to the runner's speed; on a real iPhone it is a
HANDOFF item. Remaining premain cost is dyld loading the 15.6 MB dylib itself.

D-0105 (2026-11-07, p2/ios) — The iOS app creates and starts the Rust core off the main
actor (`AppModel.start()`: `CallXl(config:)` opens SQLite and the keychain, `start()`
spins up the media engine); Home renders from a placeholder snapshot and "New session"
is enabled once the core runs. Measured under XCUITest (Debug), launch to Home went from
4306 ms (core built in `App.init`, run 42357184261) to 2662 ms (run 42358988220); the
core's own creation + start then took 0.2 ms + 169 ms on a background task.

D-0106 (2026-11-07, p2/ios) — How §1.5 "launch to Home under 1 s on the Simulator" is
measured: `packaging/ios/measure-launch.sh` installs the Release build and launches it
with `xcrun simctl launch` (1 cold + 5 warm launches, real core); the app writes
`<ms to Home> premain=… app.init=… home.appear=…` (process start = kernel start time;
`premain` = a C constructor in the app, i.e. every dylib loaded and initialised) to
Documents. The gate is the warm median. Not XCUITest: its launch procedure added about
1.5 s before any app code ran, in Debug and Release alike (runs 42358988220, 42360979987:
process start → App.init 1550–1912 ms), so the UI test only reports. A bare SwiftUI app
(`LaunchBaseline` target, same clock) is measured in the same job as the Simulator's
floor: 606–625 ms to its first screen (premain ~320 ms) on the macos-latest runner.
Unit tests run the Debug build (`CallXL` scheme, @testable), UI smoke the Release build
(`CallXLUI` scheme).

D-0107 (2026-11-07, p2/ios) — The §1.5 iOS launch gate is relative (supersedes the absolute
1000 ms check of D-0106; orchestrator review of PR #4): in one job, on one Simulator,
CallXL's warm median launch to Home minus the bare `LaunchBaseline` app's warm median must
be ≤ 700 ms (`packaging/ios/launch-gate.py`). An absolute CallXL median above 1000 ms only
prints a `::warning::`; both medians and every launch go to the `ios-metrics` artifact
(JSON). Reason: the shared macos-latest runner's speed varies about 2x between jobs (bare
app 410–746 ms, CallXL 951–1583 ms across runs), so an absolute limit measures the runner.
Reference job (run 42373009146, simctl, Release, warm medians): bare SwiftUI app 410 ms;
+ CallXLCore.framework linked, never called 729 ms; + Charts/CallKit/AVKit 620 ms; CallXL
951 ms (overhead 541 ms). The overhead also grows on slow runners (run 42370298128:
1583 − 746 = 837 ms), so the budget catches a core-load regression but can still trip on an
unusually slow runner. Launch time on a real iPhone is unmeasured (HANDOFF.md).

D-0160 (2026-11-07, phase1) — Root cause of the intermittent `netem_path_switch_v4_to_v6`
failure ("never returned to IPv6"; CI run 42373009146 on p2/ios, no controller decision on
either side, call on IPv4 throughout). With glare at setup (alice rolled back her offer)
both agents' ICE controllers briefly saw the *controlled* role, and the role was settled
afterwards by an ICE role conflict (the offerer bob ended up controlled: "nomination on
the controlled side"). `CallXlIceController::Filter` returned libwebrtc's result on the
controlled side without scheduling its own recheck, so when a pending recheck fired while
that side was still controlled the chain ended; when the role then flipped to controlling
libwebrtc requested no sort (stable call), and the switching policy never ran again for
the call. Fix: the controlled branch also keeps the recheck chain alive (one allocation-
free evaluation per 500 ms), so a later role change is always picked up within 500 ms.
`CALLXL_ICE_TRACE` now also logs every role change the controller observes. Evidence:
local repro with two IPv4 paths (127.0.0.1 + 127.0.0.2, proxy backend, glare at setup and a
late controlled → controlling flip in every run): before, 1 of 22 calls with zero policy
evaluations on both sides (the CI symptom); after, 0 of 24. CI proof: three netem matrix
runs on `phase1/netem-flakes` (PR description).

D-0161 (2026-11-07, phase1) — Root cause of the intermittent `netem_loss_backoff_recovery`
failure (CI run 42362442415 on p2/ios: minimum Opus target 420.8 kbps within 10 s of 5 %
loss; the requirement is a ≥ 30 % drop, ≤ 357 kbps). D-0022's LossBasedBweV2 instant upper
bound `15 kbps / (loss − 1 %)` caps BWE at ≈ 375 kbps at exactly 5 % loss (≈ 330 kbps Opus),
i.e. right at the 30 % line, so the outcome followed the ±1 % sampling noise of libwebrtc's
loss estimate over its observation window (that run: BWE 491 kbps 5 s into the loss ⇒
estimate ≈ 4.05 %, no meaningful back-off until 13 s). Balance lowered to 10 kbps: 5 % →
≈ 250 kbps BWE, 4 % → ≈ 333, 8 % → ≈ 143, still inert below ≈ 2.7 % loss, so light loss
costs no fidelity (§1.1). Local, proxy backend, 5 runs each: before min 361.0 (fail),
256.8, 249.2, 250.0, 239.1 kbps; after 224.5, 172.2, 127.2, 216.8, 190.6 kbps (all pass),
recovery to ≥ 450 kbps 1.6–5.6 s after the loss cleared in both sets.
`netem_frame_adaptation` unchanged (10→20 ms at 10.0 s, 20→10 ms at 47.0 s, 0 gaps).
Supersedes the balance value in D-0022; no test threshold changed.

D-0190 (2026-11-07, orchestrator) — DECISIONS.md housekeeping after the four merges. The
two one-line p2/ios entries numbered D-0038 and D-0039 (both "iOS core packaging: …holds
a") are truncated rebase leftovers of D-0100 and carry no decision of their own; D-0100
is authoritative. The phase1 D-0038 (timing races) is unaffected. Numbering ranges used
by the Phase 2 agents: p2/desktop D-0050–D-0061 and D-0130+, p2/infra-qa D-0070+, p2/ios
D-0100+, phase1 follow-ups D-0160+, orchestrator D-0190+. Gaps are expected.

D-0191 (2026-11-07, orchestrator) — The iOS launch requirement (§1.5, §14 "Simulator UI
smoke test passes") is gated relative to a bare SwiftUI app in the same job (D-0107),
because the shared Simulator runner varied 2x between jobs for an unchanged bare app
(410–746 ms). The absolute Simulator number (1144 ms in run 42376780186) is reported, not
gated, and real-iPhone launch time is listed as unmeasured in HANDOFF.md §5/§6.
