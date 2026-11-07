//! Chat and file transfer over the PeerConnection's data channels (CLAUDE.md §6).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use callxl_e2e::netem::{Family, Impairment, Netem};
use callxl_e2e::*;
use rand::RngCore;
use serde_json::{Value, json};

const CONNECT: Duration = Duration::from_secs(30);

/// `n` peers on 127.0.0.1, each told the LAN signaling address of every earlier one.
async fn mesh(h: &Harness, names: &[&str], extra: &[String]) -> Result<Vec<Peer>> {
    let mut peers: Vec<Peer> = Vec::new();
    for (i, n) in names.iter().enumerate() {
        let mut o = PeerOpts::new(n).bind("127.0.0.1").wav(i % 2).extra(extra.iter().cloned());
        for p in &peers {
            o = o.lan_peer(p.lan.as_deref().expect("lan"));
        }
        peers.push(Peer::spawn(h, o).await?);
    }
    Ok(peers)
}

fn random_file(path: &Path, bytes: usize) -> Result<String> {
    let mut data = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut data);
    std::fs::write(path, &data)?;
    Ok(blake3::hash(&data).to_hex().to_string())
}

fn transfer_event(p: &Peer, id: &str) -> Vec<Value> {
    p.events("TransferProgress").into_iter().map(|v| v["transfer"].clone()).filter(|t| t["id"] == id).collect()
}

