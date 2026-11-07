//! Parsing of libwebrtc `GetStats` reports (JSON array of RTCStats objects, camelCase
//! members, `type` tag) and per-second rates from two consecutive reports.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CandidateStats {
    pub address: String,
    pub port: u16,
    pub candidate_type: String,
    pub protocol: String,
    pub network_type: String,
}

/// The audio-relevant subset of one GetStats report.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StatsReport {
    pub ts_ms: f64,
    // outbound-rtp (audio)
    pub out_packets: u64,
    pub out_payload_bytes: u64,
    pub out_header_bytes: u64,
    pub out_target_bitrate: Option<f64>,
    // inbound-rtp (audio)
    pub in_packets: u64,
    pub in_lost: i64,
    pub in_payload_bytes: u64,
    pub in_header_bytes: u64,
    pub in_jitter_s: f64,
    pub jb_delay_s: f64,
    pub jb_emitted: u64,
    pub jb_minimum_delay_s: f64,
    pub concealment_events: u64,
    pub concealed_samples: u64,
    pub total_samples_received: u64,
    pub fec_packets_received: u64,
    pub in_audio_level: f64,
    // remote-inbound-rtp (what the far end reports about our stream)
    pub remote_fraction_lost: Option<f64>,
    pub remote_packets_lost: Option<i64>,
    pub remote_rtt_s: Option<f64>,
    // send codec
    pub codec_mime: String,
    pub codec_clock_rate: u32,
    pub codec_channels: u32,
    pub codec_fmtp: String,
    // selected candidate pair
    pub pair_rtt_s: Option<f64>,
    pub pair_bytes_sent: u64,
    pub pair_bytes_received: u64,
    pub available_outgoing_bitrate: Option<f64>,
    pub local: Option<CandidateStats>,
    pub remote: Option<CandidateStats>,
    pub selected_pair_changes: u64,
    // transport totals (includes data channels)
    pub transport_bytes_sent: u64,
    pub transport_bytes_received: u64,
}

fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_f64).map(|f| f.max(0.0) as u64).unwrap_or(0)
}
fn f(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}
fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn candidate(v: &Value) -> CandidateStats {
    CandidateStats {
        address: {
            let a = s(v, "address");
            if a.is_empty() { s(v, "ip") } else { a }
        },
        port: u(v, "port") as u16,
        candidate_type: s(v, "candidateType"),
        protocol: s(v, "protocol"),
        network_type: s(v, "networkType"),
    }
}

impl StatsReport {
    pub fn parse(json: &str) -> Option<Self> {
        let root: Value = serde_json::from_str(json).ok()?;
        let items: Vec<Value> = match root {
            Value::Array(a) => a,
            Value::Object(m) => m.into_values().collect(),
            _ => return None,
        };
        let by_id = |id: &str| items.iter().find(|o| o.get("id").and_then(Value::as_str) == Some(id));
        let kind = |o: &Value| {
            o.get("kind").and_then(Value::as_str).or_else(|| o.get("mediaType").and_then(Value::as_str))
                == Some("audio")
        };
        let mut r = StatsReport::default();
        let mut send_codec_id = String::new();
        let mut selected_pair_id = String::new();
        for o in &items {
            // libwebrtc's RTCStats JSON carries `timestamp` in microseconds.
            let ts = f(o, "timestamp").unwrap_or(0.0);
            r.ts_ms = r.ts_ms.max(if ts > 1e14 { ts / 1000.0 } else { ts });
            match o.get("type").and_then(Value::as_str).unwrap_or("") {
                "outbound-rtp" if kind(o) => {
                    r.out_packets += u(o, "packetsSent");
                    r.out_payload_bytes += u(o, "bytesSent");
                    r.out_header_bytes += u(o, "headerBytesSent");
                    r.out_target_bitrate = f(o, "targetBitrate").or(r.out_target_bitrate);
                    send_codec_id = s(o, "codecId");
                }
                "inbound-rtp" if kind(o) => {
                    r.in_packets += u(o, "packetsReceived");
                    r.in_lost += o.get("packetsLost").and_then(Value::as_i64).unwrap_or(0);
                    r.in_payload_bytes += u(o, "bytesReceived");
                    r.in_header_bytes += u(o, "headerBytesReceived");
                    r.in_jitter_s = f(o, "jitter").unwrap_or(0.0);
                    r.jb_delay_s += f(o, "jitterBufferDelay").unwrap_or(0.0);
                    r.jb_emitted += u(o, "jitterBufferEmittedCount");
                    r.jb_minimum_delay_s = f(o, "jitterBufferMinimumDelay").unwrap_or(0.0);
                    r.concealment_events += u(o, "concealmentEvents");
                    r.concealed_samples += u(o, "concealedSamples");
                    r.total_samples_received += u(o, "totalSamplesReceived");
                    r.fec_packets_received += u(o, "fecPacketsReceived");
                    r.in_audio_level = r.in_audio_level.max(f(o, "audioLevel").unwrap_or(0.0));
                }
                "remote-inbound-rtp" if kind(o) => {
                    r.remote_fraction_lost = f(o, "fractionLost");
                    r.remote_packets_lost = o.get("packetsLost").and_then(Value::as_i64);
                    r.remote_rtt_s = f(o, "roundTripTime");
                }
                "transport" => {
                    r.transport_bytes_sent += u(o, "bytesSent");
                    r.transport_bytes_received += u(o, "bytesReceived");
                    r.selected_pair_changes += u(o, "selectedCandidatePairChanges");
                    if selected_pair_id.is_empty() {
                        selected_pair_id = s(o, "selectedCandidatePairId");
                    }
                }
                _ => {}
            }
        }
        if let Some(c) = by_id(&send_codec_id) {
            r.codec_mime = s(c, "mimeType");
            r.codec_clock_rate = u(c, "clockRate") as u32;
            r.codec_channels = u(c, "channels") as u32;
            r.codec_fmtp = s(c, "sdpFmtpLine");
        }
        if let Some(p) = by_id(&selected_pair_id) {
            r.pair_rtt_s = f(p, "currentRoundTripTime");
            r.pair_bytes_sent = u(p, "bytesSent");
            r.pair_bytes_received = u(p, "bytesReceived");
            r.available_outgoing_bitrate = f(p, "availableOutgoingBitrate");
            r.local = by_id(&s(p, "localCandidateId")).map(candidate);
            r.remote = by_id(&s(p, "remoteCandidateId")).map(candidate);
        }
        Some(r)
    }
}

