//! File transfer (§6): sender picks recipients; per recipient: offer (name, size, BLAKE3)
//! → recipient always asked → dedicated reliable channel → 16 KiB chunks, ≤ 1 MiB in
//! flight, resumable by offset after reconnect, hash verified, no size limit.
//! On internet paths the chunk rate follows the audio-first throttle (`link::Throttle`).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use callxl_media::DataChannelOptions;
use serde::{Deserialize, Serialize};

use crate::config::{FILE_CHUNK, FILE_WINDOW};
use crate::engine::{Core, Internal, short};
use crate::link::ChannelRole;
use crate::types::*;

const ACK_EVERY: u64 = 256 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)] // variant names are the wire tags (file_offer, ...)
enum Ctl {
    FileOffer { id: TransferId, name: String, size: u64, hash: String, session: SessionId },
    FileAccept { id: TransferId, offset: u64 },
    FileDecline { id: TransferId },
    FileAck { id: TransferId, received: u64 },
    FileDone { id: TransferId, ok: bool, reason: String },
    FileCancel { id: TransferId },
}

pub struct Transfer {
    pub info: TransferInfo,
    file: Option<File>,
    sent: u64,
    acked: u64,
    last_ack_sent: u64,
    last_emit: Instant,
}

impl Transfer {
    fn new(info: TransferInfo) -> Self {
        Self {
            info,
            file: None,
            sent: 0,
            acked: 0,
            last_ack_sent: 0,
            last_emit: Instant::now() - Duration::from_secs(1),
        }
    }
}

type Key = (TransferId, PeerKey, TransferDirection);

#[derive(Default)]
pub struct Transfers {
    pub map: HashMap<Key, Transfer>,
}

impl Transfers {
    pub fn load(items: Vec<TransferInfo>) -> Self {
        let mut t = Self::default();
        for mut info in items {
            if matches!(info.state, TransferState::Transferring | TransferState::Accepted) {
                info.state = TransferState::Paused;
            }
            t.map.insert((info.id.clone(), info.peer_key.clone(), info.direction), Transfer::new(info));
        }
        t
    }

    pub fn list(&self) -> Vec<TransferInfo> {
        let mut v: Vec<_> = self.map.values().map(|t| t.info.clone()).collect();
        v.sort_by(|a, b| a.id.cmp(&b.id).then(a.peer_key.cmp(&b.peer_key)));
        v
    }
}

fn part_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!(".{id}.part"))
}

/// `name` sanitised to a plain file name, made unique inside `dir`.
fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let clean: String = Path::new(name)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into())
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let clean = if clean.is_empty() || clean.starts_with('.') { format!("file{clean}") } else { clean };
    let mut p = dir.join(&clean);
    let stem = Path::new(&clean).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = Path::new(&clean).extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
    let mut n = 1;
    while p.exists() {
        p = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    p
}

pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_hex().to_string())
}

impl Core {
    fn ctl(&mut self, key: &str, msg: &Ctl) {
        let b = serde_json::to_vec(msg).unwrap_or_default();
        if let Some(l) = self.links.get_mut(key) {
            l.send_ctl(b);
        }
    }

    fn progress(&mut self, k: &Key, force: bool) {
        let Some(t) = self.transfers.map.get_mut(k) else { return };
        if force || t.last_emit.elapsed() > Duration::from_millis(250) {
            t.last_emit = Instant::now();
            let info = t.info.clone();
            self.store.save_transfer(&info);
            self.emit(Event::TransferProgress { transfer: info });
        }
    }

    pub fn offer_file(&mut self, id: TransferId, path: String, recipients: Vec<PeerKey>) {
        let Some(session) = self.current_session().map(str::to_string) else {
            self.error("join a session before sending a file");
            return;
        };
        let _ = session;
        let p = PathBuf::from(&path);
        let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let size = match std::fs::metadata(&p) {
            Ok(m) if m.is_file() => m.len(),
            _ => {
                self.error(format!("the file {path} cannot be read"));
                return;
            }
        };
        let tx = self.internal_tx.clone();
        tokio::task::spawn_blocking(move || {
            let hash = hash_file(&p).map_err(|e| e.to_string());
            let _ = tx.send(Internal::HashDone { id, path, name, size, hash, recipients });
        });
    }

