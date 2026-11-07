//! E2E harness: spawns `callxl-cli` peers, drives them over their JSON control sockets,
//! collects events, samples diagnostics, and stores logs/recordings/stats as artifacts.

pub mod audio;
/// The core's diagnostics-export validator, shared as source (no dependency on the core).
#[path = "../../../crates/core/src/diagnostics/schema_v1.rs"]
pub mod diag_schema;
pub mod mitm;
pub mod netem;
pub mod worker;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;

pub fn cli_path() -> PathBuf {
    if let Ok(p) = std::env::var("CALLXL_CLI") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("current exe");
    // target/<profile>/deps/<test> -> target/<profile>/callxl-cli
    exe.parent().and_then(Path::parent).map(|p| p.join("callxl-cli")).expect("cli path")
}

pub fn artifacts_root() -> PathBuf {
    std::env::var("CALLXL_E2E_ARTIFACTS").map(PathBuf::from).unwrap_or_else(|_| {
        let exe = std::env::current_exe().expect("current exe");
        exe.parent().and_then(Path::parent).map(|p| p.join("e2e-artifacts")).expect("artifacts")
    })
}

/// IPv6 loopback availability. Without it: SKIP locally, FAIL in CI (D-0011).
pub fn ipv6_or_skip(test: &str) -> bool {
    let ok = std::net::UdpSocket::bind("[::1]:0").is_ok();
    if !ok {
        if std::env::var("CI").is_ok() {
            panic!("{test}: IPv6 loopback (::1) is required in CI but unavailable");
        }
        eprintln!("SKIP {test}: this machine has no IPv6 (::1); runs in CI (D-0011)");
    }
    ok
}

/// `CALLXL_E2E_ADM=platform`: use the OS audio stack (CI: PulseAudio null sink) instead of
/// the WAV test ADM. Recordings are then unavailable and continuity checks are skipped.
pub fn platform_adm() -> bool {
    std::env::var("CALLXL_E2E_ADM").is_ok_and(|v| v == "platform")
}

pub struct Harness {
    pub name: String,
    pub dir: PathBuf,
    pub wav: Vec<PathBuf>,
}

impl Harness {
    pub fn new(name: &str) -> Result<Self> {
        let dir = artifacts_root().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let assets = artifacts_root().join("assets");
        std::fs::create_dir_all(&assets)?;
        let mut wav = Vec::new();
        for t in audio::TALKERS {
            let p = assets.join(format!("tone_speech_48k_mono_v2_{}.wav", t.tone_hz as u32));
            if !p.exists() {
                let tmp = assets.join(format!("tmp-{}-{}.wav", std::process::id(), t.tone_hz as u32));
                audio::write_wav(&tmp, &audio::tone_speech(t))?;
                std::fs::rename(&tmp, &p)?;
            }
            wav.push(p);
        }
        let cli = cli_path();
        if !cli.exists() {
            bail!("{} not found: run `cargo build -p callxl-cli` first (or set CALLXL_CLI)", cli.display());
        }
        Ok(Self { name: name.to_string(), dir, wav })
    }

    pub fn save_json(&self, file: &str, v: &Value) {
        let _ = std::fs::write(self.dir.join(file), serde_json::to_string_pretty(v).unwrap_or_default());
    }

    /// Stores `p`'s diagnostics export (schema v1) as `<name>-export.json` next to the
    /// recordings (CI uploads both per scenario). Schema validity itself is asserted by
    /// `diagnostics_complete`; here a violation is only reported.
    pub async fn save_export(&self, p: &Peer) {
        let Ok(text) = p.call("export_diagnostics", Value::Null).await else { return };
        let text = text.as_str().unwrap_or_default();
        if let Ok(v) = serde_json::from_str::<Value>(text)
            && let Err(e) = diag_schema::validate(&v)
        {
            eprintln!("{}: export violates schema v1: {e:?}", p.name);
        }
        let _ = std::fs::write(self.dir.join(format!("{}-export.json", p.name)), text);
    }
}

#[derive(Clone, Debug, Default)]
pub struct PeerOpts {
    pub name: String,
    pub bind: Vec<String>,
    pub lan_peers: Vec<String>,
    pub mdns: bool,
    pub cloud_url: Option<String>,
    pub lan_off: bool,
    /// Index into `Harness::wav` for the microphone; recording always on.
    pub wav: usize,
    pub extra: Vec<String>,
    /// Extra environment variables for the CLI process.
    pub env: Vec<(String, String)>,
    pub reset_identity: bool,
    /// Reuse this data dir (restart of the same device).
    pub data_dir: Option<PathBuf>,
}

