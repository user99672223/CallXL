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
