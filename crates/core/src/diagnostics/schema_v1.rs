//! Validator for the diagnostics export, schema v1 (docs/diagnostics-export.md).
//!
//! Self-contained (only `serde_json`): `callxl-core` uses it in its unit tests and the
//! e2e crate includes this very file with `#[path]`, so both check the same rules
//! without the e2e tests linking the core (and libwebrtc).

use serde_json::{Map, Value};

pub const SCHEMA_VERSION: u64 = 1;
/// Longest sparkline (60 s at 1 Hz, CLAUDE.md §7).
pub const MAX_SERIES_LEN: usize = 60;
/// Most log lines an export carries.
pub const MAX_LOG_TAIL: usize = 1000;

const PEER_STATS_NUM: &[&str] = &[
    "rtt_ms",
    "jitter_ms",
    "loss_pct",
    "remote_loss_pct",
    "send_kbps",
    "recv_kbps",
    "jitter_buffer_ms",
    "opus_bitrate_kbps",
    "opus_payload_kbps",
    "available_outgoing_kbps",
    "audio_level",
    "file_rate_limit_kbps",
];
const PEER_STATS_INT: &[&str] =
    &["opus_frame_ms", "concealment_events", "pair_switches", "clock_rate", "channels", "opus_complexity"];
const GLOBAL_NUM: &[&str] = &[
    "uplink_kbps",
    "downlink_kbps",
    "media_thread_cpu_pct",
    "signaling_latency_ms",
    "ana_uplink_loss_pct",
    "aec_erl_db",
    "aec_erle_db",
];
const GLOBAL_INT: &[&str] =
    &["turn_bytes_month", "ana_frame_ms", "ana_switches", "rejected_signaling", "glare_events", "dropped_media_events"];

struct Check {
    errors: Vec<String>,
}

impl Check {
    fn err(&mut self, at: &str, msg: impl std::fmt::Display) {
        self.errors.push(format!("{at}: {msg}"));
    }
    fn obj<'a>(&mut self, v: &'a Value, at: &str) -> Option<&'a Map<String, Value>> {
        let o = v.as_object();
        if o.is_none() {
            self.err(at, "expected an object");
        }
        o
    }
    fn field<'a>(&mut self, o: &'a Map<String, Value>, at: &str, k: &str) -> Option<&'a Value> {
        let v = o.get(k);
        if v.is_none() {
            self.err(&format!("{at}.{k}"), "missing");
        }
        v
    }
    fn string(&mut self, o: &Map<String, Value>, at: &str, k: &str) -> Option<String> {
        let v = self.field(o, at, k)?;
        let s = v.as_str().map(str::to_string);
        if s.is_none() {
            self.err(&format!("{at}.{k}"), format!("expected a string, got {v}"));
        }
        s
    }
    fn opt_string(&mut self, o: &Map<String, Value>, at: &str, k: &str) {
        if let Some(v) = self.field(o, at, k)
            && !(v.is_null() || v.is_string())
        {
            self.err(&format!("{at}.{k}"), format!("expected a string or null, got {v}"));
        }
    }
    fn boolean(&mut self, o: &Map<String, Value>, at: &str, k: &str) {
        if let Some(v) = self.field(o, at, k)
            && !v.is_boolean()
        {
            self.err(&format!("{at}.{k}"), format!("expected a boolean, got {v}"));
        }
    }
    fn number(&mut self, o: &Map<String, Value>, at: &str, k: &str) {
        if let Some(v) = self.field(o, at, k)
            && !v.as_f64().is_some_and(f64::is_finite)
        {
            self.err(&format!("{at}.{k}"), format!("expected a finite number, got {v}"));
        }
    }
    fn int(&mut self, o: &Map<String, Value>, at: &str, k: &str) {
        if let Some(v) = self.field(o, at, k)
            && !(v.is_u64() || v.is_i64())
        {
            self.err(&format!("{at}.{k}"), format!("expected an integer, got {v}"));
        }
    }
    fn one_of(&mut self, o: &Map<String, Value>, at: &str, k: &str, allowed: &[&str]) {
        if let Some(s) = self.string(o, at, k)
            && !allowed.contains(&s.as_str())
        {
            self.err(&format!("{at}.{k}"), format!("{s:?} is not one of {allowed:?}"));
        }
    }
    fn hex(&mut self, o: &Map<String, Value>, at: &str, k: &str, len: usize) {
        if let Some(s) = self.string(o, at, k)
            && !(s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            self.err(&format!("{at}.{k}"), format!("expected {len} hex digits, got {s:?}"));
        }
    }
    fn apm(&mut self, v: &Value, at: &str) {
        let Some(o) = self.obj(v, at) else { return };
        for k in ["aec", "noise_suppression", "high_pass", "agc"] {
            self.boolean(o, at, k);
        }
        self.one_of(o, at, "ns_level", &["Low", "Moderate", "High"]);
    }
}

