//! mDNS advertise + browse of `_callxl._tcp.local` (mdns-sd).
//!
//! TXT records: `k` = Ed25519 key (hex), `x` = X25519 key (hex), `n` = display name,
//! `v` = protocol version. Sessions are not put in TXT (size-limited); they are
//! exchanged in the `hello` on the direct TCP connection that discovery triggers.

use std::collections::HashMap;
use std::net::IpAddr;

use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

use crate::LanError;

pub const SERVICE_TYPE: &str = "_callxl._tcp.local.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanAdvert {
    pub key: String,
    pub agree_pk: String,
    pub name: String,
    pub port: u16,
    /// Addresses to advertise; empty = all interface addresses.
    pub addrs: Vec<IpAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryEvent {
    Found { key: String, agree_pk: String, name: String, addrs: Vec<IpAddr>, port: u16 },
    Lost { key: String },
}

pub struct Discovery {
    daemon: ServiceDaemon,
    fullname: String,
}

fn instance_for(key: &str) -> String {
    format!("callxl-{}", &key[..key.len().min(16)])
}

impl Discovery {
    /// Starts advertising `me` and browsing for other CallXL devices.
    /// `loopback` additionally enables 127.0.0.1/::1 (tests and same-host peers).
    pub fn start(me: &LanAdvert, loopback: bool, events: mpsc::Sender<DiscoveryEvent>) -> Result<Self, LanError> {
        let daemon = ServiceDaemon::new().map_err(|e| LanError::Mdns(e.to_string()))?;
        if loopback {
            let _ = daemon.enable_interface(vec![IfKind::LoopbackV4, IfKind::LoopbackV6]);
        }
        let instance = instance_for(&me.key);
        let host = format!("{instance}.local.");
        let props = [("k", me.key.as_str()), ("x", me.agree_pk.as_str()), ("n", me.name.as_str()), ("v", "1")];
        let info = if me.addrs.is_empty() {
            ServiceInfo::new(SERVICE_TYPE, &instance, &host, "", me.port, &props[..]).map(|i| i.enable_addr_auto())
        } else {
            ServiceInfo::new(SERVICE_TYPE, &instance, &host, &me.addrs[..], me.port, &props[..])
        }
        .map_err(|e| LanError::Mdns(e.to_string()))?;
        let fullname = info.get_fullname().to_string();
        daemon.register(info).map_err(|e| LanError::Mdns(e.to_string()))?;

        let rx = daemon.browse(SERVICE_TYPE).map_err(|e| LanError::Mdns(e.to_string()))?;
        let own_key = me.key.clone();
        std::thread::Builder::new()
            .name("callxl-mdns".into())
            .spawn(move || {
                let mut by_fullname: HashMap<String, String> = HashMap::new();
                while let Ok(ev) = rx.recv() {
                    let out = match ev {
                        ServiceEvent::ServiceResolved(info) => {
                            let Some(key) = info.get_property_val_str("k").map(str::to_string) else { continue };
                            if key == own_key {
                                continue;
                            }
                            by_fullname.insert(info.get_fullname().to_string(), key.clone());
                            DiscoveryEvent::Found {
                                key,
                                agree_pk: info.get_property_val_str("x").unwrap_or_default().to_string(),
                                name: info.get_property_val_str("n").unwrap_or_default().to_string(),
                                addrs: info.get_addresses().iter().copied().collect(),
                                port: info.get_port(),
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => match by_fullname.remove(&fullname) {
                            Some(key) => DiscoveryEvent::Lost { key },
                            None => continue,
                        },
                        _ => continue,
                    };
                    if events.blocking_send(out).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| LanError::Mdns(e.to_string()))?;
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}
