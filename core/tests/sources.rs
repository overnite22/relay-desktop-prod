use relay_desktop_core::*;
use std::collections::BTreeMap;

/// Exercises any data source with the same scenario.
fn scenario(src: &dyn DataSource) {
    let parent = src
        .save_folder(&Folder {
            name: "Serveurs Linux".into(),
            ..Default::default()
        })
        .unwrap();
    let child = src
        .save_folder(&Folder {
            name: "Prod".into(),
            parent_id: Some(parent.id.clone()),
            ..Default::default()
        })
        .unwrap();

    let cred = src
        .save_credential(
            None,
            &CredentialInput {
                name: "root lab".into(),
                username: "root".into(),
                password: Some("s3cret!".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(cred.has_password && !cred.has_private_key);
    assert_eq!(src.reveal(&cred.id, SecretField::Password).unwrap(), "s3cret!");

    // Updating metadata without a password keeps the stored secret.
    let renamed = src
        .save_credential(
            Some(&cred.id),
            &CredentialInput {
                name: "root".into(),
                username: "root".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(renamed.name, "root");
    assert_eq!(src.reveal(&cred.id, SecretField::Password).unwrap(), "s3cret!");

    let mut e = Entry::new(Kind::Ssh);
    e.name = "web-01".into();
    e.host = "10.0.0.5".into();
    e.folder_id = Some(child.id.clone());
    let e = src.save_entry(&e).unwrap();
    assert!(!e.id.is_empty());
    assert_eq!(e.port, 22);

    let mut edited = e.clone();
    edited.notes = "# Notes\nok".into();
    edited.credential_id = Some(cred.id.clone());
    assert_eq!(src.save_entry(&edited).unwrap().notes, "# Notes\nok");

    // Per-entry connection settings survive a round trip (desktop-only keys included).
    let mut ssh_opts = conn::SshOptions::from_settings(&edited.settings);
    ssh_opts.font_size = 16;
    ssh_opts.agent = true;
    ssh_opts.keepalive = Some(0);
    ssh_opts.apply(&mut edited.settings);
    let saved = src.save_entry(&edited).unwrap();
    assert_eq!(conn::SshOptions::from_settings(&saved.settings), ssh_opts);
    let mut rdp = Entry::new(Kind::Rdp);
    rdp.name = "rds-01".into();
    rdp.host = "10.0.0.9".into();
    let opts = conn::RdpOptions {
        resolution: conn::Resolution::Fixed,
        width: 1600,
        height: 900,
        gateway: conn::Gateway::Auto,
        gateway_host: "gw.lan".into(),
        console: true,
        ..conn::RdpOptions::from_settings(&rdp.settings)
    };
    opts.apply(&mut rdp.settings);
    let rdp = src.save_entry(&rdp).unwrap();
    assert_eq!(conn::RdpOptions::from_settings(&rdp.settings), opts);
    src.delete_entry(&rdp.id).unwrap();

    let tree = src.tree().unwrap();
    assert!(tree
        .entries
        .iter()
        .any(|x| x.id == e.id && x.folder_id.as_deref() == Some(child.id.as_str())));
    assert_eq!(effective_credential(&tree, &edited), Some(cred.id.clone()));

    src.delete_entry(&e.id).unwrap();
    src.delete_credential(&cred.id).unwrap();
    src.delete_folder(&child.id).unwrap();
    src.delete_folder(&parent.id).unwrap();
    assert!(src.tree().unwrap().entries.iter().all(|x| x.id != e.id));
}

#[test]
fn local_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relay.db");
    assert!(matches!(LocalSource::create(&path, "court"), Err(Error::Invalid(_))));
    let src = LocalSource::create(&path, "mot de passe maître").unwrap();
    scenario(&src);

    // Secrets are not stored in clear and need the master password.
    let c = src
        .save_credential(
            None,
            &CredentialInput {
                name: "x".into(),
                password: Some("Tr3s-Secret".into()),
                ..Default::default()
            },
        )
        .unwrap();
    drop(src);
    assert!(!std::fs::read(&path).unwrap().windows(11).any(|w| w == b"Tr3s-Secret"));
    assert!(matches!(LocalSource::open(&path, "mauvais mot de passe"), Err(Error::BadPassword)));
    let src = LocalSource::open(&path, "mot de passe maître").unwrap();
    assert_eq!(src.reveal(&c.id, SecretField::Password).unwrap(), "Tr3s-Secret");
    assert!(matches!(LocalSource::create(&path, "mot de passe maître"), Err(Error::Invalid(_))));
}

#[test]
fn local_ssh_key_update_reaches_linked_and_inherited_entries() {
    use russh::keys::{encode_pkcs8_pem_encrypted, ssh_key::private::Ed25519Keypair, PrivateKey};

    let dir = tempfile::tempdir().unwrap();
    let src = LocalSource::create(&dir.path().join("relay.db"), "mot de passe maître").unwrap();
    let original = CredentialInput {
        name: "SSH lab".into(),
        username: "test".into(),
        password: Some("synthetic password".into()),
        ..Default::default()
    };
    let cred = src.save_credential(None, &original).unwrap();
    let folder = src
        .save_folder(&Folder {
            name: "Linux".into(),
            ..Default::default()
        })
        .unwrap();
    src.set_folder_credentials(&folder.id, &BTreeMap::from([("ssh".into(), Some(cred.id.clone()))]))
        .unwrap();
    let mut linked = Entry::new(Kind::Ssh);
    linked.name = "Linked".into();
    linked.host = "linked.example.test".into();
    linked.credential_id = Some(cred.id.clone());
    let linked = src.save_entry(&linked).unwrap();
    let mut inherited = Entry::new(Kind::Ssh);
    inherited.name = "Inherited".into();
    inherited.host = "inherited.example.test".into();
    inherited.folder_id = Some(folder.id);
    let inherited = src.save_entry(&inherited).unwrap();

    let key = PrivateKey::from(Ed25519Keypair::from_seed(&[42; 32]));
    let mut encrypted = Vec::new();
    encode_pkcs8_pem_encrypted(&key, b"test passphrase", 1000, &mut encrypted).unwrap();
    let saved = src
        .save_credential(
            Some(&cred.id),
            &CredentialInput {
                password: None,
                private_key: Some(String::from_utf8(encrypted).unwrap()),
                passphrase: Some("test passphrase".into()),
                ..original
            },
        )
        .unwrap();
    assert_eq!(saved.id, cred.id);
    assert!(saved.has_password && saved.has_private_key && saved.has_passphrase);

    let tree = src.tree().unwrap();
    for entry in [linked, inherited] {
        let id = effective_credential(&tree, &entry).unwrap();
        let private_key = src.reveal(&id, SecretField::PrivateKey).unwrap();
        let passphrase = src.reveal(&id, SecretField::Passphrase).unwrap();
        assert!(ssh::decode_private_key(&private_key, None).is_err());
        assert_eq!(
            ssh::decode_private_key(&private_key, Some(&passphrase)).unwrap().public_key(),
            key.public_key()
        );
    }
}

#[test]
fn local_rules() {
    let dir = tempfile::tempdir().unwrap();
    let src = LocalSource::create(&dir.path().join("r.db"), "mot de passe maître").unwrap();
    let a = src
        .save_folder(&Folder {
            name: "A".into(),
            ..Default::default()
        })
        .unwrap();
    let b = src
        .save_folder(&Folder {
            name: "B".into(),
            parent_id: Some(a.id.clone()),
            ..Default::default()
        })
        .unwrap();
    // No cycles, no deleting non-empty folders or used credentials.
    assert!(src
        .save_folder(&Folder {
            parent_id: Some(b.id.clone()),
            ..a.clone()
        })
        .is_err());
    assert!(src.delete_folder(&a.id).is_err());
    let cred = src
        .save_credential(
            None,
            &CredentialInput {
                name: "c".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let mut e = Entry::new(Kind::Rdp);
    e.name = "dc".into();
    e.host = "dc.lab".into();
    e.credential_id = Some(cred.id.clone());
    src.save_entry(&e).unwrap();
    assert!(src.delete_credential(&cred.id).is_err());
    let mut ghost = Entry::new(Kind::Rdp);
    ghost.name = "x".into();
    ghost.host = "x".into();
    ghost.folder_id = Some("missing".into());
    assert!(src.save_entry(&ghost).is_err());
}

#[test]
fn inheritance_uses_nearest_folder() {
    let mut creds = BTreeMap::new();
    creds.insert("rdp".to_string(), Some("domain-admin".to_string()));
    let tree = Tree {
        folders: vec![
            Folder {
                id: "root".into(),
                name: "Root".into(),
                credentials: creds,
                ..Default::default()
            },
            Folder {
                id: "sub".into(),
                parent_id: Some("root".into()),
                name: "Sub".into(),
                ..Default::default()
            },
        ],
        entries: vec![],
    };
    let mut e = Entry::new(Kind::Rdp);
    e.folder_id = Some("sub".into());
    assert_eq!(effective_credential(&tree, &e).as_deref(), Some("domain-admin"));
    e.kind = Kind::Ssh;
    assert_eq!(effective_credential(&tree, &e), None);
}

/// Runs against a live Relay: RELAY_TEST_SERVER=http://127.0.0.1:port RELAY_TEST_TOKEN=rlt_…
#[test]
#[ignore]
fn server_source() {
    let server = std::env::var("RELAY_TEST_SERVER").unwrap();
    let token = std::env::var("RELAY_TEST_TOKEN").unwrap();
    assert!(matches!(
        ServerSource::connect(&server, "rlt_invalid", "test"),
        Err(Error::Unauthorized)
    ));
    let src = ServerSource::connect(&server, &token, "relay-desktop-test").unwrap();
    assert!(!src.user().is_empty());
    scenario(&src);
}

/// Folder defaults, workspaces and TOTP on any source.
fn extras(src: &dyn DataSource) {
    let f = src
        .save_folder(&Folder {
            name: "Extras".into(),
            ..Default::default()
        })
        .unwrap();
    let c = src
        .save_credential(
            None,
            &CredentialInput {
                name: "otp".into(),
                totp: Some("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let mut m = BTreeMap::new();
    m.insert("ssh".to_string(), Some(c.id.clone()));
    src.set_folder_credentials(&f.id, &m).unwrap();
    let tree = src.tree().unwrap();
    let mut e = Entry::new(Kind::Ssh);
    e.folder_id = Some(f.id.clone());
    assert_eq!(effective_credential(&tree, &e), Some(c.id.clone()));
    let code = src.totp(&c.id).unwrap();
    assert_eq!(code.code.len(), 6);
    e.name = "ws-entry".into();
    e.host = "10.0.0.9".into();
    let e = src.save_entry(&e).unwrap();
    let w = src
        .save_workspace(&Workspace {
            name: "Matin".into(),
            entry_ids: vec![e.id.clone()],
            ..Default::default()
        })
        .unwrap();
    assert!(src
        .workspaces()
        .unwrap()
        .iter()
        .any(|x| x.id == w.id && x.entry_ids == vec![e.id.clone()]));
    let data = b"\x00binaire \xff contenu".to_vec();
    let a = src.add_attachment(&e.id, "C:\\dossier\\notes de site.bin", &data).unwrap();
    assert_eq!((a.name.as_str(), a.size), ("notes de site.bin", data.len() as i64));
    assert!(src.add_attachment(&e.id, "vide.txt", b"").is_err());
    assert!(src.add_attachment(&e.id, "..", b"x").is_err());
    assert_eq!(src.attachments(&e.id).unwrap(), vec![a.clone()]);
    assert_eq!(src.attachment_data(&e.id, &a.id).unwrap(), data);
    src.delete_attachment(&e.id, &a.id).unwrap();
    assert!(src.attachments(&e.id).unwrap().is_empty());
    assert!(matches!(src.attachment_data(&e.id, &a.id), Err(Error::NotFound)));
    src.add_attachment(&e.id, "reste.txt", b"x").unwrap();
    let sc = src
        .save_command(&relay_desktop_core::command::SavedCommand {
            name: "Disque".into(),
            command: "df -h".into(),
            ..Default::default()
        })
        .unwrap();
    let mut sc2 = sc.clone();
    sc2.sudo = true;
    src.save_command(&sc2).unwrap();
    let list = src.saved_commands().unwrap();
    assert!(list.iter().any(|x| x.id == sc.id && x.sudo && x.command == "df -h"));
    src.delete_command(&sc.id).unwrap();
    src.delete_workspace(&w.id).unwrap();
    src.delete_entry(&e.id).unwrap();
    src.set_folder_credentials(&f.id, &BTreeMap::new()).unwrap();
    src.delete_credential(&c.id).unwrap();
    src.delete_folder(&f.id).unwrap();
}

#[test]
fn local_extras() {
    let dir = tempfile::tempdir().unwrap();
    let src = LocalSource::create(&dir.path().join("x.db"), "mot de passe maître").unwrap();
    extras(&src);
    assert!(src.audit(10).is_err());
}

#[test]
#[ignore]
fn server_extras() {
    let src = ServerSource::connect(
        &std::env::var("RELAY_TEST_SERVER").unwrap(),
        &std::env::var("RELAY_TEST_TOKEN").unwrap(),
        "t",
    )
    .unwrap();
    extras(&src);
    assert!(!src.audit(5).unwrap().is_empty());
    let archive = src.export_backup("sauvegarde-test").unwrap();
    assert!(archive.len() > 100 && !relay_desktop_core::backup::is_local(&archive));
    assert!(src.restore_backup(&archive, "mauvais").is_err());
    src.restore_backup(&archive, "sauvegarde-test").unwrap();
    src.tree().unwrap();
}

#[test]
fn local_backup_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let src = LocalSource::create(&dir.path().join("a.rdb"), "mot de passe maître").unwrap();
    src.save_folder(&Folder {
        name: "Sauvé".into(),
        ..Default::default()
    })
    .unwrap();
    let sealed = src.export_backup("sauvegarde!").unwrap();
    assert!(src.restore_backup(&sealed, "sauvegarde!").is_err());
    let db = relay_desktop_core::backup::open(&sealed, "sauvegarde!").unwrap();
    let path = dir.path().join("restauré.rdb");
    std::fs::write(&path, db.as_slice()).unwrap();
    let back = LocalSource::open(&path, "mot de passe maître").unwrap();
    assert!(back.tree().unwrap().folders.iter().any(|f| f.name == "Sauvé"));
}

/// Sync sources, duplicates, RDM import and recordings on a live Relay.
#[test]
#[ignore]
fn server_admin() {
    use relay_desktop_core::admin::ImportOptions;
    let src = ServerSource::connect(
        &std::env::var("RELAY_TEST_SERVER").unwrap(),
        &std::env::var("RELAY_TEST_TOKEN").unwrap(),
        "t",
    )
    .unwrap();
    src.sync_sources().unwrap();
    // Synchronisation source: create, edit (secret kept), list, delete.
    let mut input = admin::SyncSourceInput {
        kind: "proxmox".into(),
        name: "PVE test".into(),
        url: "https://127.0.0.1:1".into(),
        token_id: "root@pam!relay".into(),
        secret: None,
        interval_min: 0,
        ..Default::default()
    };
    assert!(input.validate(true).is_err());
    input.secret = Some("00000000-0000-0000-0000-000000000000".into());
    input.options.insert("nodeEntries".into(), serde_json::Value::Bool(false));
    let created = src.save_sync_source(None, &input).unwrap();
    assert!(created.has_secret && !created.option_on("nodeEntries") && created.option_on("guestEntries"));
    input.secret = None;
    input.name = "PVE renommé".into();
    input.interval_min = 60;
    let edited = src.save_sync_source(Some(&created.id), &input).unwrap();
    assert_eq!(
        (edited.name.as_str(), edited.interval_min, edited.has_secret),
        ("PVE renommé", 60, true)
    );
    assert!(src
        .sync_sources()
        .unwrap()
        .iter()
        .any(|s| s.id == created.id && s.token_id == "root@pam!relay"));
    src.delete_sync_source(&created.id).unwrap();
    assert!(!src.sync_sources().unwrap().iter().any(|s| s.id == created.id));
    src.recordings().unwrap();
    let xml = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sample.rdm")).unwrap();
    let plan = src.import_preview(&xml).unwrap();
    assert_eq!((plan.entries.len(), plan.credentials.len(), plan.skipped.len()), (4, 1, 2));
    assert!(src.import_preview(b"<autre/>").is_err());
    let opt = ImportOptions {
        root: "Import test".into(),
        ..Default::default()
    };
    let res = src.import_apply(&xml, &opt).unwrap();
    assert_eq!((res.entries, res.credentials), (4, 1));
    // A second import creates duplicates of every target.
    src.import_apply(&xml, &opt).unwrap();
    let groups = src.duplicates().unwrap();
    let g = groups.iter().find(|g| g.entries.len() >= 2).unwrap();
    let others: Vec<String> = g.entries[1..].iter().map(|e| e.id.clone()).collect();
    let kept = src.merge_entries(&g.entries[0].id, &others).unwrap();
    assert_eq!(kept.id, g.entries[0].id);
    assert!(src.duplicates().unwrap().len() < groups.len());
}
