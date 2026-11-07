# CallXL — project brief for Claude Code agents

CallXL is a private, peer-to-peer, end-to-end-encrypted voice chat app with chat and
file transfer, for Windows 10/11, Debian/Ubuntu Linux, and iOS (sideloaded).
Media engine: Google's libwebrtc (prebuilt, via LiveKit's `libwebrtc`/`webrtc-sys`
Rust crates). Everything around it: Rust. Desktop UI: Slint. iOS UI: SwiftUI over a
UniFFI-generated Swift binding of the shared Rust core.

This file is the single source of truth. When code and this file disagree, fix the code.
When this file is silent, decide using the Philosophy section, record the decision in
`DECISIONS.md`, and move on. Never wait for the user.

---

## 1. Philosophy (apply to every decision)

1. **Fidelity when stable, stability always wins ties.** The app targets the maximum
   Opus quality (510 kbps, 10 ms frames) but only ever *reaches* it adaptively. Nothing
   that improves fidelity may ever cause a dropout, glitch, or delay in joining.
2. **LAN is the primary use case.** Same-Wi-Fi calls must work with zero internet and
   zero servers. Internet is the fallback, not the design centre.
3. **Rust is the control plane, libwebrtc is the data plane.** Audio never crosses into
   Rust. Rust owns state, policy, signaling, crypto, discovery, UI data. Do not
   reimplement anything libwebrtc already does (AEC, jitter buffer, BWE, SRTP, ICE).
4. **Private app, relaxed rules.** Security is strong by construction (E2EE, pinned
   keys) but the UX never nags. One-tap trust, no badges, no warnings except a key change.
5. **Performance is a feature.** Small binaries, instant launch, no allocations on
   real-time threads, UI never blocks, stats polled off the UI thread, release builds
   fully optimised. Never trade quality for speed or speed for quality — find the option
   that gives both.
6. **Autonomous.** Agents make decisions, log them, and keep going until the Definition
   of Done is met. Problems are solved by adjusting the approach, never by lowering the
   quality bar or asking the user.

---

## 2. Repository layout (Cargo workspace)

```
callxl/
  CLAUDE.md                 this file
  DECISIONS.md              append-only log of decisions agents made and why
  Cargo.toml                workspace
  crates/
    core/                   state machine, call policy, config, persistence → UniFFI API
    media/                  libwebrtc wrapper: factory, PeerConnection, Opus/APM/ICE config, stats
    signal/                 message schema, signing/encryption, WebSocket client, lobby client
    lan/                    mDNS discovery + direct TCP signaling
    cli/                    `callxl-cli` headless peer used by all automated tests
  apps/
    desktop/                Slint app (Windows + Linux), single binary
    ios/                    Xcode project, SwiftUI, links CallXLCore.xcframework
  infra/
    worker/                 Cloudflare Worker + Durable Objects (TypeScript, wrangler)
  packaging/
    windows/                Inno Setup script → single CallXL-Setup.exe
    linux/                  cargo-deb metadata, .desktop file, icons
    ios/                    build-unsigned-ipa.sh
  tests/
    e2e/                    loopback + netem scenarios driving two or more `callxl-cli` peers
  .github/workflows/        ci.yml (matrix), release.yml (tag → artifacts), worker.yml (deploy)
  design/                   design tokens (colours, spacing, type), icon sources
```

Identity: app name **CallXL**, bundle id `com.callxl.app`, license **MIT**,
Rust edition 2024, MSRV = current stable at project start (pin in `rust-toolchain.toml`).

---

## 3. Audio profile (crates/media)

All values are defaults; every processing toggle is user-changeable in Settings.

