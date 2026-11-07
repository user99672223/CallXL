//! Diagnostics (CLAUDE.md §7): every metric present and live after a few seconds of a
//! call, 60 s sparklines, and the export.

use std::time::Duration;

use anyhow::{Result, ensure};
use callxl_e2e::netem::{Family, Impairment, Netem};
use callxl_e2e::*;
use serde_json::{Value, json};

fn num(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(f64::NAN)
}

/// Two LAN peers over an impaired loopback (20 ms ± 5 ms, 2 % loss) so RTT, jitter, loss
/// and concealment are non-zero; after 8 s every §7 field must be populated.
#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_complete() -> Result<()> {
    let h = Harness::new("diagnostics_complete")?;
    let netem = Netem::new().await?;
    netem.set(Family::All, Impairment { delay_ms: 20, jitter_ms: 5, loss_pct: 2.0, rate_kbps: 0 })?;
    let extra = netem.cli_args();
    let a = Peer::spawn(&h, PeerOpts::new("alice").bind("127.0.0.1").wav(0).extra(extra.clone())).await?;
    let b =
        Peer::spawn(&h, PeerOpts::new("bob").bind("127.0.0.1").wav(1).extra(extra).lan_peer(a.lan.as_deref().unwrap()))
            .await?;
    call_all(&[&a, &b], Duration::from_secs(30)).await?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    let d = a.diagnostics().await?;
    h.save_json("alice-diagnostics.json", &d);
    let p = &d["peers"][0];
    let g = &d["global"];
    eprintln!("peer: {p}\nglobal: {g}");

    // Per peer (§7).
    let path = &p["path"];
    let types = [path["local_candidate_type"].as_str(), path["remote_candidate_type"].as_str()];
    // host<->host is LAN; the proxy backend's relay makes the local side peer-reflexive
    // (like a NAT), which is correctly classified as a direct internet path.
    let expect = if types == [Some("host"), Some("host")] { "Lan" } else { "Direct" };
    ensure!(path["kind"] == expect, "path type {} for candidates {types:?}", path["kind"]);
    for side in ["local", "remote"] {
        let addr = path[format!("{side}_addr")].as_str().unwrap_or_default();
        ensure!(addr.parse::<std::net::SocketAddr>().is_ok(), "{side} address {addr:?}");
        ensure!(path[format!("{side}_family")] == "V4", "{side} family tag");
        let t = path[format!("{side}_candidate_type")].as_str().unwrap_or_default();
        ensure!(["host", "srflx", "prflx", "relay"].contains(&t), "{side} candidate type {t:?}");
    }
    let series = d["series"].as_array().cloned().unwrap_or_default();
    let tag = &b.key[..8];
    let peak = |m: &str| {
        series
            .iter()
            .find(|s| s["metric"] == format!("{m}:{tag}").as_str())
            .and_then(|s| s["values"].as_array())
            .map_or(0.0, |v| v.iter().filter_map(Value::as_f64).fold(0.0, f64::max))
    };
    for k in ["rtt_ms", "jitter_ms", "loss_pct", "send_kbps", "recv_kbps", "jitter_buffer_ms", "opus_bitrate_kbps"] {
        ensure!(p[k].is_f64(), "{k} missing");
        ensure!(peak(k) > 0.0, "{k}: never > 0 in {}s under 20 ms ± 5 ms / 2 % loss", 8);
    }
    ensure!(num(p, "rtt_ms") >= 30.0, "rtt {} below the 2×20 ms netem delay", p["rtt_ms"]);
    ensure!(p["opus_frame_ms"] == 10 || p["opus_frame_ms"] == 20, "opus_frame_ms {}", p["opus_frame_ms"]);
    ensure!(p["concealment_events"].as_u64().unwrap_or(0) > 0, "no concealment events under 2 % loss");
    ensure!(p["pair_switches"].is_u64(), "pair_switches missing");
    ensure!(p["fec_enabled"] == true && p["dtx_enabled"] == false);

    // Global (§7).
    ensure!(num(g, "uplink_kbps") > 0.0 && num(g, "downlink_kbps") > 0.0, "uplink/downlink totals");
    ensure!(num(g, "media_thread_cpu_pct") > 0.0, "media thread CPU {}", g["media_thread_cpu_pct"]);
    ensure!(g["signaling_transport"] == "Lan", "signaling transport {}", g["signaling_transport"]);
    ensure!(num(g, "signaling_latency_ms") > 0.0, "signaling latency {}", g["signaling_latency_ms"]);
    ensure!(g["turn_bytes_month"].is_u64(), "turn_bytes_month missing");
    let apm = json!({"aec": true, "noise_suppression": true, "ns_level": "Moderate", "high_pass": true, "agc": true});
    ensure!(g["apm"] == apm, "APM state {} != §3 defaults", g["apm"]);

    // 60 s sparklines per metric.
    let mut want: Vec<String> = [
        "rtt_ms",
        "jitter_ms",
        "loss_pct",
        "send_kbps",
        "recv_kbps",
        "opus_bitrate_kbps",
        "opus_frame_ms",
        "jitter_buffer_ms",
        "concealment_events",
        "pair_switches",
    ]
    .iter()
    .map(|m| format!("{m}:{tag}"))
    .collect();
    want.extend(["uplink_kbps", "downlink_kbps", "media_thread_cpu_pct", "signaling_latency_ms"].map(String::from));
    for m in &want {
        let s = series.iter().find(|s| s["metric"] == m.as_str());
        let n = s.and_then(|s| s["values"].as_array()).map_or(0, Vec::len);
        ensure!((5..=60).contains(&n), "sparkline {m}: {n} samples");
    }
    eprintln!("{} sparklines, all §7 ones with 5..=60 samples", series.len());

    // Export: JSON text with the series, also written to the download folder.
    let text = a.call("export_diagnostics", Value::Null).await?;
    let report: Value = serde_json::from_str(text.as_str().unwrap_or_default())?;
    ensure!(report["diagnostics"]["series"].as_array().is_some_and(|s| !s.is_empty()), "export lacks series");
    ensure!(report["log"].as_array().is_some_and(|l| !l.is_empty()), "export lacks the log ring");
    let dir = a.data_dir.join("downloads");
    let files: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("callxl-diagnostics-"))
        .collect();
    ensure!(files.len() == 1, "export not written to {}", dir.display());
    let on_disk: Value = serde_json::from_slice(&std::fs::read(files[0].path())?)?;
    ensure!(on_disk["diagnostics"]["series"] == report["diagnostics"]["series"], "file differs from returned text");
    eprintln!(
        "export: {} bytes, {} log lines, written to {}",
        text.as_str().map_or(0, str::len),
        report["log"].as_array().map_or(0, Vec::len),
        files[0].path().display()
    );
    netem.clear()?;
    a.stop().await?;
    b.stop().await?;
    Ok(())
}
