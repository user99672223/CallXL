//! SDP munging. Rust owns SDP (CLAUDE.md §3 "SDP fmtp" row); libwebrtc only ever sees
//! descriptions that went through these functions.

/// The Opus `a=fmtp` parameters from CLAUDE.md §3.
pub const OPUS_FMTP: &str = "minptime=10;useinbandfec=1;maxaveragebitrate=510000;stereo=0;sprop-stereo=0;\
maxplaybackrate=48000;usedtx=0;cbr=0;ptime=10;maxptime=20";

fn lines(sdp: &str) -> Vec<&str> {
    sdp.split('\n').map(|l| l.trim_end_matches('\r')).filter(|l| !l.is_empty()).collect()
}

fn join(lines: &[String]) -> String {
    let mut s = lines.join("\r\n");
    s.push_str("\r\n");
    s
}

fn payload_of<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.strip_prefix(prefix).and_then(|r| r.split([' ', ':']).next())
}

/// Applies the §3 Opus profile to every audio section: §3 fmtp, `a=ptime:10`,
/// `a=maxptime:20`, Opus first in the payload list, RED removed. `max_bitrate_bps`
/// replaces `maxaveragebitrate` (user slider, 32..=510 kbps).
pub fn apply_opus_profile(sdp: &str, max_bitrate_bps: u32) -> String {
    let input = lines(sdp);
    let fmtp = OPUS_FMTP.replace("maxaveragebitrate=510000", &format!("maxaveragebitrate={max_bitrate_bps}"));
    // Find payload types.
    let mut opus_pts = Vec::new();
    let mut red_pts = Vec::new();
    for l in &input {
        if let Some(rest) = l.strip_prefix("a=rtpmap:") {
            let mut it = rest.splitn(2, ' ');
            let pt = it.next().unwrap_or("").to_string();
            let codec = it.next().unwrap_or("").to_ascii_lowercase();
            if codec.starts_with("opus/") {
                opus_pts.push(pt);
            } else if codec.starts_with("red/") {
                red_pts.push(pt);
            }
        }
    }
    let mut out: Vec<String> = Vec::with_capacity(input.len() + 4);
    let mut in_audio = false;
    let flush_audio_extras = |out: &mut Vec<String>| {
        out.push("a=ptime:10".into());
        out.push("a=maxptime:20".into());
    };
    for l in input {
        if l.starts_with("m=") {
            if in_audio {
                flush_audio_extras(&mut out);
            }
            in_audio = l.starts_with("m=audio");
            if in_audio {
                // m=audio <port> <proto> <pt...>
                let parts: Vec<&str> = l.split(' ').collect();
                if parts.len() > 3 {
                    let mut pts: Vec<&str> =
                        parts[3..].iter().copied().filter(|p| !red_pts.iter().any(|r| r == p)).collect();
                    pts.sort_by_key(|p| if opus_pts.iter().any(|o| o == p) { 0 } else { 1 });
                    out.push(format!("{} {} {} {}", parts[0], parts[1], parts[2], pts.join(" ")));
                    continue;
                }
            }
            out.push(l.to_string());
            continue;
        }
        if in_audio {
            if l.starts_with("a=ptime:") || l.starts_with("a=maxptime:") {
                continue;
            }
            let pt =
                payload_of(l, "a=rtpmap:").or_else(|| payload_of(l, "a=fmtp:")).or_else(|| payload_of(l, "a=rtcp-fb:"));
            if let Some(pt) = pt {
                if red_pts.iter().any(|r| r == pt) {
                    continue;
                }
                if l.starts_with("a=fmtp:") && opus_pts.iter().any(|o| o == pt) {
                    continue; // re-emitted right after its rtpmap
                }
            }
            out.push(l.to_string());
            if let Some(pt) = payload_of(l, "a=rtpmap:")
                && opus_pts.iter().any(|o| o == pt)
            {
                out.push(format!("a=fmtp:{pt} {fmtp}"));
            }
            continue;
        }
        out.push(l.to_string());
    }
    if in_audio {
        flush_audio_extras(&mut out);
    }
    join(&out)
}

