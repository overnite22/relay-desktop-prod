//! SFTP browser against an in-process SSH + SFTP server (russh, russh-sftp)
//! serving a temporary folder as `/`.
#[path = "support/sftp_server.rs"]
mod sftp_server;

use relay_desktop_core::sftp::{self, Progress, Remote};
use relay_desktop_core::ssh::{spawn, Event, Input, Params};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::Duration;
use zeroize::Zeroizing;

/// Terminal session to the test server, then its SFTP channel.
fn connect(rt: &tokio::runtime::Runtime, port: u16, known: &Path) -> (tokio::sync::mpsc::UnboundedSender<Input>, Arc<Remote>) {
    let (tx, rx) = mpsc::channel();
    let p = Params {
        host: "127.0.0.1".into(),
        port,
        username: "test".into(),
        password: Some(Zeroizing::new("secret".into())),
        cols: 80,
        rows: 24,
        known_hosts: known.to_path_buf(),
        ..Default::default()
    };
    let input = spawn(rt.handle(), p, move |e| {
        let _ = tx.send(e);
    });
    match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
        Event::Connected { .. } => {}
        other => panic!("{other:?}"),
    }
    let (reply, opened) = tokio::sync::oneshot::channel();
    input.send(Input::OpenSftp(reply)).unwrap();
    let remote = rt.block_on(opened).unwrap().unwrap();
    (input, Arc::new(remote))
}

#[test]
fn browse_and_transfer() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let served = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    std::fs::create_dir(served.path().join("srv-lab-logs")).unwrap();
    std::fs::write(served.path().join("srv-lab-logs/app.log"), b"ligne 1\n").unwrap();
    std::fs::write(served.path().join("notes.txt"), b"bonjour").unwrap();
    let port = rt.block_on(sftp_server::serve(served.path().to_path_buf(), "127.0.0.1:0"));
    let (input, r) = connect(&rt, port, &local.path().join("known_hosts.json"));

    rt.block_on(async {
        assert_eq!(r.canonical(".").await.unwrap(), "/");
        let items = r.list("/").await.unwrap();
        let names: Vec<_> = items.iter().map(|i| (i.name.as_str(), i.dir)).collect();
        assert_eq!(names, [("srv-lab-logs", true), ("notes.txt", false)]);
        assert_eq!(items[1].size, 7);
        assert_eq!(items[1].path, "/notes.txt");
        assert!(items[1].mtime.is_some());
        assert!(items[0].perms().starts_with('d'));
        let err = r.list("/absent").await.unwrap_err();
        assert!(err.contains("introuvable"), "{err}");

        // Upload 1 Mio + 1 octet, then download it back.
        let data: Vec<u8> = (0..1_048_577u32).map(|i| (i % 251) as u8).collect();
        let src = local.path().join("gros fichier.bin");
        std::fs::write(&src, &data).unwrap();
        r.mkdir("/envoi").await.unwrap();
        assert!(r.mkdir("/envoi").await.is_err());
        let p = Progress::default();
        r.upload(&src, "/envoi/gros fichier.bin", &p).await.unwrap();
        assert_eq!(p.done.load(Ordering::Acquire), data.len() as u64);
        assert_eq!(p.ratio(), 1.0);
        assert_eq!(std::fs::read(served.path().join("envoi/gros fichier.bin")).unwrap(), data);
        assert_eq!(r.stat("/envoi/gros fichier.bin").await.unwrap().size, data.len() as u64);

        let back = local.path().join("retour.bin");
        std::fs::write(&back, b"ancien").unwrap();
        let p = Progress::default();
        r.download("/envoi/gros fichier.bin", &back, &p).await.unwrap();
        assert_eq!(std::fs::read(&back).unwrap(), data);
        assert_eq!(p.total.load(Ordering::Acquire), data.len() as u64);
        assert!(!local.path().join("retour.bin.part").exists());

        // Cancelled transfers leave nothing behind.
        let p = Progress::default();
        p.cancel();
        assert_eq!(r.upload(&src, "/envoi/annule.bin", &p).await.unwrap_err(), sftp::CANCELLED);
        assert!(!served.path().join("envoi/annule.bin").exists());
        let p = Progress::default();
        p.cancel();
        let dest = local.path().join("annule.bin");
        assert_eq!(r.download("/notes.txt", &dest, &p).await.unwrap_err(), sftp::CANCELLED);
        assert!(!dest.exists() && !local.path().join("annule.bin.part").exists());

        // Rename (refused onto an existing name), then recursive delete.
        r.rename("/notes.txt", "/lisez-moi.txt").await.unwrap();
        assert!(served.path().join("lisez-moi.txt").exists());
        assert!(r.rename("/lisez-moi.txt", "/envoi").await.unwrap_err().contains("existe déjà"));
        assert!(r.rename("/lisez-moi.txt", "/a/b").await.is_err());
        std::fs::create_dir(served.path().join("envoi/sous-dossier")).unwrap();
        std::fs::write(served.path().join("envoi/sous-dossier/x"), b"x").unwrap();
        r.remove("/envoi").await.unwrap();
        assert!(!served.path().join("envoi").exists());
        r.remove("/lisez-moi.txt").await.unwrap();
        assert!(r.remove("/").await.is_err());
        let names: Vec<_> = r.list("/").await.unwrap().into_iter().map(|i| i.name).collect();
        assert_eq!(names, ["srv-lab-logs"]);
        r.close().await;
    });
    input.send(Input::Close).unwrap();
}
