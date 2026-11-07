//! Diagnostics export, schema v1 (docs/diagnostics-export.md): the "Export report" of
//! CLAUDE.md §7. Pure function of the core snapshot, the diagnostics snapshot and the
//! log ring, so it is unit-tested on recorded data.
//!
//! Redaction (§10: no PII beyond IPs the user can already see): the only display name
//! is the user's own; peers appear as public key + 8-hex tag; transfer file names,
//! chat, session names, device names and local paths are left out; in log lines every
//! known peer name becomes `peer:<tag>`, every transfer file name `<file>`, and the
//! home directory `~`.

use serde_json::{Map, Value, json};

use crate::types::*;

pub use super::schema_v1::{MAX_LOG_TAIL, SCHEMA_VERSION, validate};

/// Short identifier of a peer used in sparkline names and redacted log lines.
pub fn tag(key: &str) -> &str {
    &key[..key.len().min(8)]
}

fn call_state(s: &CallState) -> (&'static str, Option<&str>) {
    match s {
        CallState::Idle => ("Idle", None),
        CallState::Joining => ("Joining", None),
        CallState::InCall => ("InCall", None),
        CallState::Reconnecting { reason } => ("Reconnecting", Some(reason)),
        CallState::Left { reason } => ("Left", Some(reason)),
    }
}

/// Replaces personal strings in log lines (see the module docs).
struct Redactor {
    /// (needle, replacement), longest needle first so "Bob Smith" wins over "Bob".
    pairs: Vec<(String, String)>,
}

impl Redactor {
    fn new(snap: &Snapshot, home: Option<&str>) -> Self {
        let own = snap.device.display_name.as_str();
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut name = |n: &str, key: &str| {
            if n.chars().count() >= 2 && n != own {
                pairs.push((n.to_string(), format!("peer:{}", tag(key))));
            }
        };
        for p in &snap.peers {
            name(&p.display_name, &p.key);
        }
        for t in &snap.trusted_devices {
            name(&t.display_name, &t.key);
        }
        for s in &snap.sessions {
            for m in &s.members {
                name(&m.display_name, &m.key);
            }
        }
        for t in &snap.transfers {
            if !t.name.is_empty() {
                pairs.push((t.name.clone(), "<file>".into()));
            }
        }
        if let Some(h) = home.filter(|h| h.len() > 1) {
            pairs.push((h.trim_end_matches(['/', '\\']).to_string(), "~".into()));
        }
        let mut seen = std::collections::HashSet::new();
        pairs.retain(|(n, _)| seen.insert(n.clone()));
        pairs.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
        Self { pairs }
    }

    fn line(&self, l: &str) -> String {
        let mut out = l.to_string();
        for (needle, rep) in &self.pairs {
            if out.contains(needle.as_str()) {
                out = out.replace(needle.as_str(), rep);
            }
        }
        out
    }
}

fn stats_value(ps: &PeerStats) -> Value {
    let mut v = serde_json::to_value(ps).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        for k in ["display_name", "peer_key", "path"] {
            o.remove(k);
        }
    }
    v
}

