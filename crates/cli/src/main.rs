//! `callxl-cli`: a headless CallXL peer exposing the full core over a newline-delimited
//! JSON control socket (CLAUDE.md §11).
//!
//! Request:  `{"id":1,"cmd":"join_session","args":{"id":"…"}}`
//! Response: `{"id":1,"ok":true,"result":…}` or `{"id":1,"ok":false,"error":"…"}`
//! Events:   `{"event":"PeerJoined","data":{…}}` (pushed to every connected client)
//! On start one line is printed on stdout: `{"ready":true,"control":…,"lan":…,"key":…}`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use callxl_core::{AppConfig, ApmConfig, CallXl, DevOptions};
use clap::Parser;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, broadcast};

#[derive(Parser, Debug)]
#[command(name = "callxl-cli", version, about = "Headless CallXL peer with a JSON control socket")]
struct Args {
    /// unix:///path.sock or tcp://127.0.0.1:PORT
    #[arg(long)]
    control: String,
    #[arg(long)]
    data_dir: String,
    #[arg(long)]
    name: String,
    /// test:play=in.wav,record=out.wav | platform
    #[arg(long, default_value = "platform")]
    adm: String,
    #[arg(long)]
    signaling_url: Option<String>,
    /// Disable cloud signaling.
    #[arg(long)]
    no_cloud: bool,
    /// LAN only: no cloud signaling.
    #[arg(long)]
    lan_only: bool,
    /// Disable LAN discovery and LAN signaling (cloud only).
    #[arg(long)]
    lan_off: bool,
    /// Disable mDNS (direct LAN peers via --lan-peer still work).
    #[arg(long)]
    no_mdns: bool,
    /// Local addresses to use for ICE and LAN signaling (repeatable).
    #[arg(long)]
    bind: Vec<IpAddr>,
    #[arg(long, default_value_t = 0)]
    lan_port: u16,
    /// LAN peer signaling address to connect to directly (repeatable).
    #[arg(long)]
    lan_peer: Vec<SocketAddr>,
    #[arg(long)]
    download_dir: Option<String>,
    /// `internet`: treat host<->host pairs as internet paths (tests).
    #[arg(long)]
    debug_force_path: Option<String>,
    /// tcp://HOST:PORT of the e2e impairment relay control socket.
    #[arg(long)]
    debug_candidate_rewrite: Option<String>,
    /// Discard stored identity keys at start (simulates a reinstall).
    #[arg(long)]
    reset_identity: bool,
    #[arg(long, default_value = "info")]
    log: String,
}

fn parse_adm(s: &str) -> Result<Option<(String, String)>> {
    if s == "platform" {
        return Ok(None);
    }
    let rest = s.strip_prefix("test:").ok_or_else(|| anyhow!("--adm must be 'platform' or 'test:play=…,record=…'"))?;
    let (mut play, mut record) = (String::new(), String::new());
    for kv in rest.split(',') {
        match kv.split_once('=') {
            Some(("play", v)) => play = v.to_string(),
            Some(("record", v)) => record = v.to_string(),
            _ => bail!("unknown --adm option '{kv}'"),
        }
    }
    Ok(Some((play, record)))
}

fn s(args: &Value, k: &str) -> Result<String> {
    args.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| anyhow!("missing string argument '{k}'"))
}
fn opt_s(args: &Value, k: &str) -> Option<String> {
    args.get(k).and_then(Value::as_str).map(str::to_string)
}
fn b(args: &Value, k: &str) -> Result<bool> {
    args.get(k).and_then(Value::as_bool).ok_or_else(|| anyhow!("missing boolean argument '{k}'"))
}
fn key_bytes(args: &Value) -> Result<Vec<u8>> {
    hex::decode(s(args, "signing_key")?).context("signing_key must be hex")
}
fn to<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

