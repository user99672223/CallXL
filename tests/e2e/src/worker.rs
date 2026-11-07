//! Runs the Cloudflare Worker locally (`wrangler dev --local`) for cloud-signaling tests.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::process::{Child, Command};

pub struct LocalWorker {
    child: Child,
    pub port: u16,
}

pub fn worker_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../infra/worker")
}

impl LocalWorker {
    /// Starts `wrangler dev --local` on `port` with test secrets; waits until it answers.
    pub async fn start(port: u16, app_secret: &str, log: PathBuf) -> Result<Self> {
        let dir = worker_dir();
        let wrangler = dir.join("node_modules/.bin/wrangler");
        if !wrangler.exists() {
            bail!("{} missing: run `npm ci` in infra/worker", wrangler.display());
        }
        let log = std::fs::File::create(log)?;
        let child = Command::new(wrangler)
            .current_dir(&dir)
            .args(["dev", "--local", "--ip", "127.0.0.1", "--port", &port.to_string(), "--show-interactive-dev-session=false"])
            .args(["--var", &format!("APP_SECRET:{app_secret}"), "--var", "TOKEN_SIGNING_KEY:e2e-signing-key"])
            .env("WRANGLER_SEND_METRICS", "false")
            .env("CI", "true")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .spawn()
            .context("spawn wrangler")?;
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if let Ok(mut s) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let _ = s.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await;
                let mut buf = Vec::new();
                let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf)).await;
                if String::from_utf8_lossy(&buf).contains("200") {
                    return Ok(Self { child, port });
                }
            }
            if Instant::now() > deadline {
                bail!("wrangler dev did not come up on port {port}");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub fn url(&self) -> String {
        format!("ws://127.0.0.1:{}/ws", self.port)
    }
}

impl Drop for LocalWorker {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