impl PeerOpts {
    pub fn new(name: &str) -> Self {
        Self { name: name.into(), ..Default::default() }
    }
    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }
    pub fn bind(mut self, a: &str) -> Self {
        self.bind.push(a.into());
        self
    }
    pub fn lan_peer(mut self, a: &str) -> Self {
        self.lan_peers.push(a.into());
        self
    }
    pub fn wav(mut self, i: usize) -> Self {
        self.wav = i;
        self
    }
    pub fn extra<I: IntoIterator<Item = String>>(mut self, a: I) -> Self {
        self.extra.extend(a);
        self
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

pub struct Peer {
    pub name: String,
    pub key: String,
    pub lan: Option<String>,
    pub data_dir: PathBuf,
    pub record: PathBuf,
    child: Child,
    writer: tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>,
    pending: Pending,
    events: Arc<Mutex<Vec<(Instant, Value)>>>,
    next: AtomicU64,
    pub started: Instant,
}

impl Peer {
    pub async fn spawn(h: &Harness, o: PeerOpts) -> Result<Peer> {
        let data_dir = o.data_dir.clone().unwrap_or_else(|| h.dir.join(format!("{}-data", o.name)));
        std::fs::create_dir_all(&data_dir)?;
        let n = std::fs::read_dir(&h.dir).map(|d| d.count()).unwrap_or(0);
        let record = h.dir.join(format!("{}-recorded-{n}.wav", o.name));
        let log = std::fs::File::create(h.dir.join(format!("{}-{n}.log", o.name)))?;
        let mut cmd = Command::new(cli_path());
        cmd.arg("--control")
            .arg("tcp://127.0.0.1:0")
            .arg("--data-dir")
            .arg(&data_dir)
            .arg("--name")
            .arg(&o.name)
            .arg("--adm")
            .arg(if platform_adm() {
                "platform".to_string()
            } else {
                format!("test:play={},record={}", h.wav[o.wav].display(), record.display())
            })
            .arg("--log")
            .arg("info");
        for b in &o.bind {
            cmd.arg("--bind").arg(b);
        }
        for p in &o.lan_peers {
            cmd.arg("--lan-peer").arg(p);
        }
        if !o.mdns {
            cmd.arg("--no-mdns");
        }
        match &o.cloud_url {
            Some(u) => {
                cmd.arg("--signaling-url").arg(u);
            }
            None => {
                cmd.arg("--no-cloud");
            }
        }
        if o.lan_off {
            cmd.arg("--lan-off");
        }
        if o.reset_identity {
            cmd.arg("--reset-identity");
        }
        cmd.args(&o.extra);
        cmd.envs(o.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd.stdout(Stdio::piped()).stderr(Stdio::from(log)).kill_on_drop(true);
        let mut child = cmd.spawn().context("spawn callxl-cli")?;
        let stdout = child.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        let ready = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
            .await
            .context("callxl-cli did not become ready")??
            .ok_or_else(|| anyhow!("callxl-cli exited before ready"))?;
        let ready: Value = serde_json::from_str(&ready)?;
        let control = ready["control"].as_str().unwrap_or_default().trim_start_matches("tcp://").to_string();
        let stream = TcpStream::connect(&control).await?;
        stream.set_nodelay(true)?;
        let (r, w) = stream.into_split();
        let pending: Pending = Default::default();
        let events: Arc<Mutex<Vec<(Instant, Value)>>> = Default::default();
        let (p2, e2) = (pending.clone(), events.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                if v.get("event").is_some() {
                    if v["event"] != "StatsTick" {
                        e2.lock().unwrap().push((Instant::now(), v));
                    }
                } else if let Some(id) = v.get("id").and_then(Value::as_u64)
                    && let Some(tx) = p2.lock().unwrap().remove(&id)
                {
                    let _ = tx.send(v);
                }
            }
        });
        Ok(Peer {
            name: o.name.clone(),
            key: ready["key"].as_str().unwrap_or_default().to_string(),
            lan: ready["lan"].as_str().map(str::to_string),
            data_dir,
            record,
            child,
            writer: tokio::sync::Mutex::new(w),
            pending,
            events,
            next: AtomicU64::new(1),
            started: Instant::now(),
        })
    }

    pub async fn call(&self, cmd: &str, args: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let line = json!({"id": id, "cmd": cmd, "args": args}).to_string() + "\n";
        self.writer.lock().await.write_all(line.as_bytes()).await?;
        let v = tokio::time::timeout(Duration::from_secs(20), rx).await.context("control call timed out")??;
        if v["ok"].as_bool() == Some(true) {
            Ok(v["result"].clone())
        } else {
            Err(anyhow!("{} {cmd}: {}", self.name, v["error"]))
        }
    }

    pub async fn debug(&self, name: &str) -> Result<Value> {
        self.call("debug", json!({"name": name})).await
    }

    pub async fn diagnostics(&self) -> Result<Value> {
        self.call("diagnostics", Value::Null).await
    }

    pub async fn snapshot(&self) -> Result<Value> {
        self.call("snapshot", Value::Null).await
    }

    pub fn events(&self, name: &str) -> Vec<Value> {
        self.events.lock().unwrap().iter().filter(|(_, v)| v["event"] == name).map(|(_, v)| v["data"].clone()).collect()
    }

    /// Waits for an event `name` whose data satisfies `pred` (already-received ones count).
    pub async fn wait_event(&self, name: &str, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(v) = self.events(name).into_iter().find(|v| pred(v)) {
                return Ok(v);
            }
            if Instant::now() > deadline {
                bail!("{}: no {name} event within {timeout:?}", self.name);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Polls `diagnostics` until `pred` holds for this peer's first PeerStats.
    pub async fn wait_stats(&self, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let d = self.diagnostics().await?;
            if let Some(p) = d["peers"].get(0)
                && pred(p)
            {
                return Ok(p.clone());
            }
            if Instant::now() > deadline {
                bail!("{}: stats condition not met within {timeout:?}; last: {}", self.name, d["peers"]);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub async fn wait_connected(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.snapshot().await?;
            let peers_ok =
                s["peers"].as_array().is_some_and(|p| !p.is_empty() && p.iter().all(|x| x["connected"] == true));
            if s["call_state"] == "InCall" && peers_ok {
                return Ok(());
            }
            if Instant::now() > deadline {
                bail!("{}: not connected within {timeout:?}: {}", self.name, s["call_state"]);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Flushes the test ADM recording and stops the CLI gracefully.
    pub async fn stop(mut self) -> Result<PathBuf> {
        let _ = self.debug("test_adm_flush").await;
        let _ = self.call("stop", Value::Null).await;
        let _ = tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await;
        let _ = self.child.start_kill();
        Ok(self.record.clone())
    }

    /// Hard kill (simulates a crash).
    pub async fn kill(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

/// Samples `diagnostics` of `peer` once per second for `secs`, returning each sample's
/// first PeerStats (with `t` = seconds since `t0` and the ANA frame length).
pub async fn sample(peer: &Peer, t0: Instant, secs: f64) -> Result<Vec<Value>> {
    sample_every(peer, t0, secs, Duration::from_secs(1)).await
}

/// Like [`sample`] at a custom period (pair with `CALLXL_STATS_INTERVAL_MS` on the peer).
pub async fn sample_every(peer: &Peer, t0: Instant, secs: f64, every: Duration) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let end = Instant::now() + Duration::from_secs_f64(secs);
    let mut tick = tokio::time::interval(every);
    while Instant::now() < end {
        tick.tick().await;
        let d = peer.diagnostics().await?;
        if let Some(p) = d["peers"].get(0) {
            let mut p = p.clone();
            p["t"] = json!((Instant::now() - t0).as_secs_f64());
            p["ana_frame_ms"] = d["global"]["ana_frame_ms"].clone();
            p["ana_loss_pct"] = d["global"]["ana_uplink_loss_pct"].clone();
            eprintln!("{} {}", peer.name, brief(&p));
            out.push(p);
        }
    }
    Ok(out)
}

pub fn brief(p: &Value) -> String {
    format!(
        "t={:>5.1} send={:>6.1} target={:>6.1} payload={:>6.1} bwe={:>5.0} frame={} ana={}/{:.1}% rtt={:>5.1} loss={:>4.1}/{:>4.1} path={}/{}@{} sw={}",
        p["t"].as_f64().unwrap_or(0.0),
        p["send_kbps"].as_f64().unwrap_or(0.0),
        p["opus_bitrate_kbps"].as_f64().unwrap_or(0.0),
        p["opus_payload_kbps"].as_f64().unwrap_or(0.0),
        p["available_outgoing_kbps"].as_f64().unwrap_or(0.0),
        p["opus_frame_ms"],
        p["ana_frame_ms"],
        p["ana_loss_pct"].as_f64().unwrap_or(0.0),
        p["rtt_ms"].as_f64().unwrap_or(0.0),
        p["loss_pct"].as_f64().unwrap_or(0.0),
        p["remote_loss_pct"].as_f64().unwrap_or(0.0),
        p["path"]["kind"].as_str().unwrap_or("-"),
        p["path"]["local_family"].as_str().unwrap_or("-"),
        p["path"]["local_addr"].as_str().unwrap_or("-"),
        p["pair_switches"],
    )
}

/// Creates a session on the first peer and joins it from every other peer once they
/// see it; waits until everyone is connected. Returns the session id.
pub async fn call_all(peers: &[&Peer], timeout: Duration) -> Result<String> {
    let sid = peers[0].call("create_session", json!({"name": "e2e"})).await?.as_str().unwrap_or_default().to_string();
    for p in &peers[1..] {
        let deadline = Instant::now() + timeout;
        loop {
            let s = p.snapshot().await?;
            if s["sessions"].as_array().is_some_and(|v| v.iter().any(|x| x["id"] == sid)) {
                break;
            }
            if Instant::now() > deadline {
                bail!("{} never saw session {sid}", p.name);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        p.call("join_session", json!({"id": sid})).await?;
    }
    for p in peers {
        p.wait_connected(timeout).await?;
    }
    Ok(sid)
}

/// §3 defaults with AEC3 off: used while continuity is measured. AEC3 never converges
/// with the test ADM (no acoustic path, permanent double talk: ERLE ≈ 0 dB, delay
/// estimate 0 ms) and then suppresses the near end — that is AEC behaviour, not a
/// transport dropout. NS, high-pass and AGC2 stay on. See DECISIONS.md.
pub fn apm_no_aec() -> Value {
    json!({"aec": false, "noise_suppression": true, "ns_level": "Moderate", "high_pass": true, "agc": true})
}

pub async fn aec_off(p: &Peer) -> Result<()> {
    p.call("set_audio_processing", json!({"cfg": apm_no_aec()})).await.map(|_| ())
}

/// Asserts the last `tail_secs` of the far-end recording at `path` have no gap > 20 ms.
pub fn assert_continuous_tail(path: &Path, tail_secs: f64) -> Result<audio::Continuity> {
    if platform_adm() {
        eprintln!("platform ADM: no recording, continuity not measured");
        return Ok(audio::Continuity { active_secs: 0.0, longest_gap_ms: 0.0, gaps_over_20ms: 0, median_rms: 0.0 });
    }
    let s = audio::read_wav(path)?;
    let keep = (tail_secs * f64::from(audio::RATE)) as usize;
    let start = s.len().saturating_sub(keep);
    let c = audio::continuity(&s[start..]);
    eprintln!("continuity (last {tail_secs} s) {}: {c:?}", path.display());
    if c.gaps_over_20ms > 0 {
        bail!("{}: {} gap(s) > 20 ms, longest {:.0} ms", path.display(), c.gaps_over_20ms, c.longest_gap_ms);
    }
    Ok(c)
}

/// Asserts the far-end recording at `path` has no gap > 20 ms; returns the analysis.
pub fn assert_continuous(path: &Path) -> Result<audio::Continuity> {
    if platform_adm() {
        eprintln!("platform ADM: no recording, continuity not measured");
        return Ok(audio::Continuity { active_secs: 0.0, longest_gap_ms: 0.0, gaps_over_20ms: 0, median_rms: 0.0 });
    }
    let s = audio::read_wav(path)?;
    let c = audio::continuity(&s);
    eprintln!("continuity {}: {c:?}", path.display());
    if c.active_secs < 3.0 {
        bail!("{}: only {:.1} s of audio recorded", path.display(), c.active_secs);
    }
    if c.gaps_over_20ms > 0 {
        bail!("{}: {} gap(s) > 20 ms, longest {:.0} ms", path.display(), c.gaps_over_20ms, c.longest_gap_ms);
    }
    Ok(c)
}
