//! Interactive SSH sessions for embedded terminals (russh, pure Rust).
//!
//! Host keys are pinned per `host:port` in a JSON file: the first key seen is
//! recorded (trust on first use) and any later change aborts the connection.

use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::ChannelMsg;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

pub struct Params {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<Zeroizing<String>>,
    pub private_key: Option<Zeroizing<String>>,
    pub passphrase: Option<Zeroizing<String>>,
    pub cols: u16,
    pub rows: u16,
    pub known_hosts: PathBuf,
}

#[derive(Debug)]
pub enum Event {
    /// Authenticated shell; `new_key` is true when the host key was just pinned.
    Connected {
        fingerprint: String,
        new_key: bool,
    },
    Data(Vec<u8>),
    Closed(Option<String>),
}

#[derive(Debug)]
pub enum Input {
    Data(Vec<u8>),
    Resize(u16, u16),
    Close,
}

enum KeyCheck {
    Known(String),
    New(String),
    Changed(String),
}

struct Client {
    host_id: String,
    known_hosts: PathBuf,
    check: Arc<Mutex<Option<KeyCheck>>>,
}

fn load_known(path: &PathBuf) -> BTreeMap<String, String> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            *self.check.lock().unwrap() = Some(KeyCheck::Changed("certificat d'hôte non pris en charge".into()));
            return Ok(false);
        };
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        let line = key.to_openssh().unwrap_or_default();
        let mut known = load_known(&self.known_hosts);
        let verdict = match known.get(&self.host_id) {
            Some(k) if *k == line => KeyCheck::Known(fp),
            Some(_) => KeyCheck::Changed(fp),
            None => {
                known.insert(self.host_id.clone(), line);
                if let Some(dir) = self.known_hosts.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(&self.known_hosts, serde_json::to_vec_pretty(&known).unwrap_or_default());
                KeyCheck::New(fp)
            }
        };
        let ok = !matches!(verdict, KeyCheck::Changed(_));
        *self.check.lock().unwrap() = Some(verdict);
        Ok(ok)
    }
}

/// Starts a session on `rt`; `events` is called from the runtime threads.
pub fn spawn(rt: &tokio::runtime::Handle, p: Params, events: impl Fn(Event) + Send + Sync + 'static) -> mpsc::UnboundedSender<Input> {
    let (tx, rx) = mpsc::unbounded_channel();
    let events = Arc::new(events);
    rt.spawn(async move {
        let e = events.clone();
        let result = run(p, rx, move |ev| e(ev)).await;
        events(Event::Closed(result.err()));
    });
    tx
}

/// Connects, checks the pinned host key and authenticates.
async fn open(p: &Params) -> Result<(client::Handle<Client>, String, bool), String> {
    let check = Arc::new(Mutex::new(None));
    let handler = Client {
        host_id: format!("{}:{}", p.host, p.port),
        known_hosts: p.known_hosts.clone(),
        check: check.clone(),
    };
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    });
    let connect = client::connect(config, (p.host.as_str(), p.port), handler);
    let mut session = match tokio::time::timeout(std::time::Duration::from_secs(15), connect).await {
        Err(_) => return Err(format!("{}:{} ne répond pas", p.host, p.port)),
        Ok(Err(e)) => {
            return Err(match check.lock().unwrap().take() {
                Some(KeyCheck::Changed(fp)) => format!("la clé d'hôte de {} a changé ({fp}) : connexion refusée", p.host),
                _ => format!("connexion impossible : {e}"),
            })
        }
        Ok(Ok(s)) => s,
    };
    let (fingerprint, new_key) = match check.lock().unwrap().take() {
        Some(KeyCheck::New(fp)) => (fp, true),
        Some(KeyCheck::Known(fp)) => (fp, false),
        _ => (String::new(), false),
    };

    let mut authenticated = false;
    if let Some(pem) = p.private_key.as_deref().filter(|k| !k.trim().is_empty()) {
        let key = russh::keys::decode_secret_key(pem, p.passphrase.as_deref().map(|s| s.as_str()).filter(|s| !s.is_empty()))
            .map_err(|e| format!("clé privée illisible : {e}"))?;
        let hash = session.best_supported_rsa_hash().await.ok().flatten().flatten();
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
        authenticated = session
            .authenticate_publickey(&p.username, key)
            .await
            .map_err(|e| e.to_string())?
            .success();
    }
    if !authenticated {
        if let Some(pw) = p.password.as_deref() {
            authenticated = session
                .authenticate_password(&p.username, pw.as_str())
                .await
                .map_err(|e| e.to_string())?
                .success();
        }
    }
    if !authenticated {
        return Err(format!("authentification refusée pour « {} »", p.username));
    }

    Ok((session, fingerprint, new_key))
}

