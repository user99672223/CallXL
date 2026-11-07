//! Screenshot tests: every screen rendered offscreen with Slint's software renderer from
//! fake core snapshots, through the same `ViewModel` patches the app applies. PNGs land in
//! `<target>/screens/`. Assertions: the image is not blank, the accent colour is present,
//! and screen-specific pixels/rows prove the data reached the UI.

use std::rc::Rc;

use callxl_core::*;
use callxl_desktop::bridge::{self, Patch, ViewModel};
use callxl_desktop::{App, AppWindow, Colors};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Model, PhysicalSize};

const W: u32 = 1120;
const H: u32 = 740;

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn key(n: u8) -> String {
    format!("{:02x}", n).repeat(32)
}

fn apm() -> ApmConfig {
    ApmConfig::default()
}

fn settings() -> Settings {
    Settings {
        display_name: "Ada Lovelace".into(),
        signaling_url: None,
        effective_signaling_url: "wss://callxl.example.workers.dev/ws".into(),
        download_dir: "/home/ada/Downloads/CallXL".into(),
        bitrate_ceiling_kbps: 510,
        apm: apm(),
        raw_mic: false,
        input_device: None,
        output_device: None,
    }
}

fn path(kind: PathKind, fam: AddrFamily) -> PathInfo {
    PathInfo {
        kind,
        local_addr: if fam == AddrFamily::V6 { "fd00::12".into() } else { "192.168.1.12:50000".into() },
        local_family: fam,
        remote_addr: if fam == AddrFamily::V6 { "fd00::34".into() } else { "192.168.1.34:50002".into() },
        remote_family: fam,
        local_candidate_type: "host".into(),
        remote_candidate_type: if kind == PathKind::Relayed { "relay".into() } else { "host".into() },
        switches: 0,
    }
}

fn peer(n: u8, name: &str, kind: PathKind, fam: AddrFamily, speaking: bool) -> PeerInfo {
    PeerInfo {
        key: key(n),
        display_name: name.into(),
        trusted: true,
        connected: true,
        speaking,
        lan: kind == PathKind::Lan,
        path: Some(path(kind, fam)),
    }
}

fn home_snapshot() -> Snapshot {
    Snapshot {
        device: DeviceCard { display_name: "Ada Lovelace".into(), signing_key: key(1), agreement_key: key(2) },
        sessions: vec![
            SessionSummary {
                id: "s1".into(),
                name: Some("Movie night".into()),
                members: vec![
                    SessionMember { key: key(3), display_name: "Grace".into() },
                    SessionMember { key: key(4), display_name: "Linus".into() },
                ],
                lan: true,
                cloud: false,
                joined: false,
            },
            SessionSummary {
                id: "s2".into(),
                name: None,
                members: vec![SessionMember { key: key(5), display_name: "Margaret".into() }],
                lan: false,
                cloud: true,
                joined: false,
            },
        ],
        current_session: None,
        call_state: CallState::Idle,
        peers: vec![],
        muted: false,
        trusted_devices: vec![
            TrustedDevice { key: key(3), display_name: "Grace".into(), online: true, pinned_ms: 1 },
            TrustedDevice { key: key(5), display_name: "Margaret".into(), online: false, pinned_ms: 2 },
        ],
        transfers: vec![],
        settings: settings(),
        cloud_connected: true,
    }
}

fn call_snapshot() -> Snapshot {
    let mut s = home_snapshot();
    s.sessions[0].joined = true;
    s.current_session = Some("s1".into());
    s.call_state = CallState::InCall;
    s.peers = vec![
        peer(3, "Grace", PathKind::Lan, AddrFamily::V6, true),
        peer(4, "Linus", PathKind::Direct, AddrFamily::V4, false),
        peer(6, "Barbara", PathKind::Relayed, AddrFamily::V4, false),
    ];
    s.transfers = vec![TransferInfo {
        id: "t1".into(),
        peer_key: key(4),
        direction: TransferDirection::Incoming,
        name: "holiday-photos.zip".into(),
        size: 48_300_000,
        hash: "00".into(),
        bytes_done: 19_000_000,
        state: TransferState::Transferring,
        path: "/home/ada/Downloads/CallXL/holiday-photos.zip".into(),
        session: "s1".into(),
    }];
    s
}

