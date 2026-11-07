//! Identity, trust, signed/encrypted signaling, discovery and glare (CLAUDE.md §4, §5).

use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use callxl_e2e::mitm::{Mitm, Tamper};
use callxl_e2e::*;
use serde_json::{Value, json};

const CONNECT: Duration = Duration::from_secs(20);

fn rejected(d: &Value) -> u64 {
    d["global"]["rejected_signaling"].as_u64().unwrap_or(0)
}

/// Peer B discovers peer A's session over real mDNS on loopback (no addresses given),
/// shows it with the LAN glyph within 3 s, and the call then connects over LAN signaling.
#[tokio::test(flavor = "multi_thread")]
async fn mdns_discovery_loopback() -> Result<()> {
    let h = Harness::new("mdns_discovery_loopback")?;
    let a = Peer::spawn(&h, PeerOpts { mdns: true, ..PeerOpts::new("alice").bind("127.0.0.1").wav(0) }).await?;
    let b = Peer::spawn(&h, PeerOpts { mdns: true, ..PeerOpts::new("bob").bind("127.0.0.1").wav(1) }).await?;
    let t0 = Instant::now();
    let sid = a.call("create_session", json!({"name": "mdns"})).await?.as_str().unwrap_or_default().to_string();
    let seen = loop {
        let s = b.snapshot().await?;
        if let Some(x) = s["sessions"].as_array().and_then(|v| v.iter().find(|x| x["id"] == sid.as_str())) {
            break x.clone();
        }
        ensure!(t0.elapsed() < Duration::from_secs(10), "bob never saw alice's session");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let dt = t0.elapsed();
    eprintln!("bob saw session {sid} after {:.0} ms: {seen}", dt.as_secs_f64() * 1000.0);
    ensure!(dt <= Duration::from_secs(3), "discovery took {dt:?} > 3 s");
    ensure!(seen["lan"] == true, "session not flagged as LAN: {seen}");
    ensure!(seen["cloud"] == false, "cloud is off, yet flagged cloud: {seen}");
    b.call("join_session", json!({"id": sid})).await?;
    a.wait_connected(CONNECT).await?;
    b.wait_connected(CONNECT).await?;
    let sb = b.snapshot().await?;
    eprintln!("bob's peer: {}", sb["peers"][0]);
    ensure!(sb["peers"][0]["lan"] == true, "signaling with alice is not over the LAN");
    let deadline = Instant::now() + Duration::from_secs(3);
    while b.diagnostics().await?["global"]["signaling_transport"] != "Lan" {
        ensure!(Instant::now() < deadline, "diagnostics do not report LAN signaling");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    a.stop().await?;
    b.stop().await?;
    Ok(())
}

/// First contact shows the same 6-word phrase on both sides; after a trust, a device
/// with the same name and a new key (reinstall) prompts once with `key_changed`.
#[tokio::test(flavor = "multi_thread")]
async fn trust_flow_and_key_change() -> Result<()> {
    let h = Harness::new("trust_flow_and_key_change")?;
    let b = Peer::spawn(&h, PeerOpts::new("bob").bind("127.0.0.1").wav(1)).await?;
    let blan = b.lan.clone().expect("lan");
    let a = Peer::spawn(&h, PeerOpts::new("alice").bind("127.0.0.1").wav(0).lan_peer(&blan)).await?;
    let pa = a.wait_event("TrustPrompt", CONNECT, |v| v["peer_key"] == b.key.as_str()).await?;
    let pb = b.wait_event("TrustPrompt", CONNECT, |v| v["peer_key"] == a.key.as_str()).await?;
    eprintln!("alice sees: {pa}\nbob sees:   {pb}");
    let words: Vec<&str> = pa["phrase"].as_str().unwrap_or_default().split(' ').collect();
    ensure!(words.len() == 6 && words.iter().all(|w| !w.is_empty()), "phrase is not 6 words: {words:?}");
    ensure!(pa["phrase"] == pb["phrase"], "phrases differ");
    ensure!(pa["key_changed"] == false && pb["key_changed"] == false);
    ensure!(pb["display_name"] == "alice");

    // One tap: bob trusts alice. Then alice "reinstalls" (same name, new keys).
    b.call("trust_device", json!({"signing_key": a.key})).await?;
    let pinned = b
        .wait_event("TrustedDevicesChanged", CONNECT, |v| {
            v["devices"].as_array().is_some_and(|t| t.iter().any(|d| d["key"] == a.key.as_str()))
        })
        .await?;
    eprintln!("bob pinned alice: {}", pinned["devices"]);
    let old_key = a.key.clone();
    let data = a.data_dir.clone();
    a.stop().await?;
    let a2 = Peer::spawn(
        &h,
        PeerOpts {
            reset_identity: true,
            data_dir: Some(data),
            ..PeerOpts::new("alice").bind("127.0.0.1").wav(0).lan_peer(&blan)
        },
    )
    .await?;
    ensure!(a2.key != old_key, "reset identity kept the old key");
    let pc = b.wait_event("TrustPrompt", CONNECT, |v| v["peer_key"] == a2.key.as_str()).await?;
    eprintln!("after alice's reinstall bob sees: {pc}");
    ensure!(pc["key_changed"] == true, "key change not detected: {pc}");
    ensure!(pc["phrase"] != pa["phrase"], "phrase did not change with the key");
    // Asked once only.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let n = b.events("TrustPrompt").iter().filter(|v| v["peer_key"] == a2.key.as_str()).count();
    ensure!(n == 1, "prompted {n} times for the new key");
    b.call("trust_device", json!({"signing_key": a2.key})).await?;
    let t = b
        .wait_event("TrustedDevicesChanged", CONNECT, |v| {
            v["devices"].as_array().is_some_and(|t| t.iter().any(|d| d["key"] == a2.key.as_str()))
        })
        .await?["devices"]
        .clone();
    eprintln!("bob's trusted devices after re-trust: {t}");
    ensure!(t.as_array().is_some_and(|t| t.len() == 1 && t[0]["key"] == a2.key.as_str()), "old key still pinned");
    a2.stop().await?;
    b.stop().await?;
    Ok(())
}

async fn tamper_case(h: &Harness, mode: Tamper) -> Result<()> {
    let a = Peer::spawn(h, PeerOpts::new(&format!("alice-{mode:?}")).bind("127.0.0.1").wav(0)).await?;
    let mitm = Mitm::start(a.lan.clone().expect("lan").parse()?, mode).await?;
    let b = Peer::spawn(
        h,
        PeerOpts::new(&format!("bob-{mode:?}")).bind("127.0.0.1").wav(1).lan_peer(&mitm.addr.to_string()),
    )
    .await?;
    let sid = a.call("create_session", json!({"name": "tamper"})).await?;
    let deadline = Instant::now() + CONNECT;
    while !b.snapshot().await?["sessions"].as_array().is_some_and(|v| v.iter().any(|x| x["id"] == sid)) {
        ensure!(Instant::now() < deadline, "bob never saw the session");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    b.call("join_session", json!({"id": sid})).await?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    let (da, db) = (a.diagnostics().await?, b.diagnostics().await?);
    let (sa, sb) = (a.snapshot().await?, b.snapshot().await?);
    let errors: Vec<Value> = a.events("Error").into_iter().chain(b.events("Error")).collect();
    eprintln!(
        "{mode:?}: offers/answers relayed {}, tampered {}, rejected alice {} bob {}; errors {errors:?}",
        mitm.sdp_frames(),
        mitm.tampered(),
        rejected(&da),
        rejected(&db)
    );
    let connected = |s: &Value| s["peers"].as_array().is_some_and(|p| p.iter().any(|x| x["connected"] == true));
    ensure!(mitm.tampered() > 0, "{mode:?}: nothing was tampered");
    ensure!(rejected(&da) + rejected(&db) >= mitm.tampered(), "{mode:?}: not every tampered message was rejected");
    ensure!(!connected(&sa) && !connected(&sb), "{mode:?}: the call was established despite tampering");
    ensure!(da["peers"].as_array().is_none_or(|p| p.is_empty()) || da["peers"][0]["send_kbps"].as_f64() == Some(0.0));
    ensure!(!errors.is_empty(), "{mode:?}: no plain-English error surfaced");
    a.kill().await;
    b.kill().await;
    Ok(())
}

/// A man in the middle corrupts the signed fingerprint field or the ciphertext of every
/// offer/answer: each is rejected (counter), an error is surfaced, no call is set up.
/// A third case is a validly signed message whose fingerprint field lies about the SDP
/// (a malicious peer): rejected by the fingerprint check before libwebrtc sees the SDP.
#[tokio::test(flavor = "multi_thread")]
async fn tampered_sdp_rejected() -> Result<()> {
    let h = Harness::new("tampered_sdp_rejected")?;
    tamper_case(&h, Tamper::Fingerprint).await?;
    tamper_case(&h, Tamper::Ciphertext).await?;

    // Control: the same path through the MITM untampered connects.
    let a = Peer::spawn(&h, PeerOpts::new("alice-ok").bind("127.0.0.1").wav(0)).await?;
    let mitm = Mitm::start(a.lan.clone().expect("lan").parse()?, Tamper::None).await?;
    let b = Peer::spawn(&h, PeerOpts::new("bob-ok").bind("127.0.0.1").wav(1).lan_peer(&mitm.addr.to_string())).await?;
    call_all(&[&a, &b], CONNECT).await?;
    eprintln!("control: {} offers/answers relayed untouched, call connected", mitm.sdp_frames());
    a.kill().await;
    b.kill().await;

    // Signed but lying fingerprint.
    let a = Peer::spawn(&h, PeerOpts::new("alice-forge").bind("127.0.0.1").wav(0)).await?;
    let b =
        Peer::spawn(&h, PeerOpts::new("bob-forge").bind("127.0.0.1").wav(1).lan_peer(&a.lan.clone().unwrap())).await?;
    a.call("debug", json!({"name": "forge_fingerprint", "args": {"on": true}})).await?;
    b.call("debug", json!({"name": "forge_fingerprint", "args": {"on": true}})).await?;
    let sid = a.call("create_session", json!({"name": "forge"})).await?;
    let deadline = Instant::now() + CONNECT;
    while !b.snapshot().await?["sessions"].as_array().is_some_and(|v| v.iter().any(|x| x["id"] == sid)) {
        ensure!(Instant::now() < deadline, "bob never saw the session");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    b.call("join_session", json!({"id": sid})).await?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    let (ra, rb) = (rejected(&a.diagnostics().await?), rejected(&b.diagnostics().await?));
    let errs: Vec<String> = a
        .events("Error")
        .into_iter()
        .chain(b.events("Error"))
        .map(|e| e["reason"].as_str().unwrap_or_default().to_string())
        .collect();
    eprintln!("forged fingerprint: rejected alice {ra} bob {rb}; errors {errs:?}");
    ensure!(ra + rb > 0, "forged fingerprint accepted");
    ensure!(errs.iter().any(|e| e.contains("fingerprint")), "no fingerprint error surfaced");
    ensure!(b.snapshot().await?["peers"].as_array().is_none_or(|p| p.iter().all(|x| x["connected"] != true)));
    a.kill().await;
    b.kill().await;
    Ok(())
}

/// Both peers renegotiate at the same instant (offer glare): perfect negotiation resolves
/// it (polite peer rolls back), the call stays up on one PeerConnection, audio unbroken.
#[tokio::test(flavor = "multi_thread")]
async fn signaling_glare() -> Result<()> {
    let h = Harness::new("signaling_glare")?;
    let a = Peer::spawn(&h, PeerOpts::new("alice").bind("127.0.0.1").wav(0)).await?;
    let b = Peer::spawn(&h, PeerOpts::new("bob").bind("127.0.0.1").wav(1).lan_peer(&a.lan.clone().unwrap())).await?;
    aec_off(&a).await?;
    aec_off(&b).await?;
    call_all(&[&a, &b], CONNECT).await?;
    let g0 = a.diagnostics().await?["global"]["glare_events"].as_u64().unwrap_or(0)
        + b.diagnostics().await?["global"]["glare_events"].as_u64().unwrap_or(0);
    tokio::time::sleep(Duration::from_secs(2)).await;
    for round in 0..3 {
        let (ra, rb) = tokio::join!(a.debug("renegotiate"), b.debug("renegotiate"));
        ra?;
        rb?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let la = a.debug("link_info").await?;
        let lb = b.debug("link_info").await?;
        eprintln!(
            "round {round}: alice {} polite={} offers={}, bob {} polite={} offers={}",
            la[0]["signaling_state"],
            la[0]["polite"],
            la[0]["offers_sent"],
            lb[0]["signaling_state"],
            lb[0]["polite"],
            lb[0]["offers_sent"]
        );
        ensure!(la.as_array().map(Vec::len) == Some(1) && lb.as_array().map(Vec::len) == Some(1), "more than one link");
        ensure!(la[0]["polite"] != lb[0]["polite"], "both peers have the same politeness");
    }
    let (da, db) = (a.diagnostics().await?, b.diagnostics().await?);
    let glare =
        da["global"]["glare_events"].as_u64().unwrap_or(0) + db["global"]["glare_events"].as_u64().unwrap_or(0) - g0;
    let la = a.debug("link_info").await?;
    let lb = b.debug("link_info").await?;
    eprintln!(
        "glare events during the test: {glare}; final states {} / {}",
        la[0]["signaling_state"], lb[0]["signaling_state"]
    );
    ensure!(glare > 0, "no glare happened: the test did not exercise perfect negotiation");
    ensure!(la[0]["signaling_state"] == 0 && lb[0]["signaling_state"] == 0, "not stable (0 = stable)");
    a.wait_connected(CONNECT).await?;
    b.wait_connected(CONNECT).await?;
    ensure!(da["peers"][0]["recv_kbps"].as_f64().unwrap_or(0.0) > 100.0, "no audio after glare: {}", da["peers"][0]);
    let ra = a.stop().await?;
    let rb = b.stop().await?;
    assert_continuous_tail(&ra, 6.0)?;
    assert_continuous_tail(&rb, 6.0)?;
    Ok(())
}

/// Cloud signaling through the Worker run locally (`wrangler dev --local`): LAN off on
/// both peers, the session list arrives from the Lobby DO, offers/answers/candidates are
/// relayed by the Session DO, media still flows peer-to-peer on loopback; the session is
/// deleted when its last member leaves.
#[tokio::test(flavor = "multi_thread")]
async fn signaling_roundtrip_wrangler() -> Result<()> {
    let h = Harness::new("signaling_roundtrip_wrangler")?;
    // The CLI's build-time APP_SECRET (a random dev value unless CALLXL_APP_SECRET was set).
    let probe = Peer::spawn(&h, PeerOpts::new("probe").bind("127.0.0.1")).await?;
    let secret = probe.call("debug", json!({"name": "app_secret"})).await?.as_str().unwrap_or_default().to_string();
    probe.kill().await;
    let port = std::net::TcpListener::bind("127.0.0.1:8787").map(|_| 8787).unwrap_or(0);
    let port = if port == 0 { std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port() } else { port };
    let t = Instant::now();
    let worker = callxl_e2e::worker::LocalWorker::start(port, &secret, h.dir.join("wrangler.log")).await?;
    eprintln!("wrangler dev up on {} after {:.1} s", worker.url(), t.elapsed().as_secs_f64());
    let cloud = |n: &str, w: usize| PeerOpts {
        cloud_url: Some(worker.url()),
        lan_off: true,
        ..PeerOpts::new(n).bind("127.0.0.1").wav(w)
    };
    let a = Peer::spawn(&h, cloud("alice", 0)).await?;
    let b = Peer::spawn(&h, cloud("bob", 1)).await?;
    for p in [&a, &b] {
        let deadline = Instant::now() + CONNECT;
        while p.snapshot().await?["cloud_connected"] != true {
            ensure!(Instant::now() < deadline, "{} never connected to the lobby", p.name);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    aec_off(&a).await?;
    aec_off(&b).await?;
    let t0 = Instant::now();
    let sid = call_all(&[&a, &b], CONNECT).await?;
    eprintln!("call over cloud signaling connected {:.2} s after create", t0.elapsed().as_secs_f64());
    let seen = b.snapshot().await?["sessions"]
        .as_array()
        .and_then(|v| v.iter().find(|x| x["id"] == sid.as_str()).cloned())
        .unwrap_or_default();
    eprintln!("bob's view of the session: {seen}");
    ensure!(seen["cloud"] == true && seen["lan"] == false, "session not seen via the cloud only: {seen}");
    let pa = a.wait_stats(Duration::from_secs(10), |p| p["recv_kbps"].as_f64().unwrap_or(0.0) > 100.0).await?;
    let d = a.diagnostics().await?;
    eprintln!(
        "alice: transport={} signaling_latency={} ms path={} recv={} kbps",
        d["global"]["signaling_transport"], d["global"]["signaling_latency_ms"], pa["path"], pa["recv_kbps"]
    );
    h.save_json("alice-diagnostics.json", &d);
    ensure!(d["global"]["signaling_transport"] == "Cloud", "signaling not over the cloud");
    ensure!(pa["path"]["kind"] == "Lan", "media should stay peer-to-peer on loopback: {}", pa["path"]);
    ensure!(a.snapshot().await?["peers"][0]["lan"] == false, "peer marked as LAN-signaled with LAN off");

    tokio::time::sleep(Duration::from_secs(4)).await;

    // Session lifecycle: bob leaves (alice stays), then alice leaves → session deleted.
    b.call("leave_session", Value::Null).await?;
    a.wait_event("PeerLeft", CONNECT, |v| v["peer_key"] == b.key.as_str()).await?;
    a.call("leave_session", Value::Null).await?;
    let deadline = Instant::now() + CONNECT;
    loop {
        let s = b.snapshot().await?["sessions"].clone();
        if !s.as_array().is_some_and(|v| v.iter().any(|x| x["id"] == sid.as_str())) {
            eprintln!("session {sid} gone from the lobby after its last member left: {s}");
            break;
        }
        ensure!(Instant::now() < deadline, "session still listed after everyone left: {s}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let ra = a.stop().await?;
    let rb = b.stop().await?;
    drop(worker);
    assert_continuous(&ra)?;
    assert_continuous(&rb)?;
    Ok(())
}
