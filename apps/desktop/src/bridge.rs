//! The bridge between the UI thread and `callxl-core` (CLAUDE.md §9, §10).
//!
//! - `on_event` (core dispatcher thread) only forwards the event into this module's
//!   channel; it never blocks and never calls back into the core.
//! - One `callxl-ui-bridge` thread owns the `CallXl` handle: it runs every command the UI
//!   sends, turns events and snapshots into `Send` row data (`view.rs`) and posts small
//!   patches to the UI thread with `slint::invoke_from_event_loop`.
//! - The UI thread only applies patches to models (diffed, so unchanged rows are never
//!   touched and tiles never re-layout).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use callxl_core::{
    ApmConfig, AudioDevice, AudioDeviceKind, CallXl, ChatMessage, DiagnosticsSnapshot, Event, EventListener, NsLevel,
    ReceiptStatus, Snapshot, TransferDirection, TransferInfo, TransferState,
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::platform::{self, Notifier, Prefs};
use crate::view::{self, PeerDiagRow};
use crate::{App, AppWindow, AudioDeviceItem, ChatItem, Colors, PeerDiag, RecipientItem, Tray};

/// A change to apply on the UI thread.
pub type Patch = Box<dyn FnOnce(&AppWindow, Option<&Tray>) + Send>;

/// Commands from the UI (callbacks) to the bridge thread.
#[derive(Debug)]
pub enum UiCmd {
    CreateSession(String),
    Join(String),
    Leave,
    ToggleMute,
    SetDisplayName(String),
    Trust(String),
    DismissTrust,
    Untrust(String),
    SendChat(String),
    ChatShown,
    RespondOffer(String, bool),
    CancelTransfer(String),
    OpenFolder(String),
    PickFiles,
    FilesChosen(Vec<PathBuf>),
    ToggleRecipient(usize),
    SendToRecipients,
    CancelSend,
    SelectInput(String),
    SelectOutput(String),
    RefreshDevices,
    SetApm(ApmConfig),
    SetRawMic(bool),
    SetBitrate(u32),
    SetDownloadDir(String),
    PickDownloadDir,
    SetSignalingUrl(String),
    SetTheme(bool),
    ExportDiagnostics,
    Focused(bool),
    Quit,
}

pub enum Msg {
    Event(Event),
    Ui(UiCmd),
}

/// Forwards core events into the bridge channel (§10: one non-blocking send).
struct Listener(Sender<Msg>);

impl EventListener for Listener {
    fn on_event(&self, event: Event) {
        let _ = self.0.send(Msg::Event(event));
    }
}

/// Replaces the rows of a `VecModel` with `new`, touching only rows that changed.
pub fn sync<T: Clone + PartialEq + 'static>(model: &ModelRc<T>, new: Vec<T>) {
    let Some(vm) = model.as_any().downcast_ref::<VecModel<T>>() else { return };
    if vm.row_count() == new.len() {
        for (i, r) in new.into_iter().enumerate() {
            if vm.row_data(i).as_ref() != Some(&r) {
                vm.set_row_data(i, r);
            }
        }
    } else {
        vm.set_vec(new);
    }
}

/// Installs empty `VecModel`s for every list so patches can update them in place.
pub fn init_models(w: &AppWindow) {
    let app = w.global::<App>();
    app.set_sessions(ModelRc::new(VecModel::default()));
    app.set_trusted(ModelRc::new(VecModel::default()));
    app.set_peers(ModelRc::new(VecModel::default()));
    app.set_chat(ModelRc::new(VecModel::default()));
    app.set_inputs(ModelRc::new(VecModel::default()));
    app.set_outputs(ModelRc::new(VecModel::default()));
    app.set_diag_peers(ModelRc::new(VecModel::default()));
    app.set_diag_global(ModelRc::new(VecModel::default()));
    app.set_recipients(ModelRc::new(VecModel::default()));
    app.set_version(env!("CARGO_PKG_VERSION").into());
}

fn patch(f: impl FnOnce(&AppWindow, Option<&Tray>) + Send + 'static) -> Patch {
    Box::new(f)
}

/// Pure UI state derived from the core (lives on the bridge thread). Every method returns
/// the patches that bring the UI up to date; it never calls the core itself, so the
/// screenshot tests drive exactly the same code with fake snapshots.
#[derive(Default)]
pub struct ViewModel {
    snap: Option<Snapshot>,
    chat_session: Option<String>,
    chat_ids: Vec<String>,
    chat_index: HashMap<String, usize>,
    chat_rows: Vec<ChatItem>,
    trust_queue: VecDeque<(String, String, String, bool)>,
    offer_queue: VecDeque<TransferInfo>,
    pending_files: Vec<PathBuf>,
    recipients: Vec<RecipientItem>,
    devices: Vec<AudioDevice>,
    pub focused: bool,
    peer_names: HashMap<String, String>,
}

impl ViewModel {
    pub fn new() -> Self {
        Self { focused: true, ..Default::default() }
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snap.as_ref()
    }

    fn name_of(&self, key: &str) -> String {
        self.peer_names.get(key).cloned().unwrap_or_else(|| format!("{}…", &key[..key.len().min(8)]))
    }

    /// The chat session that should be loaded when it differs from the loaded one.
    pub fn wanted_chat_session(&self, s: &Snapshot) -> Option<Option<String>> {
        let want = s.current_session.clone().or_else(|| self.chat_session.clone());
        if want != self.chat_session { Some(want) } else { None }
    }