fn stats(n: u8, name: &str, kind: PathKind) -> PeerStats {
    PeerStats {
        peer_key: key(n),
        display_name: name.into(),
        path: Some(path(kind, AddrFamily::V6)),
        rtt_ms: 2.0,
        jitter_ms: 0.8,
        loss_pct: 0.0,
        remote_loss_pct: 0.0,
        send_kbps: 548.0,
        recv_kbps: 551.0,
        opus_frame_ms: 10,
        jitter_buffer_ms: 20.0,
        concealment_events: 0,
        pair_switches: 0,
        opus_bitrate_kbps: 510.0,
        opus_payload_kbps: 505.0,
        fec_enabled: true,
        dtx_enabled: false,
        codec: "audio/opus".into(),
        clock_rate: 48000,
        channels: 1,
        opus_complexity: 10,
        available_outgoing_kbps: 600.0,
        audio_level: 0.1,
        file_rate_limit_kbps: 0.0,
    }
}

fn diagnostics() -> DiagnosticsSnapshot {
    let ramp: Vec<f64> = (0..60).map(|i| 128.0 + (i as f64) * 6.5).collect();
    let tag = &key(3)[..8];
    DiagnosticsSnapshot {
        ts_ms: 1,
        peers: vec![stats(3, "Grace", PathKind::Lan), stats(4, "Linus", PathKind::Direct)],
        global: GlobalStats {
            uplink_kbps: 1100.0,
            downlink_kbps: 1102.0,
            media_thread_cpu_pct: 3.2,
            signaling_transport: SignalingTransport::Lan,
            signaling_latency_ms: 1.0,
            turn_bytes_month: 0,
            apm: apm(),
            frame_adaptation_mechanism: "ana".into(),
            ana_frame_ms: 10,
            ana_switches: 0,
            ana_uplink_loss_pct: 0.0,
            rejected_signaling: 0,
            glare_events: 0,
            aec_erl_db: -1.0,
            aec_erle_db: -1.0,
            aec_delay_ms: -1,
            dropped_media_events: 0,
        },
        series: vec![
            Sparkline { metric: "uplink_kbps".into(), values: ramp.clone() },
            Sparkline { metric: format!("opus_bitrate_kbps:{tag}"), values: ramp.clone() },
            Sparkline { metric: format!("rtt_ms:{tag}"), values: (0..60).map(|i| 2.0 + ((i % 7) as f64)).collect() },
        ],
    }
}

fn apply(w: &AppWindow, patches: Vec<Patch>) {
    for p in patches {
        p(w, None);
    }
}

struct Shot {
    pixels: Vec<Rgb565Pixel>,
}

impl Shot {
    fn rgb(&self, i: usize) -> (u8, u8, u8) {
        let p = self.pixels[i];
        let r = ((p.0 >> 11) & 0x1f) as u8;
        let g = ((p.0 >> 5) & 0x3f) as u8;
        let b = (p.0 & 0x1f) as u8;
        ((r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2))
    }

    /// Pixels within ±10 per channel of `c` (RGB565 quantisation).
    fn count_near(&self, c: (u8, u8, u8)) -> usize {
        (0..self.pixels.len())
            .filter(|&i| {
                let (r, g, b) = self.rgb(i);
                r.abs_diff(c.0) <= 10 && g.abs_diff(c.1) <= 10 && b.abs_diff(c.2) <= 10
            })
            .count()
    }

    fn distinct(&self) -> usize {
        let mut v: Vec<u16> = self.pixels.iter().map(|p| p.0).collect();
        v.sort_unstable();
        v.dedup();
        v.len()
    }

