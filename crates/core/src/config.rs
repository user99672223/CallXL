//! Build-time and default configuration (CLAUDE.md §5, §12).

/// Cloud signaling endpoint: the Worker deployed by `.github/workflows/worker.yml`
/// (run 42355101376). Overridable in Settings (`set_signaling_url`); after a redeploy
/// under another Cloudflare account, put the URL that workflow prints here.
pub const DEFAULT_SIGNALING_URL: &str = "wss://callxl.hbadgujar44.workers.dev/ws";

/// Presented to the Worker to obtain a 1-hour token. Baked in at build time from env
/// `CALLXL_APP_SECRET` (CI secret); a random DEV value otherwise (see build.rs).
pub const APP_SECRET: &str = env!("CALLXL_BUILD_APP_SECRET");

pub fn app_secret_is_dev() -> bool {
    APP_SECRET.starts_with("dev-")
}

/// The signaling URL is a placeholder that cannot resolve; skip cloud quietly.
pub fn is_placeholder_url(url: &str) -> bool {
    url.contains('<') || url.contains('>')
}

/// Bitrate ceiling bounds (§3 user slider).
pub const MIN_CEILING_KBPS: u32 = 32;
pub const MAX_CEILING_KBPS: u32 = 510;

/// File transfer chunk size and in-flight window (§6).
pub const FILE_CHUNK: usize = 16 * 1024;
pub const FILE_WINDOW: u64 = 1024 * 1024;

/// Resilience timings (§4).
pub const ICE_RESTART_AFTER_MS: u64 = 500;
pub const RECONNECTING_AFTER_MS: u64 = 15_000;
pub const LEAVE_AFTER_MS: u64 = 30_000;

pub fn default_download_dir() -> String {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into());
    std::path::Path::new(&home).join("Downloads").join("CallXL").to_string_lossy().into_owned()
}