async fn run(p: Params, mut rx: mpsc::UnboundedReceiver<Input>, events: impl Fn(Event)) -> Result<(), String> {
    let (session, fingerprint, new_key) = open(&p).await?;
    let mut channel = session.channel_open_session().await.map_err(|e| e.to_string())?;
    channel
        .request_pty(false, "xterm-256color", p.cols.into(), p.rows.into(), 0, 0, &[])
        .await
        .map_err(|e| e.to_string())?;
    channel.request_shell(false).await.map_err(|e| e.to_string())?;
    events(Event::Connected { fingerprint, new_key });

    loop {
        tokio::select! {
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) => events(Event::Data(data.to_vec())),
                Some(ChannelMsg::ExtendedData { data, .. }) => events(Event::Data(data.to_vec())),
                Some(ChannelMsg::ExitStatus { .. }) | Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            },
            input = rx.recv() => match input {
                Some(Input::Data(bytes)) => channel.data(&bytes[..]).await.map_err(|e| e.to_string())?,
                Some(Input::Resize(c, r)) => { let _ = channel.window_change(c.into(), r.into(), 0, 0).await; }
                Some(Input::Close) | None => { let _ = channel.close().await; break; }
            },
        }
    }
    let _ = session.disconnect(russh::Disconnect::ByApplication, "", "fr").await;
    Ok(())
}

/// Outcome of a non-interactive command on one server.
#[derive(Debug, Default, Clone)]
pub struct ExecOutput {
    pub exit_code: Option<u32>,
    pub output: String,
    pub truncated: bool,
    pub fingerprint: String,
    pub new_key: bool,
}

/// Runs `command` without a terminal; stdout and stderr are merged and kept
/// up to `max_output` bytes. `stdin` is written first (sudo password).
pub async fn exec(p: &Params, command: &str, stdin: Option<Zeroizing<String>>, max_output: usize) -> Result<ExecOutput, String> {
    let (session, fingerprint, new_key) = open(p).await?;
    let mut channel = session.channel_open_session().await.map_err(|e| e.to_string())?;
    channel.exec(true, command).await.map_err(|e| e.to_string())?;
    if let Some(input) = stdin {
        let _ = channel.data(input.as_bytes()).await;
        let _ = channel.eof().await;
    }
    let mut out = ExecOutput {
        fingerprint,
        new_key,
        ..Default::default()
    };
    let mut buf: Vec<u8> = Vec::new();
    let mut push = |data: &[u8], out: &mut ExecOutput| {
        let room = max_output.saturating_sub(buf.len());
        if data.len() > room {
            out.truncated = true;
        }
        buf.extend_from_slice(&data[..data.len().min(room)]);
    };
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { data } => push(&data, &mut out),
            ChannelMsg::ExtendedData { data, .. } => push(&data, &mut out),
            ChannelMsg::ExitStatus { exit_status } => out.exit_code = Some(exit_status),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    let _ = session.disconnect(russh::Disconnect::ByApplication, "", "fr").await;
    out.output = String::from_utf8_lossy(&buf).into_owned();
    Ok(out)
}