    fn save(&self, name: &str) -> std::path::PathBuf {
        let target = std::env::var("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
        let dir = target.join("screens");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        let mut enc = png::Encoder::new(std::fs::File::create(&path).unwrap(), W, H);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut data = Vec::with_capacity(self.pixels.len() * 3);
        for i in 0..self.pixels.len() {
            let (r, g, b) = self.rgb(i);
            data.extend([r, g, b]);
        }
        enc.write_header().unwrap().write_image_data(&data).unwrap();
        path
    }
}

fn render(window: &MinimalSoftwareWindow) -> Shot {
    // Let animations (drawer, glow) settle: advance Slint's clock past their duration.
    for _ in 0..30 {
        slint::platform::update_timers_and_animations();
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    window.request_redraw();
    let mut pixels = vec![Rgb565Pixel(0); (W * H) as usize];
    window.draw_if_needed(|r| {
        r.render(&mut pixels, W as usize);
    });
    Shot { pixels }
}

fn hex(s: &str) -> (u8, u8, u8) {
    let h = s.trim_start_matches('#');
    (
        u8::from_str_radix(&h[0..2], 16).unwrap(),
        u8::from_str_radix(&h[2..4], 16).unwrap(),
        u8::from_str_radix(&h[4..6], 16).unwrap(),
    )
}

fn tokens() -> serde_json::Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/tokens.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// One test renders every screen (Slint's platform is process-global).
#[test]
fn screens() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone()))).unwrap();
    window.set_size(PhysicalSize::new(W, H));
    let ui = AppWindow::new().unwrap();
    bridge::init_models(&ui);
    ui.show().unwrap();
    let t = tokens();
    let accent = hex(t["color"]["dark"]["accent"].as_str().unwrap());
    let bg = hex(t["color"]["dark"]["bg"].as_str().unwrap());
    let mut vm = ViewModel::new();

    // ---- Home ----
    apply(&ui, vm.on_snapshot(home_snapshot()));
    let app = ui.global::<App>();
    assert_eq!(app.get_sessions().row_count(), 2);
    assert_eq!(app.get_trusted().row_count(), 2);
    let shot = render(&window);
    let path = shot.save("home");
    eprintln!("home: {} ({} colours, {} accent px)", path.display(), shot.distinct(), shot.count_near(accent));
    assert!(shot.distinct() > 50, "home screen is blank");
    assert!(shot.count_near(bg) > (W * H / 4) as usize, "dark background missing");
    assert!(shot.count_near(accent) > 500, "accent colour missing (New session / Join buttons)");

    // ---- Trust prompt over Home ----
    apply(
        &ui,
        vm.on_event(
            &Event::TrustPrompt {
                peer_key: key(7),
                display_name: "Katherine".into(),
                phrase: "orbit velvet canyon maple drift harbor".into(),
                key_changed: false,
            },
            None,
            None,
        ),
    );
    assert!(app.get_trust_open());
    let shot = render(&window);
    shot.save("trust");
    assert!(shot.count_near(accent) > 300, "Trust button missing");
    apply(&ui, vm.trust_done(None));
    assert!(!app.get_trust_open());

    // ---- In call: 3 peers + you, one speaking, chat drawer open ----
    apply(&ui, vm.on_snapshot(call_snapshot()));
    apply(&ui, vm.load_chat(Some("s1".into()), vec![chat("m1", "Grace", key(3), "Can everyone hear me?", false)]));
    assert_eq!(app.get_screen(), 1, "joining switches to the call screen");
    assert_eq!(app.get_peers().row_count(), 4, "you + 3 peers");
    let speaking: Vec<bool> = (0..4).map(|i| app.get_peers().row_data(i).unwrap().speaking).collect();
    assert_eq!(speaking, [false, true, false, false]);
    let badges: Vec<String> = (1..4)
        .map(|i| {
            let p = app.get_peers().row_data(i).unwrap();
            format!("{} · {}", p.path_kind, p.path_family)
        })
        .collect();
    assert_eq!(badges, ["LAN · IPv6", "Direct · IPv4", "Relayed · IPv4"]);
    let shot = render(&window);
    shot.save("call");
    // The glow ring is speaking_glow (with alpha) blended over the tile surface.
    let g = t["color"]["dark"]["speaking_glow"].as_str().unwrap();
    let (gc, a) = (hex(&g[..7]), u8::from_str_radix(&g[7..9], 16).unwrap() as f32 / 255.0);
    let surface = hex(t["color"]["dark"]["surface"].as_str().unwrap());
    let mix = |x: u8, y: u8| (x as f32 * a + y as f32 * (1.0 - a)).round() as u8;
    let glow = (mix(gc.0, surface.0), mix(gc.1, surface.1), mix(gc.2, surface.2));
    let glow_px = shot.count_near(glow);
    eprintln!("call: speaking glow {glow:?}: {glow_px} px");
    assert!(glow_px > 500, "speaking glow not drawn");
    // SpeakingChanged updates one row in place (no model reset).
    apply(&ui, vm.on_event(&Event::SpeakingChanged { peer_key: key(4), speaking: true }, None, None));
    assert!(app.get_peers().row_data(2).unwrap().speaking);

    app.set_drawer_open(true);
    let shot = render(&window);
    shot.save("call-chat-drawer");
    assert_eq!(app.get_chat().row_count(), 2, "message + file card");

    // ---- Chat screen ----
    app.set_drawer_open(false);
    app.set_screen(2);
    apply(&ui, vm.own_message("m2", "Loud and clear, at 510 kbps."));
    let shot = render(&window);
    shot.save("chat");
    assert_eq!(app.get_chat().row_count(), 3);
    assert!(shot.count_near(accent) > 500, "own bubble in accent");

    // ---- Diagnostics (two StatsTicks: rows are updated in place) ----
    app.set_screen(3);
    apply(&ui, vm.diag_patches(&diagnostics()));
    apply(&ui, vm.diag_patches(&diagnostics()));
    let peers = app.get_diag_peers();
    assert_eq!(peers.row_count(), 2);
    let m0 = peers.row_data(0).unwrap().metrics;
    let labels: Vec<String> = (0..m0.row_count()).map(|i| m0.row_data(i).unwrap().label.to_string()).collect();
    for l in [
        "Round trip",
        "Jitter",
        "Loss in / out",
        "Sending",
        "Receiving",
        "Opus frame",
        "Jitter buffer",
        "Concealment events",
        "Path switches",
    ] {
        assert!(labels.iter().any(|x| x == l), "missing per-peer metric {l}");
    }
    assert!(!m0.row_data(5).unwrap().spark.is_empty(), "Opus bitrate sparkline");
    let shot = render(&window);
    shot.save("diagnostics");
    assert!(shot.count_near(accent) > 200, "sparklines drawn in accent");

    // ---- Settings ----
    app.set_screen(4);
    let shot = render(&window);
    shot.save("settings");
    assert_eq!(app.get_bitrate(), 510);
    assert!(app.get_aec() && app.get_ns() && app.get_hpf() && app.get_agc());
    assert!(shot.count_near(accent) > 300, "toggles on in accent");

    // ---- Light theme (functional) ----
    ui.global::<Colors>().set_dark(false);
    app.set_screen(0);
    let shot = render(&window);
    shot.save("home-light");
    let light_bg = hex(t["color"]["light"]["bg"].as_str().unwrap());
    assert!(shot.count_near(light_bg) > (W * H / 4) as usize, "light background");
}

fn chat(id: &str, name: &str, key: String, body: &str, own: bool) -> ChatMessage {
    ChatMessage {
        id: id.into(),
        session: "s1".into(),
        ts_ms: 1_762_500_000_000,
        sender_key: key,
        sender_name: name.into(),
        kind: ChatKind::Text,
        body: body.into(),
        own,
        status: ReceiptStatus::Delivered,
    }
}