fn path(c: &mut Check, v: &Value, at: &str) {
    if v.is_null() {
        return;
    }
    let Some(o) = c.obj(v, at) else { return };
    c.one_of(o, at, "kind", &["Lan", "Direct", "Relayed"]);
    for side in ["local", "remote"] {
        if let Some(a) = c.string(o, at, &format!("{side}_addr"))
            && a.parse::<std::net::SocketAddr>().is_err()
        {
            c.err(&format!("{at}.{side}_addr"), format!("{a:?} is not ip:port"));
        }
        c.one_of(o, at, &format!("{side}_family"), &["V4", "V6"]);
        c.one_of(o, at, &format!("{side}_candidate_type"), &["host", "srflx", "prflx", "relay"]);
    }
}

fn peer(c: &mut Check, v: &Value, at: &str) {
    let Some(o) = c.obj(v, at) else { return };
    // Redaction rule: only the user's own display name may appear in an export.
    if o.contains_key("display_name") {
        c.err(at, "peers must not carry a display_name (redaction rule)");
    }
    c.hex(o, at, "key", 64);
    c.hex(o, at, "tag", 8);
    for k in ["connected", "trusted", "lan_signaling"] {
        c.boolean(o, at, k);
    }
    if let Some(p) = c.field(o, at, "path") {
        path(c, p, &format!("{at}.path"));
    }
    let Some(s) = c.field(o, at, "stats") else { return };
    if s.is_null() {
        return;
    }
    let sat = format!("{at}.stats");
    let Some(so) = c.obj(s, &sat) else { return };
    if so.contains_key("display_name") {
        c.err(&sat, "stats must not carry a display_name (redaction rule)");
    }
    for k in PEER_STATS_NUM {
        c.number(so, &sat, k);
    }
    for k in PEER_STATS_INT {
        c.int(so, &sat, k);
    }
    c.boolean(so, &sat, "fec_enabled");
    c.boolean(so, &sat, "dtx_enabled");
    c.string(so, &sat, "codec");
}

