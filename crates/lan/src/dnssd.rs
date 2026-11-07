//! Apple (iOS, macOS) discovery through the system's DNS-SD API (`dns_sd.h`,
//! mDNSResponder), advertising and browsing the same `_callxl._tcp.local` service with
//! the same TXT records as the mdns-sd backend, so desktop and iPhone see each other.
//!
//! Why not mdns-sd on Apple: since iOS 14 an app that sends or receives multicast UDP on
//! its own BSD sockets needs Apple's restricted `com.apple.developer.networking.multicast`
//! entitlement, which a sideloaded build cannot have; the system Bonjour API needs only
//! `NSLocalNetworkUsageDescription` + `NSBonjourServices` (DECISIONS.md, p2/ios).
//!
//! One thread owns every `DNSServiceRef` and runs the callbacks (poll on their sockets),
//! so the per-service state needs no locks. Addresses are reported IPv4 first (the LAN
//! signaling listener binds `0.0.0.0`); link-local IPv6 is skipped (no scope id in
//! `IpAddr`).

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd, OwnedFd};
use std::ptr;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::LanError;
use crate::mdns::{DiscoveryEvent, LanAdvert, instance_for};

const REGTYPE: &CStr = c"_callxl._tcp";
const DOMAIN: &CStr = c"local.";

#[allow(non_camel_case_types)]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub type DNSServiceRef = *mut c_void;
    pub type DNSServiceFlags = u32;
    pub type DNSServiceErrorType = i32;

    pub const FLAGS_MORE_COMING: u32 = 0x1;
    pub const FLAGS_ADD: u32 = 0x2;
    pub const PROTOCOL_IPV4: u32 = 0x1;
    pub const PROTOCOL_IPV6: u32 = 0x2;
    pub const NO_ERROR: i32 = 0;

    pub type RegisterReply = extern "C" fn(
        DNSServiceRef,
        DNSServiceFlags,
        DNSServiceErrorType,
        *const c_char,
        *const c_char,
        *const c_char,
        *mut c_void,
    );
    pub type BrowseReply = extern "C" fn(
        DNSServiceRef,
        DNSServiceFlags,
        u32,
        DNSServiceErrorType,
        *const c_char,
        *const c_char,
        *const c_char,
        *mut c_void,
    );
    pub type ResolveReply = extern "C" fn(
        DNSServiceRef,
        DNSServiceFlags,
        u32,
        DNSServiceErrorType,
        *const c_char,
        *const c_char,
        u16,
        u16,
        *const u8,
        *mut c_void,
    );
    pub type AddrInfoReply = extern "C" fn(
        DNSServiceRef,
        DNSServiceFlags,
        u32,
        DNSServiceErrorType,
        *const c_char,
        *const libc_sockaddr,
        u32,
        *mut c_void,
    );

    #[repr(C)]
    pub struct libc_sockaddr {
        pub sa_len: u8,
        pub sa_family: u8,
        pub sa_data: [u8; 14],
    }

    #[repr(C)]
    pub struct pollfd {
        pub fd: c_int,
        pub events: i16,
        pub revents: i16,
    }
    pub const POLLIN: i16 = 0x1;

    // dns_sd.h and poll.h live in libSystem, which every Apple binary links.
    unsafe extern "C" {
        pub fn DNSServiceRegister(
            sd: *mut DNSServiceRef,
            flags: DNSServiceFlags,
            interface: u32,
            name: *const c_char,
            regtype: *const c_char,
            domain: *const c_char,
            host: *const c_char,
            port_be: u16,
            txt_len: u16,
            txt: *const c_void,
            cb: RegisterReply,
            ctx: *mut c_void,
        ) -> DNSServiceErrorType;
        pub fn DNSServiceBrowse(
            sd: *mut DNSServiceRef,
            flags: DNSServiceFlags,
            interface: u32,
            regtype: *const c_char,
            domain: *const c_char,
            cb: BrowseReply,
            ctx: *mut c_void,
        ) -> DNSServiceErrorType;
        pub fn DNSServiceResolve(
            sd: *mut DNSServiceRef,
            flags: DNSServiceFlags,
            interface: u32,
            name: *const c_char,
            regtype: *const c_char,
            domain: *const c_char,
            cb: ResolveReply,
            ctx: *mut c_void,
        ) -> DNSServiceErrorType;
        pub fn DNSServiceGetAddrInfo(
            sd: *mut DNSServiceRef,
            flags: DNSServiceFlags,
            interface: u32,
            protocol: u32,
            hostname: *const c_char,
            cb: AddrInfoReply,
            ctx: *mut c_void,
        ) -> DNSServiceErrorType;
        pub fn DNSServiceRefSockFD(sd: DNSServiceRef) -> c_int;
        pub fn DNSServiceProcessResult(sd: DNSServiceRef) -> DNSServiceErrorType;
        pub fn DNSServiceRefDeallocate(sd: DNSServiceRef);
        pub fn poll(fds: *mut pollfd, n: u32, timeout_ms: c_int) -> c_int;
        pub fn pipe(fds: *mut c_int) -> c_int;
        pub fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
    }
}

