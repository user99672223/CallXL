//! Pure conversions from core state to UI rows (run on the bridge thread, never on the
//! UI thread). Everything returned here is `Send`.

use std::fmt::Write as _;

use callxl_core::{
    AddrFamily, CallState, ChatMessage, DiagnosticsSnapshot, PathInfo, PathKind, PeerStats, ReceiptStatus,
    SessionSummary, SignalingTransport, Snapshot, TransferDirection, TransferInfo, TransferState, TrustedDevice,
};
use slint::{Color, SharedString};

use crate::{ChatItem, DeviceItem, Metric, PeerItem, SessionItem};

/// Up to two initials from a display name ("Ada Lovelace" → "AL", "bob" → "B").
pub fn initials(name: &str) -> String {
    let mut out = String::new();
    for w in name.split_whitespace().take(2) {
        if let Some(c) = w.chars().find(|c| c.is_alphanumeric()) {
            out.extend(c.to_uppercase());
        }
    }
    if out.is_empty() { "?".into() } else { out }
}

/// A stable, calm avatar colour per identity key (HSL with fixed saturation/lightness).
pub fn hue(key: &str) -> Color {
    let h = key.bytes().fold(2_166_136_261u32, |a, b| (a ^ u32::from(b)).wrapping_mul(16_777_619));
    let hue = (h % 360) as f32;
    let (s, l) = (0.62f32, 0.62f32);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::from_rgb_u8(to(r), to(g), to(b))
}

pub fn session_title(s: &SessionSummary) -> String {
    match s.name.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => match s.members.first() {
            Some(m) => format!("{}'s session", m.display_name),
            None => "Untitled session".into(),
        },
    }
}

pub fn session_items(sessions: &[SessionSummary]) -> Vec<SessionItem> {
    sessions
        .iter()
        .map(|s| {
            let names: Vec<&str> = s.members.iter().map(|m| m.display_name.as_str()).collect();
            let members = match names.len() {
                0 => "Nobody here yet".to_string(),
                1..=4 => names.join(", "),
                n => format!("{} and {} others", names[..3].join(", "), n - 3),
            };
            SessionItem {
                id: s.id.as_str().into(),
                title: session_title(s).into(),
                members: members.into(),
                member_count: s.members.len() as i32,
                lan: s.lan,
                joined: s.joined,
            }
        })
        .collect()
}

pub fn device_items(devices: &[TrustedDevice]) -> Vec<DeviceItem> {
    let mut v: Vec<&TrustedDevice> = devices.iter().collect();
    v.sort_by(|a, b| {
        b.online.cmp(&a.online).then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
    });
    v.into_iter()
        .map(|d| DeviceItem {
            key: d.key.as_str().into(),
            name: d.display_name.as_str().into(),
            initials: initials(&d.display_name).into(),
            hue: hue(&d.key),
            online: d.online,
        })
        .collect()
}

pub fn path_kind(k: PathKind) -> &'static str {
    match k {
        PathKind::Lan => "LAN",
        PathKind::Direct => "Direct",
        PathKind::Relayed => "Relayed",
    }
}

pub fn family(f: AddrFamily) -> &'static str {
    match f {
        AddrFamily::V4 => "IPv4",
        AddrFamily::V6 => "IPv6",
    }
}

/// "you" first, then every peer in the call.
pub fn peer_items(s: &Snapshot) -> Vec<PeerItem> {
    let mut v = vec![PeerItem {
        key: s.device.signing_key.as_str().into(),
        name: s.settings.display_name.as_str().into(),
        initials: initials(&s.settings.display_name).into(),
        hue: hue(&s.device.signing_key),
        is_self: true,
        connected: true,
        speaking: false,
        muted: s.muted,
        trusted: true,
        path_kind: SharedString::new(),
        path_family: SharedString::new(),
    }];
    for p in &s.peers {
        let (kind, fam) = match (&p.path, p.connected) {
            (Some(path), true) => (path_kind(path.kind), family(path.remote_family)),
            _ => ("", ""),
        };
        v.push(PeerItem {
            key: p.key.as_str().into(),
            name: p.display_name.as_str().into(),
            initials: initials(&p.display_name).into(),
            hue: hue(&p.key),
            is_self: false,
            connected: p.connected,
            speaking: p.speaking,
            muted: false,
            trusted: p.trusted,
            path_kind: kind.into(),
            path_family: fam.into(),
        });
    }
    v
}