/// Checks `v` against schema v1. Returns every violation found (empty = valid).
pub fn validate(v: &Value) -> Result<(), Vec<String>> {
    let mut c = Check { errors: Vec::new() };
    let Some(top) = c.obj(v, "$") else { return Err(c.errors) };
    match top.get("schema").and_then(Value::as_u64) {
        Some(SCHEMA_VERSION) => {}
        other => c.err("$.schema", format!("expected {SCHEMA_VERSION}, got {other:?}")),
    }
    c.string(top, "$", "app_version");
    c.int(top, "$", "ts");

    if let Some(p) = c.field(top, "$", "platform")
        && let Some(o) = c.obj(p, "$.platform")
    {
        c.string(o, "$.platform", "os");
        c.string(o, "$.platform", "arch");
        c.boolean(o, "$.platform", "debug_build");
        c.boolean(o, "$.platform", "dev_app_secret");
    }

    if let Some(call) = c.field(top, "$", "call")
        && let Some(o) = c.obj(call, "$.call")
    {
        c.one_of(o, "$.call", "state", &["Idle", "Joining", "InCall", "Reconnecting", "Left"]);
        c.opt_string(o, "$.call", "reason");
        c.opt_string(o, "$.call", "session_id");
        c.boolean(o, "$.call", "muted");
        c.int(o, "$.call", "peer_count");
        if let Some(me) = c.field(o, "$.call", "self")
            && let Some(m) = c.obj(me, "$.call.self")
        {
            c.string(m, "$.call.self", "display_name");
            c.hex(m, "$.call.self", "key", 64);
        }
        if let Some(sig) = c.field(o, "$.call", "signaling")
            && let Some(s) = c.obj(sig, "$.call.signaling")
        {
            c.one_of(s, "$.call.signaling", "transport", &["Lan", "Cloud", "None"]);
            c.string(s, "$.call.signaling", "url");
            c.boolean(s, "$.call.signaling", "cloud_connected");
        }
        if let Some(st) = c.field(o, "$.call", "settings")
            && let Some(s) = c.obj(st, "$.call.settings")
        {
            c.int(s, "$.call.settings", "bitrate_ceiling_kbps");
            c.boolean(s, "$.call.settings", "raw_mic");
            if let Some(a) = c.field(s, "$.call.settings", "apm") {
                c.apm(a, "$.call.settings.apm");
            }
        }
        if let Some(t) = c.field(o, "$.call", "transfers")
            && let Some(o) = c.obj(t, "$.call.transfers")
        {
            for k in ["active", "completed", "failed", "bytes_done"] {
                c.int(o, "$.call.transfers", k);
            }
        }
    }

    if let Some(peers) = c.field(top, "$", "peers") {
        match peers.as_array() {
            Some(a) => {
                for (i, p) in a.iter().enumerate() {
                    peer(&mut c, p, &format!("$.peers[{i}]"));
                }
            }
            None => c.err("$.peers", "expected an array"),
        }
    }

    if let Some(g) = c.field(top, "$", "global")
        && let Some(o) = c.obj(g, "$.global")
    {
        for k in GLOBAL_NUM {
            c.number(o, "$.global", k);
        }
        for k in GLOBAL_INT {
            c.int(o, "$.global", k);
        }
        c.int(o, "$.global", "aec_delay_ms");
        c.int(o, "$.global", "stats_ts");
        c.one_of(o, "$.global", "signaling_transport", &["Lan", "Cloud", "None"]);
        c.string(o, "$.global", "frame_adaptation_mechanism");
        if let Some(a) = c.field(o, "$.global", "apm") {
            c.apm(a, "$.global.apm");
        }
    }

    if let Some(s) = c.field(top, "$", "series")
        && let Some(o) = c.obj(s, "$.series")
    {
        for (k, v) in o {
            let at = format!("$.series[{k:?}]");
            match v.as_array() {
                Some(a) if a.len() <= MAX_SERIES_LEN => {
                    if a.iter().any(|x| !x.as_f64().is_some_and(f64::is_finite)) {
                        c.err(&at, "non-numeric sample");
                    }
                }
                Some(a) => c.err(&at, format!("{} samples > {MAX_SERIES_LEN}", a.len())),
                None => c.err(&at, "expected an array of numbers"),
            }
        }
    }

    if let Some(l) = c.field(top, "$", "log_tail") {
        match l.as_array() {
            Some(a) if a.len() <= MAX_LOG_TAIL => {
                if a.iter().any(|x| !x.is_string()) {
                    c.err("$.log_tail", "non-string line");
                }
            }
            Some(a) => c.err("$.log_tail", format!("{} lines > {MAX_LOG_TAIL}", a.len())),
            None => c.err("$.log_tail", "expected an array of strings"),
        }
    }

    if c.errors.is_empty() { Ok(()) } else { Err(c.errors) }
}