    /// Diffs a fresh snapshot against the last one and returns the patches.
    pub fn on_snapshot(&mut self, s: Snapshot) -> Vec<Patch> {
        let mut out = Vec::new();
        let old = self.snap.take();
        let changed = |f: &dyn Fn(&Snapshot) -> bool| old.as_ref().is_none_or(f);

        for p in &s.peers {
            self.peer_names.insert(p.key.clone(), p.display_name.clone());
        }
        for d in &s.trusted_devices {
            self.peer_names.entry(d.key.clone()).or_insert_with(|| d.display_name.clone());
        }
        for ses in &s.sessions {
            for m in &ses.members {
                self.peer_names.entry(m.key.clone()).or_insert_with(|| m.display_name.clone());
            }
        }

        if changed(&|o| o.settings.display_name != s.settings.display_name || o.device != s.device) {
            let name: SharedString = s.settings.display_name.as_str().into();
            let ini: SharedString = view::initials(&s.settings.display_name).into();
            let hue = view::hue(&s.device.signing_key);
            let key = s.device.signing_key.clone();
            let short: SharedString = key
                .as_bytes()
                .chunks(4)
                .take(8)
                .map(|c| std::str::from_utf8(c).unwrap_or(""))
                .collect::<Vec<_>>()
                .join(" ")
                .into();
            out.push(patch(move |w, _| {
                let app = w.global::<App>();
                app.set_display_name(name);
                app.set_device_initials(ini);
                app.set_device_hue(hue);
                app.set_safety_key(short);
            }));
        }
        if changed(&|o| o.sessions != s.sessions) {
            let rows = view::session_items(&s.sessions);
            out.push(patch(move |w, _| sync(&w.global::<App>().get_sessions(), rows)));
        }
        if changed(&|o| o.trusted_devices != s.trusted_devices) {
            let rows = view::device_items(&s.trusted_devices);
            out.push(patch(move |w, _| sync(&w.global::<App>().get_trusted(), rows)));
        }
        if changed(&|o| o.cloud_connected != s.cloud_connected) {
            let status = if s.cloud_connected { "Your network and the internet" } else { "Your local network" };
            out.push(patch(move |w, _| w.global::<App>().set_network_status(status.into())));
        }
        if changed(&|o| {
            o.peers != s.peers
                || o.muted != s.muted
                || o.call_state != s.call_state
                || o.current_session != s.current_session
                || o.sessions != s.sessions
                || o.settings.display_name != s.settings.display_name
        }) {
            let was_in_call = old.as_ref().is_some_and(view::in_call);
            let in_call = view::in_call(&s);
            let rows = if in_call { view::peer_items(&s) } else { Vec::new() };
            let (status, reconnecting) = view::call_status(&s.call_state, s.peers.len());
            let title = s
                .current_session
                .as_ref()
                .and_then(|id| s.sessions.iter().find(|x| &x.id == id))
                .map(view::session_title)
                .unwrap_or_else(|| "Session".into());
            let muted = s.muted;
            out.push(patch(move |w, tray| {
                let app = w.global::<App>();
                sync(&app.get_peers(), rows);
                app.set_in_call(in_call);
                app.set_call_status(status.into());
                app.set_call_reconnecting(reconnecting);
                app.set_call_title(title.as_str().into());
                app.set_chat_title(if in_call { title.as_str().into() } else { "Chat".into() });
                app.set_muted(muted);
                if in_call && !was_in_call {
                    app.set_screen(1);
                }
                if !in_call && was_in_call {
                    if app.get_screen() == 1 {
                        app.set_screen(0);
                    }
                    app.set_drawer_open(false);
                }
                if let Some(t) = tray {
                    t.set_in_call(in_call);
                    t.set_muted(muted);
                }
            }));
        }
        if changed(&|o| o.settings != s.settings) {
            let st = s.settings.clone();
            out.push(patch(move |w, _| {
                let app = w.global::<App>();
                app.set_aec(st.apm.aec);
                app.set_ns(st.apm.noise_suppression);
                app.set_ns_level(match st.apm.ns_level {
                    NsLevel::Low => 0,
                    NsLevel::Moderate => 1,
                    NsLevel::High => 2,
                });
                app.set_hpf(st.apm.high_pass);
                app.set_agc(st.apm.agc);
                app.set_raw_mic(st.raw_mic);
                app.set_bitrate(st.bitrate_ceiling_kbps as i32);
                app.set_download_dir(st.download_dir.as_str().into());
                app.set_signaling_url(st.signaling_url.clone().unwrap_or_default().into());
                app.set_effective_signaling_url(st.effective_signaling_url.as_str().into());
            }));
        }
        // Transfers of the loaded chat session become (or update) file cards.
        let prev: HashMap<&str, &TransferInfo> =
            old.as_ref().map(|o| o.transfers.iter().map(|t| (t.id.as_str(), t)).collect()).unwrap_or_default();
        let fresh: Vec<TransferInfo> =
            s.transfers.iter().filter(|t| prev.get(t.id.as_str()) != Some(t)).cloned().collect();
        self.snap = Some(s);
        for t in fresh {
            out.extend(self.upsert_transfer(&t));
        }
        if old.is_none() {
            out.extend(self.device_patches());
        }
        out
    }

