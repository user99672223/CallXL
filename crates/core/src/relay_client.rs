//! Client for the e2e userspace impairment relay (D-0003 `proxy` backend): registers a
//! local host candidate and gets back the relay address that stands in for it.
//! Only used when `DevOptions::candidate_rewrite` is set (`--debug-candidate-rewrite`).

use std::net::SocketAddr;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

pub async fn register(relay: SocketAddr, address: &str, port: u16) -> std::io::Result<SocketAddr> {
    let fut = async {
        let mut s = TcpStream::connect(relay).await?;
        let target: SocketAddr = format!("{}:{port}", if address.contains(':') { format!("[{address}]") } else { address.to_string() })
            .parse()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{e}")))?;
        s.write_all(format!("{{\"register\":\"{target}\"}}\n").as_bytes()).await?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).await?;
        let v: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        v.get("addr")
            .and_then(|a| a.as_str())
            .and_then(|a| a.parse().ok())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "relay gave no address"))
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), fut)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "relay did not answer"))?
}