| Item | Value |
|---|---|
| Codec | Opus, **mono**, 48 kHz (fullband). Mono also makes libwebrtc select Opus `VOIP` application mode — intended. |
| Bitrate floor / target-ceiling | 32 kbps / **510 kbps** (user slider, default 510) |
| Start bitrate | **LAN path (host↔host): 510 kbps immediately. Internet path (srflx/relay): 128 kbps, ramp to ceiling as BWE allows.** |
| Frame size | **10 ms default**, adaptation to 20 ms permitted under sustained loss, back to 10 ms when clean |
| FEC / DTX / CNG / VBR | in-band FEC on, DTX off, comfort noise off, VBR |
| Encoder complexity | 10 on all platforms |
| SDP fmtp | `maxaveragebitrate=510000; stereo=0; sprop-stereo=0; maxplaybackrate=48000; useinbandfec=1; usedtx=0; cbr=0; ptime=10; minptime=10; maxptime=20` |
| APM defaults | AEC3 **on**, noise suppression **on (moderate)**, high-pass **on**, AGC2 **on** (adaptive digital, mild). "Raw mic" preset disables all four in one tap. Changes apply live (`ApplyConfig`). |
| Jitter buffer | min delay 10 ms on LAN path, automatic on internet; fast-accelerate on |
| Devices | libwebrtc's own Audio Device Module on every platform (WASAPI / PipeWire-via-Pulse / AVAudioSession). Never a custom audio source — AEC needs aligned clocks. |

Implementation notes (verify each against the pinned libwebrtc version; adjust if an API moved):
- Bitrate: `PeerConnection::SetBitrate(BitrateSettings{min, start, max})` with max ≈ 600 kbps
  to leave room for RTP/FEC overhead at a 510 kbps Opus rate. Re-issue on path change.
- Audio must participate in BWE: field trials such as `WebRTC-Audio-SendSideBwe/Enabled/`,
  `WebRTC-SendSideBwe-WithOverhead/Enabled/`, `WebRTC-Audio-Allocation/min:32kbps,max:600kbps/`.
  Confirm with stats that the Opus bitrate actually drops under netem loss and recovers.
- Frame-size adaptation: prefer libwebrtc's audio network adaptor (frame-length controller,
  10↔20 ms). If it cannot be enabled through the binding, fall back to Rust policy:
  renegotiate `ptime` based on 5 s loss averages (switch to 20 ms above 5 % loss, back to
  10 ms after 15 s below 1 %). Either way it must be visible in Diagnostics.
- If `webrtc-sys` lacks a knob (field trials, SetBitrate, APM config, ICE controller
  factory, port-allocator flags), **fork it into `third_party/webrtc-sys` and add the cxx
  glue**. Keep the fork minimal and documented in `DECISIONS.md`.

---

## 4. Sessions, discovery, connection (crates/core, lan, signal, media)

**Model:** no room codes. Anyone creates a session (optional name); the Home screen lists
all open sessions; tap to join. Join = full mesh, one PeerConnection per other member. No
participant cap. A session ends when its last member leaves.

**Discovery, merged from two sources, LAN preferred:**
- LAN: mDNS service `_callxl._tcp.local` advertising device name, identity public key,
  TCP signaling port. Sessions visible instantly, no server involved.
- Internet: persistent WebSocket to the Lobby Durable Object; it pushes the session list.
- The same session seen both ways is shown once; LAN signaling path is used when available.
- Start both lookups in parallel; LAN result cancels the cloud one.

**Signaling protocol (identical over LAN TCP and cloud WebSocket):**
`hello`, `offer`, `answer`, `candidate` (trickle), `renegotiate`, `bye`, plus session
`create/list/join/leave`. Perfect-negotiation pattern (polite/impolite peer) to handle glare.
Payloads addressed to a peer are encrypted to that peer (see §5); the server relays bytes.

**ICE:**
- Candidates: host, srflx (STUN `stun.cloudflare.com:3478`, fallback `stun.l.google.com:19302`),
  relay (Cloudflare TURN, credentials from the Worker). STUN/TURN list fetched from the Worker
  at startup, cached, with built-in defaults if offline.
- Port allocator: enable IPv6 and IPv6-on-Wi-Fi. Discard Teredo/6to4 and unscoped link-local.
- Continual gathering; ICE restart within 1 s on `disconnected`.
- **IPv6 preference:** IPv6 pairs get higher priority and are sent first; IPv4 srflx
  candidates are delayed ~300 ms, relay ~1 s.