    /// Replaces the chat timeline with a session's history and file cards.
    pub fn load_chat(&mut self, session: Option<String>, history: Vec<ChatMessage>) -> Vec<Patch> {
        self.chat_session = session.clone();
        self.chat_ids.clear();
        self.chat_index.clear();
        self.chat_rows.clear();
        for m in &history {
            self.push_row(m.id.clone(), view::chat_item(m));
        }
        let transfers: Vec<TransferInfo> = self
            .snap
            .as_ref()
            .map(|s| s.transfers.iter().filter(|t| Some(&t.session) == session.as_ref()).cloned().collect())
            .unwrap_or_default();
        for t in &transfers {
            let name = self.name_of(&t.peer_key);
            self.push_row(t.id.clone(), view::transfer_item(t, &name));
        }
        let rows = self.chat_rows.clone();
        vec![patch(move |w, _| {
            let app = w.global::<App>();
            if let Some(vm) = app.get_chat().as_any().downcast_ref::<VecModel<ChatItem>>() {
                vm.set_vec(rows);
            }
            app.set_unread(0);
        })]
    }

    fn push_row(&mut self, id: String, row: ChatItem) {
        self.chat_index.insert(id.clone(), self.chat_ids.len());
        self.chat_ids.push(id);
        self.chat_rows.push(row);
    }

    /// Adds a row (or updates it in place) and returns the matching patch.
    fn upsert_row(&mut self, id: &str, row: ChatItem) -> Patch {
        if let Some(&i) = self.chat_index.get(id) {
            if self.chat_rows[i] == row {
                return patch(|_, _| {});
            }
            self.chat_rows[i] = row.clone();
            patch(move |w, _| {
                if let Some(vm) = w.global::<App>().get_chat().as_any().downcast_ref::<VecModel<ChatItem>>()
                    && i < vm.row_count()
                {
                    vm.set_row_data(i, row);
                }
            })
        } else {
            self.push_row(id.to_string(), row.clone());
            patch(move |w, _| {
                if let Some(vm) = w.global::<App>().get_chat().as_any().downcast_ref::<VecModel<ChatItem>>() {
                    vm.push(row);
                }
            })
        }
    }

    fn upsert_transfer(&mut self, t: &TransferInfo) -> Vec<Patch> {
        if self.chat_session.as_ref() != Some(&t.session) {
            return Vec::new();
        }
        let name = self.name_of(&t.peer_key);
        vec![self.upsert_row(&t.id, view::transfer_item(t, &name))]
    }

    pub fn own_message(&mut self, id: &str, body: &str) -> Vec<Patch> {
        let (name, session) = match &self.snap {
            Some(s) => (s.settings.display_name.clone(), s.current_session.clone()),
            None => return Vec::new(),
        };
        if session.is_none() || session != self.chat_session {
            return Vec::new();
        }
        let row = view::own_chat_item(id, body, &name, now_ms());
        vec![self.upsert_row(id, row)]
    }

    fn chat_visible_patch(tx: Option<Sender<Msg>>) -> Patch {
        patch(move |w, _| {
            let app = w.global::<App>();
            let visible = app.get_screen() == 2 || app.get_drawer_open();
            if visible {
                if let Some(tx) = tx {
                    let _ = tx.send(Msg::Ui(UiCmd::ChatShown));
                }
            } else {
                app.set_unread(app.get_unread() + 1);
            }
        })
    }

    /// Last incoming message id (for read receipts).
    pub fn last_incoming(&self) -> Option<String> {
        self.chat_ids.iter().zip(&self.chat_rows).rev().find(|(_, r)| !r.own && !r.is_file).map(|(id, _)| id.clone())
    }

