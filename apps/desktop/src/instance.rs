//! Single instance (CLAUDE.md §8): the first launch owns a local socket (abstract Unix
//! socket on Linux, named pipe on Windows — neither leaves stale files behind); a second
//! launch connects, asks the first to show its window, and exits.

use std::io::{BufRead, BufReader, Write};

use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream, ToNsName, prelude::*};

pub enum Instance {
    /// We are the first instance; `on_show` runs (on a helper thread) whenever a later
    /// launch asks us to come to the front.
    Primary,
    /// Another instance is running and has been asked to show itself.
    Secondary,
}

/// Socket name scoped to the user and the profile directory.
pub fn socket_name(profile: &str) -> String {
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
    let h = profile
        .bytes()
        .chain(user.bytes())
        .fold(0xcbf2_9ce4_8422_2325u64, |a, b| (a ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    format!("com.callxl.app-{h:016x}.sock")
}

pub fn acquire(name: &str, on_show: impl Fn() + Send + 'static) -> std::io::Result<Instance> {
    let ns = name.to_ns_name::<GenericNamespaced>()?;
    for _ in 0..2 {
        match ListenerOptions::new().name(ns.clone()).create_sync() {
            Ok(listener) => {
                std::thread::Builder::new().name("callxl-instance".into()).spawn(move || {
                    for conn in listener.incoming().flatten() {
                        let mut line = String::new();
                        if BufReader::new(conn).read_line(&mut line).is_ok() && line.trim() == "show" {
                            on_show();
                        }
                    }
                })?;
                return Ok(Instance::Primary);
            }
            // Linux: AddrInUse. Windows: the first pipe instance was created with
            // FILE_FLAG_FIRST_PIPE_INSTANCE, so another creation is "Access is denied".
            Err(e) if matches!(e.kind(), std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied) => {
                if let Ok(mut s) = Stream::connect(ns.clone()) {
                    let _ = s.write_all(b"show\n");
                    let _ = s.flush();
                    return Ok(Instance::Secondary);
                }
                // The owner exited between our two calls: try to become primary again.
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("could not acquire or reach the running CallXL instance"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn second_launch_focuses_the_first() {
        let name = socket_name(&format!("test-{}-{:?}", std::process::id(), std::time::Instant::now()));
        let (tx, rx) = mpsc::channel();
        let first = acquire(&name, move || tx.send(()).unwrap()).unwrap();
        assert!(matches!(first, Instance::Primary));
        let second = acquire(&name, || panic!("the second instance must not listen")).unwrap();
        assert!(matches!(second, Instance::Secondary));
        rx.recv_timeout(Duration::from_secs(5)).expect("first instance was asked to show itself");
        let third = acquire(&name, || {}).unwrap();
        assert!(matches!(third, Instance::Secondary));
        rx.recv_timeout(Duration::from_secs(5)).expect("asked again");
    }
}
