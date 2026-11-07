//! The UniFFI API (`CallXl`): the only surface the desktop app, the iOS app and
//! `callxl-cli` use. Every method returns immediately; work happens on the core task.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use callxl_media::{AdmMode, EngineOptions, MediaEngine, NetworkFilter};
use callxl_signal::{DeviceCard as Card, SignalCrypto};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use crate::engine::{Command, Core, DevOptions, start_lan};
use crate::store::Store;
use crate::types::*;
use crate::{config, diag, files, identity, logring};

#[uniffi::export(callback_interface)]
pub trait EventListener: Send + Sync {
    fn on_event(&self, event: Event);
}

/// State shared between the core task and API callers (cheap clones under a lock that
/// is held only for the copy).
pub struct Shared {
    snapshot: RwLock<Snapshot>,
    diagnostics: RwLock<DiagnosticsSnapshot>,
    events: mpsc::UnboundedSender<Event>,
}

impl Shared {
    pub fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }
    pub fn set_snapshot(&self, s: Snapshot) {
        if let Ok(mut w) = self.snapshot.write() {
            *w = s;
        }
    }
    pub fn set_diagnostics(&self, d: DiagnosticsSnapshot) {
        if let Ok(mut w) = self.diagnostics.write() {
            *w = d;
        }
    }
}

pub fn card_to_api(c: &Card) -> DeviceCard {
    DeviceCard { display_name: c.name.clone(), signing_key: c.sign_pk.clone(), agreement_key: c.agree_pk.clone() }
}

fn default_settings(config: &AppConfig) -> Settings {
    Settings {
        display_name: config.device_name.clone(),
        signaling_url: config.signaling_url.clone(),
        effective_signaling_url: config.signaling_url.clone().unwrap_or_else(|| config::DEFAULT_SIGNALING_URL.into()),
        download_dir: if config.download_dir.is_empty() {
            config::default_download_dir()
        } else {
            config.download_dir.clone()
        },
        bitrate_ceiling_kbps: config::MAX_CEILING_KBPS,
        apm: ApmConfig::default(),
        raw_mic: false,
        input_device: None,
        output_device: None,
    }
}

fn empty_diagnostics() -> DiagnosticsSnapshot {
    DiagnosticsSnapshot {
        ts_ms: 0,
        peers: Vec::new(),
        global: GlobalStats {
            uplink_kbps: 0.0,
            downlink_kbps: 0.0,
            media_thread_cpu_pct: 0.0,
            signaling_transport: SignalingTransport::None,
            signaling_latency_ms: 0.0,
            turn_bytes_month: 0,
            apm: ApmConfig::default(),
            frame_adaptation_mechanism: diag::FRAME_MECHANISM.into(),
            ana_frame_ms: 0,
            ana_switches: 0,
            ana_uplink_loss_pct: 0.0,
            rejected_signaling: 0,
            glare_events: 0,
            aec_erl_db: -1.0,
            aec_erle_db: -1.0,
            aec_delay_ms: -1,
            dropped_media_events: 0,
        },
        series: Vec::new(),
    }
}

/// Interface names that do not carry any of `bind` (so libwebrtc ignores them).
fn ignored_interfaces(bind: &[std::net::IpAddr]) -> Vec<String> {
    let mut keep = std::collections::HashSet::new();
    let mut all = std::collections::HashSet::new();
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        for i in ifs {
            all.insert(i.name.clone());
            if bind.contains(&i.ip()) {
                keep.insert(i.name.clone());
            }
        }
    }
    all.difference(&keep).cloned().collect()
}

struct Running {
    rt: tokio::runtime::Runtime,
}

#[derive(uniffi::Object)]
pub struct CallXl {
    config: AppConfig,
    dev: DevOptions,
    store: Arc<Store>,
    crypto: Arc<SignalCrypto>,
    shared: Arc<Shared>,
    listeners: Arc<RwLock<Vec<Box<dyn EventListener>>>>,
    cmd_tx: mpsc::UnboundedSender<Command>,
    pending: Mutex<Option<(mpsc::UnboundedReceiver<Command>, mpsc::UnboundedReceiver<Event>)>>,
    running: Mutex<Option<Running>>,
    engine: OnceLock<Arc<MediaEngine>>,
    raw_tap: Mutex<Vec<mpsc::UnboundedSender<Event>>>,
    lan_addr: Mutex<Option<std::net::SocketAddr>>,
}

