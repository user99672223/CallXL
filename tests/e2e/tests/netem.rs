//! Internet-path policy under impairment (CLAUDE.md §3/§4, D-0003): BWE ramp, back-off and
//! recovery, 10↔20 ms frame adaptation, IPv6 preference and dynamic path switching.
//! Every scenario also asserts audio continuity (§1.1). Backend: `CALLXL_NETEM=tc|proxy`.
//!
//! Bitrates are asserted on `opus_bitrate_kbps`, the encoder target driven by BWE: libopus
//! saturates mono at ~261 kbps of payload whatever the target (DECISIONS.md), so on-wire
//! bytes cannot show a 450 kbps level. `send_kbps` is logged next to it.

use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use callxl_e2e::netem::{Family, Impairment, Netem};
use callxl_e2e::*;
use serde_json::{Value, json};

const CONNECT: Duration = Duration::from_secs(30);
/// libwebrtc's selected-pair switches in the peer logs (artifacts) for the path tests.
const ICE_LOG: &[(&str, &str)] = &[
    ("CALLXL_RTC_LOG", "Switching selected connection|Changed selected connection|CallXL ICE"),
    ("CALLXL_ICE_TRACE", "1"),
];

async fn decisions(a: &Peer, b: &Peer) {
    for p in [a, b] {
        if let Ok(l) = p.debug("link_info").await {
            eprintln!("{} controller decisions: {}", p.name, l[0]["controller_decisions"]);
        }
    }
}

fn f(p: &Value, k: &str) -> f64 {
    p[k].as_f64().unwrap_or(0.0)
}

/// Two peers on `addrs`, internet policy forced, impairment backend wired in, AEC off.
async fn internet_pair(h: &Harness, netem: &Netem, addrs: &[&str], internet: bool) -> Result<(Peer, Peer)> {
    internet_pair_env(h, netem, addrs, internet, &[]).await
}

async fn internet_pair_env(
    h: &Harness,
    netem: &Netem,
    addrs: &[&str],
    internet: bool,
    env: &[(&str, &str)],
) -> Result<(Peer, Peer)> {
    let mut extra = netem.cli_args();
    if internet {
        extra.extend(["--debug-force-path".to_string(), "internet".to_string()]);
    }
    let mut oa = PeerOpts::new("alice").wav(0).extra(extra.clone());
    for (k, v) in env {
        oa = oa.env(k, v);
    }
    for a in addrs {
        oa = oa.bind(a);
    }
    let a = Peer::spawn(h, oa).await?;
    let mut ob = PeerOpts::new("bob").wav(1).extra(extra).lan_peer(&a.lan.clone().expect("lan"));
    for (k, v) in env {
        ob = ob.env(k, v);
    }
    for x in addrs {
        ob = ob.bind(x);
    }
    let b = Peer::spawn(h, ob).await?;
    aec_off(&a).await?;
    aec_off(&b).await?;
    Ok((a, b))
}

async fn finish(h: &Harness, a: Peer, b: Peer, samples: &[Value], tail: f64) -> Result<()> {
    h.save_json("alice-samples.json", &json!(samples));
    h.save_json("alice-diagnostics.json", &a.diagnostics().await?);
    h.save_json("bob-diagnostics.json", &b.diagnostics().await?);
    h.save_export(&a).await;
    let ra = a.stop().await?;
    let rb = b.stop().await?;
    assert_continuous_tail(&rb, tail)?;
    assert_continuous_tail(&ra, tail)?;
    Ok(())
}

