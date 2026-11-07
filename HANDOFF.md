# CallXL — handoff (DRAFT)

> Draft by `p2/infra-qa`. Lines marked **TODO(orchestrator)** are filled in Phase 3 from
> the final release run. Everything listed under "What CI verified" links to a real run;
> nothing that needs a real network or a real phone is claimed as tested.

## 1. What this is

CallXL is a private, peer-to-peer, end-to-end-encrypted voice chat app with chat and file
transfer for Windows 10/11, Debian/Ubuntu Linux and iOS 16+ (sideloaded). Same-Wi-Fi calls
need no internet and no server (mDNS discovery + direct TCP signaling); over the internet
a small Cloudflare Worker provides the session list and relays encrypted signaling, and
Cloudflare TURN relays (encrypted) media only for peers that cannot connect directly.
Audio is Opus mono 48 kHz, up to 510 kbps with 10 ms frames, reached adaptively.

Release artifacts (GitHub Release of the tag, built by `.github/workflows/release.yml`):
`CallXL-Setup.exe`, `callxl_<ver>_amd64.deb`, `CallXL-unsigned.ipa`, `SHA256SUMS.txt`.

**TODO(orchestrator):** link the v0.1.0 Release here.

## 2. Install

<!-- install:begin -->
- **Windows 10/11 (x64):** download `CallXL-Setup.exe`, run it (installs per user into
  `%LOCALAPPDATA%\Programs\CallXL`, no admin rights, Start-menu entry, optional desktop
  shortcut). SmartScreen may warn because the installer is unsigned: "More info" →
  "Run anyway". Uninstall from Settings → Apps.
- **Debian 12+ / Ubuntu 22.04+ (x64):** `sudo apt install ./callxl_<ver>_amd64.deb`, then
  start "CallXL" from the app menu (or `callxl`). Remove with `sudo apt remove callxl`.
- **iOS 16+ (sideload):** install `CallXL-unsigned.ipa` with
  [AltStore](https://altstore.io) or [Sideloadly](https://sideloadly.io) using your Apple
  ID. With a free Apple ID the app must be re-signed every 7 days (AltStore does this
  automatically while AltServer runs on your computer on the same Wi-Fi).
- Verify downloads: `sha256sum -c SHA256SUMS.txt`.
<!-- install:end -->

## 3. Secrets (GitHub → repository → Settings → Secrets and variables → Actions)

| Secret | Where to get it | Used by | Without it |
|---|---|---|---|
| `APP_SECRET` | any long random string: `openssl rand -hex 32` | `release.yml` bakes it into all three apps (`CALLXL_APP_SECRET`, `crates/core/build.rs`); `worker.yml` stores it on the Worker | builds use a random per-build **dev** secret (loud build warning; Diagnostics shows `dev_app_secret: true`): apps from different builds cannot use the same cloud lobby. LAN calls are unaffected. |
| `CLOUDFLARE_API_TOKEN` | Cloudflare dashboard → My Profile → API Tokens → Create Custom Token: *Account · Workers Scripts · Edit* and *Account · Account Settings · Read* | `worker.yml` (`wrangler deploy`, `wrangler secret put`) | `worker.yml` stays green and prints these steps; no cloud signaling (LAN only) |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare dashboard → Workers & Pages → Overview → "Account ID" | `worker.yml` | as above |
| `TURN_KEY_ID`, `TURN_API_TOKEN` (optional) | Cloudflare dashboard → Calls → TURN keys → Create | `worker.yml` sets them on the Worker | STUN only: peers behind symmetric NATs cannot reach each other over the internet |

After adding them: Actions → **worker** → Run workflow; then push a tag (§4) so the apps
are rebuilt with `APP_SECRET`. `TOKEN_SIGNING_KEY` needs no action: `worker.yml`
generates it once and keeps it in Cloudflare's secret store (rotate by hand with
`npx wrangler secret put TOKEN_SIGNING_KEY` in `infra/worker`).

