//! Two peers over loopback with direct LAN TCP signaling: the §3 LAN audio profile,
//! proven from libwebrtc `GetStats` and the far-end recordings.

use std::time::{Duration, Instant};

use anyhow::Result;
use callxl_e2e::*;
use serde_json::{Value, json};

const CONNECT: Duration = Duration::from_secs(20);

async fn lan_pair(h: &Harness, addr: &str) -> Result<(Peer, Peer)> {
    let a = Peer::spawn(h, PeerOpts::new("alice").bind(addr).wav(0)).await?;
    let lan = a.lan.clone().expect("lan addr");
    let b = Peer::spawn(h, PeerOpts::new("bob").bind(addr).wav(1).lan_peer(&lan)).await?;
    Ok((a, b))
}

/// Asserts every §3 LAN-profile number on one side's stats.
fn assert_lan_profile(who: &str, d: &Value, family: &str) {
    let p = &d["peers"][0];
    eprintln!("{who} stats: {}", serde_json::to_string_pretty(p).unwrap());
    assert_eq!(p["codec"], "audio/opus", "{who} codec");
    assert_eq!(p["channels"], 1, "{who} encodes mono");
    assert_eq!(p["clock_rate"], 48000, "{who} clock rate");
    assert_eq!(p["opus_frame_ms"], 10, "{who} 10 ms frames");
    assert_eq!(p["opus_complexity"], 10, "{who} complexity 10");
    assert_eq!(p["fec_enabled"], true, "{who} in-band FEC");
    assert_eq!(p["dtx_enabled"], false, "{who} DTX off");
    let target = p["opus_bitrate_kbps"].as_f64().unwrap();
    assert!(target >= 470.0, "{who} Opus target {target} kbps < 470");
    // §3 "LAN path: 510 kbps immediately": every 1 Hz sample since media started is at
    // the LAN start or above: 510 kbps BWE start − packet overhead = 452.4 kbps Opus on
    // IPv4, 436.4 kbps on IPv6 (20 more header bytes × 100 packets/s).
    let tag = &p["peer_key"].as_str().unwrap()[..8];
    let series: Vec<f64> = d["series"]
        .as_array()
        .and_then(|v| v.iter().find(|s| s["metric"] == format!("opus_bitrate_kbps:{tag}").as_str()))
        .and_then(|s| s["values"].as_array())
        .map(|v| v.iter().filter_map(Value::as_f64).filter(|x| *x > 0.0).collect())
        .unwrap_or_default();
    eprintln!("{who} Opus target since media start: {series:?}");
    assert!(series.len() >= 5, "{who}: too few samples");
    let floor = if family == "V6" { 430.0 } else { 445.0 };
    assert!(series.iter().all(|x| *x >= floor), "{who}: LAN call did not start at the LAN rate: {series:?}");
    assert_eq!(p["path"]["kind"], "Lan", "{who} path kind");
    assert_eq!(p["path"]["local_family"], family, "{who} family");
    assert_eq!(p["path"]["local_candidate_type"], "host");
    assert_eq!(p["path"]["remote_candidate_type"], "host");
    let apm = &d["global"]["apm"];
    assert_eq!(
        apm,
        &json!({"aec": true, "noise_suppression": true, "ns_level": "Moderate", "high_pass": true, "agc": true}),
        "{who} APM = §3 defaults"
    );
}

async fn lan_policy(name: &str, addr: &str, family: &str) -> Result<()> {
    let h = Harness::new(name)?;
    let (a, b) = lan_pair(&h, addr).await?;
    call_all(&[&a, &b], CONNECT).await?;
    let t0 = Instant::now();
    let samples = sample(&a, t0, 8.0).await?;
    let da = a.diagnostics().await?;
    let db = b.diagnostics().await?;
    h.save_json("alice-samples.json", &json!(samples));
    h.save_json("alice-diagnostics.json", &da);
    h.save_json("bob-diagnostics.json", &db);
    assert_lan_profile("alice", &da, family);
    assert_lan_profile("bob", &db, family);
    // Continuity phase (AEC3 off, see `apm_no_aec`): 7 s more, analyse the last 6 s.
    aec_off(&a).await?;
    aec_off(&b).await?;
    let more = sample(&a, t0, 7.0).await?;
    h.save_json("alice-samples-continuity.json", &json!(more));
    let ra = a.stop().await?;
    let rb = b.stop().await?;
    assert_continuous_tail(&ra, 6.0)?;
    assert_continuous_tail(&rb, 6.0)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_v4_lan_policy() -> Result<()> {
    lan_policy("loopback_v4_lan_policy", "127.0.0.1", "V4").await
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_v6_lan_policy() -> Result<()> {
    if !ipv6_or_skip("loopback_v6_lan_policy") {
        return Ok(());
    }
    lan_policy("loopback_v6_lan_policy", "::1", "V6").await
}

/// §1.1: live changes (bitrate ceiling 510 → 64 → 510, raw-mic preset on and off)
/// never cause a dropout: the far-end recordings stay free of gaps > 20 ms.
#[tokio::test(flavor = "multi_thread")]
async fn audio_continuity_no_gaps() -> Result<()> {
    let h = Harness::new("audio_continuity_no_gaps")?;
    let (a, b) = lan_pair(&h, "127.0.0.1").await?;
    aec_off(&a).await?;
    aec_off(&b).await?;
    call_all(&[&a, &b], CONNECT).await?;
    let t0 = Instant::now();
    let mut all = sample(&a, t0, 4.0).await?;
    a.call("set_bitrate_ceiling_kbps", json!({"kbps": 64})).await?;
    all.extend(sample(&a, t0, 5.0).await?);
    let low = all.last().unwrap()["opus_bitrate_kbps"].as_f64().unwrap();
    a.call("set_bitrate_ceiling_kbps", json!({"kbps": 510})).await?;
    all.extend(sample(&a, t0, 5.0).await?);
    let high = all.last().unwrap()["opus_bitrate_kbps"].as_f64().unwrap();
    a.call("set_raw_mic", json!({"raw": true})).await?;
    all.extend(sample(&a, t0, 3.0).await?);
    let raw = a.diagnostics().await?["global"]["apm"].clone();
    a.call("set_audio_processing", json!({"cfg": apm_no_aec()})).await?;
    all.extend(sample(&a, t0, 3.0).await?);
    h.save_json("alice-samples.json", &json!(all));
    eprintln!("ceiling 64 -> target {low} kbps; back to 510 -> {high} kbps; raw apm {raw}");
    assert!(low <= 70.0, "ceiling 64 kbps not applied: {low}");
    assert!(high >= 470.0, "ceiling 510 kbps not restored: {high}");
    assert_eq!(
        raw,
        json!({"aec": false, "noise_suppression": false, "ns_level": "Moderate", "high_pass": false, "agc": false})
    );
    let ra = a.stop().await?;
    let rb = b.stop().await?;
    // Bob hears alice through all of alice's changes.
    let c = assert_continuous(&rb)?;
    assert!(c.active_secs > 15.0, "{c:?}");
    assert_continuous(&ra)?;
    Ok(())
}