/// 40 ms delay, no loss: the internet path starts at 128 kbps (send ≤ 160 kbps in the
/// first second of media) and BWE takes the Opus target to ≥ 450 kbps within 20 s.
/// Stats are sampled at 4 Hz so the first second of media is actually observed.
#[tokio::test(flavor = "multi_thread")]
async fn netem_internet_ramp() -> Result<()> {
    let h = Harness::new("netem_internet_ramp")?;
    let netem = Netem::new().await?;
    netem.set(Family::All, Impairment { delay_ms: 40, ..Impairment::CLEAN })?;
    let (a, b) = internet_pair_env(&h, &netem, &["127.0.0.1"], true, &[("CALLXL_STATS_INTERVAL_MS", "250")]).await?;
    let sid = a.call("create_session", json!({"name": "ramp"})).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    b.call("join_session", json!({"id": sid})).await?;
    let t0 = Instant::now();
    let s = sample_every(&a, t0, 21.0, Duration::from_millis(250)).await?;
    h.save_json("alice-samples.json", &json!(s));
    // Media starts at the first sample carrying a BWE estimate.
    let m0 = s.iter().find(|p| f(p, "available_outgoing_kbps") > 0.0).map(|p| f(p, "t"));
    ensure!(m0.is_some(), "media never started");
    let m0 = m0.unwrap();
    let first_sec: Vec<&Value> = s.iter().filter(|p| f(p, "t") >= m0 && f(p, "t") < m0 + 1.0).collect();
    let first = first_sec[0].clone();
    let max_send = first_sec.iter().map(|p| f(p, "send_kbps")).fold(0.0, f64::max);
    let reached = s.iter().find(|p| f(p, "opus_bitrate_kbps") >= 450.0).map(|p| f(p, "t") - m0);
    eprintln!(
        "media start t={m0:.2}: {}  max send in first second {max_send:.1} kbps; reached 450 kbps {reached:?} s after media start",
        brief(&first)
    );
    ensure!(first["path"]["kind"] == "Direct", "internet policy not in force: {}", first["path"]);
    ensure!(f(&first, "available_outgoing_kbps") <= 160.0, "BWE did not start at 128 kbps: {}", brief(&first));
    ensure!(max_send <= 160.0, "first second of media sent {max_send} kbps > 160");
    ensure!(reached.is_some_and(|t| t <= 20.0), "never reached 450 kbps within 20 s");
    netem.clear()?;
    finish(&h, a, b, &s, 8.0).await
}

/// 5 % loss for 15 s, then clean: the target drops ≥ 30 % within 10 s of loss onset and
/// recovers to ≥ 450 kbps within 30 s after the loss clears.
#[tokio::test(flavor = "multi_thread")]
async fn netem_loss_backoff_recovery() -> Result<()> {
    let h = Harness::new("netem_loss_backoff_recovery")?;
    let netem = Netem::new().await?;
    netem.set(Family::All, Impairment { delay_ms: 20, ..Impairment::CLEAN })?;
    let (a, b) = internet_pair(&h, &netem, &["127.0.0.1"], true).await?;
    call_all(&[&a, &b], CONNECT).await?;
    let t0 = Instant::now();
    let mut s = sample(&a, t0, 1.0).await?;
    let warm = a.wait_stats(Duration::from_secs(25), |p| f(p, "opus_bitrate_kbps") >= 450.0).await?;
    let before = f(&warm, "opus_bitrate_kbps");
    let loss_on = (Instant::now() - t0).as_secs_f64();
    netem.set(Family::All, Impairment { delay_ms: 20, loss_pct: 5.0, ..Impairment::CLEAN })?;
    s.extend(sample(&a, t0, 15.0).await?);
    let loss_off = (Instant::now() - t0).as_secs_f64();
    netem.set(Family::All, Impairment { delay_ms: 20, ..Impairment::CLEAN })?;
    s.extend(sample(&a, t0, 31.0).await?);
    h.save_json("alice-samples.json", &json!(s));
    let dropped = s
        .iter()
        .filter(|p| f(p, "t") > loss_on && f(p, "t") <= loss_on + 10.0)
        .map(|p| f(p, "opus_bitrate_kbps"))
        .fold(f64::INFINITY, f64::min);
    let recovered =
        s.iter().find(|p| f(p, "t") > loss_off && f(p, "opus_bitrate_kbps") >= 450.0).map(|p| f(p, "t") - loss_off);
    eprintln!("before {before} kbps, min within 10 s of loss {dropped} kbps, recovered after {recovered:?} s");
    ensure!(dropped <= before * 0.7, "target only dropped {before} -> {dropped} kbps under 5 % loss");
    ensure!(recovered.is_some_and(|t| t <= 30.0), "did not recover to 450 kbps within 30 s");
    finish(&h, a, b, &s, 10.0).await
}

