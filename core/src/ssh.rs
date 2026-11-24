//! Interactive SSH sessions for embedded terminals (russh, pure Rust).
//!
//! Host keys are pinned per `host:port` in a JSON file: the first key seen is
//! recorded (trust on first use); a later change aborts the connection and is
//! reported with both fingerprints so the user can trust the new key.

use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::ChannelMsg;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

#[derive(Default)]
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
    /// Seconds between keepalives (0 = none).
    pub keepalive: u32,
    /// Try the keys of the local SSH agent (OpenSSH agent pipe, then Pageant).
    pub agent: bool,
    /// Private key file read when `private_key` is empty.
    pub key_file: Option<PathBuf>,
    /// Environment sent before the shell (LANG, LC_ALL); servers often refuse it.
    pub env: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum Event {
    /// Authenticated shell; `new_key` is true when the host key was just pinned.
    Connected {
        fingerprint: String,
        new_key: bool,
    },
    Data(Vec<u8>),
    /// The pinned host key differs (sent just before `Closed`).
    KeyChanged(KeyChange),
    Closed(Option<String>),
}

/// A host key that no longer matches the pinned one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChange {
    /// `host:port` entry of the known hosts file.
    pub host_id: String,
    /// SHA256 fingerprints of the pinned and of the presented keys.
    pub old: String,
    pub new: String,
    /// Presented key (OpenSSH line), pinned by [`trust_key`].
    pub key: String,
}

impl KeyChange {
    pub fn message(&self) -> String {
        format!("la clé d'hôte de {} a changé ({}) : connexion refusée", self.host_id, self.new)
    }
}

/// SHA256 fingerprint of an OpenSSH public key line.
pub fn fingerprint(line: &str) -> Option<String> {
    russh::keys::PublicKey::from_openssh(line.trim())
        .ok()
        .map(|k| k.fingerprint(HashAlg::Sha256).to_string())
}

/// Pins the new key of a [`KeyChange`] ("Faire confiance à la nouvelle clé").
pub fn trust_key(known_hosts: &Path, change: &KeyChange) -> std::io::Result<()> {
    let mut known = load_known(known_hosts);
    known.insert(change.host_id.clone(), change.key.clone());
    save_known(known_hosts, &known)
}

#[derive(Debug)]
pub enum Input {
    Data(Vec<u8>),
    Resize(u16, u16),
    /// Opens the SFTP subsystem on a new channel of this connection.
    OpenSftp(tokio::sync::oneshot::Sender<Result<crate::sftp::Remote, String>>),
    Close,
}

enum KeyCheck {
    Known(String),
    New(String),
    Changed(KeyChange),
    Refused(String),
}

struct Client {
    host_id: String,
    known_hosts: PathBuf,
    check: Arc<Mutex<Option<KeyCheck>>>,
}

fn load_known(path: &Path) -> BTreeMap<String, String> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_known(path: &Path, known: &BTreeMap<String, String>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(known).unwrap_or_default())
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            *self.check.lock().unwrap() = Some(KeyCheck::Refused("certificat d'hôte non pris en charge".into()));
            return Ok(false);
        };
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        let line = key.to_openssh().unwrap_or_default();
        let mut known = load_known(&self.known_hosts);
        let verdict = match known.get(&self.host_id) {
            Some(k) if *k == line => KeyCheck::Known(fp),
            Some(old) => KeyCheck::Changed(KeyChange {
                host_id: self.host_id.clone(),
                old: fingerprint(old).unwrap_or_else(|| "illisible".into()),
                new: fp,
                key: line,
            }),
            None => {
                known.insert(self.host_id.clone(), line);
                let _ = save_known(&self.known_hosts, &known);
                KeyCheck::New(fp)
            }
        };
        let ok = matches!(verdict, KeyCheck::Known(_) | KeyCheck::New(_));
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

