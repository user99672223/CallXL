//! OS integration that is not UI: profile directory, notifications, "open in folder",
//! and the desktop-only preferences file.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// `%APPDATA%\CallXL` on Windows, `$XDG_DATA_HOME/CallXL` (or `~/.local/share/CallXL`) on Linux.
pub fn default_data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(a) = std::env::var("APPDATA") {
            return PathBuf::from(a).join("CallXL");
        }
    }
    if let Ok(x) = std::env::var("XDG_DATA_HOME")
        && !x.is_empty()
    {
        return PathBuf::from(x).join("CallXL");
    }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".local/share/CallXL")
}

/// First-run display name: the account's user name, capitalised ("alice" → "Alice").
pub fn default_display_name() -> String {
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
    let mut c = user.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => "CallXL user".into(),
    }
}

pub fn utc_offset_secs() -> i64 {
    i64::from(chrono::Local::now().offset().local_minus_utc())
}

/// Reveals a file (or opens a folder) in the system file manager. Never blocks.
pub fn open_in_folder(path: &Path) {
    #[cfg(windows)]
    let r = if path.is_file() {
        std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn()
    } else {
        std::process::Command::new("explorer").arg(path).spawn()
    };
    #[cfg(not(windows))]
    let r = {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        std::process::Command::new("xdg-open").arg(dir).spawn()
    };
    if let Err(e) = r {
        tracing::warn!("could not open the file manager: {e}");
    }
}

/// Desktop-only preferences (the core owns everything else).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Prefs {
    pub dark: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { dark: true }
    }
}

pub fn load_prefs(data_dir: &Path) -> Prefs {
    std::fs::read_to_string(data_dir.join("desktop.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_prefs(data_dir: &Path, p: &Prefs) {
    let _ = std::fs::create_dir_all(data_dir);
    if let Err(e) = std::fs::write(data_dir.join("desktop.json"), serde_json::to_string(p).unwrap_or_default()) {
        tracing::warn!("could not save preferences: {e}");
    }
}

/// Desktop notifications (Windows toasts, freedesktop D-Bus) on their own thread: the
/// D-Bus call blocks for a round trip and must not delay the bridge.
pub struct Notifier {
    tx: mpsc::Sender<(String, String)>,
}

impl Notifier {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<(String, String)>();
        let _ = std::thread::Builder::new().name("callxl-notify".into()).spawn(move || {
            for (summary, body) in rx {
                let mut n = notify_rust::Notification::new();
                n.summary(&summary).body(&body).appname("CallXL");
                #[cfg(windows)]
                n.app_id(crate::APP_ID);
                #[cfg(all(unix, not(target_os = "macos")))]
                n.icon("callxl");
                if let Err(e) = n.show() {
                    tracing::debug!("notification not shown: {e}");
                }
            }
        });
        Self { tx }
    }

    pub fn notify(&self, summary: &str, body: &str) {
        let _ = self.tx.send((summary.to_string(), body.to_string()));
    }
}