/// Owned `DNSServiceRef`, deallocated on drop (closes its socket).
struct Svc(ffi::DNSServiceRef);

impl Svc {
    fn fd(&self) -> c_int {
        unsafe { ffi::DNSServiceRefSockFD(self.0) }
    }
}

impl Drop for Svc {
    fn drop(&mut self) {
        unsafe { ffi::DNSServiceRefDeallocate(self.0) }
    }
}

/// One discovered remote instance (keyed by service name).
struct Peer {
    /// Interfaces currently reporting it (browse add/remove are per interface).
    interfaces: u32,
    resolve: Option<Svc>,
    addrinfo: Option<Svc>,
    key: Option<String>,
    agree_pk: String,
    name: String,
    port: u16,
    addrs: Vec<IpAddr>,
    announced: bool,
}

/// State owned by the discovery thread; callbacks get `*mut Ctx`.
struct Ctx {
    own_instance: String,
    own_key: String,
    peers: HashMap<String, Peer>,
    /// (service name) whose resolve / addrinfo callback fired during this pass.
    resolved: Vec<(String, ResolveResult)>,
    addr_updates: Vec<(String, IpAddr, bool)>,
    addr_batch_done: Vec<String>,
    browse: Vec<(String, u32, bool)>,
    /// Service name a ref belongs to while its callback runs.
    current: String,
    events: mpsc::Sender<DiscoveryEvent>,
}

struct ResolveResult {
    host: String,
    port: u16,
    txt: HashMap<String, String>,
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// TXT record: a sequence of length-prefixed `key=value` strings (RFC 6763 §6).
pub(crate) fn encode_txt(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in entries {
        let mut e = format!("{k}={v}").into_bytes();
        // Truncate on a char boundary to the 255-byte entry limit.
        if e.len() > 255 {
            let s = format!("{k}={v}");
            let mut end = 255;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            e = s.as_bytes()[..end].to_vec();
        }
        out.push(e.len() as u8);
        out.extend_from_slice(&e);
    }
    out
}

pub(crate) fn decode_txt(mut b: &[u8]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    while let Some((&len, rest)) = b.split_first() {
        let len = (len as usize).min(rest.len());
        let (entry, rest) = rest.split_at(len);
        let s = String::from_utf8_lossy(entry);
        if let Some((k, v)) = s.split_once('=') {
            m.insert(k.to_string(), v.to_string());
        }
        b = rest;
    }
    m
}

extern "C" fn on_register(
    _sd: ffi::DNSServiceRef,
    _flags: ffi::DNSServiceFlags,
    err: ffi::DNSServiceErrorType,
    name: *const c_char,
    _regtype: *const c_char,
    _domain: *const c_char,
    _ctx: *mut c_void,
) {
    if err != ffi::NO_ERROR {
        tracing::warn!("Bonjour registration failed ({err})");
    } else {
        tracing::info!("Bonjour: advertising {}", cstr(name));
    }
}

extern "C" fn on_browse(
    _sd: ffi::DNSServiceRef,
    flags: ffi::DNSServiceFlags,
    interface: u32,
    err: ffi::DNSServiceErrorType,
    name: *const c_char,
    _regtype: *const c_char,
    _domain: *const c_char,
    ctx: *mut c_void,
) {
    if err != ffi::NO_ERROR {
        tracing::warn!("Bonjour browse error ({err})");
        return;
    }
    let ctx = unsafe { &mut *(ctx as *mut Ctx) };
    ctx.browse.push((cstr(name), interface, flags & ffi::FLAGS_ADD != 0));
}

extern "C" fn on_resolve(
    _sd: ffi::DNSServiceRef,
    _flags: ffi::DNSServiceFlags,
    _interface: u32,
    err: ffi::DNSServiceErrorType,
    _fullname: *const c_char,
    host: *const c_char,
    port_be: u16,
    txt_len: u16,
    txt: *const u8,
    ctx: *mut c_void,
) {
    let ctx = unsafe { &mut *(ctx as *mut Ctx) };
    if err != ffi::NO_ERROR {
        tracing::warn!("Bonjour resolve of {} failed ({err})", ctx.current);
        return;
    }
    let txt = if txt.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(txt, txt_len as usize) } };
    let r = ResolveResult { host: cstr(host), port: u16::from_be(port_be), txt: decode_txt(txt) };
    let name = ctx.current.clone();
    ctx.resolved.push((name, r));
}