/// Connects, checks the pinned host key and authenticates; a changed host key
/// is also returned in `changed`.
async fn open(p: &Params, changed: &mut Option<KeyChange>) -> Result<(client::Handle<Client>, String, bool), String> {
    let check = Arc::new(Mutex::new(None));
    let handler = Client {
        host_id: format!("{}:{}", p.host, p.port),
        known_hosts: p.known_hosts.clone(),
        check: check.clone(),
    };
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: (p.keepalive > 0).then(|| std::time::Duration::from_secs(p.keepalive.into())),
        ..Default::default()
    });
    let connect = client::connect(config, (p.host.as_str(), p.port), handler);
    let mut session = match tokio::time::timeout(std::time::Duration::from_secs(15), connect).await {
        Err(_) => return Err(format!("{}:{} ne répond pas", p.host, p.port)),
        Ok(Err(e)) => {
            return Err(match check.lock().unwrap().take() {
                Some(KeyCheck::Changed(k)) => {
                    let msg = k.message();
                    *changed = Some(k);
                    msg
                }
                Some(KeyCheck::Refused(why)) => format!("clé d'hôte refusée : {why}"),
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
    let from_file = match (&p.key_file, p.private_key.as_deref().is_some_and(|k| !k.trim().is_empty())) {
        (Some(path), false) => Some(Zeroizing::new(
            std::fs::read_to_string(path).map_err(|e| format!("fichier de clé {} illisible : {e}", path.display()))?,
        )),
        _ => None,
    };
    if p.agent {
        authenticated = agent_auth(&mut session, &p.username).await;
    }
    if let Some(pem) = p
        .private_key
        .as_ref()
        .or(from_file.as_ref())
        .filter(|k| !k.trim().is_empty() && !authenticated)
    {
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

/// Tries every key of the local SSH agent; errors only mean "not authenticated".
async fn agent_auth(session: &mut client::Handle<Client>, user: &str) -> bool {
    #[cfg(windows)]
    {
        use russh::keys::agent::client::AgentClient;
        if let Ok(mut agent) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
            if agent_try(session, user, &mut agent).await {
                return true;
            }
        }
        if let Ok(mut agent) = AgentClient::connect_pageant().await {
            return agent_try(session, user, &mut agent).await;
        }
        false
    }
    #[cfg(not(windows))]
    {
        match russh::keys::agent::client::AgentClient::connect_env().await {
            Ok(mut agent) => agent_try(session, user, &mut agent).await,
            Err(_) => false,
        }
    }
}

async fn agent_try<S>(session: &mut client::Handle<Client>, user: &str, agent: &mut russh::keys::agent::client::AgentClient<S>) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use russh::keys::agent::AgentIdentity;
    let Ok(ids) = agent.request_identities().await else { return false };
    let hash = session.best_supported_rsa_hash().await.ok().flatten().flatten();
    for id in ids {
        let AgentIdentity::PublicKey { key, .. } = id else { continue };
        if let Ok(r) = session.authenticate_publickey_with(user, key, hash, agent).await {
            if r.success() {
                return true;
            }
        }
    }
    false
}

async fn run(p: Params, mut rx: mpsc::UnboundedReceiver<Input>, events: impl Fn(Event)) -> Result<(), String> {
    let mut changed = None;
    let opened = open(&p, &mut changed).await;
    if let Some(k) = changed {
        events(Event::KeyChanged(k));
    }
    let (session, fingerprint, new_key) = opened?;
    let mut channel = session.channel_open_session().await.map_err(|e| e.to_string())?;
    channel
        .request_pty(false, "xterm-256color", p.cols.into(), p.rows.into(), 0, 0, &[])
        .await
        .map_err(|e| e.to_string())?;
    for (name, value) in &p.env {
        let _ = channel.set_env(false, name.as_str(), value.as_str()).await;
    }
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
                Some(Input::OpenSftp(reply)) => { let _ = reply.send(open_sftp(&session).await); }
                Some(Input::Close) | None => { let _ = channel.close().await; break; }
            },
        }
    }
    let _ = session.disconnect(russh::Disconnect::ByApplication, "", "fr").await;
    Ok(())
}

/// `sftp` subsystem on a new channel of an authenticated connection.
async fn open_sftp(session: &client::Handle<Client>) -> Result<crate::sftp::Remote, String> {
    let wait = std::time::Duration::from_secs(15);
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|e| format!("canal SFTP refusé : {e}"))?;
    channel.request_subsystem(true, "sftp").await.map_err(|e| e.to_string())?;
    loop {
        match tokio::time::timeout(wait, channel.wait()).await {
            Err(_) => return Err("le serveur ne répond pas à la demande SFTP".into()),
            Ok(Some(ChannelMsg::Success)) => break,
            Ok(Some(ChannelMsg::Failure)) | Ok(Some(ChannelMsg::Close)) | Ok(None) => {
                return Err("SFTP indisponible sur ce serveur (sous-système « sftp » refusé)".into())
            }
            Ok(Some(_)) => {}
        }
    }
    crate::sftp::Remote::start(channel.into_stream()).await
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
    let (session, fingerprint, new_key) = open(p, &mut None).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILQVkpVq0hrQctVoP4nMSPXxTz0apeHg3cgscn6o92Q+";

    #[test]
    fn trust_new_key() {
        assert_eq!(
            fingerprint(KEY).as_deref(),
            Some("SHA256:b9mGdcaAQUTqkdZl25dEunmFN1ZVneY/mLOpNn925AM")
        );
        assert_eq!(fingerprint("pas une clé"), None);
        let dir = std::env::temp_dir().join(format!("relay-kh-{}", std::process::id()));
        let path = dir.join("known_hosts.json");
        save_known(
            &path,
            &BTreeMap::from([
                ("h:22".to_string(), "ancienne".to_string()),
                ("x:22".to_string(), "autre".to_string()),
            ]),
        )
        .unwrap();
        let change = KeyChange {
            host_id: "h:22".into(),
            old: "illisible".into(),
            new: "SHA256:b9mG".into(),
            key: KEY.into(),
        };
        assert!(change.message().contains("h:22"));
        trust_key(&path, &change).unwrap();
        let known = load_known(&path);
        assert_eq!(known.get("h:22").map(String::as_str), Some(KEY));
        assert_eq!(known.get("x:22").map(String::as_str), Some("autre"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
