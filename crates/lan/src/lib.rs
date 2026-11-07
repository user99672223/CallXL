//! CallXL LAN path (CLAUDE.md §4): mDNS service `_callxl._tcp.local` for zero-server
//! discovery, and direct TCP signaling (length-prefixed envelope frames).

#[cfg(target_vendor = "apple")]
mod dnssd;
pub mod mdns;
pub mod tcp;

#[cfg(target_vendor = "apple")]
pub use dnssd::Discovery;
#[cfg(not(target_vendor = "apple"))]
pub use mdns::Discovery;
pub use mdns::{DiscoveryEvent, LanAdvert, SERVICE_TYPE};
pub use tcp::{ConnId, LanEvent, LanSignaling};

#[derive(Debug, thiserror::Error)]
pub enum LanError {
    #[error("could not open the local signaling port: {0}")]
    Bind(std::io::Error),
    #[error("local network discovery is unavailable: {0}")]
    Mdns(String),
}