impl CallXl {
    /// Rust-only constructor with developer/test options (used by `callxl-cli`).
    pub fn with_dev_options(config: AppConfig, dev: DevOptions) -> Arc<Self> {
        let data_dir = PathBuf::from(&config.data_dir);
        let store = Arc::new(Store::open(&data_dir).unwrap_or_else(|e| {
            tracing::error!("cannot open the database in {}: {e}; using a temporary one", data_dir.display());
            Store::open(&std::env::temp_dir().join(format!("callxl-{}", uuid::Uuid::new_v4()))).expect("temp db")
        }));
        let id = identity::load_or_create(&data_dir, config.headless, dev.reset_identity);
        let crypto = Arc::new(SignalCrypto::new(id));
        let mut settings: Settings = store
            .get("settings")
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_else(|| default_settings(&config));
        if !config.device_name.is_empty() && store.get("settings").is_none() {
            settings.display_name = config.device_name.clone();
        }
        if config.signaling_url.is_some() {
            settings.signaling_url = config.signaling_url.clone();
            settings.effective_signaling_url = config.signaling_url.clone().unwrap_or_default();
        }
        if !config.download_dir.is_empty() {
            settings.download_dir = config.download_dir.clone();
        }
        store.set("settings", &serde_json::to_string(&settings).unwrap_or_default());
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let card = card_to_api(&crypto.card(&settings.display_name));
        let snapshot = Snapshot {
            device: card,
            sessions: Vec::new(),
            current_session: None,
            call_state: CallState::Idle,
            peers: Vec::new(),
            muted: false,
            trusted_devices: Vec::new(),
            transfers: Vec::new(),
            settings,
            cloud_connected: false,
        };
        let shared = Arc::new(Shared {
            snapshot: RwLock::new(snapshot),
            diagnostics: RwLock::new(empty_diagnostics()),
            events: ev_tx,
        });
        Arc::new(Self {
            config,
            dev,
            store,
            crypto,
            shared,
            listeners: Arc::new(RwLock::new(Vec::new())),
            cmd_tx,
            pending: Mutex::new(Some((cmd_rx, ev_rx))),
            running: Mutex::new(None),
            engine: OnceLock::new(),
            raw_tap: Mutex::new(Vec::new()),
            lan_addr: Mutex::new(None),
        })
    }