/// Builds the v1 report. `log` is the ring (oldest first); `home` the home directory
/// to redact from log lines (`$HOME` / `%USERPROFILE%`).
pub fn build(snap: &Snapshot, diag: &DiagnosticsSnapshot, log: &[String], home: Option<&str>, ts_ms: u64) -> Value {
    let redact = Redactor::new(snap, home);
    let mut peers = Vec::new();
    for p in &snap.peers {
        let st = diag.peers.iter().find(|s| s.peer_key == p.key);
        peers.push(json!({
            "key": p.key,
            "tag": tag(&p.key),
            "connected": p.connected,
            "trusted": p.trusted,
            "lan_signaling": p.lan,
            "path": st.and_then(|s| s.path.clone()).or_else(|| p.path.clone()),
            "stats": st.map(stats_value),
        }));
    }
    // Stats for a link the snapshot no longer lists (peer leaving between the two reads).
    for s in diag.peers.iter().filter(|s| !snap.peers.iter().any(|p| p.key == s.peer_key)) {
        peers.push(json!({
            "key": s.peer_key,
            "tag": tag(&s.peer_key),
            "connected": false,
            "trusted": snap.trusted_devices.iter().any(|t| t.key == s.peer_key),
            "lan_signaling": false,
            "path": s.path,
            "stats": stats_value(s),
        }));
    }

    let mut global = serde_json::to_value(&diag.global).unwrap_or(Value::Null);
    if let Some(o) = global.as_object_mut() {
        o.insert("stats_ts".into(), json!(diag.ts_ms));
    }
    let series: Map<String, Value> = diag.series.iter().map(|s| (s.metric.clone(), json!(s.values))).collect();

    let (state, reason) = call_state(&snap.call_state);
    let mut transfers = (0u64, 0u64, 0u64, 0u64);
    for t in &snap.transfers {
        match t.state {
            TransferState::Completed => transfers.1 += 1,
            TransferState::Failed { .. } => transfers.2 += 1,
            TransferState::Declined | TransferState::Cancelled => {}
            _ => transfers.0 += 1,
        }
        transfers.3 += t.bytes_done;
    }
    let tail_start = log.len().saturating_sub(MAX_LOG_TAIL);

    json!({
        "schema": SCHEMA_VERSION,
        "app_version": env!("CARGO_PKG_VERSION"),
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "debug_build": cfg!(debug_assertions),
            "dev_app_secret": crate::config::app_secret_is_dev(),
        },
        "ts": ts_ms,
        "call": {
            "state": state,
            "reason": reason,
            "session_id": snap.current_session,
            "muted": snap.muted,
            "peer_count": snap.peers.len(),
            "self": { "display_name": snap.device.display_name, "key": snap.device.signing_key },
            "signaling": {
                "transport": diag.global.signaling_transport,
                "url": snap.settings.effective_signaling_url,
                "cloud_connected": snap.cloud_connected,
            },
            "settings": {
                "bitrate_ceiling_kbps": snap.settings.bitrate_ceiling_kbps,
                "raw_mic": snap.settings.raw_mic,
                "apm": snap.settings.apm,
            },
            "transfers": {
                "active": transfers.0,
                "completed": transfers.1,
                "failed": transfers.2,
                "bytes_done": transfers.3,
            },
        },
        "peers": peers,
        "global": global,
        "series": series,
        "log_tail": log[tail_start..].iter().map(|l| redact.line(l)).collect::<Vec<_>>(),
    })
}