    /// Patches for an event (snapshot-driven state is handled by `on_snapshot`).
    pub fn on_event(&mut self, e: &Event, tx: Option<Sender<Msg>>, notifier: Option<&Notifier>) -> Vec<Patch> {
        let mut out = Vec::new();
        match e {
            Event::SpeakingChanged { peer_key, speaking } => {
                if let Some(s) = self.snap.as_mut()
                    && let Some(p) = s.peers.iter_mut().find(|p| &p.key == peer_key)
                {
                    p.speaking = *speaking;
                }
                let (key, sp) = (peer_key.clone(), *speaking);
                out.push(patch(move |w, _| {
                    let m = w.global::<App>().get_peers();
                    for i in 0..m.row_count() {
                        if let Some(mut r) = m.row_data(i)
                            && r.key == key.as_str()
                        {
                            if r.speaking != sp {
                                r.speaking = sp;
                                m.set_row_data(i, r);
                            }
                            break;
                        }
                    }
                }));
            }
            Event::ChatReceived { message } => {
                self.peer_names.insert(message.sender_key.clone(), message.sender_name.clone());
                if Some(&message.session) == self.chat_session.as_ref() {
                    out.push(self.upsert_row(&message.id, view::chat_item(message)));
                    out.push(Self::chat_visible_patch(tx));
                }
                if !self.focused
                    && let Some(n) = notifier
                {
                    n.notify(&message.sender_name, &message.body);
                }
            }
            Event::ReceiptUpdated { message_id, status, .. } => {
                if let Some(&i) = self.chat_index.get(message_id) {
                    let mut row = self.chat_rows[i].clone();
                    let rank = |s: &str| ["Sending", "Sent", "Delivered", "Read"].iter().position(|x| *x == s);
                    let new = match status {
                        ReceiptStatus::Sending => "Sending",
                        ReceiptStatus::Sent => "Sent",
                        ReceiptStatus::Delivered => "Delivered",
                        ReceiptStatus::Read => "Read",
                    };
                    if row.own && rank(new) > rank(row.status.as_str()) {
                        row.status = new.into();
                        out.push(self.upsert_row(message_id, row));
                    }
                }
            }
            Event::FileOffered { transfer } => {
                let from = self.name_of(&transfer.peer_key);
                if !self.focused
                    && let Some(n) = notifier
                {
                    n.notify(
                        &format!("{from} wants to send you a file"),
                        &format!("{} ({})", transfer.name, view::human_bytes(transfer.size)),
                    );
                }
                self.offer_queue.push_back(transfer.clone());
                out.extend(self.upsert_transfer(transfer));
                out.extend(self.offer_patch());
            }
            Event::TransferProgress { transfer } => {
                if let Some(s) = self.snap.as_mut() {
                    match s.transfers.iter_mut().find(|t| t.id == transfer.id) {
                        Some(t) => *t = transfer.clone(),
                        None => s.transfers.push(transfer.clone()),
                    }
                }
                if transfer.direction == TransferDirection::Incoming && transfer.state != TransferState::Offered {
                    let before = self.offer_queue.len();
                    self.offer_queue.retain(|t| t.id != transfer.id);
                    if self.offer_queue.len() != before {
                        out.extend(self.offer_patch());
                    }
                }
                out.extend(self.upsert_transfer(transfer));
            }
            Event::TrustPrompt { peer_key, display_name, phrase, key_changed } => {
                self.trust_queue.retain(|t| &t.0 != peer_key);
                self.trust_queue.push_back((peer_key.clone(), display_name.clone(), phrase.clone(), *key_changed));
                out.extend(self.trust_patch());
            }
            Event::PeerJoined { peer } => {
                self.peer_names.insert(peer.key.clone(), peer.display_name.clone());
                if !self.focused
                    && let Some(n) = notifier
                {
                    n.notify(&format!("{} joined", peer.display_name), "CallXL");
                }
            }
            Event::PeerLeft { peer_key, .. } => {
                let name = self.name_of(peer_key);
                out.push(toast(format!("{name} left the call"), false));
            }
            Event::CallStateChanged { state: callxl_core::CallState::Left { reason } } => {
                if reason != "you left the session" {
                    out.push(toast(capitalise(reason), false));
                }
            }
            Event::StatsTick { snapshot } => out.extend(self.diag_patches(snapshot)),
            Event::Error { reason } => out.push(toast(capitalise(reason), true)),
            _ => {}
        }
        out
    }

    pub fn diag_patches(&self, d: &DiagnosticsSnapshot) -> Vec<Patch> {
        let cloud = self.snap.as_ref().is_some_and(|s| s.cloud_connected);
        let global = view::global_metrics(d, cloud);
        let peers: Vec<PeerDiagRow> = d.peers.iter().map(|p| view::peer_diag(d, p)).collect();
        vec![patch(move |w, _| {
            let app = w.global::<App>();
            sync(&app.get_diag_global(), global);
            let model = app.get_diag_peers();
            let Some(vm) = model.as_any().downcast_ref::<VecModel<PeerDiag>>() else { return };
            if vm.row_count() == peers.len() {
                for (i, p) in peers.into_iter().enumerate() {
                    let Some(mut row) = vm.row_data(i) else { continue };
                    sync(&row.metrics, p.metrics);
                    let head = (
                        p.name.as_str(),
                        p.path_kind.as_str(),
                        p.local.as_str(),
                        p.remote.as_str(),
                        p.candidates.as_str(),
                    );
                    if head
                        != (
                            row.name.as_str(),
                            row.path_kind.as_str(),
                            row.local.as_str(),
                            row.remote.as_str(),
                            row.candidates.as_str(),
                        )
                    {
                        row.name = p.name.into();
                        row.path_kind = p.path_kind.into();
                        row.local = p.local.into();
                        row.remote = p.remote.into();
                        row.candidates = p.candidates.into();
                        vm.set_row_data(i, row);
                    }
                }
            } else {
                vm.set_vec(
                    peers
                        .into_iter()
                        .map(|p| PeerDiag {
                            name: p.name.into(),
                            path_kind: p.path_kind.into(),
                            local: p.local.into(),
                            remote: p.remote.into(),
                            candidates: p.candidates.into(),
                            metrics: ModelRc::new(VecModel::from(p.metrics)),
                        })
                        .collect::<Vec<_>>(),
                );
            }
        })]
    }

    fn trust_patch(&self) -> Vec<Patch> {
        let next = self.trust_queue.front().cloned();
        vec![patch(move |w, _| {
            let app = w.global::<App>();
            match next {
                Some((key, name, phrase, changed)) => {
                    app.set_trust_key(key.into());
                    app.set_trust_name(name.into());
                    app.set_trust_phrase(phrase.into());
                    app.set_trust_key_changed(changed);
                    app.set_trust_open(true);
                }
                None => app.set_trust_open(false),
            }
        })]
    }

    pub fn trust_done(&mut self, key: Option<&str>) -> Vec<Patch> {
        match key {
            Some(k) => self.trust_queue.retain(|t| t.0 != k),
            None => {
                self.trust_queue.pop_front();
            }
        }
        self.trust_patch()
    }

    fn offer_patch(&self) -> Vec<Patch> {
        let next = self
            .offer_queue
            .front()
            .map(|t| (t.id.clone(), self.name_of(&t.peer_key), t.name.clone(), view::human_bytes(t.size)));
        vec![patch(move |w, _| {
            let app = w.global::<App>();
            match next {
                Some((id, from, name, size)) => {
                    app.set_offer_id(id.into());
                    app.set_offer_from(from.into());
                    app.set_offer_name(name.into());
                    app.set_offer_size(size.into());
                    app.set_offer_open(true);
                }
                None => app.set_offer_open(false),
            }
        })]
    }