    /// Rust-only: a channel receiving every event (the CLI forwards them as JSON).
    pub fn event_stream(&self) -> mpsc::UnboundedReceiver<Event> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.raw_tap.lock().unwrap().push(tx);
        rx
    }

    /// Rust-only debug hooks for the e2e harness (`renegotiate`, `link_info`, ...).
    pub fn debug_request(&self, name: &str, args: Value) -> oneshot::Receiver<Result<Value, String>> {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmd_tx.send(Command::Debug { name: name.to_string(), args, reply: tx });
        rx
    }

    fn send(&self, c: Command) {
        let _ = self.cmd_tx.send(c);
    }

    fn start_inner(&self) -> Result<(), String> {
        let (cmd_rx, ev_rx) = self.pending.lock().unwrap().take().ok_or("already started")?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("callxl-core")
            .enable_all()
            .build()
            .map_err(|e| format!("could not start worker threads: {e}"))?;

        let adm = match &self.dev.test_adm {
            Some((play, record)) => {
                AdmMode::Test { play_wav: play.clone(), record_wav: record.clone(), record_max_secs: 300 }
            }
            None => AdmMode::Platform,
        };
        let network = if self.dev.bind.is_empty() {
            NetworkFilter::default()
        } else {
            NetworkFilter {
                ignore_mask: Some(if self.dev.bind.iter().any(|a| a.is_loopback()) { 0 } else { 1 << 4 }),
                ignore_interfaces: ignored_interfaces(&self.dev.bind),
            }
        };
        let settings = self.shared.snapshot.read().unwrap().settings.clone();
        let engine = Arc::new(
            MediaEngine::new(EngineOptions {
                adm,
                network,
                apm: settings.apm.to_media(),
                ana: callxl_media::AnaPolicy {
                    // Diagnostic switch only (comparisons in DECISIONS.md); default on.
                    enabled: std::env::var("CALLXL_DISABLE_ANA").is_err(),
                    ..Default::default()
                },
                // Diagnostic override (first match wins): CALLXL_EXTRA_FIELD_TRIALS.
                field_trials: format!(
                    "{}{}",
                    std::env::var("CALLXL_EXTRA_FIELD_TRIALS").unwrap_or_default(),
                    callxl_media::engine_default_field_trials()
                ),
                ..Default::default()
            })
            .map_err(|e| e.to_string())?,
        );
        let mic = engine.create_audio_track(settings.apm.to_media()).map_err(|e| e.to_string())?;
        let _ = self.engine.set(engine.clone());

        // Event dispatcher: listeners run here, never on the core task.
        let listeners = self.listeners.clone();
        let taps: Vec<_> = self.raw_tap.lock().unwrap().clone();
        let mut ev_rx = ev_rx;
        rt.spawn(async move {
            while let Some(e) = ev_rx.recv().await {
                for t in &taps {
                    let _ = t.send(e.clone());
                }
                let ls = listeners.read().unwrap();
                for l in ls.iter() {
                    l.on_event(e.clone());
                }
            }
        });

        let shared = self.shared.clone();
        let store = self.store.clone();
        let crypto = self.crypto.clone();
        let dev = self.dev.clone();
        let rt_handle = rt.handle().clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Option<std::net::SocketAddr>>();
        rt_handle.spawn(async move {
            let (media_tx, media_rx) = mpsc::channel(8192);
            let (internal_tx, internal_rx) = mpsc::unbounded_channel();
            let (lan_tx, lan_rx) = mpsc::channel(1024);
            let (disc_tx, disc_rx) = mpsc::channel(64);
            let (cloud_tx, cloud_rx) = mpsc::channel(1024);
            let (lan, discovery) = start_lan(&dev, &crypto, &settings.display_name, lan_tx, disc_tx).await;
            let lan_addr = lan.as_ref().map(|l| l.local_addr());
            let (views_tx, views_rx) = watch::channel(diag::DiagInputs::default());
            diag::spawn(engine.clone(), views_rx, internal_tx.clone());
            let transfers = files::Transfers::load(store.transfers());
            let core = Core {
                shared,
                store,
                crypto,
                instance: uuid::Uuid::new_v4().to_string(),
                dev,
                settings,
                engine,
                mic,
                muted: false,
                media_tx,
                internal_tx,
                lan,
                discovery,
                lan_conn_keys: Default::default(),
                cloud: None,
                cloud_tx,
                dir: Default::default(),
                cloud_sessions: Vec::new(),
                my_session: None,
                call_state: CallState::Idle,
                links: Default::default(),
                peer_ids: Default::default(),
                next_peer_id: 1,
                prompted: Default::default(),
                transfers,
                rejected_signaling: 0,
                glare_events: 0,
                link_views: views_tx,
                last_stats: None,
                stopping: false,
                cloud_url_fn: None,
                dialing: Default::default(),
            };
            let _ = ready_tx.send(lan_addr);
            core.run(cmd_rx, media_rx, internal_rx, lan_rx, disc_rx, cloud_rx).await;
        });
        *self.lan_addr.lock().unwrap() = ready_rx.recv_timeout(Duration::from_secs(10)).ok().flatten();
        *self.running.lock().unwrap() = Some(Running { rt });
        Ok(())
    }

    /// Rust-only: the LAN signaling listener address (after `start`).
    pub fn lan_addr(&self) -> Option<std::net::SocketAddr> {
        *self.lan_addr.lock().unwrap()
    }
}

#[uniffi::export]
impl CallXl {
    #[uniffi::constructor]
    pub fn new(config: AppConfig) -> Arc<Self> {
        logring::init("info");
        Self::with_dev_options(config, DevOptions::production())
    }

    /// Spawns the Tokio runtime, the media engine and the core task.
    pub fn start(&self) -> Result<(), CoreError> {
        if self.running.lock().unwrap().is_some() {
            return Err(CoreError::AlreadyStarted);
        }
        self.start_inner().map_err(|reason| CoreError::StartFailed { reason })
    }