- **Dynamic path switching:** custom ICE controller (via `ice_controller_factory`). Backup
  pairs keep being pinged. Switch selected pair when an alternative has had lower RTT by
  ≥ 15 ms *or* clearly lower loss for 5 consecutive seconds; 30 s cooldown after any switch.
  Works both directions (v6→v4 and v4→v6). Switching must be inaudible (same DTLS session).
- Relay is per link: in a mesh only the pairs that cannot connect directly use TURN.

**Path-keyed policy** (re-evaluated on every selected-pair change): see start bitrate,
frame size and jitter-buffer rows in §3.

**Resilience:** signaling socket drop → reconnect with backoff, media unaffected. ICE
failed > 15 s → "reconnecting" state; > 30 s → leave with a clear reason.

---

## 5. Identity, trust, encryption (crates/core, signal)

- Per-install Ed25519 signing key + X25519 agreement key in the OS keystore
  (`keyring` crate: Windows Credential Manager, Secret Service, iOS Keychain). Never exported.
- "Device card" = display name + both public keys. Trust = card pinned locally.
- Trust flow: first contact shows a 6-word phrase derived from both keys and a single
  **"Trust this device"** button. No unverified badges, no nagging. A pinned key that
  changes (reinstall/new phone) asks once: "Key changed — trust again?".
- Every SDP/candidate message is signed with the sender's Ed25519 key and encrypted to the
  recipient (X25519 + ChaCha20-Poly1305). Rust verifies the signature and that the DTLS
  fingerprint inside matches **before** passing SDP to libwebrtc. Mismatch → refuse.
- Media/chat/files: DTLS-SRTP / DTLS-SCTP peer-to-peer. TURN relays ciphertext only.
- **Access control:** `APP_SECRET` (build-time, from CI secret; a random dev value with a
  loud warning if absent) is presented to the Worker, which returns a 1-hour signed token
  used for the WebSocket and TURN credential minting. Anyone with the secret can see the
  session list; joining still requires the normal signaling handshake. Secrets never in git.

---

## 6. Chat and file transfer (crates/core)

Both use WebRTC data channels on the existing PeerConnection (E2EE, LAN/internet identical).
- **Chat:** one ordered reliable channel per peer; JSON messages (id, ts, sender key, kind,
  body); delivered/read receipts; stored forever in SQLite (`rusqlite`, bundled).
- **Files:** sender chooses recipients (any subset of participants). Per recipient:
  offer (name, size, BLAKE3 hash) → recipient **always asked to accept** → dedicated
  reliable channel → 16 KiB chunks, ≤ 1 MiB in flight, resumable by offset after
  reconnect, hash verified. **No size limit.**
- **Audio always wins:** on internet paths, chunk send rate is throttled when audio loss or
  RTT rises (data channels have separate congestion control, so this throttle is mandatory).
  On LAN paths, no throttle.