/// 8 % loss for 20 s then clean: Opus frames go 10 → 20 → 10 ms, done by libwebrtc's
/// audio network adaptor (recorded in diagnostics). The way back is bounded by §3's rule
/// itself plus the loss feedback path: the receiver's audio RTCP report interval (5 s in
/// libwebrtc, not configurable for receive streams) + the 5 s loss average + 15 s below
/// 1 %, i.e. ≤ 25 s after the loss clears (measured 24 s); the clean phase is 32 s.
#[tokio::test(flavor = "multi_thread")]
async fn netem_frame_adaptation() -> Result<()> {
    let h = Harness::new("netem_frame_adaptation")?;
    let netem = Netem::new().await?;
    netem.set(Family::All, Impairment { delay_ms: 20, ..Impairment::CLEAN })?;
    let (a, b) = internet_pair(&h, &netem, &["127.0.0.1"], true).await?;
    call_all(&[&a, &b], CONNECT).await?;
    let t0 = Instant::now();
    let mut s = sample(&a, t0, 3.0).await?;
    ensure!(s.last().is_some_and(|p| p["opus_frame_ms"] == 10), "did not start at 10 ms");
    let loss_on = (Instant::now() - t0).as_secs_f64();
    netem.set(Family::All, Impairment { delay_ms: 20, loss_pct: 8.0, ..Impairment::CLEAN })?;
    s.extend(sample(&a, t0, 20.0).await?);
    let loss_off = (Instant::now() - t0).as_secs_f64();
    netem.set(Family::All, Impairment { delay_ms: 20, ..Impairment::CLEAN })?;
    s.extend(sample(&a, t0, 32.0).await?);
    h.save_json("alice-samples.json", &json!(s));
    let up = s.iter().find(|p| f(p, "t") > loss_on && p["opus_frame_ms"] == 20).map(|p| f(p, "t"));
    let down = s.iter().find(|p| f(p, "t") > loss_off && up.is_some() && p["opus_frame_ms"] == 10).map(|p| f(p, "t"));
    let d = a.diagnostics().await?;
    eprintln!(
        "10->20 at {up:?} s (loss on {loss_on:.1}), 20->10 at {down:?} s (loss off {loss_off:.1}); mechanism: {}",
        d["global"]["frame_adaptation_mechanism"]
    );
    ensure!(up.is_some(), "never switched to 20 ms under 8 % loss");
    ensure!(down.is_some(), "never returned to 10 ms after the loss cleared");
    ensure!(d["global"]["ana_switches"].as_u64().unwrap_or(0) >= 2, "ANA did not record the switches");
    let series: Vec<String> =
        d["series"].as_array().unwrap().iter().map(|x| x["metric"].as_str().unwrap_or("").to_string()).collect();
    ensure!(series.iter().any(|m| m == "ana_frame_ms"), "frame mechanism series missing");
    ensure!(d["global"]["frame_adaptation_mechanism"].as_str().unwrap_or("").contains("audio network adaptor"));
    finish(&h, a, b, &s, 10.0).await
}

/// Dual-stack call starts on IPv6 (preferred); 60 ms added to IPv6 only → within 10 s
/// the controller switches to IPv4 (exactly one switch) with no audible gap.
#[tokio::test(flavor = "multi_thread")]
async fn netem_path_switch_v6_to_v4() -> Result<()> {
    if !ipv6_or_skip("netem_path_switch_v6_to_v4") {
        return Ok(());
    }
    let h = Harness::new("netem_path_switch_v6_to_v4")?;
    let netem = Netem::new().await?;
    netem.clear()?;
    let (a, b) = internet_pair_env(&h, &netem, &["::1", "127.0.0.1"], false, ICE_LOG).await?;
    call_all(&[&a, &b], CONNECT).await?;
    let t0 = Instant::now();
    let mut s = sample(&a, t0, 4.0).await?;
    let start = s.last().cloned().unwrap_or_default();
    ensure!(start["path"]["local_family"] == "V6", "call did not start on IPv6: {}", start["path"]);
    let degrade = (Instant::now() - t0).as_secs_f64();
    netem.set(Family::V6, Impairment { delay_ms: 60, ..Impairment::CLEAN })?;
    s.extend(sample(&a, t0, 12.0).await?);
    h.save_json("alice-samples.json", &json!(s));
    let switched =
        s.iter().find(|p| f(p, "t") > degrade && p["path"]["local_family"] == "V4").map(|p| f(p, "t") - degrade);
    let last = s.last().cloned().unwrap_or_default();
    eprintln!("switched to V4 {switched:?} s after degrading IPv6; switches = {}", last["pair_switches"]);
    decisions(&a, &b).await;
    ensure!(switched.is_some_and(|t| t <= 10.0), "no switch to IPv4 within 10 s");
    ensure!(last["pair_switches"] == 1, "expected exactly one switch, got {}", last["pair_switches"]);
    netem.clear()?;
    finish(&h, a, b, &s, 14.0).await
}