/// (status line, reconnecting?) for the call header.
pub fn call_status(state: &CallState, peers: usize) -> (String, bool) {
    match state {
        CallState::Idle => (String::new(), false),
        CallState::Joining => ("Joining…".into(), false),
        CallState::InCall => match peers {
            0 => ("Waiting for others to join".into(), false),
            1 => ("Connected · 2 people".into(), false),
            n => (format!("Connected · {} people", n + 1), false),
        },
        CallState::Reconnecting { reason } => (format!("Reconnecting… {reason}"), true),
        CallState::Left { reason } => (reason.clone(), false),
    }
}

pub fn in_call(s: &Snapshot) -> bool {
    s.current_session.is_some()
        && matches!(s.call_state, CallState::Joining | CallState::InCall | CallState::Reconnecting { .. })
}

pub fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else if v < 10.0 {
        format!("{v:.1} {}", U[i])
    } else {
        format!("{v:.0} {}", U[i])
    }
}

/// Local wall-clock "HH:MM" for a Unix timestamp (UTC offset taken from the system once).
pub fn clock(ts_ms: u64) -> String {
    let secs = (ts_ms / 1000) as i64 + local_offset_secs();
    let day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}

fn local_offset_secs() -> i64 {
    use std::sync::OnceLock;
    static OFF: OnceLock<i64> = OnceLock::new();
    *OFF.get_or_init(crate::platform::utc_offset_secs)
}

fn status(s: ReceiptStatus) -> &'static str {
    match s {
        ReceiptStatus::Sending => "Sending",
        ReceiptStatus::Sent => "Sent",
        ReceiptStatus::Delivered => "Delivered",
        ReceiptStatus::Read => "Read",
    }
}

pub fn chat_item(m: &ChatMessage) -> ChatItem {
    ChatItem {
        id: m.id.as_str().into(),
        sender: m.sender_name.as_str().into(),
        body: m.body.as_str().into(),
        time: clock(m.ts_ms).into(),
        own: m.own,
        status: if m.own { status(m.status).into() } else { SharedString::new() },
        ..Default::default()
    }
}

pub fn own_chat_item(id: &str, body: &str, sender: &str, ts_ms: u64) -> ChatItem {
    ChatItem {
        id: id.into(),
        sender: sender.into(),
        body: body.into(),
        time: clock(ts_ms).into(),
        own: true,
        status: "Sending".into(),
        ..Default::default()
    }
}

pub fn transfer_state(t: &TransferInfo) -> String {
    let pct = t.bytes_done.saturating_mul(100).checked_div(t.size).unwrap_or(0);
    match (&t.state, t.direction) {
        (TransferState::Offered, TransferDirection::Incoming) => "Waiting for you".into(),
        (TransferState::Offered, TransferDirection::Outgoing) => "Waiting for them to accept".into(),
        (TransferState::Accepted, _) => "Starting…".into(),
        (TransferState::Transferring, TransferDirection::Outgoing) => format!("Sending {pct}%"),
        (TransferState::Transferring, TransferDirection::Incoming) => format!("Receiving {pct}%"),
        (TransferState::Paused, _) => format!("Paused at {pct}% — resumes when reconnected"),
        (TransferState::Completed, TransferDirection::Incoming) => "Received · verified".into(),
        (TransferState::Completed, TransferDirection::Outgoing) => "Sent · verified".into(),
        (TransferState::Declined, _) => "Declined".into(),
        (TransferState::Cancelled, _) => "Cancelled".into(),
        (TransferState::Failed { reason }, _) => format!("Failed: {reason}"),
    }
}