    /// Leaves the session, flushes state and stops all threads. Blocks ≤ 5 s.
    /// Must not be called from inside an async runtime thread.
    pub fn stop(&self) {
        let Some(r) = self.running.lock().unwrap().take() else { return };
        let (tx, rx) = oneshot::channel();
        let _ = self.cmd_tx.send(Command::Stop(tx));
        let _ = r.rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), rx).await });
        r.rt.shutdown_timeout(Duration::from_secs(2));
    }

    pub fn subscribe(&self, listener: Box<dyn EventListener>) {
        self.listeners.write().unwrap().push(listener);
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.snapshot.read().unwrap().clone()
    }

    pub fn set_display_name(&self, name: String) {
        self.send(Command::SetDisplayName(name));
    }

    pub fn device_card(&self) -> DeviceCard {
        let name = self.shared.snapshot.read().unwrap().settings.display_name.clone();
        card_to_api(&self.crypto.card(&name))
    }

    pub fn trust_device(&self, signing_key: Vec<u8>) {
        self.send(Command::Trust(hex::encode(signing_key)));
    }

    pub fn untrust_device(&self, signing_key: Vec<u8>) {
        self.send(Command::Untrust(hex::encode(signing_key)));
    }

    pub fn create_session(&self, name: Option<String>) -> SessionId {
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        self.send(Command::CreateSession { id: id.clone(), name });
        id
    }

    pub fn join_session(&self, id: SessionId) {
        self.send(Command::Join(id));
    }

    pub fn leave_session(&self) {
        self.send(Command::Leave);
    }

    pub fn set_muted(&self, muted: bool) {
        self.send(Command::SetMuted(muted));
    }

    pub fn list_audio_devices(&self) -> Vec<AudioDevice> {
        let Some(e) = self.engine.get() else { return Vec::new() };
        let mut seen_in = false;
        let mut seen_out = false;
        e.audio_devices()
            .into_iter()
            .map(|d| {
                let first = if d.input {
                    !std::mem::replace(&mut seen_in, true)
                } else {
                    !std::mem::replace(&mut seen_out, true)
                };
                AudioDevice {
                    kind: if d.input { AudioDeviceKind::Input } else { AudioDeviceKind::Output },
                    id: d.id,
                    name: d.name,
                    is_default: first,
                }
            })
            .collect()
    }

    pub fn set_input_device(&self, id: Option<String>) {
        self.send(Command::SetInputDevice(id));
    }

    pub fn set_output_device(&self, id: Option<String>) {
        self.send(Command::SetOutputDevice(id));
    }

    pub fn set_audio_processing(&self, cfg: ApmConfig) {
        self.send(Command::SetApm(cfg));
    }

    pub fn set_raw_mic(&self, raw: bool) {
        self.send(Command::SetRawMic(raw));
    }

    pub fn set_bitrate_ceiling_kbps(&self, kbps: u32) {
        self.send(Command::SetBitrate(kbps));
    }

    pub fn send_chat(&self, body: String) -> MessageId {
        let id = uuid::Uuid::new_v4().to_string();
        self.send(Command::SendChat { id: id.clone(), body });
        id
    }

    pub fn mark_read(&self, up_to: MessageId) {
        self.send(Command::MarkRead(up_to));
    }

    pub fn chat_history(&self, session: SessionId, limit: u32, before: Option<MessageId>) -> Vec<ChatMessage> {
        self.store.history(&session, limit, before.as_deref())
    }

    pub fn offer_file(&self, path: String, recipients: Vec<PeerKey>) -> TransferId {
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.send(Command::OfferFile { id: id.clone(), path, recipients });
        id
    }

    pub fn respond_file_offer(&self, id: TransferId, accept: bool) {
        self.send(Command::RespondFile { id, accept });
    }

    pub fn cancel_transfer(&self, id: TransferId) {
        self.send(Command::CancelTransfer(id));
    }

    pub fn set_signaling_url(&self, url: Option<String>) {
        self.send(Command::SetSignalingUrl(url));
    }

    pub fn set_download_dir(&self, path: String) {
        self.send(Command::SetDownloadDir(path));
    }

    pub fn settings(&self) -> Settings {
        self.shared.snapshot.read().unwrap().settings.clone()
    }

    pub fn diagnostics(&self) -> DiagnosticsSnapshot {
        self.shared.diagnostics.read().unwrap().clone()
    }

    /// JSON report (diagnostics + state + recent log lines); also written to the
    /// download folder as `callxl-diagnostics-<ts>.json`.
    pub fn export_diagnostics(&self) -> String {
        let snap = self.snapshot();
        let report = json!({
            "app": "CallXL",
            "version": env!("CARGO_PKG_VERSION"),
            "generated_ms": callxl_signal::now_ms(),
            "dev_app_secret": config::app_secret_is_dev(),
            "data_dir": self.config.data_dir,
            "diagnostics": self.diagnostics(),
            "state": {
                "device": snap.device,
                "call_state": snap.call_state,
                "current_session": snap.current_session,
                "peers": snap.peers,
                "sessions": snap.sessions,
                "settings": snap.settings,
                "transfers": snap.transfers,
                "cloud_connected": snap.cloud_connected,
            },
            "log": logring::lines(),
        });
        let text = serde_json::to_string_pretty(&report).unwrap_or_default();
        let dir = PathBuf::from(&snap.settings.download_dir);
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(format!("callxl-diagnostics-{}.json", callxl_signal::now_ms()));
        if let Err(e) = std::fs::write(&file, &text) {
            tracing::warn!("could not write {}: {e}", file.display());
        }
        text
    }
}

impl Drop for CallXl {
    fn drop(&mut self) {
        if let Some(r) = self.running.lock().unwrap().take() {
            r.rt.shutdown_background();
        }
    }
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn f<T: Send + Sync>() {}
    f::<CallXl>();
    let _ = &config::APP_SECRET;
}