/// File name of an export written at `ts_ms`.
pub fn file_name(ts_ms: u64) -> String {
    format!("CallXL-diagnostics-{ts_ms}.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `diagnostics` snapshot recorded from `diagnostics_complete` (two peers over
    /// an impaired loopback; see the fixture's README line in docs/diagnostics-export.md).
    const FIXTURE: &str = include_str!("../../tests/fixtures/diagnostics-snapshot.json");

    fn snapshot_for(d: &DiagnosticsSnapshot) -> Snapshot {
        let bob = &d.peers[0];
        let settings = Settings {
            display_name: "alice".into(),
            signaling_url: None,
            effective_signaling_url: crate::config::DEFAULT_SIGNALING_URL.into(),
            download_dir: "/home/tester/Downloads/CallXL".into(),
            bitrate_ceiling_kbps: 510,
            apm: ApmConfig::default(),
            raw_mic: false,
            input_device: Some("Tester's Headset".into()),
            output_device: None,
        };
        Snapshot {
            device: DeviceCard {
                display_name: "alice".into(),
                signing_key: "a1".repeat(32),
                agreement_key: "b2".repeat(32),
            },
            sessions: vec![SessionSummary {
                id: "s1".into(),
                name: Some("Family dinner".into()),
                members: vec![SessionMember { key: bob.peer_key.clone(), display_name: bob.display_name.clone() }],
                lan: true,
                cloud: false,
                joined: true,
            }],
            current_session: Some("s1".into()),
            call_state: CallState::InCall,
            peers: vec![PeerInfo {
                key: bob.peer_key.clone(),
                display_name: bob.display_name.clone(),
                trusted: true,
                connected: true,
                speaking: false,
                lan: true,
                path: bob.path.clone(),
            }],
            muted: false,
            trusted_devices: vec![],
            transfers: vec![TransferInfo {
                id: "t1".into(),
                peer_key: bob.peer_key.clone(),
                direction: TransferDirection::Outgoing,
                name: "tax-return-2026.pdf".into(),
                size: 10,
                hash: "00".repeat(32),
                bytes_done: 4,
                state: TransferState::Transferring,
                path: "/home/tester/tax-return-2026.pdf".into(),
                session: "s1".into(),
            }],
            settings,
            cloud_connected: false,
        }
    }

    #[test]
    fn export_schema_v1() {
        let d: DiagnosticsSnapshot = serde_json::from_str(FIXTURE).expect("fixture matches DiagnosticsSnapshot");
        assert!(!d.peers.is_empty() && !d.series.is_empty(), "fixture has peers and series");
        let snap = snapshot_for(&d);
        let bob = d.peers[0].display_name.clone();
        let log = vec![
            format!("1 INFO callxl_core: {bob} joined"),
            "2 INFO callxl_core::files: resuming tax-return-2026.pdf from 3f2a".to_string(),
            "3 WARN callxl_core: could not write /home/tester/Downloads/CallXL/x.json".to_string(),
        ];
        let v = build(&snap, &d, &log, Some("/home/tester"), 1_762_500_000_000);
        if let Err(e) = validate(&v) {
            panic!("export does not validate:\n{}", e.join("\n"));
        }

        // Every §7 metric is present.
        let p = &v["peers"][0];
        for k in [
            "rtt_ms",
            "jitter_ms",
            "loss_pct",
            "send_kbps",
            "recv_kbps",
            "opus_frame_ms",
            "jitter_buffer_ms",
            "concealment_events",
            "pair_switches",
        ] {
            assert!(p["stats"][k].is_number(), "peer stat {k}");
        }
        for k in ["kind", "local_addr", "local_family", "remote_addr", "remote_family", "local_candidate_type"] {
            assert!(p["path"][k].is_string(), "path {k}");
        }
        for k in [
            "uplink_kbps",
            "downlink_kbps",
            "media_thread_cpu_pct",
            "signaling_transport",
            "signaling_latency_ms",
            "turn_bytes_month",
            "apm",
        ] {
            assert!(!v["global"][k].is_null(), "global {k}");
        }
        assert_eq!(v["series"].as_object().unwrap().len(), d.series.len());

        // Redaction: the peer's name, file names, session name, device names and the home
        // directory appear nowhere; the user's own name does.
        let text = serde_json::to_string(&v).unwrap();
        for secret in [bob.as_str(), "tax-return-2026", "Family dinner", "Tester's Headset", "/home/tester"] {
            assert!(!text.contains(secret), "{secret:?} leaked into the export");
        }
        assert_eq!(v["call"]["self"]["display_name"], "alice");
        let tagged = format!("peer:{} joined", tag(&d.peers[0].peer_key));
        assert!(v["log_tail"][0].as_str().unwrap().ends_with(&tagged), "{}", v["log_tail"][0]);
        assert!(v["log_tail"][1].as_str().unwrap().contains("resuming <file> from"));
        assert!(v["log_tail"][2].as_str().unwrap().contains("~/Downloads/CallXL/x.json"));
    }

    #[test]
    fn export_schema_v1_rejects_violations() {
        let d: DiagnosticsSnapshot = serde_json::from_str(FIXTURE).unwrap();
        let snap = snapshot_for(&d);
        let good = build(&snap, &d, &[], None, 1);
        let mut bad = good.clone();
        bad["schema"] = json!(2);
        bad["peers"][0]["display_name"] = json!("bob");
        bad["peers"][0]["stats"]["rtt_ms"] = json!("fast");
        bad["series"]["rtt_ms:x"] = json!(vec![1.0; 61]);
        bad["global"].as_object_mut().unwrap().remove("apm");
        let errs = validate(&bad).unwrap_err();
        assert_eq!(errs.len(), 5, "{errs:#?}");
        let log: Vec<String> = (0..MAX_LOG_TAIL + 10).map(|i| format!("line {i}")).collect();
        let v = build(&snap, &d, &log, None, 1);
        assert_eq!(v["log_tail"].as_array().unwrap().len(), MAX_LOG_TAIL);
        assert_eq!(v["log_tail"][0], "line 10", "the tail keeps the newest lines");
    }
}