/// Removes `a=candidate` / `a=end-of-candidates`: candidates always travel as signed,
/// encrypted trickle messages (and can be paced / rewritten individually).
pub fn strip_candidates(sdp: &str) -> String {
    let out: Vec<String> = lines(sdp)
        .into_iter()
        .filter(|l| !l.starts_with("a=candidate:") && !l.starts_with("a=end-of-candidates"))
        .map(str::to_string)
        .collect();
    join(&out)
}

/// The DTLS fingerprint (`a=fingerprint:`), normalised to `"<alg> <HEX:HEX…>"`.
/// Returns `None` if absent or if sections disagree (which libwebrtc would also reject).
pub fn fingerprint(sdp: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for l in lines(sdp) {
        if let Some(rest) = l.strip_prefix("a=fingerprint:") {
            let mut it = rest.split_whitespace();
            let alg = it.next()?.to_ascii_lowercase();
            let hex = it.next()?.to_ascii_uppercase();
            let fp = format!("{alg} {hex}");
            match &found {
                Some(f) if *f != fp => return None,
                _ => found = Some(fp),
            }
        }
    }
    found
}

/// The Opus fmtp line content of the first audio section (diagnostics).
pub fn opus_fmtp(sdp: &str) -> Option<String> {
    let ls = lines(sdp);
    let pt = ls.iter().find_map(|l| {
        let rest = l.strip_prefix("a=rtpmap:")?;
        let (pt, codec) = rest.split_once(' ')?;
        codec.to_ascii_lowercase().starts_with("opus/").then(|| pt.to_string())
    })?;
    let prefix = format!("a=fmtp:{pt} ");
    ls.iter().find_map(|l| l.strip_prefix(&prefix).map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFER: &str = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0 1\r\n\
m=audio 9 UDP/TLS/RTP/SAVPF 63 111 9 0\r\nc=IN IP4 0.0.0.0\r\na=mid:0\r\n\
a=fingerprint:sha-256 aa:bb:cc\r\na=rtpmap:111 opus/48000/2\r\na=rtcp-fb:111 transport-cc\r\n\
a=fmtp:111 minptime=10;useinbandfec=1\r\na=rtpmap:63 red/48000/2\r\na=fmtp:63 111/111\r\n\
a=rtpmap:9 G722/8000\r\na=rtpmap:0 PCMU/8000\r\na=candidate:1 1 udp 1 127.0.0.1 5000 typ host\r\n\
m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=mid:1\r\na=fingerprint:sha-256 AA:BB:CC\r\n";

    #[test]
    fn opus_profile_applied() {
        let s = apply_opus_profile(OFFER, 510_000);
        assert!(s.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111 9 0\r\n"), "{s}");
        assert!(s.contains(&format!("a=fmtp:111 {OPUS_FMTP}\r\n")));
        assert!(!s.contains("red/48000"));
        assert!(!s.contains("a=fmtp:63"));
        assert_eq!(s.matches("a=ptime:10").count(), 1);
        assert_eq!(s.matches("a=maxptime:20").count(), 1);
        // ptime lines are inside the audio section, before the application m-line.
        assert!(s.find("a=ptime:10").unwrap() < s.find("m=application").unwrap());
        assert_eq!(opus_fmtp(&s).unwrap(), OPUS_FMTP);
        // Idempotent.
        assert_eq!(apply_opus_profile(&s, 510_000), s);
    }

    #[test]
    fn bitrate_ceiling_in_fmtp() {
        let s = apply_opus_profile(OFFER, 128_000);
        assert!(opus_fmtp(&s).unwrap().contains("maxaveragebitrate=128000"));
    }

    #[test]
    fn candidates_stripped() {
        assert!(!strip_candidates(OFFER).contains("a=candidate"));
    }

    #[test]
    fn fingerprint_parsed_and_normalised() {
        assert_eq!(fingerprint(OFFER).unwrap(), "sha-256 AA:BB:CC");
        let bad = OFFER.replace("AA:BB:CC", "AA:BB:CD");
        assert_eq!(fingerprint(&bad), None);
    }
}