    pub fn offer_done(&mut self, id: &str) -> Vec<Patch> {
        self.offer_queue.retain(|t| t.id != id);
        self.offer_patch()
    }

    /// Files dropped or picked: ask who should receive them.
    pub fn files_chosen(&mut self, files: Vec<PathBuf>) -> Vec<Patch> {
        let files: Vec<PathBuf> = files.into_iter().filter(|p| p.is_file()).collect();
        if files.is_empty() {
            return vec![toast("Only files can be sent (not folders).".into(), false)];
        }
        let Some(s) = self.snap.as_ref().filter(|s| view::in_call(s)) else {
            return vec![toast("Join a session to send files.".into(), false)];
        };
        let peers: Vec<RecipientItem> = s
            .peers
            .iter()
            .filter(|p| p.connected)
            .map(|p| RecipientItem { key: p.key.as_str().into(), name: p.display_name.as_str().into(), checked: true })
            .collect();
        if peers.is_empty() {
            return vec![toast("Nobody else is connected yet.".into(), false)];
        }
        let names: Vec<String> =
            files.iter().map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        let summary =
            if names.len() == 1 { names[0].clone() } else { format!("{} files: {}", names.len(), names.join(", ")) };
        self.pending_files = files;
        self.recipients = peers.clone();
        vec![patch(move |w, _| {
            let app = w.global::<App>();
            sync(&app.get_recipients(), peers);
            app.set_send_files(summary.into());
            app.set_send_open(true);
        })]
    }

    pub fn toggle_recipient(&mut self, i: usize) -> Vec<Patch> {
        if let Some(r) = self.recipients.get_mut(i) {
            r.checked = !r.checked;
        }
        let rows = self.recipients.clone();
        vec![patch(move |w, _| sync(&w.global::<App>().get_recipients(), rows))]
    }

    /// (files, recipient keys) to offer; closes the picker.
    pub fn take_send(&mut self) -> (Vec<PathBuf>, Vec<String>, Vec<Patch>) {
        let keys: Vec<String> = self.recipients.iter().filter(|r| r.checked).map(|r| r.key.to_string()).collect();
        let files = std::mem::take(&mut self.pending_files);
        self.recipients.clear();
        (files, keys, vec![patch(|w, _| w.global::<App>().set_send_open(false))])
    }

    pub fn set_devices(&mut self, devices: Vec<AudioDevice>) -> Vec<Patch> {
        if devices == self.devices {
            return Vec::new();
        }
        self.devices = devices;
        self.device_patches()
    }

    pub fn devices(&self) -> &[AudioDevice] {
        &self.devices
    }

    fn device_patches(&self) -> Vec<Patch> {
        let (sel_in, sel_out) = self
            .snap
            .as_ref()
            .map(|s| (s.settings.input_device.clone(), s.settings.output_device.clone()))
            .unwrap_or_default();
        let rows = |kind: AudioDeviceKind, sel: &Option<String>| -> (Vec<AudioDeviceItem>, String) {
            let mut v = vec![AudioDeviceItem { id: "".into(), name: "System default".into(), selected: sel.is_none() }];
            let mut name = "System default".to_string();
            for d in self.devices.iter().filter(|d| d.kind == kind) {
                let selected = sel.as_deref() == Some(d.id.as_str());
                if selected {
                    name = d.name.clone();
                }
                v.push(AudioDeviceItem { id: d.id.as_str().into(), name: d.name.as_str().into(), selected });
            }
            (v, name)
        };
        let (ins, in_name) = rows(AudioDeviceKind::Input, &sel_in);
        let (outs, out_name) = rows(AudioDeviceKind::Output, &sel_out);
        vec![patch(move |w, _| {
            let app = w.global::<App>();
            sync(&app.get_inputs(), ins);
            sync(&app.get_outputs(), outs);
            app.set_input_name(in_name.into());
            app.set_output_name(out_name.into());
        })]
    }
}

pub fn toast(text: String, error: bool) -> Patch {
    patch(move |w, _| {
        let app = w.global::<App>();
        let text: SharedString = text.into();
        app.set_toast(text.clone());
        app.set_toast_error(error);
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_secs(if error { 6 } else { 4 }), move || {
            if let Some(w) = weak.upgrade() {
                let app = w.global::<App>();
                if app.get_toast() == text {
                    app.set_toast(SharedString::new());
                }
            }
        });
    })
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => {
            let mut out: String = f.to_uppercase().chain(c).collect();
            if !out.ends_with(['.', '!', '?']) {
                out.push('.');
            }
            out
        }
        None => String::new(),
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

thread_local! {
    /// The tray icon (UI thread only). Created after the window's first frame so that
    /// registering with the desktop's tray host never delays startup.
    pub static TRAY: std::cell::RefCell<Option<Tray>> = const { std::cell::RefCell::new(None) };
}

/// Handles for posting patches to the UI thread.
#[derive(Clone)]
pub struct UiHandle {
    pub win: slint::Weak<AppWindow>,
}

impl UiHandle {
    pub fn post(&self, patches: Vec<Patch>) {
        if patches.is_empty() {
            return;
        }
        let win = self.win.clone();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = win.upgrade() else { return };
            TRAY.with(|t| {
                let t = t.borrow();
                for p in patches {
                    p(&w, t.as_ref());
                }
            });
        });
    }
}

