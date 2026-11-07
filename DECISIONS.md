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
