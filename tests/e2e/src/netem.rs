//! Network impairment backends (D-0003), selected by `CALLXL_NETEM=tc|proxy`:
//!
//! - `tc`: real `tc netem` on `lo` via `scripts/netem.sh` (needs root / CAP_NET_ADMIN).
//!   Family-specific impairment uses a prio qdisc with `protocol ip|ipv6` filters.
//! - `proxy`: a userspace UDP relay. Each CLI runs with `--debug-candidate-rewrite`, so its
//!   host candidates are registered here and replaced by relay addresses; packets from
//!   S to relay(X) are forwarded to X *from* relay(S) after delay/jitter/loss/rate cap.
//!
//! The same test code drives both through [`Netem::set`].

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rand::Rng;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UdpSocket};
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Impairment {
    pub delay_ms: u32,
    pub jitter_ms: u32,
    pub loss_pct: f64,
    /// Bottleneck rate in kbit/s (0 = unlimited), per direction.
    pub rate_kbps: u32,
}

impl Impairment {
    pub const CLEAN: Impairment = Impairment { delay_ms: 0, jitter_ms: 0, loss_pct: 0.0, rate_kbps: 0 };
    pub fn delay(ms: u32) -> Self {
        Self { delay_ms: ms, ..Self::CLEAN }
    }
    pub fn loss(pct: f64) -> Self {
        Self { loss_pct: pct, ..Self::CLEAN }
    }
    pub fn rate(kbps: u32) -> Self {
        Self { rate_kbps: kbps, ..Self::CLEAN }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    All,
    V4,
    V6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Tc,
    Proxy,
}

pub struct Netem {
    pub backend: Backend,
    relay: Option<Relay>,
}

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/netem.sh")
}

fn tc(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new(script()).args(args).output().context("run scripts/netem.sh")?;
    if !out.status.success() {
        bail!("netem.sh {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

impl Netem {
    /// Chooses the backend: `CALLXL_NETEM` if set, else `tc` when it works, else `proxy`.
    pub async fn new() -> Result<Self> {
        let want = std::env::var("CALLXL_NETEM").unwrap_or_default();
        let backend = match want.as_str() {
            "tc" => Backend::Tc,
            "proxy" => Backend::Proxy,
            _ => {
                if tc(&["probe"]).is_ok() {
                    Backend::Tc
                } else {
                    Backend::Proxy
                }
            }
        };
        let relay = match backend {
            Backend::Proxy => Some(Relay::start().await?),
            Backend::Tc => {
                tc(&["del"]).ok();
                None
            }
        };
        eprintln!("netem backend: {backend:?}");
        Ok(Self { backend, relay })
    }

    /// Extra `callxl-cli` arguments this backend needs.
    pub fn cli_args(&self) -> Vec<String> {
        match &self.relay {
            Some(r) => vec!["--debug-candidate-rewrite".into(), format!("tcp://{}", r.control)],
            None => Vec::new(),
        }
    }

    pub fn set(&self, family: Family, imp: Impairment) -> Result<()> {
        eprintln!("netem set {family:?} {imp:?}");
        match self.backend {
            Backend::Proxy => {
                let r = self.relay.as_ref().expect("relay");
                let mut g = r.shared.imp.lock().unwrap();
                match family {
                    Family::All => {
                        g.insert(Family::V4, imp);
                        g.insert(Family::V6, imp);
                    }
                    f => {
                        g.insert(f, imp);
                    }
                }
                Ok(())
            }
            Backend::Tc => {
                let fam = match family {
                    Family::All => "all",
                    Family::V4 => "v4",
                    Family::V6 => "v6",
                };
                tc(&[
                    "set",
                    fam,
                    &imp.delay_ms.to_string(),
                    &imp.jitter_ms.to_string(),
                    &format!("{}", imp.loss_pct),
                    &imp.rate_kbps.to_string(),
                ])
            }
        }
    }

    pub fn clear(&self) -> Result<()> {
        match self.backend {
            Backend::Proxy => self.set(Family::All, Impairment::CLEAN),
            Backend::Tc => tc(&["del"]),
        }
    }
}

impl Drop for Netem {
    fn drop(&mut self) {
        if self.backend == Backend::Tc {
            let _ = tc(&["del"]);
        }
    }
}

// ---------------------------------------------------------------------- proxy relay

struct RelayShared {
    imp: Mutex<HashMap<Family, Impairment>>,
    /// real candidate address -> relay socket standing in for it
    by_target: Mutex<HashMap<SocketAddr, Arc<UdpSocket>>>,
    /// per (relay socket that sends) bottleneck: time the link is busy until
    busy_until: Mutex<HashMap<SocketAddr, Instant>>,
}

struct Relay {
    control: SocketAddr,
    shared: Arc<RelayShared>,
}

impl Relay {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let control = listener.local_addr()?;
        let shared = Arc::new(RelayShared {
            imp: Mutex::new(HashMap::new()),
            by_target: Mutex::new(HashMap::new()),
            busy_until: Mutex::new(HashMap::new()),
        });
        let sh = shared.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let sh = sh.clone();
                tokio::spawn(async move {
                    let (r, mut w) = sock.into_split();
                    let mut line = String::new();
                    if BufReader::new(r).read_line(&mut line).await.is_err() {
                        return;
                    }
                    let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
                    let Some(target) = v.get("register").and_then(|t| t.as_str()).and_then(|t| t.parse::<SocketAddr>().ok()) else {
                        return;
                    };
                    match register(&sh, target).await {
                        Ok(addr) => {
                            let _ = w.write_all(format!("{{\"addr\":\"{addr}\"}}\n").as_bytes()).await;
                        }
                        Err(e) => eprintln!("relay register {target}: {e}"),
                    }
                });
            }
        });
        Ok(Self { control, shared })
    }
}