pub struct BridgeOptions {
    pub data_dir: PathBuf,
    pub prefs: Prefs,
    /// Debug-only (`CALLXL_DEBUG_AUTOJOIN=1`, used by `tests/gui_join.sh`): join the first
    /// session that appears, through the same UI command path as a click on "Join".
    pub autojoin: bool,
}

/// Sends UI commands to the bridge.
#[derive(Clone)]
pub struct Bridge {
    tx: Sender<Msg>,
    done: Arc<std::sync::Mutex<Receiver<()>>>,
}

impl Bridge {
    pub fn send(&self, c: UiCmd) {
        let _ = self.tx.send(Msg::Ui(c));
    }

    pub fn sender(&self) -> Sender<Msg> {
        self.tx.clone()
    }

    /// Waits (bounded) for the bridge thread to stop the core and exit.
    pub fn join(&self, timeout: Duration) {
        if let Ok(rx) = self.done.lock() {
            let _ = rx.recv_timeout(timeout);
        }
    }
}

/// Spawns the bridge thread; `core` is created and started on it (keystore and database
/// access never touch the UI thread). `on_started` runs on the bridge thread once the
/// core is running (or failed to start, with the reason).
pub fn spawn(
    make_core: impl FnOnce() -> Arc<CallXl> + Send + 'static,
    ui: UiHandle,
    opts: BridgeOptions,
    on_started: impl FnOnce(Result<(), String>) + Send + 'static,
) -> Bridge {
    let (tx, rx) = mpsc::channel::<Msg>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let tx2 = tx.clone();
    std::thread::Builder::new()
        .name("callxl-ui-bridge".into())
        .spawn(move || {
            let core = make_core();
            core.subscribe(Box::new(Listener(tx2.clone())));
            let started = core.start().map_err(|e| e.to_string());
            let ok = started.is_ok();
            on_started(started.clone());
            if let Err(reason) = started {
                ui.post(vec![toast(reason, true)]);
            }
            run(core, rx, tx2, ui, opts, ok);
            let _ = done_tx.send(());
        })
        .expect("spawn bridge thread");
    Bridge { tx, done: Arc::new(std::sync::Mutex::new(done_rx)) }
}

fn run(core: Arc<CallXl>, rx: Receiver<Msg>, tx: Sender<Msg>, ui: UiHandle, mut opts: BridgeOptions, started: bool) {
    let notifier = Notifier::spawn();
    let mut vm = ViewModel::new();
    let mut autojoined = false;
    let refresh = |vm: &mut ViewModel, core: &CallXl| -> Vec<Patch> {
        let s = core.snapshot();
        let reload = vm.wanted_chat_session(&s);
        let mut p = vm.on_snapshot(s);
        if let Some(sess) = reload {
            let hist = sess.as_ref().map(|id| core.chat_history(id.clone(), 500, None)).unwrap_or_default();
            p.extend(vm.load_chat(sess, hist));
        }
        p
    };
    let mut patches = refresh(&mut vm, &core);
    if started {
        patches.extend(vm.set_devices(core.list_audio_devices()));
        patches.push(patch(|w, _| w.global::<App>().set_started(true)));
    }
    ui.post(patches);
    let mut last_device_poll = Instant::now();

    loop {
        let msg = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut out: Vec<Patch> = Vec::new();
        let mut needs_refresh = msg.is_none();
        match msg {
            None => {}
            Some(Msg::Event(e)) => {
                if matches!(e, Event::DevicesChanged) {
                    out.extend(vm.set_devices(core.list_audio_devices()));
                }
                out.extend(vm.on_event(&e, Some(tx.clone()), Some(&notifier)));
                needs_refresh = !matches!(e, Event::SpeakingChanged { .. });
            }
            Some(Msg::Ui(UiCmd::Quit)) => break,
            Some(Msg::Ui(c)) => {
                needs_refresh = true;
                out.extend(handle(&core, &mut vm, c, &tx, &mut opts));
            }
        }
        if needs_refresh {
            out.extend(refresh(&mut vm, &core));
        }
        // Device hot-plug: poll the device list every 3 s; if the chosen device vanished,
        // fall back to the system default (§9 device picker, live switching).
        if started && last_device_poll.elapsed() >= Duration::from_secs(3) {
            last_device_poll = Instant::now();
            let devs = core.list_audio_devices();
            if let Some(s) = vm.snapshot() {
                let gone = |sel: &Option<String>, kind| {
                    sel.as_ref().is_some_and(|id| !devs.iter().any(|d| d.kind == kind && &d.id == id))
                };
                if !devs.is_empty() && gone(&s.settings.input_device, AudioDeviceKind::Input) {
                    core.set_input_device(None);
                    out.push(toast("Microphone disconnected — using the system default.".into(), false));
                }
                if !devs.is_empty() && gone(&s.settings.output_device, AudioDeviceKind::Output) {
                    core.set_output_device(None);
                    out.push(toast("Speaker disconnected — using the system default.".into(), false));
                }
            }
            out.extend(vm.set_devices(devs));
        }
        if opts.autojoin
            && !autojoined
            && let Some(s) = vm.snapshot()
            && let Some(first) = s.sessions.iter().find(|x| !x.joined)
        {
            autojoined = true;
            tracing::info!("debug autojoin: joining session {}", first.id);
            let id: SharedString = first.id.as_str().into();
            // Same path as a click on "Join": the UI callback.
            out.push(patch(move |w, _| w.global::<App>().invoke_join_session(id)));
        }
        ui.post(out);
    }
    core.stop();
}