- Storage: Windows `%USERPROFILE%\Downloads\CallXL\`, Linux `~/Downloads/CallXL/`,
  iOS app Documents (visible in Files, share sheet). Folder changeable in Settings.

---

## 7. Diagnostics (every platform)

Stats polled at 1 Hz off the UI thread from libwebrtc `GetStats` plus core state.
Per peer: path type (LAN / Direct / Relayed), local + remote IP with v4/v6 tag, candidate
types, RTT, jitter, loss %, send/recv bitrate, Opus frame size in use, jitter-buffer delay,
concealment events, selected-pair switches. Global: uplink/downlink totals, media thread CPU,
signaling transport (LAN/cloud) and latency, TURN bytes this month, APM state.
60 s sparkline per metric. "Export report" writes a text/JSON dump for bug reports.

---

## 8. Platforms and packaging

**Windows 10 (latest) and 11:** x64, static CRT (`-C target-feature=+crt-static`), no
redistributables. Deliverable: **one file, `CallXL-Setup.exe`** (Inno Setup, available on
GitHub Windows runners) that installs to `%LOCALAPPDATA%\Programs\CallXL`, creates Start
menu + optional desktop shortcut, launches the app at the end, and uninstalls cleanly.
Fallback if Inno is unavailable: `cargo-wix` MSI. Tray icon with mute/leave, toast
notifications, single-instance.

**Linux (Debian 12+, Ubuntu 22.04+):** x64 `.deb` via `cargo-deb`, built on `ubuntu-22.04`
for glibc compatibility. Audio via PipeWire's Pulse layer (libwebrtc Pulse backend), ALSA
fallback. `.desktop` entry, icons, D-Bus notifications, StatusNotifier tray.

**iOS (sideloaded, iOS 16+):** SwiftUI. `AVAudioSession` category `.playAndRecord`, mode
`.voiceChat`, Bluetooth + speaker allowed; background mode `audio`. CallKit for system call
UI and interruption handling (no entitlement needed). **No PushKit / APNs** — incoming calls
only while the app is open or in a call; document this. Deliverable: **unsigned `.ipa`**
(`xcodebuild -sdk iphoneos CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO` → `Payload/`
→ zip → `.ipa`). Rust core shipped as `CallXLCore.xcframework` (device arm64 + simulator)
with UniFFI Swift bindings.

---

## 9. UI (apps/desktop with Slint, apps/ios with SwiftUI)

English only. Dark theme default, light available. Modern, calm, smooth: one accent colour,
generous spacing, 60 fps transitions, speaking-glow on participant tiles, large tap targets.
Interactive without being busy. Shared `design/tokens.json` consumed by both UIs.

Screens: **Home** (session list with LAN glyph, "+ New session", trusted devices
online/offline) · **In call** (participant tiles, path badge per peer, mute, device picker,
slide-in chat drawer, file drop zone / share sheet) · **Chat** (per-session thread, file
cards with progress and "open in folder") · **Diagnostics** (§7) · **Settings** (audio
processing toggles + "Raw mic" preset, bitrate slider default 510, devices, display name,
trusted devices, download folder).

Rule: UI reads state from and sends commands to `core` only. No call logic, no network, no
blocking calls on the UI thread.

---

## 10. Engineering rules

- **Threading:** libwebrtc callbacks do exactly one thing — push an event on a Tokio
  channel. `core` consumes on one task and is the only caller back into libwebrtc. No locks
  shared with C++, no re-entrancy from callbacks.
- **Real-time safety:** nothing on audio or network threads allocates, logs synchronously,
  or takes a mutex that the UI could hold.
- **Errors:** `thiserror` in libraries, `anyhow` at edges; every user-facing failure has a
  plain-English reason.
- **Logging:** `tracing` with a ring buffer feeding Diagnostics export; no PII in logs
  beyond IPs the user can already see.
- **Release profile:** `opt-level = 3`, `lto = "fat"`, `codegen-units = 1`,
  `panic = "abort"`, `strip = true`. Slint UI compiled, not interpreted.
- **CI speed:** `sccache` + cargo registry/target caches; cache the libwebrtc download and
  any custom libwebrtc build keyed on its tag.
- **Dependencies:** `tokio`, `serde`/`serde_json`, `uniffi`, `ed25519-dalek`,
  `x25519-dalek`, `chacha20poly1305`, `blake3`, `keyring`, `mdns-sd`, `tokio-tungstenite`,
  `rusqlite` (bundled), `tracing`, `slint`. Add others when justified; log it.
- **Formatting/lint:** `cargo fmt`, `cargo clippy -D warnings`, `swiftformat` for Swift.

---

## 11. Testing strategy (no real network available — this is sufficient)

`callxl-cli` is a headless peer exposing the full core over a JSON control socket. All
e2e tests spawn two or more of them on one machine.
- Audio in tests uses libwebrtc's fake/test audio device module playing a known WAV and
  recording the far end; tests assert continuity (no gaps > 20 ms), achieved bitrate, and
  frame size from stats. On Linux, PipeWire/Pulse null sinks are also exercised once.
- Network: loopback over IPv4 `127.0.0.1` **and** IPv6 `::1`; on Linux, `tc netem` on `lo`
  to inject latency/loss/jitter and prove: 128→510 ramp on "internet" paths, back-off and
  recovery under loss, 10→20→10 ms frame adaptation, path switching when one address family
  degrades, file-transfer throttling keeping audio loss flat.
- Signaling: Worker run locally with `wrangler dev`; lobby/session/TURN-credential flows
  tested against it. LAN path tested with real mDNS on loopback.
- Windows runner: build, unit tests, loopback e2e with fake ADM, installer build + silent
  install/uninstall smoke test.
- macOS runner: XCFramework build, `xcodebuild test` on iOS Simulator (UI smoke + core
  bindings), unsigned IPA build.
- Crypto: signature/encryption round-trips, tampered-SDP rejection, key-change detection.

---

## 12. CI/CD (.github/workflows)

- `ci.yml`: matrix `ubuntu-22.04` (fmt, clippy, unit, e2e + netem, `.deb`),
  `windows-latest` (build, tests, `CallXL-Setup.exe`), `macos-latest` (xcframework,
  Simulator tests, `.ipa`). Artifacts uploaded on every run.
- `release.yml`: on tag `v*` → builds all three and attaches `CallXL-Setup.exe`,
  `callxl_<ver>_amd64.deb`, `CallXL-unsigned.ipa` to a GitHub Release.
- `worker.yml`: deploys `infra/worker` with `wrangler deploy` when `CLOUDFLARE_API_TOKEN`
  and `CLOUDFLARE_ACCOUNT_ID` secrets exist; otherwise skips and prints manual steps.
- Secrets expected (user adds once): `APP_SECRET`, `CLOUDFLARE_API_TOKEN`,
  `CLOUDFLARE_ACCOUNT_ID`. Signaling URL lives in `crates/core/src/config.rs` as
  `DEFAULT_SIGNALING_URL` (placeholder `wss://callxl.<account>.workers.dev`), overridable
  in Settings.