/// Per-second figures derived from two reports `dt` apart.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    pub send_kbps: f64,
    pub recv_kbps: f64,
    /// Opus payload bitrate actually produced (payload bytes only).
    pub opus_payload_kbps: f64,
    pub send_pps: f64,
    pub loss_pct: f64,
    pub jitter_buffer_ms: f64,
    pub concealment_events: u64,
    pub transport_send_kbps: f64,
    pub transport_recv_kbps: f64,
}

pub fn rates(prev: &StatsReport, cur: &StatsReport) -> Rates {
    let dt = ((cur.ts_ms - prev.ts_ms) / 1000.0).max(1e-3);
    let d = |a: u64, b: u64| b.saturating_sub(a) as f64;
    let out_bytes = d(prev.out_payload_bytes + prev.out_header_bytes, cur.out_payload_bytes + cur.out_header_bytes);
    let in_bytes = d(prev.in_payload_bytes + prev.in_header_bytes, cur.in_payload_bytes + cur.in_header_bytes);
    let pkts = d(prev.out_packets, cur.out_packets);
    // Receive-side loss over the interval (what this peer hears).
    let got = d(prev.in_packets, cur.in_packets);
    let lost = (cur.in_lost - prev.in_lost).max(0) as f64;
    let loss_pct = if got + lost > 0.0 { 100.0 * lost / (got + lost) } else { 0.0 };
    let emitted = d(prev.jb_emitted, cur.jb_emitted);
    let jb_ms = if emitted > 0.0 { 1000.0 * (cur.jb_delay_s - prev.jb_delay_s).max(0.0) / emitted } else { 0.0 };
    Rates {
        // RTP payload + header bytes, i.e. the audio stream's on-wire rate excluding
        // IP/UDP/SRTP overhead (matches what browsers report for send bitrate).
        send_kbps: out_bytes * 8.0 / dt / 1000.0,
        recv_kbps: in_bytes * 8.0 / dt / 1000.0,
        opus_payload_kbps: d(prev.out_payload_bytes, cur.out_payload_bytes) * 8.0 / dt / 1000.0,
        send_pps: pkts / dt,
        loss_pct,
        jitter_buffer_ms: jb_ms,
        concealment_events: cur.concealment_events.saturating_sub(prev.concealment_events),
        transport_send_kbps: d(prev.transport_bytes_sent, cur.transport_bytes_sent) * 8.0 / dt / 1000.0,
        transport_recv_kbps: d(prev.transport_bytes_received, cur.transport_bytes_received) * 8.0 / dt / 1000.0,
    }
}

/// Opus frame length implied by the packet rate (one Opus frame per RTP packet).
pub fn frame_ms_from_pps(pps: f64) -> Option<u32> {
    if pps < 5.0 {
        return None;
    }
    let ms = 1000.0 / pps;
    [10u32, 20, 40, 60]
        .into_iter()
        .min_by(|a, b| (f64::from(*a) - ms).abs().partial_cmp(&(f64::from(*b) - ms).abs()).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_report_and_rates() {
        let a = r#"[{"type":"outbound-rtp","id":"O","kind":"audio","timestamp":1000,"packetsSent":100,"bytesSent":6000,"headerBytesSent":1200,"codecId":"C","targetBitrate":510000},
            {"type":"codec","id":"C","mimeType":"audio/opus","clockRate":48000,"channels":1,"sdpFmtpLine":"useinbandfec=1"},
            {"type":"transport","id":"T","selectedCandidatePairId":"P","bytesSent":9000},
            {"type":"candidate-pair","id":"P","localCandidateId":"L","remoteCandidateId":"R","currentRoundTripTime":0.002},
            {"type":"local-candidate","id":"L","address":"::1","port":5000,"candidateType":"host","protocol":"udp"},
            {"type":"remote-candidate","id":"R","address":"::1","port":5001,"candidateType":"host","protocol":"udp"}]"#;
        let b = a
            .replace("\"timestamp\":1000", "\"timestamp\":2000")
            .replace("\"packetsSent\":100", "\"packetsSent\":200")
            .replace("\"bytesSent\":6000", "\"bytesSent\":66000");
        let ra = StatsReport::parse(a).unwrap();
        let rb = StatsReport::parse(&b).unwrap();
        assert_eq!(ra.codec_mime, "audio/opus");
        assert_eq!(ra.local.as_ref().unwrap().address, "::1");
        let r = rates(&ra, &rb);
        assert_eq!(r.send_pps, 100.0);
        assert_eq!(frame_ms_from_pps(r.send_pps), Some(10));
        assert!((r.opus_payload_kbps - 480.0).abs() < 1e-6);
        assert_eq!(frame_ms_from_pps(50.0), Some(20));
    }
}