fn handle(core: &CallXl, vm: &mut ViewModel, c: UiCmd, tx: &Sender<Msg>, opts: &mut BridgeOptions) -> Vec<Patch> {
    let mut out = Vec::new();
    match c {
        UiCmd::CreateSession(name) => {
            let name = name.trim().to_string();
            core.create_session(if name.is_empty() { None } else { Some(name) });
        }
        UiCmd::Join(id) => core.join_session(id),
        UiCmd::Leave => core.leave_session(),
        UiCmd::ToggleMute => {
            let muted = vm.snapshot().is_some_and(|s| s.muted);
            core.set_muted(!muted);
        }
        UiCmd::SetDisplayName(n) => core.set_display_name(n.trim().to_string()),
        UiCmd::Trust(key) => {
            if let Ok(bytes) = hex_decode(&key) {
                core.trust_device(bytes);
            }
            out.extend(vm.trust_done(Some(&key)));
        }
        UiCmd::DismissTrust => out.extend(vm.trust_done(None)),
        UiCmd::Untrust(key) => {
            if let Ok(bytes) = hex_decode(&key) {
                core.untrust_device(bytes);
            }
        }
        UiCmd::SendChat(body) => {
            let body = body.trim().to_string();
            if !body.is_empty() {
                let id = core.send_chat(body.clone());
                out.extend(vm.own_message(&id, &body));
            }
        }
        UiCmd::ChatShown => {
            if let Some(id) = vm.last_incoming() {
                core.mark_read(id);
            }
            out.push(patch(|w, _| w.global::<App>().set_unread(0)));
        }
        UiCmd::RespondOffer(id, accept) => {
            core.respond_file_offer(id.clone(), accept);
            out.extend(vm.offer_done(&id));
        }
        UiCmd::CancelTransfer(id) => core.cancel_transfer(id),
        UiCmd::OpenFolder(path) => platform::open_in_folder(std::path::Path::new(&path)),
        UiCmd::PickFiles => {
            let tx = tx.clone();
            let _ = std::thread::Builder::new().name("callxl-file-dialog".into()).spawn(move || {
                if let Some(files) = rfd::FileDialog::new().set_title("Send files").pick_files() {
                    let _ = tx.send(Msg::Ui(UiCmd::FilesChosen(files)));
                }
            });
        }
        UiCmd::FilesChosen(files) => out.extend(vm.files_chosen(files)),
        UiCmd::ToggleRecipient(i) => out.extend(vm.toggle_recipient(i)),
        UiCmd::SendToRecipients => {
            let (files, keys, p) = vm.take_send();
            out.extend(p);
            if keys.is_empty() {
                out.push(toast("Choose at least one person.".into(), false));
            } else {
                for f in files {
                    core.offer_file(f.to_string_lossy().into_owned(), keys.clone());
                }
            }
        }
        UiCmd::CancelSend => {
            let _ = vm.take_send();
            out.push(patch(|w, _| w.global::<App>().set_send_open(false)));
        }
        UiCmd::SelectInput(id) => core.set_input_device(if id.is_empty() { None } else { Some(id) }),
        UiCmd::SelectOutput(id) => core.set_output_device(if id.is_empty() { None } else { Some(id) }),
        UiCmd::RefreshDevices => out.extend(vm.set_devices(core.list_audio_devices())),
        UiCmd::SetApm(cfg) => core.set_audio_processing(cfg),
        UiCmd::SetRawMic(raw) => core.set_raw_mic(raw),
        UiCmd::SetBitrate(k) => core.set_bitrate_ceiling_kbps(k.clamp(32, 510)),
        UiCmd::SetDownloadDir(d) => core.set_download_dir(d),
        UiCmd::PickDownloadDir => {
            let tx = tx.clone();
            let _ = std::thread::Builder::new().name("callxl-file-dialog".into()).spawn(move || {
                if let Some(dir) = rfd::FileDialog::new().set_title("Download folder").pick_folder() {
                    let _ = tx.send(Msg::Ui(UiCmd::SetDownloadDir(dir.to_string_lossy().into_owned())));
                }
            });
        }
        UiCmd::SetSignalingUrl(u) => {
            let u = u.trim().to_string();
            core.set_signaling_url(if u.is_empty() { None } else { Some(u) });
        }
        UiCmd::SetTheme(dark) => {
            opts.prefs.dark = dark;
            platform::save_prefs(&opts.data_dir, &opts.prefs);
        }
        UiCmd::ExportDiagnostics => {
            let _ = core.export_diagnostics();
            let dir = vm.snapshot().map(|s| s.settings.download_dir.clone()).unwrap_or_default();
            let text = match newest_report(std::path::Path::new(&dir)) {
                Some(p) => format!("Report saved to {}", p.display()),
                None => format!("Report saved to {dir} ({})", report_prefix() + "….json"),
            };
            out.push(patch(move |w, _| w.global::<App>().set_export_result(text.into())));
        }
        UiCmd::Focused(f) => vm.focused = f,
        UiCmd::Quit => {}
    }
    out
}

/// The export file name prefix, taken from the core's own naming
/// (`callxl_core::diagnostics::export::file_name`), e.g. "CallXL-diagnostics-".
fn report_prefix() -> String {
    let probe = callxl_core::diagnostics::export::file_name(0);
    probe.strip_suffix("0.json").unwrap_or(&probe).to_string()
}

