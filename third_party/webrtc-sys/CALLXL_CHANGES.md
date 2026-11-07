# CallXL changes to webrtc-sys 0.3.53

Vendored from crates.io `webrtc-sys` 0.3.53 (Apache-2.0, LiveKit, Inc.; license headers
kept). Removed: `test/`, `libwebrtc/prefixed-jni/`, `AGENTS.md`, `Cargo.lock`, the
generated `Cargo.toml` (replaced by a plain one). The prebuilt libwebrtc `webrtc-a545de4`
is used unchanged. Every addition below exists because the upstream binding has no way to
reach the knob; all of it is glue around libwebrtc's own machinery (no media DSP in Rust).

## New files

| File | What | Why (CLAUDE.md) |
|---|---|---|
| `include/livekit/callxl.h`, `src/callxl.cpp` | `create_callxl_peer_connection_factory(CallXlFactoryOptions)`: factory with (a) field trials from a string, (b) platform ADM (via `AdmProxy`) or the test ADM, (c) Opus/ANA encoder factory, (d) remembered APM, (e) `BasicNetworkManager` ignore list and `network_ignore_mask`. `callxl_candidate_info(&Candidate)` | §3 field trials, APM control, complexity 10; §11 test ADM; loopback-only candidate gathering for tests; `PathInfo` |
| `src/callxl_audio.cpp` | `StringFieldTrials`; `CallXlAudioEncoderFactory` (Opus via `AudioEncoderOpusImpl::CreateForTesting`, complexity 10, ANA = `ControllerManagerImpl{LossFrameLengthController, audio_network_adaptor::BitrateController}`); `RememberingApmBuilder`; `ToApmConfig` | §3 frame-size adaptation inside libwebrtc's audio network adaptor (the prebuilt has `rtc_enable_protobuf=false`, so the ANA cannot be configured from its protobuf string); §3 APM defaults incl. AGC2 adaptive digital and NS level |
| `src/callxl_ice.cpp` | `CallXlIceController` (wraps `BasicIceController`), its factory, and an `IceTransportFactory` installing it | §4 dynamic path switching (Δrtt ≥ 15 ms / loss, 5 s sustain, 30 s cooldown, prefer IPv6); 500 ms recheck chain (D-0026); controlled-side anti-flap guard (D-0031); recheck path allocation-free: qualifying reason is an enum formatted once per switch, per-pair metrics created in `AddConnection` and only looked up afterwards |
| `src/callxl_test_adm.cpp` | `TestAudioDevice : webrtc::AudioDeviceModule` — preloaded WAV capture, pre-sized playout recording, one audio thread on absolute 10 ms deadlines, atomics only, no allocation/logging after init | §11 fake/test ADM playing a known WAV and recording the far end |
| `src/callxl.rs` | cxx bridge for the two free functions + `CallXlCandidateInfo` | — |

## Changed upstream files

| File | Change |
|---|---|
| `src/peer_connection_factory.rs` | Shared structs `CallXlFactoryOptions`, `CallXlApmSettings`, `CallXlIceParams`, `CallXlPcConfig`, `CallXlAnaState`, `CallXlTestAdmStats`. Methods `create_peer_connection_callxl`, `create_callxl_audio_track`, `field_trial_lookup`, `callxl_apply_apm`, `callxl_get_apm`, `callxl_ana_state`, `callxl_uses_test_adm`, `callxl_test_adm_flush`, `callxl_test_adm_stats`. Observer callback `on_ice_controller_decision(reason)` (default no-op in the trait). |
| `include/livekit/peer_connection_factory.h` | Declarations of the above, the CallXL constructor, three CallXL members. |
| `include/livekit/peer_connection.h`, `src/peer_connection.cpp` | `InitializeCallXl(config, custom_ice_controller, policy)`: same as `Initialize` plus `PeerConnectionDependencies::ice_transport_factory`. |
| `include/livekit/candidate.h` | `const webrtc::Candidate& get() const` accessor. |
| `build.rs` | Compiles the four new `.cpp` files and the new bridge. |
| `src/lib.rs` | `pub mod callxl;` |

`create_peer_connection_callxl` additionally sets these `RTCConfiguration` fields that
the upstream `RtcConfiguration` struct does not carry: `port_allocator_config.flags`
(IPv6, IPv6-on-Wi-Fi, no link-local, no TCP), `max_ipv6_networks`,
`ice_connection_receiving_timeout`, `ice_backup_candidate_pair_ping_interval`,
`stable_writable_connection_ping_interval_ms`, `audio_jitter_buffer_min_delay_ms`,
`audio_jitter_buffer_fast_accelerate`, `audio_rtcp_report_interval_ms`.

The upstream name `create_peer_connection_factory_with_options(bool, bool)` was already
taken, so the CallXL factory is `create_callxl_peer_connection_factory`.

`create_callxl_audio_track` leaves the APM fields of the source's `AudioOptions` unset and
applies the requested APM config directly (`callxl_apply_apm`): the voice engine
re-applies the source options on every send-stream reconfiguration (each renegotiation)
and otherwise resets the live APM config (DECISIONS.md D-0024).

`callxl_test_adm.cpp` builds on Windows (MSVC): the 10 ms tick uses QueryPerformanceCounter
and a high-resolution waitable timer there, `clock_nanosleep(CLOCK_MONOTONIC)` elsewhere
(DECISIONS.md D-0061).

## iOS (p2/ios)

`src/callxl_test_adm.cpp`: Apple has no `clock_nanosleep`; on `__APPLE__` `SleepUntilUs`
sleeps (`std::this_thread::sleep_for`) for the time left until the same absolute
`CLOCK_MONOTONIC` deadline. Linux and Windows paths are unchanged.