async fn wait_transfer(p: &Peer, id: &str, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Result<Value> {
    let v = p.wait_event("TransferProgress", timeout, |v| v["transfer"]["id"] == id && pred(&v["transfer"])).await?;
    Ok(v["transfer"].clone())
}

/// Three peers: a message reaches both others with the authenticated sender key; each
/// recipient's delivery receipt and one read receipt come back; history survives a restart.
#[tokio::test(flavor = "multi_thread")]
async fn chat_roundtrip() -> Result<()> {
    let h = Harness::new("chat_roundtrip")?;
    let mut peers = mesh(&h, &["alice", "bob", "carol"], &[]).await?;
    let carol = peers.pop().expect("carol");
    let bob = peers.pop().expect("bob");
    let alice = peers.pop().expect("alice");
    let sid = call_all(&[&alice, &bob, &carol], CONNECT).await?;
    let (a, b, c) = (&alice, &bob, &carol);

    let t0 = Instant::now();
    let mid = a.call("send_chat", json!({"body": "hello from alice"})).await?.as_str().unwrap_or_default().to_string();
    for p in [b, c] {
        let m = p.wait_event("ChatReceived", CONNECT, |v| v["message"]["id"] == mid.as_str()).await?["message"].clone();
        eprintln!("{} received after {:.0} ms: {m}", p.name, t0.elapsed().as_secs_f64() * 1000.0);
        ensure!(m["body"] == "hello from alice" && m["sender_key"] == a.key.as_str() && m["own"] == false);
        ensure!(m["session"] == sid.as_str(), "message filed under the wrong session");
    }
    for p in [b, c] {
        let r = a
            .wait_event("ReceiptUpdated", CONNECT, |v| {
                v["message_id"] == mid.as_str() && v["peer_key"] == p.key.as_str() && v["status"] == "Delivered"
            })
            .await?;
        eprintln!("alice: delivered receipt from {}: {r}", p.name);
    }
    b.call("mark_read", json!({"up_to": mid})).await?;
    let r = a
        .wait_event("ReceiptUpdated", CONNECT, |v| {
            v["message_id"] == mid.as_str() && v["peer_key"] == b.key.as_str() && v["status"] == "Read"
        })
        .await?;
    eprintln!("alice: read receipt from bob: {r}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    ensure!(
        !a.events("ReceiptUpdated").iter().any(|v| v["peer_key"] == c.key.as_str() && v["status"] == "Read"),
        "carol never read the message, yet a read receipt arrived"
    );

    // Ordered channel: a burst arrives in order.
    let mut burst = Vec::new();
    for i in 0..20 {
        burst.push(
            b.call("send_chat", json!({"body": format!("bob {i}")})).await?.as_str().unwrap_or_default().to_string(),
        );
    }
    let last = burst.last().cloned().unwrap_or_default();
    c.wait_event("ChatReceived", CONNECT, |v| v["message"]["id"] == last.as_str()).await?;
    let got: Vec<String> = c
        .events("ChatReceived")
        .iter()
        .filter(|v| v["message"]["sender_key"] == b.key.as_str())
        .map(|v| v["message"]["body"].as_str().unwrap_or_default().to_string())
        .collect();
    let want: Vec<String> = (0..20).map(|i| format!("bob {i}")).collect();
    ensure!(got == want, "out of order or missing: {got:?}");

    // SQLite history, persisted across a restart of carol.
    let hist = c.call("chat_history", json!({"session": sid, "limit": 100})).await?;
    let n = hist.as_array().map_or(0, Vec::len);
    ensure!(n == 21, "carol's history has {n} messages, expected 21");
    let data = c.data_dir.clone();
    carol.stop().await?;
    let c2 =
        Peer::spawn(&h, PeerOpts { data_dir: Some(data), ..PeerOpts::new("carol").bind("127.0.0.1").wav(0) }).await?;
    let hist2 = c2.call("chat_history", json!({"session": sid, "limit": 100})).await?;
    ensure!(hist2.as_array().map_or(0, Vec::len) == 21, "history lost on restart: {hist2}");
    let first = hist2.as_array().and_then(|v| v.iter().find(|m| m["id"] == mid.as_str())).cloned().unwrap_or_default();
    eprintln!("carol after restart: 21 messages in history; alice's message: {first}");
    ensure!(first["body"] == "hello from alice" && first["sender_key"] == a.key.as_str());
    let ahist = a.call("chat_history", json!({"session": sid, "limit": 100})).await?;
    let own = ahist.as_array().and_then(|v| v.iter().find(|m| m["id"] == mid.as_str())).cloned().unwrap_or_default();
    eprintln!("alice's stored status of her message: {}", own["status"]);
    ensure!(own["own"] == true && (own["status"] == "Delivered" || own["status"] == "Read"));
    c2.stop().await?;
    alice.stop().await?;
    bob.stop().await?;
    Ok(())
}

fn downloaded(p: &Peer, name: &str) -> PathBuf {
    p.data_dir.join("downloads").join(name)
}

/// Four peers; alice offers a 20 MiB file to bob and carol only. Nothing flows before a
/// recipient accepts; bob is killed at ~40 % and restarted, and resumes from the bytes
/// he already has; both copies hash-match; dave never sees an offer.
#[tokio::test(flavor = "multi_thread")]
async fn file_transfer_accept_resume_hash() -> Result<()> {
    let h = Harness::new("file_transfer_accept_resume_hash")?;
    let mut peers = mesh(&h, &["alice", "bob", "carol", "dave"], &[]).await?;
    let refs: Vec<&Peer> = peers.iter().collect();
    let sid = call_all(&refs, CONNECT).await?;
    let size = 20 * 1024 * 1024;
    let src = h.dir.join("payload.bin");
    let hash = random_file(&src, size)?;
    let dave = peers.pop().expect("dave");
    let carol = peers.pop().expect("carol");
    let bob = peers.pop().expect("bob");
    let alice = peers.pop().expect("alice");

    let id = alice
        .call("offer_file", json!({"path": src.to_string_lossy(), "recipients": [bob.key, carol.key]}))
        .await?
        .as_str()
        .unwrap_or_default()
        .to_string();
    for p in [&bob, &carol] {
        let o = p.wait_event("FileOffered", CONNECT, |v| v["transfer"]["id"] == id.as_str()).await?["transfer"].clone();
        eprintln!("{} offered: name={} size={} hash={}", p.name, o["name"], o["size"], o["hash"]);
        ensure!(o["size"] == size && o["hash"] == hash.as_str() && o["peer_key"] == alice.key.as_str());
    }
    // Always asked: nothing moves until accepted.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let st = alice.snapshot().await?["transfers"].clone();
    eprintln!("alice's transfers before any accept: {st}");
    ensure!(
        st.as_array()
            .is_some_and(|v| v.len() == 2 && v.iter().all(|t| t["state"] == "Offered" && t["bytes_done"] == 0)),
        "data moved before the recipients accepted"
    );

    carol.call("respond_file_offer", json!({"id": id, "accept": true})).await?;
    bob.call("respond_file_offer", json!({"id": id, "accept": true})).await?;
    let t0 = Instant::now();
    let at = wait_transfer(&bob, &id, Duration::from_secs(60), |t| {
        t["bytes_done"].as_u64().unwrap_or(0) >= (size as u64) * 2 / 5
    })
    .await?;
    let killed_at = at["bytes_done"].as_u64().unwrap_or(0);
    eprintln!(
        "killing bob at {killed_at} bytes ({:.0} %) after {:.2} s",
        100.0 * killed_at as f64 / size as f64,
        t0.elapsed().as_secs_f64()
    );
    ensure!(killed_at < size as u64, "transfer finished before the kill");
    let bob_data = bob.data_dir.clone();
    let bob_key = bob.key.clone();
    bob.kill().await;
    let on_disk = std::fs::metadata(bob_data.join("downloads").join(format!(".{id}.part")))?.len();
    eprintln!("bob's partial file on disk after the kill: {on_disk} bytes");

    let carol_done = wait_transfer(&carol, &id, Duration::from_secs(120), |t| t["state"] == "Completed").await?;
    eprintln!("carol completed: {carol_done}");

    let bob2 = Peer::spawn(
        &h,
        PeerOpts {
            data_dir: Some(bob_data),
            ..PeerOpts::new("bob").bind("127.0.0.1").wav(1).lan_peer(alice.lan.as_deref().unwrap())
        },
    )
    .await?;
    ensure!(bob2.key == bob_key, "bob's identity changed on restart");
    let deadline = Instant::now() + CONNECT;
    while !bob2.snapshot().await?["sessions"].as_array().is_some_and(|v| v.iter().any(|x| x["id"] == sid.as_str())) {
        ensure!(Instant::now() < deadline, "restarted bob never saw the session");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let tr = Instant::now();
    bob2.call("join_session", json!({"id": sid})).await?;
    let resumed = wait_transfer(&bob2, &id, CONNECT, |t| t["state"] == "Accepted").await?;
    let offset = resumed["bytes_done"].as_u64().unwrap_or(0);
    eprintln!("bob resumed from offset {offset} ({:.0} %)", 100.0 * offset as f64 / size as f64);
    ensure!(offset >= killed_at && offset == on_disk, "did not resume from the bytes on disk ({offset} vs {on_disk})");
    let done = wait_transfer(&bob2, &id, Duration::from_secs(120), |t| {
        t["state"] == "Completed" || t["state"].get("Failed").is_some()
    })
    .await?;
    eprintln!("bob completed {:.2} s after rejoining: {done}", tr.elapsed().as_secs_f64());
    ensure!(done["state"] == "Completed", "bob's transfer failed: {done}");
    for (p, path) in [(&bob2, downloaded(&bob2, "payload.bin")), (&carol, downloaded(&carol, "payload.bin"))] {
        let got = blake3::hash(&std::fs::read(&path)?).to_hex().to_string();
        eprintln!("{}: {} BLAKE3 {got}", p.name, path.display());
        ensure!(got == hash, "{}: hash mismatch", p.name);
    }
    let sender =
        wait_transfer(&alice, &id, CONNECT, |t| t["peer_key"] == bob2.key.as_str() && t["state"] == "Completed")
            .await?;
    eprintln!("alice sees bob's copy completed: {}", sender["state"]);
    ensure!(dave.events("FileOffered").is_empty(), "dave received an offer");
    ensure!(dave.snapshot().await?["transfers"].as_array().is_none_or(|v| v.is_empty()), "dave has a transfer");
    ensure!(!downloaded(&dave, "payload.bin").exists(), "dave has the file");
    eprintln!("dave: no offer, no transfer, no file");
    ensure!(transfer_event(&alice, &id).iter().all(|t| t["peer_key"] != dave.key.as_str()));
    for p in [alice, bob2, carol, dave] {
        p.stop().await?;
    }
    Ok(())
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 }
}

/// Internet policy over a 2 Mbit/s bottleneck: while a file is sent the audio-first
/// throttle keeps audio loss < 1 % on average and audio ≥ 100 kbps.
#[tokio::test(flavor = "multi_thread")]
async fn file_transfer_throttle_keeps_audio_loss_flat() -> Result<()> {
    let h = Harness::new("file_transfer_throttle_keeps_audio_loss_flat")?;
    let netem = Netem::new().await?;
    netem.set(Family::All, Impairment { delay_ms: 20, rate_kbps: 2000, ..Impairment::CLEAN })?;
    let mut extra = netem.cli_args();
    extra.extend(["--debug-force-path".to_string(), "internet".to_string()]);
    let mut peers = mesh(&h, &["alice", "bob"], &extra).await?;
    let b = peers.pop().expect("bob");
    let a = peers.pop().expect("alice");
    aec_off(&a).await?;
    aec_off(&b).await?;
    call_all(&[&a, &b], CONNECT).await?;
    let t0 = Instant::now();
    let before = sample(&b, t0, 8.0).await?;
    ensure!(before.last().is_some_and(|p| p["path"]["kind"] == "Direct"), "internet policy not in force");

    let size = 4 * 1024 * 1024;
    let src = h.dir.join("bulk.bin");
    let hash = random_file(&src, size)?;
    let id = a
        .call("offer_file", json!({"path": src.to_string_lossy(), "recipients": [b.key]}))
        .await?
        .as_str()
        .unwrap_or_default()
        .to_string();
    b.wait_event("FileOffered", CONNECT, |v| v["transfer"]["id"] == id.as_str()).await?;
    b.call("respond_file_offer", json!({"id": id, "accept": true})).await?;
    let ts = Instant::now();
    let mut during_a = Vec::new();
    let mut during_b = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let done = loop {
        tick.tick().await;
        let (da, db) = (a.diagnostics().await?, b.diagnostics().await?);
        let (pa, pb) = (da["peers"][0].clone(), db["peers"][0].clone());
        let st = a.snapshot().await?["transfers"][0].clone();
        eprintln!(
            "t={:>4.1} file={:>8} audio send={:>6.1} target={:>6.1} rtt={:>5.1} loss(bob rx)={:>4.1} loss(alice rx)={:>4.1} file_limit={:>7.0} kbps",
            ts.elapsed().as_secs_f64(),
            st["bytes_done"],
            pa["send_kbps"].as_f64().unwrap_or(0.0),
            pa["opus_bitrate_kbps"].as_f64().unwrap_or(0.0),
            pa["rtt_ms"].as_f64().unwrap_or(0.0),
            pb["loss_pct"].as_f64().unwrap_or(0.0),
            pa["loss_pct"].as_f64().unwrap_or(0.0),
            pa["file_rate_limit_kbps"].as_f64().unwrap_or(0.0),
        );
        during_a.push(pa);
        during_b.push(pb);
        if st["state"] == "Completed" || st["state"].get("Failed").is_some() {
            break st;
        }
        ensure!(ts.elapsed() < Duration::from_secs(120), "transfer did not finish within 120 s");
    };
    let secs = ts.elapsed().as_secs_f64();
    ensure!(done["state"] == "Completed", "transfer failed: {done}");
    let got = blake3::hash(&std::fs::read(downloaded(&b, "bulk.bin"))?).to_hex().to_string();
    ensure!(got == hash, "hash mismatch");
    let loss_rx_b = mean(&during_b.iter().map(|p| p["loss_pct"].as_f64().unwrap_or(0.0)).collect::<Vec<_>>());
    let loss_rx_a = mean(&during_a.iter().map(|p| p["loss_pct"].as_f64().unwrap_or(0.0)).collect::<Vec<_>>());
    let send = mean(&during_a.iter().map(|p| p["send_kbps"].as_f64().unwrap_or(0.0)).collect::<Vec<_>>());
    let min_send = during_a.iter().map(|p| p["send_kbps"].as_f64().unwrap_or(0.0)).fold(f64::INFINITY, f64::min);
    let limits: Vec<f64> = during_a.iter().map(|p| p["file_rate_limit_kbps"].as_f64().unwrap_or(0.0)).collect();
    eprintln!(
        "4 MiB in {secs:.1} s ({:.0} kbps goodput); audio loss avg bob rx {loss_rx_b:.2} % / alice rx {loss_rx_a:.2} %; alice audio send avg {send:.1} kbps (min {min_send:.1}); throttle range {:.0}..{:.0} kbps",
        size as f64 * 8.0 / secs / 1000.0,
        limits.iter().copied().fold(f64::INFINITY, f64::min),
        limits.iter().copied().fold(0.0, f64::max),
    );
    h.save_json("during-alice.json", &json!(during_a));
    h.save_json("during-bob.json", &json!(during_b));
    ensure!(limits.iter().all(|l| *l > 0.0), "throttle not active on the internet path");
    ensure!(loss_rx_b < 1.0, "audio loss averaged {loss_rx_b:.2} % during the transfer");
    ensure!(send >= 100.0, "audio send averaged {send:.1} kbps < 100");
    netem.clear()?;
    let ra = a.stop().await?;
    let rb = b.stop().await?;
    assert_continuous_tail(&rb, 6.0)?;
    assert_continuous_tail(&ra, 6.0)?;
    Ok(())
}