extern "C" fn on_addrinfo(
    _sd: ffi::DNSServiceRef,
    flags: ffi::DNSServiceFlags,
    _interface: u32,
    err: ffi::DNSServiceErrorType,
    _host: *const c_char,
    addr: *const ffi::libc_sockaddr,
    _ttl: u32,
    ctx: *mut c_void,
) {
    let ctx = unsafe { &mut *(ctx as *mut Ctx) };
    let name = ctx.current.clone();
    if err == ffi::NO_ERROR && !addr.is_null() {
        // sockaddr_in: len, family, port(2), addr(4); sockaddr_in6: len, family, port(2),
        // flowinfo(4), addr(16), scope(4).
        let family = unsafe { (*addr).sa_family };
        let ip = match family {
            2 => {
                let p = addr as *const u8;
                let b: [u8; 4] = unsafe { *(p.add(4) as *const [u8; 4]) };
                Some(IpAddr::V4(Ipv4Addr::from(b)))
            }
            30 => {
                let p = addr as *const u8;
                let b: [u8; 16] = unsafe { *(p.add(8) as *const [u8; 16]) };
                Some(IpAddr::V6(Ipv6Addr::from(b)))
            }
            _ => None,
        };
        if let Some(ip) = ip {
            ctx.addr_updates.push((name.clone(), ip, flags & ffi::FLAGS_ADD != 0));
        }
    }
    if flags & ffi::FLAGS_MORE_COMING == 0 {
        ctx.addr_batch_done.push(name);
    }
}

fn usable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => !a.is_unspecified() && !a.is_link_local(),
        IpAddr::V6(a) => !a.is_unspecified() && (a.segments()[0] & 0xffc0) != 0xfe80,
    }
}

impl Ctx {
    /// Applies what the callbacks of the last pass recorded (outside the callbacks, so
    /// starting/stopping refs never happens inside `DNSServiceProcessResult`).
    fn apply(&mut self, self_ptr: *mut c_void) {
        for (name, interface, add) in std::mem::take(&mut self.browse) {
            if name == self.own_instance {
                continue;
            }
            if add {
                let p = self.peers.entry(name.clone()).or_insert_with(|| Peer {
                    interfaces: 0,
                    resolve: None,
                    addrinfo: None,
                    key: None,
                    agree_pk: String::new(),
                    name: String::new(),
                    port: 0,
                    addrs: Vec::new(),
                    announced: false,
                });
                p.interfaces += 1;
                if p.resolve.is_none() && p.key.is_none() {
                    let Ok(cname) = CString::new(name.clone()) else { continue };
                    let mut sd: ffi::DNSServiceRef = ptr::null_mut();
                    let e = unsafe {
                        ffi::DNSServiceResolve(
                            &mut sd,
                            0,
                            interface,
                            cname.as_ptr(),
                            REGTYPE.as_ptr(),
                            DOMAIN.as_ptr(),
                            on_resolve,
                            self_ptr,
                        )
                    };
                    if e == ffi::NO_ERROR {
                        p.resolve = Some(Svc(sd));
                    } else {
                        tracing::warn!("Bonjour resolve of {name} could not start ({e})");
                    }
                }
            } else if let Some(p) = self.peers.get_mut(&name) {
                p.interfaces = p.interfaces.saturating_sub(1);
                if p.interfaces == 0 {
                    let p = self.peers.remove(&name).expect("present");
                    if p.announced
                        && let Some(key) = p.key
                    {
                        let _ = self.events.blocking_send(DiscoveryEvent::Lost { key });
                    }
                }
            }
        }

        for (name, r) in std::mem::take(&mut self.resolved) {
            let Some(p) = self.peers.get_mut(&name) else { continue };
            p.resolve = None; // one answer is enough
            let Some(key) = r.txt.get("k").cloned() else { continue };
            if key == self.own_key {
                continue;
            }
            p.key = Some(key);
            p.agree_pk = r.txt.get("x").cloned().unwrap_or_default();
            p.name = r.txt.get("n").cloned().unwrap_or_default();
            p.port = r.port;
            let Ok(host) = CString::new(r.host) else { continue };
            let mut sd: ffi::DNSServiceRef = ptr::null_mut();
            let e = unsafe {
                ffi::DNSServiceGetAddrInfo(
                    &mut sd,
                    0,
                    0,
                    ffi::PROTOCOL_IPV4 | ffi::PROTOCOL_IPV6,
                    host.as_ptr(),
                    on_addrinfo,
                    self_ptr,
                )
            };
            if e == ffi::NO_ERROR {
                p.addrinfo = Some(Svc(sd));
            } else {
                tracing::warn!("Bonjour address lookup for {name} could not start ({e})");
            }
        }

        for (name, ip, add) in std::mem::take(&mut self.addr_updates) {
            let Some(p) = self.peers.get_mut(&name) else { continue };
            if add && usable(&ip) && !p.addrs.contains(&ip) {
                p.addrs.push(ip);
            } else if !add {
                p.addrs.retain(|a| *a != ip);
            }
        }

        let mut done = std::mem::take(&mut self.addr_batch_done);
        done.dedup();
        for name in done {
            let Some(p) = self.peers.get_mut(&name) else { continue };
            let (Some(key), false) = (p.key.clone(), p.addrs.is_empty()) else { continue };
            let mut addrs = p.addrs.clone();
            addrs.sort_by_key(|a| a.is_ipv6()); // IPv4 first (stable)
            p.announced = true;
            let _ = self.events.blocking_send(DiscoveryEvent::Found {
                key,
                agree_pk: p.agree_pk.clone(),
                name: p.name.clone(),
                addrs,
                port: p.port,
            });
        }
    }
}

