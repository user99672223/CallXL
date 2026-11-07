//! Diagnostics collector (§7): a 1 Hz task, separate from the core task, that reads
//! libwebrtc `GetStats` for every link plus core state, keeps 60-sample rings per
//! metric, and hands the snapshot to the core (throttle, speaking) and the UI.
//! It only reads from libwebrtc; it never changes media state.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use callxl_media::stats::{self, StatsReport};
use callxl_media::{MediaEngine, PeerConnection};
use tokio::sync::{mpsc, watch};

use crate::engine::Internal;
use crate::types::*;

pub const SERIES_LEN: usize = 60;
pub const FRAME_MECHANISM: &str =
    "libwebrtc audio network adaptor (CallXL loss-driven frame-length controller + libwebrtc BitrateController)";

#[derive(Clone)]
pub struct LinkView {
    pub key: PeerKey,
    pub name: String,
    pub pc: Arc<PeerConnection>,
    pub path: Option<PathInfo>,
    pub switches: u32,
    pub file_rate_limit_kbps: f64,
}

#[derive(Clone, Default)]
pub struct DiagInputs {
    pub links: Vec<LinkView>,
    pub signaling: Option<SignalingTransport>,
    pub signaling_latency_ms: f64,
    pub rejected_signaling: u64,
    pub glare_events: u64,
    pub turn_bytes_month: u64,
}

pub fn turn_key() -> String {
    let days = callxl_signal::now_ms() / 86_400_000;
    // Civil month from days since epoch (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("turn_bytes:{y:04}-{m:02}")
}

struct Rings(HashMap<String, VecDeque<f64>>);

impl Rings {
    fn push(&mut self, k: &str, v: f64) {
        let r = self.0.entry(k.to_string()).or_default();
        if r.len() == SERIES_LEN {
            r.pop_front();
        }
        r.push_back(v);
    }
    fn series(&self) -> Vec<Sparkline> {
        let mut v: Vec<_> =
            self.0.iter().map(|(k, r)| Sparkline { metric: k.clone(), values: r.iter().copied().collect() }).collect();
        v.sort_by(|a, b| a.metric.cmp(&b.metric));
        v
    }
}

/// CPU time of libwebrtc's threads (everything that is not a Tokio/Rust worker).
struct CpuMeter {
    last: Option<(Instant, u64)>,
    tick: f64,
}

impl CpuMeter {
    fn sample(&mut self) -> f64 {
        let mut total = 0u64;
        if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
            for e in dir.flatten() {
                let p = e.path();
                let comm = std::fs::read_to_string(p.join("comm")).unwrap_or_default();
                let comm = comm.trim();
                if comm.starts_with("tokio")
                    || comm.starts_with("callxl-cli")
                    || comm.starts_with("callxl-core")
                    || comm == "callxl-mdns"
                {
                    continue;
                }
                if let Ok(stat) = std::fs::read_to_string(p.join("stat")) {
                    // Fields after the parenthesised comm: state(3) ... utime(14) stime(15).
                    if let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) {
                        let f: Vec<&str> = rest.split_whitespace().collect();
                        if f.len() > 13 {
                            total += f[11].parse::<u64>().unwrap_or(0) + f[12].parse::<u64>().unwrap_or(0);
                        }
                    }
                }
            }
        }
        let now = Instant::now();
        let pct = match self.last {
            Some((t, prev)) => {
                let dt = now.duration_since(t).as_secs_f64().max(1e-3);
                100.0 * (total.saturating_sub(prev) as f64 / self.tick) / dt
            }
            None => 0.0,
        };
        self.last = Some((now, total));
        pct
    }
}