pub fn transfer_item(t: &TransferInfo, peer_name: &str) -> ChatItem {
    let incoming = t.direction == TransferDirection::Incoming;
    let progress = if t.size > 0 { t.bytes_done as f32 / t.size as f32 } else { 0.0 };
    let active = matches!(
        t.state,
        TransferState::Offered | TransferState::Accepted | TransferState::Transferring | TransferState::Paused
    );
    ChatItem {
        id: t.id.as_str().into(),
        sender: peer_name.into(),
        body: SharedString::new(),
        time: SharedString::new(),
        own: !incoming,
        status: if incoming { SharedString::new() } else { format!("to {peer_name}").into() },
        is_file: true,
        file_name: t.name.as_str().into(),
        file_size: human_bytes(t.size).into(),
        file_state: transfer_state(t).into(),
        progress: progress.clamp(0.0, 1.0),
        can_accept: incoming && t.state == TransferState::Offered,
        can_cancel: active && !(incoming && t.state == TransferState::Offered),
        can_open: t.state == TransferState::Completed || !incoming,
        path: t.path.as_str().into(),
    }
}

/// A 60 s sparkline as SVG path commands in a 100 x 15 viewbox; "" when there is nothing
/// to draw. Scaled from 0 (or the minimum when negative) to the maximum.
pub fn spark(values: &[f64]) -> String {
    if values.len() < 2 {
        return String::new();
    }
    let max = values.iter().copied().fold(f64::MIN, f64::max);
    let min = values.iter().copied().fold(f64::MAX, f64::min).min(0.0);
    let span = if max - min > 1e-9 { max - min } else { 1.0 };
    let step = 100.0 / (values.len() - 1) as f64;
    let mut out = String::with_capacity(values.len() * 12);
    for (i, v) in values.iter().enumerate() {
        let y = 14.0 - (v - min) / span * 13.0;
        let _ = write!(out, "{}{:.1} {:.1} ", if i == 0 { "M" } else { "L" }, i as f64 * step, y);
    }
    out
}

fn series<'a>(d: &'a DiagnosticsSnapshot, metric: &str) -> &'a [f64] {
    d.series.iter().find(|s| s.metric == metric).map(|s| s.values.as_slice()).unwrap_or(&[])
}

fn metric(label: &str, value: String, spark_values: &[f64]) -> Metric {
    Metric { label: label.into(), value: value.into(), spark: spark(spark_values).into() }
}

fn path_text(p: &Option<PathInfo>) -> (String, String, String, String) {
    match p {
        Some(p) => (
            path_kind(p.kind).to_string(),
            format!("{} ({})", p.local_addr, family(p.local_family)),
            format!("{} ({})", p.remote_addr, family(p.remote_family)),
            format!("{} → {}", p.local_candidate_type, p.remote_candidate_type),
        ),
        None => ("Connecting".into(), "—".into(), "—".into(), "no pair selected yet".into()),
    }
}

/// One peer card on the Diagnostics screen (every per-peer metric of CLAUDE.md §7).
pub struct PeerDiagRow {
    pub name: String,
    pub path_kind: String,
    pub local: String,
    pub remote: String,
    pub candidates: String,
    pub metrics: Vec<Metric>,
}