## 4. Signaling URL

- Built-in default: `DEFAULT_SIGNALING_URL` in `crates/core/src/config.rs`, currently the
  placeholder `wss://callxl.<account>.workers.dev/ws` (the app then skips the cloud
  quietly and works on the LAN).
- `worker.yml` prints the real one at the end of a deploy ("Signaling URL for
  DEFAULT_SIGNALING_URL / Settings: wss://callxl.<subdomain>.workers.dev/ws") and in the
  job summary.
- Either put it into `config.rs` and tag a new release (every install then uses it), or
  enter it per device in Settings → Signaling URL (stored locally, overrides the default).

## 5. What CI verified

All on GitHub-hosted runners with two (or three) `callxl-cli` peers on one machine,
libwebrtc's test audio device playing a known WAV, real `tc netem` on `lo` (UDP only).
Numbers are from the linked runs' logs/artifacts.

| §14 item | Test(s) | Run | Key numbers |
|---|---|---|---|
| LAN policy over `127.0.0.1` (510 kbps, 10 ms) | `loopback_v4_lan_policy` | TODO(p2/infra-qa: ci run) | |
| LAN policy over `::1` | `loopback_v6_lan_policy` | TODO | |
| No audio gaps > 20 ms | `audio_continuity_no_gaps` | TODO | |
| Internet 128 → 510 ramp | `netem_internet_ramp` (netem `internet_ramp`) | TODO | |
| Back-off and recovery under loss | `netem_loss_backoff_recovery` | TODO | |
| 10 → 20 → 10 ms frame adaptation | `netem_frame_adaptation` | TODO | |
| Path switch IPv6 → IPv4 | `netem_path_switch_v6_to_v4` | TODO | |
| Path switch IPv4 → IPv6 | `netem_path_switch_v4_to_v6` | TODO | |
| File transfer keeps audio loss flat (internet path) | `file_transfer_throttle_keeps_audio_loss_flat` (netem `file_throttle`) | TODO | |
| Chat, receipts | `chat_roundtrip` | TODO | |
| Files: accept prompt, chosen recipients, resume, BLAKE3 check | `file_transfer_accept_resume_hash` | TODO | |
| Trust flow, key change | `trust_flow_and_key_change` | TODO | |
| Signed SDP, tampered SDP rejected | `tampered_sdp_rejected` | TODO | |
| mDNS discovery, glare | `mdns_discovery_loopback`, `signaling_glare` | TODO | |
| Cloud lobby/session/TURN-credential flow (local Worker) | `signaling_roundtrip_wrangler` + Worker vitest | TODO | |
| Diagnostics: every §7 metric, export (schema v1) | `diagnostics_complete`, `export_schema_v1` | TODO | |
| Platform ADM (PulseAudio null sink) | `loopback_v4_lan_policy` with `CALLXL_E2E_ADM=platform` | TODO | |
| Desktop launches < 1 s (Linux runner) | desktop `--headless-smoke` (`startup_ms`) | TODO(orchestrator, p2/desktop) | |
| Windows build, loopback e2e, installer install/uninstall | `windows` job | TODO(orchestrator, p2/desktop) | |
| iOS Simulator tests, xcframework, unsigned IPA | `macos` job | TODO(orchestrator, p2/ios) | |
| Release from a clean tag with all three artifacts | `release.yml` | TODO(orchestrator) | |
| Worker deploy workflow (no-secrets path) | `worker.yml` | TODO | |

Not provable in CI (no real network, no real devices): Wi-Fi LAN calls between
machines, NAT traversal, real IPv6 internet paths, TURN relaying through Cloudflare,
acoustic echo cancellation with real speakers/microphones, Bluetooth, iOS backgrounding
and CallKit interruptions. Those are §6.

## 6. What you must test on real devices

Open **Diagnostics** on both ends during each test; "pass" refers to what it shows.
Use "Export report" when something fails and attach the `CallXL-diagnostics-<ts>.json`.

1. **LAN call (no internet).** Two devices on the same Wi-Fi, router's internet
   unplugged. Start CallXL on both; one taps "+ New session", the other sees it with the
   LAN glyph and taps it; trust each other ("Trust this device", compare the 6 words).
   *Pass:* joins in < 2 s; path `LAN`, candidates `host`/`host`; Opus target ≈ 510 kbps
   from the start; frame 10 ms; loss ≈ 0 %; speech clear both ways, no echo.
2. **Internet call.** One device on mobile data (iOS) or another network, after the
   Worker is deployed (§3). *Pass:* the session appears on the other side's Home screen
   (no LAN glyph); path `Direct`; bitrate starts ≈ 128 kbps and climbs towards 510 within
   ~20 s on a good connection; no dropouts while it climbs.
3. **IPv6 vs IPv4 and the automatic switch.** Both devices on dual-stack networks.
   *Pass:* local/remote addresses tagged `v6`. Then degrade IPv6 (e.g. disable IPv6 on
   one router, or move to an IPv4-only hotspot): within ~5–10 s the path changes to `v4`
   with `pair switches` +1 and no audible gap; when IPv6 is good again for ≥ 5 s (and ≥ 30 s
   after the last switch) it returns to `v6`.
4. **Relayed peer in a mesh.** Three participants, one behind a restrictive NAT/firewall
   (e.g. corporate guest Wi-Fi blocking UDP to peers), TURN secrets set. *Pass:* only that
   peer's tiles show `Relayed` (candidate type `relay`) on the others; the other pair stays
   `Direct`/`LAN`; `TURN bytes this month` grows.
5. **iOS backgrounding and interruptions.** In a call on iPhone: lock the screen, switch
   apps, then take a regular phone call and hang up. *Pass:* audio continues while locked
   and in the background; the system call UI (CallKit) shows CallXL; during the phone call
   CallXL holds; afterwards it resumes without rejoining.
6. **File transfer to multiple recipients.** Three participants; send a large file
   (> 1 GB) to two of them only. *Pass:* only the chosen two get the accept prompt; both
   finish with the hash check passing (file opens); on an internet path audio loss stays
   flat during the transfer; kill Wi-Fi briefly mid-transfer — it resumes from the offset.
7. **Device hot-switch.** During a call plug in / unplug a headset (desktop) or connect
   Bluetooth earbuds (iOS). *Pass:* the device picker lists the new device; audio follows
   or can be switched without leaving the call.
8. **Key-change prompt.** Reinstall CallXL on one device (new keys) and call a device
   that trusted it. *Pass:* exactly one "Key changed — trust again?" prompt; before
   accepting, no call is set up.

**TODO(orchestrator):** add anything the desktop/iOS agents flagged.

## 7. Known limitations

- **No push notifications on iOS** (no PushKit/APNs): incoming calls ring only while
  the app is open or already in a call.
- **Bluetooth microphones are narrowband:** with Bluetooth headsets the mic uses the
  HFP/SCO profile (8–16 kHz), so the 48 kHz fullband path applies only to built-in or
  wired mics.
- **Unsigned IPA:** must be signed by AltStore/Sideloadly with your Apple ID; free
  accounts re-sign every 7 days.
- **Dev `APP_SECRET`:** if the `APP_SECRET` repository secret is absent at build time,
  each build has its own random secret, so cloud signaling only works between apps from
  the same build (LAN calls always work). Diagnostics shows `dev_app_secret: true`.
- **Unsigned Windows installer:** SmartScreen warns on first run.
- AEC3 was only exercised with the test audio device (no acoustic path); real echo
  cancellation quality is part of §6 test 1.

## 8. Deferred to v0.2

Call recording, push-to-talk, per-peer volume, light-theme polish beyond functional,
Flatpak/AppImage, MSIX, push notifications, video.