pub struct Discovery {
    /// Writing a byte here stops the thread (which owns and deallocates every ref).
    stop_w: OwnedFd,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    use std::os::fd::FromRawFd;
    let mut fds = [0 as c_int; 2];
    if unsafe { ffi::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

impl Discovery {
    /// Starts advertising `me` and browsing for other CallXL devices. mDNSResponder
    /// reports services of this host too, so `loopback` needs no special handling;
    /// `me.addrs` is not used (the system advertises the host's own addresses).
    pub fn start(me: &LanAdvert, _loopback: bool, events: mpsc::Sender<DiscoveryEvent>) -> Result<Self, LanError> {
        let err = |what: &str, e: i32| LanError::Mdns(format!("Bonjour {what} failed (error {e})"));
        let instance = instance_for(&me.key);
        let txt = encode_txt(&[("k", &me.key), ("x", &me.agree_pk), ("n", &me.name), ("v", "1")]);
        let cname = CString::new(instance.clone()).map_err(|e| LanError::Mdns(e.to_string()))?;
        let mut reg: ffi::DNSServiceRef = ptr::null_mut();
        let e = unsafe {
            ffi::DNSServiceRegister(
                &mut reg,
                0,
                0,
                cname.as_ptr(),
                REGTYPE.as_ptr(),
                ptr::null(),
                ptr::null(),
                me.port.to_be(),
                txt.len() as u16,
                txt.as_ptr() as *const c_void,
                on_register,
                ptr::null_mut(),
            )
        };
        if e != ffi::NO_ERROR {
            return Err(err("registration", e));
        }
        let reg = Svc(reg);

        let ctx = Box::new(Ctx {
            own_instance: instance,
            own_key: me.key.clone(),
            peers: HashMap::new(),
            resolved: Vec::new(),
            addr_updates: Vec::new(),
            addr_batch_done: Vec::new(),
            browse: Vec::new(),
            current: String::new(),
            events,
        });
        let ctx_ptr = Box::into_raw(ctx);
        let mut browse: ffi::DNSServiceRef = ptr::null_mut();
        let e = unsafe {
            ffi::DNSServiceBrowse(
                &mut browse,
                0,
                0,
                REGTYPE.as_ptr(),
                DOMAIN.as_ptr(),
                on_browse,
                ctx_ptr as *mut c_void,
            )
        };
        if e != ffi::NO_ERROR {
            drop(unsafe { Box::from_raw(ctx_ptr) });
            return Err(err("browse", e));
        }
        let browse = Svc(browse);
        let (stop_r, stop_w) = pipe().map_err(|e| LanError::Mdns(e.to_string()))?;

        // Raw pointers are not Send; the thread becomes their only user.
        struct SendPtr(*mut Ctx);
        unsafe impl Send for SendPtr {}
        struct SendSvc(Svc, Svc);
        unsafe impl Send for SendSvc {}
        let p = SendPtr(ctx_ptr);
        let refs = SendSvc(reg, browse);
        let thread = std::thread::Builder::new()
            .name("callxl-bonjour".into())
            .spawn(move || {
                let p = p;
                let refs = refs;
                run(p.0, &refs.0, &refs.1, stop_r);
                drop(refs);
                drop(unsafe { Box::from_raw(p.0) });
            })
            .map_err(|e| LanError::Mdns(e.to_string()))?;
        Ok(Self { stop_w, thread: Some(thread) })
    }
}

fn run(ctx_ptr: *mut Ctx, reg: &Svc, browse: &Svc, stop_r: OwnedFd) {
    let self_ptr = ctx_ptr as *mut c_void;
    loop {
        // Poll set: stop pipe, registration, browse, then every live resolve/addrinfo.
        let ctx = unsafe { &mut *ctx_ptr };
        let mut fds = vec![
            ffi::pollfd { fd: stop_r.as_raw_fd(), events: ffi::POLLIN, revents: 0 },
            ffi::pollfd { fd: reg.fd(), events: ffi::POLLIN, revents: 0 },
            ffi::pollfd { fd: browse.fd(), events: ffi::POLLIN, revents: 0 },
        ];
        let mut owners: Vec<(String, ffi::DNSServiceRef)> = Vec::new();
        for (name, p) in &ctx.peers {
            for s in [&p.resolve, &p.addrinfo].into_iter().flatten() {
                fds.push(ffi::pollfd { fd: s.fd(), events: ffi::POLLIN, revents: 0 });
                owners.push((name.clone(), s.0));
            }
        }
        let n = unsafe { ffi::poll(fds.as_mut_ptr(), fds.len() as u32, Duration::from_secs(5).as_millis() as c_int) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            tracing::warn!("Bonjour poll failed: {e}");
            return;
        }
        if fds[0].revents != 0 {
            return;
        }
        let ready = |i: usize| fds[i].revents != 0;
        if ready(1) {
            unsafe { ffi::DNSServiceProcessResult(reg.0) };
        }
        if ready(2) && unsafe { ffi::DNSServiceProcessResult(browse.0) } != ffi::NO_ERROR {
            tracing::warn!("Bonjour browse connection lost");
            return;
        }
        for (i, (name, sd)) in owners.iter().enumerate() {
            if ready(3 + i) {
                ctx.current = name.clone();
                unsafe { ffi::DNSServiceProcessResult(*sd) };
            }
        }
        ctx.apply(self_ptr);
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let b = 1u8;
        unsafe { ffi::write(self.stop_w.as_raw_fd(), &b as *const u8 as *const c_void, 1) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_round_trip() {
        let long = "é".repeat(200);
        let t = encode_txt(&[("k", "ab"), ("n", &long), ("v", "1")]);
        let m = decode_txt(&t);
        assert_eq!(m["k"], "ab");
        assert_eq!(m["v"], "1");
        assert!(m["n"].len() <= 253 && m["n"].chars().all(|c| c == 'é'));
    }

    /// Two advertisers in one process find each other through mDNSResponder
    /// (runs on the macOS CI runner; the iOS Simulator uses the same daemon).
    #[test]
    fn two_peers_discover_each_other() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let ka = "a1".repeat(32);
        let kb = "b2".repeat(32);
        let (ta, mut ra) = mpsc::channel(16);
        let (tb, mut rb) = mpsc::channel(16);
        let adv = |key: &str, port| LanAdvert {
            key: key.into(),
            agree_pk: "cc".repeat(32),
            name: format!("peer {port}"),
            port,
            addrs: vec![],
        };
        let _a = Discovery::start(&adv(&ka, 40001), true, ta).unwrap();
        let _b = Discovery::start(&adv(&kb, 40002), true, tb).unwrap();
        let t0 = std::time::Instant::now();
        let found = |rx: &mut mpsc::Receiver<DiscoveryEvent>, want: &str| {
            rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(20), async {
                    loop {
                        match rx.recv().await {
                            Some(DiscoveryEvent::Found { key, addrs, port, name, .. }) if key == want => {
                                break (addrs, port, name);
                            }
                            Some(_) => continue,
                            None => panic!("discovery stopped"),
                        }
                    }
                })
                .await
                .expect("peer not discovered within 20 s")
            })
        };
        let (addrs, port, name) = found(&mut ra, &kb);
        assert_eq!((port, name.as_str()), (40002, "peer 40002"));
        assert!(!addrs.is_empty());
        let (_, port, _) = found(&mut rb, &ka);
        assert_eq!(port, 40001);
        println!("bonjour: both peers found in {:?}, b's addrs {addrs:?}", t0.elapsed());
    }
}
