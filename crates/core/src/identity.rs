//! Per-install identity keys in the OS keystore (§5), with a file fallback.
//!
//! - Interactive builds use the `keyring` crate (Windows Credential Manager, macOS/iOS
//!   Keychain, Secret Service on Linux). The entry is scoped to the data directory so
//!   several profiles on one machine (and test peers) do not collide.
//! - Headless peers (`AppConfig::headless`, CI, servers without a Secret Service) and any
//!   keystore failure fall back to `<data_dir>/identity.key`, mode 0600. See DECISIONS.md.

use std::path::Path;

use callxl_signal::Identity;

const SERVICE: &str = "com.callxl.app";

fn entry_user(data_dir: &Path) -> String {
    let canon = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let h = blake3::hash(canon.to_string_lossy().as_bytes());
    format!("identity-{}", &h.to_hex()[..16])
}

fn file_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("identity.key")
}

fn read_file(data_dir: &Path) -> Option<Identity> {
    let s = std::fs::read_to_string(file_path(data_dir)).ok()?;
    Identity::from_secret_bytes(&hex::decode(s.trim()).ok()?)
}

fn write_file(data_dir: &Path, id: &Identity) -> std::io::Result<()> {
    let _ = std::fs::create_dir_all(data_dir);
    let path = file_path(data_dir);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, hex::encode(id.secret_bytes()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, path)
}

fn keyring_entry(data_dir: &Path) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, &entry_user(data_dir)).ok()
}

/// Loads the identity, creating (and storing) a new one on first run.
/// `reset` discards the stored keys first (debug: simulates a reinstall).
pub fn load_or_create(data_dir: &Path, headless: bool, reset: bool) -> Identity {
    let use_keyring = !headless;
    if reset {
        let _ = std::fs::remove_file(file_path(data_dir));
        if use_keyring && let Some(e) = keyring_entry(data_dir) {
            let _ = e.delete_credential();
        }
    }
    if use_keyring && let Some(e) = keyring_entry(data_dir) {
        match e.get_secret() {
            Ok(b) => {
                if let Some(id) = Identity::from_secret_bytes(&b) {
                    return id;
                }
            }
            Err(keyring::Error::NoEntry) => {
                let id = read_file(data_dir).unwrap_or_else(Identity::generate);
                if e.set_secret(&id.secret_bytes()).is_ok() {
                    let _ = std::fs::remove_file(file_path(data_dir));
                    return id;
                }
                tracing::warn!("OS keystore unavailable; keeping identity in the data directory");
                let _ = write_file(data_dir, &id);
                return id;
            }
            Err(err) => tracing::warn!("OS keystore unavailable ({err}); using the data directory"),
        }
    }
    if let Some(id) = read_file(data_dir) {
        return id;
    }
    let id = Identity::generate();
    if let Err(e) = write_file(data_dir, &id) {
        tracing::error!("could not store identity keys: {e}");
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_persists_and_resets() {
        let dir = std::env::temp_dir().join(format!("callxl-id-{}", uuid::Uuid::new_v4()));
        let a = load_or_create(&dir, true, false).key();
        assert_eq!(load_or_create(&dir, true, false).key(), a);
        assert_ne!(load_or_create(&dir, true, true).key(), a);
        let _ = std::fs::remove_dir_all(dir);
    }
}
