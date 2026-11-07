//! Path-keyed media policy (CLAUDE.md §3 start-bitrate / frame / jitter rows, §4).

use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PathKind {
    /// host ↔ host
    Lan,
    /// srflx/prflx involved, no relay
    Direct,
    /// a TURN relay candidate on either side
    Relayed,
}

impl PathKind {
    pub fn is_internet(self) -> bool {
        self != PathKind::Lan
    }
}

/// Classifies a selected pair from its candidate types (`host|srflx|prflx|relay`).
/// `force_internet` (test hook `--debug-force-path internet`) classifies host↔host as Direct.
pub fn classify(local_type: &str, remote_type: &str, force_internet: bool) -> PathKind {
    if local_type == "relay" || remote_type == "relay" {
        PathKind::Relayed
    } else if local_type == "host" && remote_type == "host" && !force_internet {
        PathKind::Lan
    } else {
        PathKind::Direct
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathPolicy {
    pub bwe_min_bps: i32,
    pub bwe_start_bps: i32,
    pub bwe_max_bps: i32,
    pub encoder_min_bps: i32,
    pub encoder_max_bps: i32,
    pub jitter_buffer_min_delay: Option<Duration>,
}

/// Audio-only BWE headroom above the Opus ceiling for RTP/SRTP/UDP/IP overhead at
/// 10 ms frames (≈ 100 pkt/s × ~110 B) — CLAUDE.md §3 "max ≈ 600 kbps".
pub const OVERHEAD_HEADROOM_BPS: i32 = 90_000;
pub const OPUS_FLOOR_BPS: i32 = 32_000;
pub const INTERNET_START_BPS: i32 = 128_000;

pub fn policy_for(kind: PathKind, ceiling_kbps: u32) -> PathPolicy {
    let ceiling = (ceiling_kbps.clamp(32, 510) * 1000) as i32;
    let start = match kind {
        PathKind::Lan => ceiling,
        _ => INTERNET_START_BPS.min(ceiling),
    };
    PathPolicy {
        bwe_min_bps: OPUS_FLOOR_BPS,
        bwe_start_bps: start,
        bwe_max_bps: ceiling + OVERHEAD_HEADROOM_BPS,
        encoder_min_bps: OPUS_FLOOR_BPS,
        // The RTP encoding limit applies to the allocation *including* packet overhead
        // (44.8 kbps measured at 100 pkt/s over IPv4); the exact Opus ceiling is enforced
        // inside the audio network adaptor (MediaEngine::set_max_opus_bitrate).
        encoder_max_bps: ceiling + OVERHEAD_HEADROOM_BPS,
        jitter_buffer_min_delay: (kind == PathKind::Lan).then(|| Duration::from_millis(10)),
    }
}

/// Fields of an `a=candidate` line we care about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedCandidate {
    pub address: String,
    pub port: u16,
    pub typ: String,
    pub protocol: String,
}

pub fn parse_candidate(line: &str) -> Option<ParsedCandidate> {
    // candidate:<foundation> <component> <proto> <prio> <addr> <port> typ <type> ...
    let l = line.strip_prefix("a=").unwrap_or(line);
    let l = l.strip_prefix("candidate:")?;
    let parts: Vec<&str> = l.split_whitespace().collect();
    if parts.len() < 8 || parts[6] != "typ" {
        return None;
    }
    Some(ParsedCandidate {
        address: parts[4].to_string(),
        port: parts[5].parse().ok()?,
        typ: parts[7].to_string(),
        protocol: parts[2].to_ascii_lowercase(),
    })
}

/// Rewrites the connection address/port of a candidate line (test relay, D-0003).
pub fn rewrite_candidate(line: &str, address: &str, port: u16) -> Option<String> {
    let prefix_a = line.starts_with("a=");
    let l = line.strip_prefix("a=").unwrap_or(line);
    let mut parts: Vec<String> = l.split_whitespace().map(str::to_string).collect();
    if parts.len() < 8 {
        return None;
    }
    parts[4] = address.to_string();
    parts[5] = port.to_string();
    let s = parts.join(" ");
    Some(if prefix_a { format!("a={s}") } else { s })
}

/// §4: discard Teredo (2001::/32), 6to4 (2002::/16) and unscoped link-local candidates.
pub fn is_discarded_address(addr: &str) -> bool {
    match addr.parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) => {
            let s = v6.segments();
            (s[0] == 0x2001 && s[1] == 0) || s[0] == 0x2002 || (s[0] & 0xffc0) == 0xfe80
        }
        Ok(IpAddr::V4(v4)) => v4.is_link_local(),
        // mDNS-obfuscated (.local) or unparsable: keep, libwebrtc resolves.
        Err(_) => false,
    }
}

pub fn is_v6(addr: &str) -> bool {
    matches!(addr.parse::<IpAddr>(), Ok(IpAddr::V6(_)))
}

/// §4 IPv6 preference: IPv6 candidates go out at once, IPv4 srflx after ~300 ms,
/// relay after ~1 s (host IPv4 is not delayed — it is the LAN path).
pub fn trickle_delay(c: &ParsedCandidate) -> Duration {
    match c.typ.as_str() {
        "relay" => Duration::from_millis(1_000),
        "srflx" | "prflx" if !is_v6(&c.address) => Duration::from_millis(300),
        _ => Duration::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify("host", "host", false), PathKind::Lan);
        assert_eq!(classify("host", "host", true), PathKind::Direct);
        assert_eq!(classify("srflx", "host", false), PathKind::Direct);
        assert_eq!(classify("host", "relay", false), PathKind::Relayed);
    }

    #[test]
    fn policies() {
        let lan = policy_for(PathKind::Lan, 510);
        assert_eq!((lan.bwe_start_bps, lan.bwe_max_bps, lan.encoder_max_bps), (510_000, 600_000, 600_000));
        assert_eq!(lan.jitter_buffer_min_delay, Some(Duration::from_millis(10)));
        let net = policy_for(PathKind::Direct, 510);
        assert_eq!(net.bwe_start_bps, 128_000);
        assert_eq!(net.jitter_buffer_min_delay, None);
        assert_eq!(policy_for(PathKind::Direct, 64).bwe_start_bps, 64_000);
    }

    #[test]
    fn candidates() {
        let l = "candidate:1 1 udp 2122260223 127.0.0.1 50000 typ host generation 0";
        let c = parse_candidate(l).unwrap();
        assert_eq!((c.address.as_str(), c.port, c.typ.as_str()), ("127.0.0.1", 50000, "host"));
        assert_eq!(trickle_delay(&c), Duration::ZERO);
        let r = rewrite_candidate(l, "127.0.0.1", 40000).unwrap();
        assert!(r.contains(" 127.0.0.1 40000 typ host"));
        let s = parse_candidate("candidate:2 1 udp 1 203.0.113.5 3478 typ srflx raddr 0.0.0.0 rport 0").unwrap();
        assert_eq!(trickle_delay(&s), Duration::from_millis(300));
        assert!(is_discarded_address("2001:0:53aa:64c::1"));
        assert!(is_discarded_address("2002:c000:0204::1"));
        assert!(is_discarded_address("fe80::1"));
        assert!(!is_discarded_address("::1"));
        assert!(!is_discarded_address("2a01:4f8::1"));
    }
}