    pub fn on_hash_done(
        &mut self,
        id: TransferId,
        path: String,
        name: String,
        size: u64,
        hash: Result<String, String>,
        recipients: Vec<PeerKey>,
    ) {
        let hash = match hash {
            Ok(h) => h,
            Err(e) => {
                self.error(format!("the file {path} cannot be read: {e}"));
                return;
            }
        };
        let session = self.current_session().unwrap_or_default().to_string();
        for r in recipients {
            if !self.links.contains_key(&r) {
                self.error(format!("{} is not in the call; the file was not offered to them", short(&r)));
                continue;
            }
            let info = TransferInfo {
                id: id.clone(),
                peer_key: r.clone(),
                direction: TransferDirection::Outgoing,
                name: name.clone(),
                size,
                hash: hash.clone(),
                bytes_done: 0,
                state: TransferState::Offered,
                path: path.clone(),
                session: session.clone(),
            };
            let k = (id.clone(), r.clone(), TransferDirection::Outgoing);
            self.transfers.map.insert(k.clone(), Transfer::new(info));
            self.ctl(
                &r,
                &Ctl::FileOffer {
                    id: id.clone(),
                    name: name.clone(),
                    size,
                    hash: hash.clone(),
                    session: session.clone(),
                },
            );
            self.progress(&k, true);
        }
        self.publish();
    }