async fn register(sh: &Arc<RelayShared>, target: SocketAddr) -> Result<SocketAddr> {
    if let Some(s) = sh.by_target.lock().unwrap().get(&target) {
        return Ok(s.local_addr()?);
    }
    let bind: SocketAddr = match target.ip() {
        IpAddr::V4(_) => "127.0.0.1:0".parse()?,
        IpAddr::V6(_) => "[::1]:0".parse()?,
    };
    let sock = Arc::new(UdpSocket::bind(bind).await?);
    let addr = sock.local_addr()?;
    sh.by_target.lock().unwrap().insert(target, sock.clone());
    let sh2 = sh.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65_536];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf).await else { return };
            // Packet from `from` to relay(target): forward to `target` from relay(from).
            let Some(out) = sh2.by_target.lock().unwrap().get(&from).cloned() else { continue };
            let fam = if target.is_ipv4() { Family::V4 } else { Family::V6 };
            let imp = sh2.imp.lock().unwrap().get(&fam).copied().unwrap_or_default();
            if imp.loss_pct > 0.0 && rand::thread_rng().gen_bool((imp.loss_pct / 100.0).min(1.0)) {
                continue;
            }
            let now = Instant::now();
            let mut at = now;
            if imp.rate_kbps > 0 {
                let ser = Duration::from_secs_f64((n as f64 * 8.0) / (f64::from(imp.rate_kbps) * 1000.0));
                let key = out.local_addr().unwrap_or(addr);
                let mut busy = sh2.busy_until.lock().unwrap();
                let start = busy.get(&key).copied().filter(|b| *b > now).unwrap_or(now);
                // Router-like tail drop once more than 100 ms are queued.
                if start.duration_since(now) > Duration::from_millis(100) {
                    continue;
                }
                let done = start + ser;
                busy.insert(key, done);
                at = done;
            }
            let jitter = if imp.jitter_ms > 0 { rand::thread_rng().gen_range(0..=imp.jitter_ms) } else { 0 };
            at += Duration::from_millis(u64::from(imp.delay_ms + jitter));
            let data = buf[..n].to_vec();
            if at <= now {
                let _ = out.send_to(&data, target).await;
            } else {
                tokio::spawn(async move {
                    tokio::time::sleep_until(at).await;
                    let _ = out.send_to(&data, target).await;
                });
            }
        }
    });
    Ok(addr)
}
