//! TCP reachability checks run from this computer (availability lights).

use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub ok: bool,
    pub millis: u128,
    pub address: String,
    pub error: String,
}

pub fn tcp(host: &str, port: u16, timeout: Duration) -> Probe {
    let start = Instant::now();
    let addrs = match (host, port).to_socket_addrs() {
        Ok(a) => a.collect::<Vec<_>>(),
        Err(e) => {
            return Probe {
                ok: false,
                millis: 0,
                address: String::new(),
                error: format!("nom introuvable : {e}"),
            }
        }
    };
    let mut last = String::from("aucune adresse");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => {
                return Probe {
                    ok: true,
                    millis: start.elapsed().as_millis(),
                    address: addr.ip().to_string(),
                    error: String::new(),
                }
            }
            Err(e) => {
                last = if e.kind() == std::io::ErrorKind::TimedOut {
                    "délai dépassé".into()
                } else {
                    e.to_string()
                }
            }
        }
    }
    Probe {
        ok: false,
        millis: start.elapsed().as_millis(),
        address: String::new(),
        error: last,
    }
}

/// Probes many `(id, host, port)` targets in parallel (bounded).
pub fn many(targets: Vec<(String, String, u16)>, timeout: Duration) -> Vec<(String, Probe)> {
    let chunk = targets.len().div_ceil(16).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = targets
            .chunks(chunk)
            .map(|part| s.spawn(move || part.iter().map(|(id, h, p)| (id.clone(), tcp(h, *p, timeout))).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn open_and_closed_ports() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let r = super::many(
            vec![("a".into(), "127.0.0.1".into(), port), ("b".into(), "127.0.0.1".into(), 1)],
            std::time::Duration::from_millis(500),
        );
        assert!(r.iter().any(|(id, p)| id == "a" && p.ok));
        assert!(r.iter().any(|(id, p)| id == "b" && !p.ok));
    }
}