    /// After a reconnect: re-offer unfinished outgoing transfers; the receiver resumes
    /// from the bytes it already has.
    pub fn resume_transfers_for(&mut self, key: &str) {
        if !self.links.get(key).is_some_and(|l| l.ctl.as_ref().is_some_and(|c| c.is_open())) {
            return;
        }
        let pending: Vec<Key> = self
            .transfers
            .map
            .iter()
            .filter(|(k, t)| {
                k.1 == key
                    && k.2 == TransferDirection::Outgoing
                    && matches!(
                        t.info.state,
                        TransferState::Paused | TransferState::Accepted | TransferState::Transferring
                    )
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in pending {
            let Some(t) = self.transfers.map.get_mut(&k) else { continue };
            if t.info.state == TransferState::Transferring
                && self.links.get(key).is_some_and(|l| l.file_channels.contains_key(&k.0))
            {
                continue;
            }
            t.info.state = TransferState::Offered;
            let m = Ctl::FileOffer {
                id: t.info.id.clone(),
                name: t.info.name.clone(),
                size: t.info.size,
                hash: t.info.hash.clone(),
                session: t.info.session.clone(),
            };
            tracing::info!("re-offering {} to {} (resume)", t.info.name, short(key));
            self.ctl(key, &m);
        }
    }

    pub fn pause_transfers_for(&mut self, key: &str) {
        let ks: Vec<Key> = self.transfers.map.keys().filter(|k| k.1 == key).cloned().collect();
        for k in ks {
            if let Some(t) = self.transfers.map.get_mut(&k)
                && matches!(t.info.state, TransferState::Transferring | TransferState::Accepted)
            {
                t.info.state = TransferState::Paused;
                t.file = None;
                self.progress(&k, true);
            }
        }
    }

    pub fn respond_file(&mut self, id: &str, accept: bool) {
        let Some(k) = self.transfers.map.keys().find(|k| k.0 == id && k.2 == TransferDirection::Incoming).cloned()
        else {
            self.error("that file offer no longer exists");
            return;
        };
        self.accept_incoming(&k, accept);
    }

    fn accept_incoming(&mut self, k: &Key, accept: bool) {
        let dir = self.download_dir();
        let Some(t) = self.transfers.map.get_mut(k) else { return };
        if !accept {
            t.info.state = TransferState::Declined;
            let m = Ctl::FileDecline { id: k.0.clone() };
            self.progress(k, true);
            self.ctl(&k.1, &m);
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            t.info.state = TransferState::Failed { reason: format!("the download folder cannot be created: {e}") };
            self.progress(k, true);
            return;
        }
        let part = part_path(&dir, &k.0);
        let offset = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0).min(t.info.size);
        match OpenOptions::new().create(true).write(true).truncate(false).open(&part) {
            Ok(mut f) => {
                let _ = f.set_len(offset);
                let _ = f.seek(SeekFrom::Start(offset));
                t.file = Some(f);
            }
            Err(e) => {
                t.info.state = TransferState::Failed { reason: format!("the file cannot be written: {e}") };
                self.progress(k, true);
                return;
            }
        }
        t.info.bytes_done = offset;
        t.last_ack_sent = offset;
        t.info.path = part.to_string_lossy().into_owned();
        t.info.state = TransferState::Accepted;
        let m = Ctl::FileAccept { id: k.0.clone(), offset };
        self.progress(k, true);
        self.ctl(&k.1, &m);
    }

    pub fn cancel_transfer(&mut self, id: &str) {
        let ks: Vec<Key> = self.transfers.map.keys().filter(|k| k.0 == id).cloned().collect();
        for k in ks {
            if let Some(t) = self.transfers.map.get_mut(&k) {
                t.info.state = TransferState::Cancelled;
                t.file = None;
            }
            if let Some(l) = self.links.get_mut(&k.1)
                && let Some(dc) = l.file_channels.remove(&k.0)
            {
                dc.close();
            }
            self.ctl(&k.1, &Ctl::FileCancel { id: k.0.clone() });
            self.progress(&k, true);
        }
    }

    pub fn on_ctl_message(&mut self, key: &str, data: &[u8]) {
        let Ok(m) = serde_json::from_slice::<Ctl>(data) else { return };
        match m {
            Ctl::FileOffer { id, name, size, hash, session } => {
                let k = (id.clone(), key.to_string(), TransferDirection::Incoming);
                if let Some(t) = self.transfers.map.get(&k) {
                    // Resume: this offer was accepted before; continue from what we have.
                    if matches!(
                        t.info.state,
                        TransferState::Paused | TransferState::Accepted | TransferState::Transferring
                    ) {
                        tracing::info!("resuming {name} from {}", short(key));
                        self.accept_incoming(&k, true);
                    }
                    return;
                }
                let info = TransferInfo {
                    id,
                    peer_key: key.to_string(),
                    direction: TransferDirection::Incoming,
                    name,
                    size,
                    hash,
                    bytes_done: 0,
                    state: TransferState::Offered,
                    path: String::new(),
                    session,
                };
                self.transfers.map.insert(k, Transfer::new(info.clone()));
                self.store.save_transfer(&info);
                self.emit(Event::FileOffered { transfer: info });
                self.publish();
            }
            Ctl::FileAccept { id, offset } => {
                let k = (id.clone(), key.to_string(), TransferDirection::Outgoing);
                let Some(t) = self.transfers.map.get_mut(&k) else { return };
                if matches!(t.info.state, TransferState::Cancelled | TransferState::Completed) {
                    return;
                }
                match File::open(&t.info.path) {
                    Ok(mut f) => {
                        let _ = f.seek(SeekFrom::Start(offset));
                        t.file = Some(f);
                    }
                    Err(e) => {
                        t.info.state = TransferState::Failed { reason: format!("the file cannot be read: {e}") };
                        self.progress(&k, true);
                        return;
                    }
                }
                t.sent = offset.min(t.info.size);
                t.acked = t.sent;
                t.info.bytes_done = t.sent;
                t.info.state = TransferState::Transferring;
                let Some(l) = self.links.get_mut(key) else { return };
                if let Some(old) = l.file_channels.remove(&id) {
                    old.close();
                }
                match l.pc.create_data_channel(&format!("file:{id}"), DataChannelOptions::default()) {
                    Ok(dc) => {
                        l.channels.insert(dc.handle(), ChannelRole::File(id.clone()));
                        l.file_channels.insert(id.clone(), dc);
                    }
                    Err(e) => {
                        self.error(format!("could not open a file channel: {e}"));
                    }
                }
                self.progress(&k, true);
            }
            Ctl::FileDecline { id } => {
                let k = (id, key.to_string(), TransferDirection::Outgoing);
                if let Some(t) = self.transfers.map.get_mut(&k) {
                    t.info.state = TransferState::Declined;
                }
                self.progress(&k, true);
            }
            Ctl::FileAck { id, received } => {
                let k = (id.clone(), key.to_string(), TransferDirection::Outgoing);
                if let Some(t) = self.transfers.map.get_mut(&k) {
                    t.acked = t.acked.max(received);
                    t.info.bytes_done = t.acked;
                }
                self.progress(&k, false);
                self.pump_file(key, &id);
            }
            Ctl::FileDone { id, ok, reason } => {
                let k = (id.clone(), key.to_string(), TransferDirection::Outgoing);
                if let Some(t) = self.transfers.map.get_mut(&k) {
                    t.info.state = if ok { TransferState::Completed } else { TransferState::Failed { reason } };
                    t.info.bytes_done = if ok { t.info.size } else { t.info.bytes_done };
                    t.file = None;
                }
                if let Some(l) = self.links.get_mut(key)
                    && let Some(dc) = l.file_channels.remove(&id)
                {
                    dc.close();
                }
                self.progress(&k, true);
            }
            Ctl::FileCancel { id } => {
                for dir in [TransferDirection::Incoming, TransferDirection::Outgoing] {
                    let k = (id.clone(), key.to_string(), dir);
                    if let Some(t) = self.transfers.map.get_mut(&k) {
                        t.info.state = TransferState::Cancelled;
                        t.file = None;
                        self.progress(&k, true);
                    }
                }
            }
        }
    }

    /// Sends as many chunks as the window (≤ 1 MiB unacknowledged), the SCTP buffer and,
    /// on internet paths, the audio-first throttle allow. `id` "" = all transfers to `key`.
    pub fn pump_file(&mut self, key: &str, id: &str) {
        let ids: Vec<TransferId> = self
            .transfers
            .map
            .iter()
            .filter(|(k, t)| {
                k.1 == key
                    && k.2 == TransferDirection::Outgoing
                    && t.info.state == TransferState::Transferring
                    && (id.is_empty() || k.0 == id)
            })
            .map(|(k, _)| k.0.clone())
            .collect();
        let Some(l) = self.links.get_mut(key) else { return };
        let internet = l.path_kind.is_some_and(|k| k.is_internet());
        let mut buf = vec![0u8; FILE_CHUNK];
        for id in ids {
            let k = (id.clone(), key.to_string(), TransferDirection::Outgoing);
            let Some(t) = self.transfers.map.get_mut(&k) else { continue };
            let Some(dc) = l.file_channels.get(&id) else { continue };
            if !dc.is_open() {
                continue;
            }
            let Some(f) = t.file.as_mut() else { continue };
            while t.sent < t.info.size && t.sent - t.acked < FILE_WINDOW && dc.buffered_amount() < FILE_WINDOW {
                if internet && l.throttle.allowance() < FILE_CHUNK as f64 {
                    break;
                }
                let n = ((t.info.size - t.sent) as usize).min(FILE_CHUNK);
                if f.seek(SeekFrom::Start(t.sent)).is_err() || f.read_exact(&mut buf[..n]).is_err() {
                    t.info.state = TransferState::Failed { reason: "the file changed or became unreadable".into() };
                    break;
                }
                if !dc.send(&buf[..n], true) {
                    break;
                }
                t.sent += n as u64;
                if internet {
                    l.throttle.tokens -= n as f64;
                }
            }
        }
    }

    pub fn on_file_data(&mut self, key: &str, id: &str, data: &[u8]) {
        let k = (id.to_string(), key.to_string(), TransferDirection::Incoming);
        let Some(t) = self.transfers.map.get_mut(&k) else { return };
        if !matches!(t.info.state, TransferState::Accepted | TransferState::Transferring) {
            return;
        }
        t.info.state = TransferState::Transferring;
        let Some(f) = t.file.as_mut() else { return };
        if let Err(e) = f.write_all(data) {
            t.info.state = TransferState::Failed { reason: format!("the file cannot be written: {e}") };
            self.progress(&k, true);
            return;
        }
        t.info.bytes_done += data.len() as u64;
        let done = t.info.bytes_done >= t.info.size;
        let need_ack = done || t.info.bytes_done - t.last_ack_sent >= ACK_EVERY;
        let received = t.info.bytes_done;
        if need_ack {
            t.last_ack_sent = received;
        }
        if need_ack {
            self.ctl(key, &Ctl::FileAck { id: id.to_string(), received });
        }
        if done {
            self.finish_incoming(&k);
        } else {
            self.progress(&k, false);
        }
    }

    fn finish_incoming(&mut self, k: &Key) {
        let dir = self.download_dir();
        let Some(t) = self.transfers.map.get_mut(k) else { return };
        if let Some(f) = t.file.take() {
            let _ = f.sync_all();
        }
        let part = PathBuf::from(&t.info.path);
        let hash = hash_file(&part).unwrap_or_default();
        let (ok, reason) = if hash == t.info.hash {
            let dest = unique_dest(&dir, &t.info.name);
            match std::fs::rename(&part, &dest) {
                Ok(()) => {
                    t.info.path = dest.to_string_lossy().into_owned();
                    t.info.state = TransferState::Completed;
                    (true, String::new())
                }
                Err(e) => {
                    let r = format!("the file could not be saved: {e}");
                    t.info.state = TransferState::Failed { reason: r.clone() };
                    (false, r)
                }
            }
        } else {
            let _ = std::fs::remove_file(&part);
            let r = "the received file is corrupt (hash mismatch)".to_string();
            t.info.state = TransferState::Failed { reason: r.clone() };
            (false, r)
        };
        let id = k.0.clone();
        self.ctl(&k.1, &Ctl::FileDone { id, ok, reason });
        self.progress(k, true);
        self.publish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dest_names_are_safe_and_unique() {
        let dir = std::env::temp_dir().join(format!("callxl-dest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = unique_dest(&dir, "../../etc/passwd");
        assert_eq!(a, dir.join("passwd"));
        std::fs::write(&a, b"x").unwrap();
        assert_eq!(unique_dest(&dir, "passwd"), dir.join("passwd (1)"));
        assert_eq!(unique_dest(&dir, ".hidden"), dir.join("file.hidden"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
