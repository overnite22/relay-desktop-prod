//! Live test: RELAY_SSH_HOST, RELAY_SSH_USER, RELAY_SSH_KEY (path to a private key).
use relay_desktop_core::ssh::{spawn, Event, Input, Params};
use std::sync::mpsc;
use std::time::Duration;
use zeroize::Zeroizing;

fn session(
    known: &std::path::Path,
) -> (
    tokio::sync::mpsc::UnboundedSender<Input>,
    mpsc::Receiver<Event>,
    tokio::runtime::Runtime,
) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (tx, rx) = mpsc::channel();
    let p = Params {
        host: std::env::var("RELAY_SSH_HOST").unwrap(),
        port: 22,
        username: std::env::var("RELAY_SSH_USER").unwrap(),
        password: None,
        private_key: Some(Zeroizing::new(
            std::fs::read_to_string(std::env::var("RELAY_SSH_KEY").unwrap()).unwrap(),
        )),
        passphrase: None,
        cols: 100,
        rows: 30,
        known_hosts: known.to_path_buf(),
        ..Default::default()
    };
    let input = spawn(rt.handle(), p, move |e| {
        let _ = tx.send(e);
    });
    (input, rx, rt)
}

#[test]
#[ignore]
fn live_ssh_session() {
    let dir = tempfile::tempdir().unwrap();
    let known = dir.path().join("known_hosts.json");

    let (input, rx, _rt) = session(&known);
    match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
        Event::Connected { new_key, fingerprint } => assert!(new_key && fingerprint.starts_with("SHA256:")),
        other => panic!("{other:?}"),
    }
    input.send(Input::Resize(120, 40)).unwrap();
    input.send(Input::Data(b"echo RELAY_$((6*7))_OK\r".to_vec())).unwrap();
    let mut out = String::new();
    while !out.contains("RELAY_42_OK") && !out.contains("RELAY_$((6*7))_OK") {
        match rx.recv_timeout(Duration::from_secs(15)).unwrap() {
            Event::Data(d) => out.push_str(&String::from_utf8_lossy(&d)),
            Event::Closed(e) => panic!("closed: {e:?}\n{out}"),
            _ => {}
        }
    }
    input.send(Input::Close).unwrap();

    // Second connection: the pinned key is recognized.
    let (input, rx, _rt2) = session(&known);
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(20)).unwrap(),
        Event::Connected { new_key: false, .. }
    ));
    input.send(Input::Close).unwrap();

    // A different pinned key aborts the connection.
    let host = format!("{}:22", std::env::var("RELAY_SSH_HOST").unwrap());
    std::fs::write(
        &known,
        format!("{{\"{host}\": \"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEZw2Pc/0UK2c6h3prmfd1MFot7/RKR0poSydxpn7BFy other\"}}"),
    )
    .unwrap();
    let (_input, rx, _rt3) = session(&known);
    match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
        Event::Closed(Some(e)) => assert!(e.contains("a changé"), "{e}"),
        other => panic!("{other:?}"),
    }
}

/// Non-interactive command on a live server:
/// RELAY_TEST_SSH=host:port RELAY_TEST_SSH_USER=… RELAY_TEST_SSH_KEY=path cargo test -- --ignored exec_live
#[test]
#[ignore]
fn exec_live() {
    let target = std::env::var("RELAY_TEST_SSH").unwrap();
    let (host, port) = target.rsplit_once(':').unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = relay_desktop_core::ssh::Params {
        host: host.into(),
        port: port.parse().unwrap(),
        username: std::env::var("RELAY_TEST_SSH_USER").unwrap(),
        password: None,
        private_key: Some(zeroize::Zeroizing::new(
            std::fs::read_to_string(std::env::var("RELAY_TEST_SSH_KEY").unwrap()).unwrap(),
        )),
        passphrase: None,
        cols: 80,
        rows: 24,
        known_hosts: dir.path().join("kh.json"),
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let out = rt.block_on(relay_desktop_core::ssh::exec(&p, "hostname", None, 1 << 16)).unwrap();
    assert_eq!(out.exit_code, Some(0));
    assert!(out.new_key && !out.output.trim().is_empty());
    let out = rt.block_on(relay_desktop_core::ssh::exec(&p, "hostname", None, 3)).unwrap();
    assert!(!out.new_key && out.truncated && out.output.len() == 3);
    let out = rt.block_on(relay_desktop_core::ssh::exec(&p, "exit 3", None, 64)).unwrap();
    assert_eq!(out.exit_code, Some(3));
}
