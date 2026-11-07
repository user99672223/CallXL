# Diagnostics export format (schema v1)

"Export report" on the Diagnostics screen (CLAUDE.md §7) calls
`CallXl::export_diagnostics()`. It returns the report as pretty-printed JSON and writes
the same text to `<download folder>/CallXL-diagnostics-<ts>.json` (`<ts>` = Unix ms).
Users attach that file to bug reports.

- Builder: `crates/core/src/diagnostics/export.rs` (`export::build`, a pure function of
  the core snapshot, the diagnostics snapshot and the log ring).
- Validator: `crates/core/src/diagnostics/schema_v1.rs` (`validate`, depends only on
  `serde_json`; the e2e crate includes the same file with `#[path]`, so both use one
  rule set without the e2e tests linking the core).
- Tests: `cargo test -p callxl-core export_schema_v1` (a real recorded snapshot,
  `crates/core/tests/fixtures/diagnostics-snapshot.json`, captured from
  `diagnostics_complete`) and the e2e test `diagnostics_complete`, which validates a
  live export of a call over an impaired loopback.

## Versioning

`schema` is an integer. Within v1 a writer may only **add** keys; readers must ignore
keys they do not know. Removing or renaming a key, or changing its type or unit, bumps
`schema` to 2 and adds a `schema_v2.rs` validator next to v1 (old reports stay readable).

## Top level

| Key | Type | Meaning |
|---|---|---|
| `schema` | int | `1` |
| `app_version` | string | `callxl-core` crate version |
| `platform` | object | `os`, `arch` (Rust `std::env::consts`), `debug_build` (bool), `dev_app_secret` (bool: built without the `APP_SECRET` CI secret) |
| `ts` | int | Unix ms when the report was generated |
| `call` | object | see below |
| `peers` | array | one entry per peer of the current call, see below |
| `global` | object | global metrics, see below |
| `series` | object | metric name → array of ≤ 60 numbers (1 Hz, oldest first) |
| `log_tail` | array of strings | the newest ≤ 1000 log lines, redacted |

### `call`