---

## 13. Agent workflow

Roles: the orchestrator plans, writes subagent prompts, reviews and merges; **all code is
written by Opus 5.5 subagents** working from concrete, self-contained prompts that name the
files they own, the interfaces they must honour, ordered milestones with verifiable checks,
and the exact CI gate to pass.

- **Phase 1 — one agent, Linux:** workspace, `core`, `media`, `signal`, `lan`, `cli`,
  `infra/worker`, e2e harness, `ci.yml` for Linux. Gate: all §11 Linux tests green,
  including the LiveKit-binding validation (bitrate, frame size, APM, ICE controller all
  provably controllable). Fork `webrtc-sys` here if needed.
- **Phase 2 — three agents in parallel on separate branches/worktrees:**
  `p2/desktop` (Slint app, Windows + Linux CI jobs, packaging), `p2/ios` (SwiftUI app,
  xcframework, Simulator tests, IPA), `p2/infra-qa` (Worker deploy workflow, netem matrix,
  Diagnostics export, release workflow). They may touch `crates/*` only for bug fixes,
  with a note in `DECISIONS.md`.
- **Phase 3 — orchestrator:** merge, run the full matrix via `gh run watch`, fix, tag
  `v0.1.0`, write `HANDOFF.md` (what was verified, what needs real devices, how to install).
- Every agent: commit small and often, keep CI green on its branch, append to
  `DECISIONS.md`, never ask the user, never stop before its gate is met.

---

## 14. Definition of Done (v0.1.0)

- Three release artifacts build from a clean tag in CI and are attached to a GitHub Release.
- Two `callxl-cli` peers complete a call over `127.0.0.1` and `::1`, with the LAN policy
  (510 kbps, 10 ms) confirmed by stats; under netem the internet policy (128→510 ramp,
  back-off, frame adaptation, path switch) is confirmed by stats.
- Chat and file transfer (accept prompt, chosen recipients, resume, hash check) pass e2e.
- Trust flow, signed SDP, tampered-SDP rejection pass.
- Desktop app launches under 1 s on the Linux runner, Simulator UI smoke test passes.
- Diagnostics shows every metric in §7; export works.
- `HANDOFF.md` lists exactly what the user must test on real devices (LAN call, internet
  call, IPv6 path, relay path, iOS backgrounding) and how to add the three CI secrets.

## 15. Deferred to v0.2 (do not build now)
Call recording, push-to-talk, per-peer volume, light-theme polish beyond functional,
Flatpak/AppImage, MSIX, push notifications, video.