async fn dispatch(core: &Arc<CallXl>, cmd: &str, args: &Value) -> Result<Value> {
    Ok(match cmd {
        "snapshot" => to(&core.snapshot()),
        "diagnostics" => to(&core.diagnostics()),
        "export_diagnostics" => Value::String(core.export_diagnostics()),
        "device_card" => to(&core.device_card()),
        "settings" => to(&core.settings()),
        "set_display_name" => {
            core.set_display_name(s(args, "name")?);
            Value::Null
        }
        "trust_device" => {
            core.trust_device(key_bytes(args)?);
            Value::Null
        }
        "untrust_device" => {
            core.untrust_device(key_bytes(args)?);
            Value::Null
        }
        "create_session" => Value::String(core.create_session(opt_s(args, "name"))),
        "join_session" => {
            core.join_session(s(args, "id")?);
            Value::Null
        }
        "leave_session" => {
            core.leave_session();
            Value::Null
        }
        "set_muted" => {
            core.set_muted(b(args, "muted")?);
            Value::Null
        }
        "list_audio_devices" => to(&core.list_audio_devices()),
        "set_input_device" => {
            core.set_input_device(opt_s(args, "id"));
            Value::Null
        }
        "set_output_device" => {
            core.set_output_device(opt_s(args, "id"));
            Value::Null
        }
        "set_audio_processing" => {
            let cfg: ApmConfig = serde_json::from_value(args.get("cfg").cloned().unwrap_or(Value::Null))
                .context("cfg must be an ApmConfig")?;
            core.set_audio_processing(cfg);
            Value::Null
        }
        "set_raw_mic" => {
            core.set_raw_mic(b(args, "raw")?);
            Value::Null
        }
        "set_bitrate_ceiling_kbps" => {
            let k = args.get("kbps").and_then(Value::as_u64).ok_or_else(|| anyhow!("missing number 'kbps'"))?;
            core.set_bitrate_ceiling_kbps(k as u32);
            Value::Null
        }
        "send_chat" => Value::String(core.send_chat(s(args, "body")?)),
        "mark_read" => {
            core.mark_read(s(args, "up_to")?);
            Value::Null
        }
        "chat_history" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100) as u32;
            to(&core.chat_history(s(args, "session")?, limit, opt_s(args, "before")))
        }
        "offer_file" => {
            let recipients: Vec<String> = serde_json::from_value(args.get("recipients").cloned().unwrap_or(Value::Null))
                .context("recipients must be a list of keys")?;
            Value::String(core.offer_file(s(args, "path")?, recipients))
        }
        "respond_file_offer" => {
            core.respond_file_offer(s(args, "id")?, b(args, "accept")?);
            Value::Null
        }
        "cancel_transfer" => {
            core.cancel_transfer(s(args, "id")?);
            Value::Null
        }
        "set_signaling_url" => {
            core.set_signaling_url(opt_s(args, "url"));
            Value::Null
        }
        "set_download_dir" => {
            core.set_download_dir(s(args, "path")?);
            Value::Null
        }
        "debug" => {
            let name = s(args, "name")?;
            let rx = core.debug_request(&name, args.get("args").cloned().unwrap_or(Value::Null));
            rx.await.map_err(|_| anyhow!("the core stopped"))?.map_err(|e| anyhow!(e))?
        }
        "ping" => json!("pong"),
        other => bail!("unknown command '{other}'"),
    })
}