| Key | Type | Meaning |
|---|---|---|
| `state` | string | `Idle` · `Joining` · `InCall` · `Reconnecting` · `Left` |
| `reason` | string \| null | reason for `Reconnecting` / `Left` |
| `session_id` | string \| null | current session id (random, not the session name) |
| `muted` | bool | |
| `peer_count` | int | |
| `self` | object | `display_name` (the user's own — the only name in a report), `key` (own Ed25519 public key, 64 hex) |
| `signaling` | object | `transport` (`Lan`/`Cloud`/`None`), `url` (effective signaling URL), `cloud_connected` |
| `settings` | object | `bitrate_ceiling_kbps`, `raw_mic`, `apm` (configured APM, see below) |
| `transfers` | object | counts only: `active`, `completed`, `failed`, `bytes_done` |

### `peers[]`

| Key | Type | Meaning |
|---|---|---|
| `key` | string | peer's Ed25519 public key (64 hex) |
| `tag` | string | first 8 hex of `key`; used in `series` names and redacted logs |
| `connected`, `trusted`, `lan_signaling` | bool | link up · device card pinned · signaling over LAN |
| `path` | object \| null | selected candidate pair: `kind` (`Lan` = host↔host, `Direct`, `Relayed`), `local_addr`, `remote_addr` (`ip:port`), `local_family`, `remote_family` (`V4`/`V6`), `local_candidate_type`, `remote_candidate_type` (`host`/`srflx`/`prflx`/`relay`), `switches` |
| `stats` | object \| null | null until the first stats tick; otherwise the keys below |

`stats` (libwebrtc `GetStats` at 1 Hz plus encoder readback): `rtt_ms`, `jitter_ms`,
`loss_pct` (inbound), `remote_loss_pct` (our stream, as the far end reports it),
`send_kbps`, `recv_kbps`, `opus_frame_ms` (frame size in use, 10/20),
`jitter_buffer_ms`, `concealment_events`, `pair_switches`, `opus_bitrate_kbps`
(encoder target), `opus_payload_kbps`, `available_outgoing_kbps` (BWE), `audio_level`,
`file_rate_limit_kbps` (audio-first file throttle; 0 = unthrottled), `fec_enabled`,
`dtx_enabled`, `codec`, `clock_rate`, `channels`, `opus_complexity`.

### `global`

`uplink_kbps`, `downlink_kbps` (totals), `media_thread_cpu_pct` (libwebrtc threads),
`signaling_transport`, `signaling_latency_ms`, `turn_bytes_month`, `apm` (live
`AudioProcessing::GetConfig()`: `aec`, `noise_suppression`, `ns_level`
`Low`/`Moderate`/`High`, `high_pass`, `agc`), `frame_adaptation_mechanism`,
`ana_frame_ms`, `ana_switches`, `ana_uplink_loss_pct`, `aec_erl_db`, `aec_erle_db`,
`aec_delay_ms` (−1 = n/a), `rejected_signaling`, `glare_events`,
`dropped_media_events`, `stats_ts` (Unix ms of the stats tick).

### `series`

Per-peer metrics are named `<metric>:<tag>`: `rtt_ms`, `jitter_ms`, `loss_pct`,
`send_kbps`, `recv_kbps`, `opus_bitrate_kbps`, `opus_frame_ms`, `jitter_buffer_ms`,
`concealment_events`, `pair_switches`. Global: `uplink_kbps`, `downlink_kbps`,
`media_thread_cpu_pct`, `signaling_latency_ms`, `ana_frame_ms`, `ana_switches`.

## Redaction rules

CLAUDE.md §10: no PII in logs beyond IPs the user can already see.

- **Kept:** IP addresses and ports of the candidate pairs (shown in Diagnostics anyway),
  public keys, the user's own display name, the effective signaling URL.
- **Left out:** peers' display names (peers are `key` + `tag`), session names, chat,
  file names and paths, the trusted-device list, audio device names, data and download
  folder paths.
- **Log lines:** every display name the core knows (call peers, session members,
  trusted devices; at least 2 characters and not the user's own) becomes `peer:<tag>`,
  every transfer file name `<file>`, the home directory `~`; longest match first.

The validator enforces the structural part (no `display_name` in `peers[]` or
`peers[].stats`); the unit test checks that a peer name, a file name, a session name,
an audio device name and the home directory appear nowhere in the text.

## Example (abridged, a real export from `diagnostics_complete`)

```json
{
  "schema": 1,
  "app_version": "0.1.0",
  "platform": { "os": "linux", "arch": "x86_64", "debug_build": true, "dev_app_secret": true },
  "ts": 1794038131974,
  "call": {
    "state": "InCall", "reason": null, "session_id": "a1723d697bbf", "muted": false, "peer_count": 1,
    "self": { "display_name": "alice", "key": "a637f698…" },
    "signaling": { "transport": "Lan", "url": "wss://callxl.<account>.workers.dev/ws", "cloud_connected": false },
    "settings": { "bitrate_ceiling_kbps": 510, "raw_mic": false,
                  "apm": { "aec": true, "noise_suppression": true, "ns_level": "Moderate", "high_pass": true, "agc": true } },
    "transfers": { "active": 0, "completed": 0, "failed": 0, "bytes_done": 0 }
  },
  "peers": [{
    "key": "ce014b40…", "tag": "ce014b40", "connected": true, "trusted": false, "lan_signaling": true,
    "path": { "kind": "Direct", "local_addr": "127.0.0.1:52898", "local_family": "V4", "local_candidate_type": "prflx",
              "remote_addr": "127.0.0.1:60944", "remote_family": "V4", "remote_candidate_type": "host", "switches": 0 },
    "stats": { "rtt_ms": 50.0, "jitter_ms": 2.0, "loss_pct": 0.96, "send_kbps": 230.6, "recv_kbps": 243.8,
               "opus_frame_ms": 10, "jitter_buffer_ms": 34.2, "concealment_events": 8, "pair_switches": 0 }
  }],
  "global": { "uplink_kbps": 246.3, "downlink_kbps": 259.0, "media_thread_cpu_pct": 5.0,
              "signaling_transport": "Lan", "signaling_latency_ms": 0.77, "turn_bytes_month": 0 },
  "series": { "rtt_ms:ce014b40": [50.0, 50.0], "uplink_kbps": [246.3] },
  "log_tail": ["1794038123874 INFO callxl_core::link: connected to ce014b40 in 98 ms"]
}
```

(The proxy netem backend's relay makes the local candidate peer-reflexive, hence
`Direct`; over a plain loopback the same call is `Lan`.)