/// Mirror case: IPv6 is degraded from the start (call moves to IPv4), then IPv6 clears;
/// the controller moves back to IPv6 only after the 30 s cooldown.
#[tokio::test(flavor = "multi_thread")]
async fn netem_path_switch_v4_to_v6() -> Result<()> {
    if !ipv6_or_skip("netem_path_switch_v4_to_v6") {
        return Ok(());
    }
    let h = Harness::new("netem_path_switch_v4_to_v6")?;
    let netem = Netem::new().await?;
    netem.set(Family::V6, Impairment { delay_ms: 60, ..Impairment::CLEAN })?;
    let (a, b) = internet_pair_env(&h, &netem, &["::1", "127.0.0.1"], false, ICE_LOG).await?;
    call_all(&[&a, &b], CONNECT).await?;
    let t0 = Instant::now();
    let mut s = sample(&a, t0, 12.0).await?;
    // With IPv6 degraded from the start the call is on IPv4 after 12 s either way:
    // libwebrtc may pick IPv4 during the initial grace period (its checks answer first;
    // no switch needed, pair_switches 0) or start on IPv6 and the CallXL policy moves
    // it (pair_switches 1). Both are correct; the mirror move IPv4 -> IPv6 is the test.
    let on_v4 = s.last().cloned().unwrap_or_default();
    if on_v4["path"]["local_family"] != "V4" {
        decisions(&a, &b).await;
    }
    ensure!(on_v4["path"]["local_family"] == "V4", "still on the degraded IPv6 path: {}", on_v4["path"]);
    let n0 = on_v4["pair_switches"].as_u64().unwrap_or(0);
    ensure!(n0 <= 1, "{n0} switches while IPv6 was degraded");
    let first = s.iter().find(|p| p["pair_switches"] == 1).map(|p| f(p, "t"));
    netem.set(Family::V6, Impairment::CLEAN)?;
    let cleared = (Instant::now() - t0).as_secs_f64();
    s.extend(sample(&a, t0, 45.0).await?);
    h.save_json("alice-samples.json", &json!(s));
    let back = s
        .iter()
        .find(|p| f(p, "t") > cleared && p["path"]["local_family"] == "V6" && p["pair_switches"] == n0 + 1)
        .map(|p| f(p, "t"));
    let last = s.last().cloned().unwrap_or_default();
    eprintln!(
        "start: {} switch(es) to reach IPv4 (first at {first:?} s); IPv6 cleared at {cleared:.1} s; back on IPv6 at {back:?} s; final {} switches {}",
        n0, last["path"], last["pair_switches"]
    );
    decisions(&a, &b).await;
    ensure!(back.is_some(), "never returned to IPv6");
    let back = back.unwrap();
    if let Some(first) = first {
        ensure!(
            back - first >= 30.0,
            "returned {:.1} s after the first switch, inside the 30 s cooldown",
            back - first
        );
    } else {
        // No earlier switch: libwebrtc's RTT smoothing (3/4 old + 1/4 new, ~1 backup
        // sample/s: 120 ms falls under the 5 ms IPv6 bonus after ~11 s) + the 5 s sustain.
        ensure!(back - cleared <= 25.0, "took {:.1} s to return to the clean IPv6 path", back - cleared);
    }
    ensure!(last["pair_switches"] == n0 + 1, "flapping: {} switches", last["pair_switches"]);
    ensure!(last["path"]["local_family"] == "V6", "not back on IPv6: {}", last["path"]);
    netem.clear()?;
    finish(&h, a, b, &s, 20.0).await
}