async fn serve_conn<R, W>(
    core: Arc<CallXl>,
    r: R,
    w: W,
    mut events: broadcast::Receiver<String>,
    stop: Arc<tokio::sync::Notify>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let w = Arc::new(Mutex::new(w));
    let ew = w.clone();
    let ev_task = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(line) => {
                    let mut g = ew.lock().await;
                    if g.write_all(line.as_bytes()).await.is_err() || g.write_all(b"\n").await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let mut g = w.lock().await;
                let _ = g
                    .write_all(format!("{}\n", json!({"ok": false, "error": format!("invalid JSON: {e}")})).as_bytes())
                    .await;
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let cmd = req.get("cmd").and_then(Value::as_str).unwrap_or("").to_string();
        let args = req.get("args").cloned().unwrap_or(Value::Null);
        let resp = if cmd == "stop" {
            json!({"id": id, "ok": true, "result": null})
        } else {
            match dispatch(&core, &cmd, &args).await {
                Ok(v) => json!({"id": id, "ok": true, "result": v}),
                Err(e) => json!({"id": id, "ok": false, "error": e.to_string()}),
            }
        };
        {
            let mut g = w.lock().await;
            let _ = g.write_all(format!("{resp}\n").as_bytes()).await;
            let _ = g.flush().await;
        }
        if cmd == "stop" {
            stop.notify_one();
            break;
        }
    }
    ev_task.abort();
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args = Args::parse();
    callxl_core::logring::init(&args.log);
    let test_adm = parse_adm(&args.adm)?;
    let cloud = !(args.no_cloud || args.lan_only);
    let relay = match &args.debug_candidate_rewrite {
        Some(u) => Some(u.strip_prefix("tcp://").unwrap_or(u).parse::<SocketAddr>().context("--debug-candidate-rewrite")?),
        None => None,
    };
    let dev = DevOptions {
        test_adm,
        bind: args.bind.clone(),
        lan_port: args.lan_port,
        lan_peers: args.lan_peer.clone(),
        mdns: !args.no_mdns && !args.lan_off,
        lan: !args.lan_off,
        cloud,
        force_internet: args.debug_force_path.as_deref() == Some("internet"),
        candidate_rewrite: relay,
        reset_identity: args.reset_identity,
        ice_policy: None,
    };
    let _ = std::fs::create_dir_all(&args.data_dir);
    let config = AppConfig {
        data_dir: args.data_dir.clone(),
        download_dir: args.download_dir.clone().unwrap_or_else(|| format!("{}/downloads", args.data_dir)),
        device_name: args.name.clone(),
        signaling_url: args.signaling_url.clone(),
        headless: true,
    };
    let core = CallXl::with_dev_options(config, dev);
    let mut stream = core.event_stream();
    let core2 = core.clone();
    tokio::task::spawn_blocking(move || core2.start()).await?.map_err(|e| anyhow!(e.to_string()))?;
    let (ev_tx, _) = broadcast::channel::<String>(4096);
    let tx = ev_tx.clone();
    tokio::spawn(async move {
        while let Some(e) = stream.recv().await {
            let line = json!({"event": e.name(), "data": e.data()}).to_string();
            let _ = tx.send(line);
        }
    });

    let stop = Arc::new(tokio::sync::Notify::new());
    let card = core.device_card();
    let control = args.control.clone();
    let lan = core.lan_addr().map(|a| a.to_string());
    if let Some(path) = control.strip_prefix("unix://") {
        let _ = std::fs::remove_file(path);
        let listener = tokio::net::UnixListener::bind(path).with_context(|| format!("bind {path}"))?;
        println!("{}", json!({"ready": true, "control": control, "lan": lan, "key": card.signing_key}));
        let (c, t, st) = (core.clone(), ev_tx.clone(), stop.clone());
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let (r, w) = sock.into_split();
                tokio::spawn(serve_conn(c.clone(), r, w, t.subscribe(), st.clone()));
            }
        });
    } else if let Some(addr) = control.strip_prefix("tcp://") {
        let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("bind {addr}"))?;
        let bound = listener.local_addr()?;
        println!("{}", json!({"ready": true, "control": format!("tcp://{bound}"), "lan": lan, "key": card.signing_key}));
        let (c, t, st) = (core.clone(), ev_tx.clone(), stop.clone());
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let _ = sock.set_nodelay(true);
                let (r, w) = sock.into_split();
                tokio::spawn(serve_conn(c.clone(), r, w, t.subscribe(), st.clone()));
            }
        });
    } else {
        bail!("--control must be unix:///path.sock or tcp://HOST:PORT");
    }

    tokio::select! {
        _ = stop.notified() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    let c = core.clone();
    tokio::task::spawn_blocking(move || c.stop()).await?;
    Ok(())
}
