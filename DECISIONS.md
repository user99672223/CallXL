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
