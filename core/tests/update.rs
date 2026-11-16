//! End-to-end update flow against a fake GitHub API (no real release).
use relay_desktop_core::update::{hex, UpdateError, Updater};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// Minimal HTTP server: path → (status, extra headers, body).
type Routes = Arc<Mutex<Vec<(String, u16, String, Vec<u8>)>>>;

fn serve(routes: Routes) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let routes = routes.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap() <= 2 {
                        break;
                    }
                }
                let found = routes.lock().unwrap().iter().find(|(p, ..)| *p == path).cloned();
                let (status, headers, body) = found
                    .map(|(_, s, h, b)| (s, h, b))
                    .unwrap_or((404, String::new(), b"not found".to_vec()));
                let mut w = s;
                let _ = write!(
                    w,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                    body.len()
                );
                let _ = w.write_all(&body);
            });
        }
    });
    port
}

#[test]
fn finds_downloads_and_verifies_update() {
    let msi: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let routes = Arc::new(Mutex::new(Vec::new()));
    let port = serve(routes.clone());
    let base = format!("http://127.0.0.1:{port}");
    let releases = format!(
        r#"[{{"tag_name":"desktop-v0.2.0","body":"Nouveautés","html_url":"{base}/r","assets":[
            {{"name":"Relay-0.2.0-x64.msi","browser_download_url":"{base}/dl/msi","size":{}}},
            {{"name":"SHA256SUMS","browser_download_url":"{base}/dl/sums"}}]}}]"#,
        msi.len()
    );
    let good = format!("{}  Relay-0.2.0-x64.msi\n", hex(&Sha256::digest(&msi)));
    {
        let mut r = routes.lock().unwrap();
        r.push((
            "/repos/overnite22/relay-desktop-prod/releases?per_page=20".into(),
            200,
            String::new(),
            releases.into_bytes(),
        ));
        // GitHub answers asset downloads with a redirect to its CDN.
        r.push(("/dl/msi".into(), 302, format!("Location: {base}/cdn/msi\r\n"), Vec::new()));
        r.push(("/cdn/msi".into(), 200, String::new(), msi.clone()));
        r.push(("/dl/sums".into(), 200, String::new(), good.into_bytes()));
    }
    let up = Updater::new(&base, "overnite22/relay-desktop-prod", "relay-desktop-test");

    assert!(up.available("0.2.0").unwrap().is_none(), "same version is not an update");
    let rel = up.available("0.1.0").unwrap().expect("0.2.0 is newer");
    assert_eq!(rel.notes, "Nouveautés");

    let dir = tempfile::tempdir().unwrap();
    let seen = Mutex::new(0u64);
    let path = up
        .download(&rel, dir.path(), |done, total| {
            assert_eq!(total, msi.len() as u64);
            *seen.lock().unwrap() = done;
        })
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), msi);
    assert_eq!(*seen.lock().unwrap(), msi.len() as u64);

    // Cancelling during transfer removes the partial package, and a retry works.
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let cancelled_dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        up.download_cancellable(
            &rel,
            cancelled_dir.path(),
            |_, _| cancelled.store(true, std::sync::atomic::Ordering::SeqCst),
            || cancelled.load(std::sync::atomic::Ordering::SeqCst),
        ),
        Err(UpdateError::Cancelled)
    ));
    assert_eq!(std::fs::read_dir(cancelled_dir.path()).unwrap().count(), 0);
    assert!(up.download(&rel, cancelled_dir.path(), |_, _| {}).is_ok());

    // A tampered package is rejected and nothing is left behind.
    routes.lock().unwrap().iter_mut().find(|(p, ..)| p == "/dl/sums").unwrap().3 =
        format!("{}  Relay-0.2.0-x64.msi\n", "0".repeat(64)).into_bytes();
    let dir2 = tempfile::tempdir().unwrap();
    assert!(matches!(up.download(&rel, dir2.path(), |_, _| {}), Err(UpdateError::Checksum)));
    assert_eq!(std::fs::read_dir(dir2.path()).unwrap().count(), 0);

    // Offline / unreachable server gives a network error, not a panic.
    let off = Updater::new("http://127.0.0.1:1", "overnite22/relay-desktop-prod", "t");
    assert!(matches!(off.available("0.1.0"), Err(UpdateError::Network(_))));
}