pub fn peer_diag(d: &DiagnosticsSnapshot, p: &PeerStats) -> PeerDiagRow {
    let tag = &p.peer_key[..p.peer_key.len().min(8)];
    let s = |m: &str| series(d, &format!("{m}:{tag}"));
    let (kind, local, remote, cands) = path_text(&p.path);
    PeerDiagRow {
        name: p.display_name.clone(),
        path_kind: kind,
        local,
        remote,
        candidates: cands,
        metrics: vec![
            metric("Round trip", format!("{:.0} ms", p.rtt_ms), s("rtt_ms")),
            metric("Jitter", format!("{:.1} ms", p.jitter_ms), s("jitter_ms")),
            metric("Loss in / out", format!("{:.1} % / {:.1} %", p.loss_pct, p.remote_loss_pct), s("loss_pct")),
            metric("Sending", format!("{:.0} kbps", p.send_kbps), s("send_kbps")),
            metric("Receiving", format!("{:.0} kbps", p.recv_kbps), s("recv_kbps")),
            metric("Opus bitrate", format!("{:.0} kbps", p.opus_bitrate_kbps), s("opus_bitrate_kbps")),
            metric("Opus frame", format!("{} ms", p.opus_frame_ms), s("opus_frame_ms")),
            metric("Jitter buffer", format!("{:.0} ms", p.jitter_buffer_ms), s("jitter_buffer_ms")),
            metric("Concealment events", p.concealment_events.to_string(), s("concealment_events")),
            metric("Path switches", p.pair_switches.to_string(), s("pair_switches")),
            metric(
                "Codec",
                format!(
                    "Opus {}k {}{}{}",
                    p.clock_rate / 1000,
                    if p.channels == 1 { "mono" } else { "stereo" },
                    if p.fec_enabled { ", FEC" } else { "" },
                    if p.dtx_enabled { ", DTX" } else { "" }
                ),
                &[],
            ),
            metric("Available uplink", format!("{:.0} kbps", p.available_outgoing_kbps), &[]),
        ],
    }
}

pub fn global_metrics(d: &DiagnosticsSnapshot, cloud_connected: bool) -> Vec<Metric> {
    let g = &d.global;
    let transport = match g.signaling_transport {
        SignalingTransport::Lan => "LAN",
        SignalingTransport::Cloud => "Cloud",
        SignalingTransport::None => {
            if cloud_connected {
                "Cloud (idle)"
            } else {
                "—"
            }
        }
    };
    let apm = if !(g.apm.aec || g.apm.noise_suppression || g.apm.high_pass || g.apm.agc) {
        "Raw mic".to_string()
    } else {
        let mut parts = Vec::new();
        if g.apm.aec {
            parts.push("AEC".to_string());
        }
        if g.apm.noise_suppression {
            parts.push(format!("NS {:?}", g.apm.ns_level).to_lowercase().replacen("ns", "NS", 1));
        }
        if g.apm.high_pass {
            parts.push("HPF".into());
        }
        if g.apm.agc {
            parts.push("AGC".into());
        }
        parts.join(" · ")
    };
    vec![
        metric("Uplink", format!("{:.0} kbps", g.uplink_kbps), series(d, "uplink_kbps")),
        metric("Downlink", format!("{:.0} kbps", g.downlink_kbps), series(d, "downlink_kbps")),
        metric("Media thread CPU", format!("{:.1} %", g.media_thread_cpu_pct), series(d, "media_thread_cpu_pct")),
        metric(
            "Signaling",
            if g.signaling_latency_ms > 0.0 {
                format!("{transport} · {:.0} ms", g.signaling_latency_ms)
            } else {
                transport.to_string()
            },
            series(d, "signaling_latency_ms"),
        ),
        metric("TURN this month", human_bytes(g.turn_bytes_month), &[]),
        metric("Audio processing", apm, &[]),
        metric(
            "Frame adaptation",
            if g.ana_frame_ms > 0 {
                format!("{} ms · {} switches · {:.1} % loss", g.ana_frame_ms, g.ana_switches, g.ana_uplink_loss_pct)
            } else {
                "idle".into()
            },
            series(d, "ana_frame_ms"),
        ),
        metric(
            "Echo canceller",
            if g.aec_erle_db >= 0.0 {
                format!("ERLE {:.1} dB · delay {} ms", g.aec_erle_db, g.aec_delay_ms)
            } else {
                "—".into()
            },
            &[],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_and_spark() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("bob"), "B");
        assert_eq!(initials("  "), "?");
        assert_eq!(spark(&[1.0]), "");
        let s = spark(&[0.0, 10.0]);
        assert!(s.starts_with("M0.0 14.0 L100.0 1.0"), "{s}");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1_500_000), "1.5 MB");
    }
}