/// The newest diagnostics report in `dir` (prefix matched case-insensitively).
pub fn newest_report(dir: &std::path::Path) -> Option<PathBuf> {
    let prefix = report_prefix().to_ascii_lowercase();
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with(&prefix))
        .max_by_key(|e| e.file_name())
        .map(|e| e.path())
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ())).collect()
}

/// Wires every `App` callback to a bridge command (UI thread; never blocks).
pub fn connect_callbacks(w: &AppWindow, bridge: &Bridge) {
    let app = w.global::<App>();
    macro_rules! on {
        ($setter:ident, ($($a:ident),*) => $cmd:expr) => {{
            let b = bridge.clone();
            app.$setter(move |$($a),*| b.send($cmd));
        }};
    }
    on!(on_create_session, (n) => UiCmd::CreateSession(n.to_string()));
    on!(on_join_session, (id) => UiCmd::Join(id.to_string()));
    on!(on_leave, () => UiCmd::Leave);
    on!(on_set_display_name, (n) => UiCmd::SetDisplayName(n.to_string()));
    on!(on_trust, (k) => UiCmd::Trust(k.to_string()));
    on!(on_dismiss_trust, () => UiCmd::DismissTrust);
    on!(on_untrust, (k) => UiCmd::Untrust(k.to_string()));
    on!(on_send_chat, (t) => UiCmd::SendChat(t.to_string()));
    on!(on_chat_shown, () => UiCmd::ChatShown);
    on!(on_respond_offer, (id, a) => UiCmd::RespondOffer(id.to_string(), a));
    on!(on_cancel_transfer, (id) => UiCmd::CancelTransfer(id.to_string()));
    on!(on_open_folder, (p) => UiCmd::OpenFolder(p.to_string()));
    on!(on_pick_files, () => UiCmd::PickFiles);
    on!(on_toggle_recipient, (i) => UiCmd::ToggleRecipient(i.max(0) as usize));
    on!(on_send_to_recipients, () => UiCmd::SendToRecipients);
    on!(on_cancel_send, () => UiCmd::CancelSend);
    on!(on_select_input, (id) => UiCmd::SelectInput(id.to_string()));
    on!(on_select_output, (id) => UiCmd::SelectOutput(id.to_string()));
    on!(on_refresh_devices, () => UiCmd::RefreshDevices);
    on!(on_set_download_dir, (d) => UiCmd::SetDownloadDir(d.to_string()));
    on!(on_pick_download_dir, () => UiCmd::PickDownloadDir);
    on!(on_set_signaling_url, (u) => UiCmd::SetSignalingUrl(u.to_string()));
    on!(on_export_diagnostics, () => UiCmd::ExportDiagnostics);

    // Optimistic local updates first (instant feedback), then the core command.
    let (b, weak) = (bridge.clone(), w.as_weak());
    app.on_toggle_mute(move || {
        if let Some(w) = weak.upgrade() {
            let app = w.global::<App>();
            app.set_muted(!app.get_muted());
        }
        b.send(UiCmd::ToggleMute);
    });
    let (b, weak) = (bridge.clone(), w.as_weak());
    app.on_set_apm(move |aec, ns, level, hpf, agc| {
        let cfg = ApmConfig {
            aec,
            noise_suppression: ns,
            ns_level: match level {
                0 => NsLevel::Low,
                2 => NsLevel::High,
                _ => NsLevel::Moderate,
            },
            high_pass: hpf,
            agc,
        };
        if let Some(w) = weak.upgrade() {
            let app = w.global::<App>();
            app.set_aec(aec);
            app.set_ns(ns);
            app.set_ns_level(level);
            app.set_hpf(hpf);
            app.set_agc(agc);
            app.set_raw_mic(cfg == ApmConfig::RAW);
        }
        b.send(UiCmd::SetApm(cfg));
    });
    let (b, weak) = (bridge.clone(), w.as_weak());
    app.on_set_raw_mic(move |raw| {
        if let Some(w) = weak.upgrade() {
            let app = w.global::<App>();
            app.set_raw_mic(raw);
            let c = if raw { ApmConfig::RAW } else { ApmConfig::default() };
            app.set_aec(c.aec);
            app.set_ns(c.noise_suppression);
            app.set_hpf(c.high_pass);
            app.set_agc(c.agc);
        }
        b.send(UiCmd::SetRawMic(raw));
    });
    let (b, weak) = (bridge.clone(), w.as_weak());
    app.on_set_bitrate(move |k| {
        if let Some(w) = weak.upgrade() {
            w.global::<App>().set_bitrate(k);
        }
        b.send(UiCmd::SetBitrate(k.max(0) as u32));
    });
    let (b, weak) = (bridge.clone(), w.as_weak());
    app.on_set_theme(move |dark| {
        if let Some(w) = weak.upgrade() {
            w.global::<Colors>().set_dark(dark);
        }
        b.send(UiCmd::SetTheme(dark));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_report_matches_the_core_export_name() {
        let dir = std::env::temp_dir().join(format!("callxl-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(newest_report(&dir), None);
        for ts in [1_762_500_000_000u64, 1_762_500_000_500] {
            let name = callxl_core::diagnostics::export::file_name(ts);
            std::fs::write(dir.join(name), "{}").unwrap();
        }
        std::fs::write(dir.join("unrelated.json"), "{}").unwrap();
        let newest = newest_report(&dir).expect("report found");
        assert_eq!(newest.file_name().unwrap().to_string_lossy(), "CallXL-diagnostics-1762500000500.json");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