pub fn spawn(
    engine: Arc<MediaEngine>,
    mut inputs: watch::Receiver<DiagInputs>,
    internal: mpsc::UnboundedSender<Internal>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut prev: HashMap<PeerKey, StatsReport> = HashMap::new();
        let mut rings = Rings(HashMap::new());
        let mut cpu = CpuMeter { last: None, tick: 100.0 };
        let mut iv = tokio::time::interval(Duration::from_secs(1));
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            iv.tick().await;
            let inp = inputs.borrow_and_update().clone();
            let ana = engine.ana_state();
            let aec = engine.apm_stats();
            let mut peers = Vec::new();
            let (mut up, mut down) = (0.0, 0.0);
            let mut turn_delta = 0u64;
            for l in &inp.links {
                let Ok(json) = l.pc.stats_json().await else { continue };
                let Some(cur) = StatsReport::parse(&json) else { continue };
                let p = prev.get(&l.key).cloned().unwrap_or_default();
                let r = if p.ts_ms > 0.0 { stats::rates(&p, &cur) } else { stats::Rates::default() };
                if l.path.as_ref().is_some_and(|x| x.kind == PathKind::Relayed) && p.ts_ms > 0.0 {
                    turn_delta += (cur.pair_bytes_sent + cur.pair_bytes_received)
                        .saturating_sub(p.pair_bytes_sent + p.pair_bytes_received);
                }
                up += r.transport_send_kbps;
                down += r.transport_recv_kbps;
                let frame = stats::frame_ms_from_pps(r.send_pps).unwrap_or(ana.frame_length_ms.max(0) as u32);
                let rtt = cur.pair_rtt_s.or(cur.remote_rtt_s).unwrap_or(0.0) * 1000.0;
                let ps = PeerStats {
                    peer_key: l.key.clone(),
                    display_name: l.name.clone(),
                    path: l.path.clone(),
                    rtt_ms: rtt,
                    jitter_ms: cur.in_jitter_s * 1000.0,
                    loss_pct: r.loss_pct,
                    remote_loss_pct: cur.remote_fraction_lost.unwrap_or(0.0) * 100.0,
                    send_kbps: r.send_kbps,
                    recv_kbps: r.recv_kbps,
                    opus_frame_ms: frame,
                    jitter_buffer_ms: r.jitter_buffer_ms,
                    concealment_events: cur.concealment_events,
                    pair_switches: l.switches,
                    opus_bitrate_kbps: cur.out_target_bitrate.map(|b| b / 1000.0).unwrap_or(r.opus_payload_kbps),
                    opus_payload_kbps: r.opus_payload_kbps,
                    fec_enabled: cur.codec_fmtp.contains("useinbandfec=1"),
                    dtx_enabled: cur.codec_fmtp.contains("usedtx=1"),
                    codec: cur.codec_mime.clone(),
                    clock_rate: cur.codec_clock_rate,
                    // The rtpmap always says opus/48000/2; what is encoded is the encoder's channel count.
                    channels: if ana.encoder_channels > 0 { ana.encoder_channels as u32 } else { cur.codec_channels },
                    opus_complexity: ana.encoder_complexity.max(0) as u32,
                    available_outgoing_kbps: cur.available_outgoing_bitrate.unwrap_or(0.0) / 1000.0,
                    audio_level: cur.in_audio_level,
                    file_rate_limit_kbps: l.file_rate_limit_kbps,
                };
                let tag = &l.key[..l.key.len().min(8)];
                for (m, v) in [
                    ("rtt_ms", ps.rtt_ms),
                    ("jitter_ms", ps.jitter_ms),
                    ("loss_pct", ps.loss_pct),
                    ("send_kbps", ps.send_kbps),
                    ("recv_kbps", ps.recv_kbps),
                    ("opus_bitrate_kbps", ps.opus_bitrate_kbps),
                    ("opus_frame_ms", f64::from(ps.opus_frame_ms)),
                    ("jitter_buffer_ms", ps.jitter_buffer_ms),
                    ("concealment_events", ps.concealment_events as f64),
                ] {
                    rings.push(&format!("{m}:{tag}"), v);
                }
                prev.insert(l.key.clone(), cur);
                peers.push(ps);
            }
            prev.retain(|k, _| inp.links.iter().any(|l| &l.key == k));
            let cpu_pct = cpu.sample();
            rings.push("uplink_kbps", up);
            rings.push("downlink_kbps", down);
            rings.push("media_thread_cpu_pct", cpu_pct);
            rings.push("ana_frame_ms", f64::from(ana.frame_length_ms.max(0)));
            rings.push("ana_switches", f64::from(ana.switches.max(0)));
            if turn_delta > 0 {
                let _ = internal.send(Internal::TurnBytes(turn_delta));
            }
            let snap = DiagnosticsSnapshot {
                ts_ms: callxl_signal::now_ms(),
                peers,
                global: GlobalStats {
                    uplink_kbps: up,
                    downlink_kbps: down,
                    media_thread_cpu_pct: cpu_pct,
                    signaling_transport: inp.signaling.unwrap_or(SignalingTransport::None),
                    signaling_latency_ms: inp.signaling_latency_ms,
                    turn_bytes_month: inp.turn_bytes_month + turn_delta,
                    apm: ApmConfig::from_media(engine.apm()),
                    frame_adaptation_mechanism: FRAME_MECHANISM.to_string(),
                    ana_frame_ms: ana.frame_length_ms.max(0) as u32,
                    ana_switches: ana.switches.max(0) as u32,
                    aec_erl_db: aec.0,
                    aec_erle_db: aec.1,
                    aec_delay_ms: aec.3,
                    rejected_signaling: inp.rejected_signaling,
                    glare_events: inp.glare_events,
                    dropped_media_events: callxl_media::dropped_events(),
                },
                series: rings.series(),
            };
            if internal.send(Internal::Stats(snap)).is_err() {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn turn_key_is_a_month() {
        let k = super::turn_key();
        assert!(k.starts_with("turn_bytes:20"), "{k}");
        assert_eq!(k.len(), "turn_bytes:2026-11".len());
    }
}
