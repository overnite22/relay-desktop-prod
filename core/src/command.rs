//! Bulk SSH commands run from this PC, in parallel, with the same rules as the
//! Relay server (sshexec): `sudo -S` fed with the password, or `sudo -n`
//! without one, merged output capped per server, per-server timeout.

use crate::model::*;
use crate::source::{effective_credential, DataSource};
use crate::ssh;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_OK: &str = "ok";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_ERROR: &str = "error";
pub const STATUS_CANCELED: &str = "canceled";

/// Output kept per server, like the server default.
pub const MAX_OUTPUT: usize = 256 << 10;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavedCommand {
    pub id: String,
    pub name: String,
    pub command: String,
    pub sudo: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default)]
pub struct RunResult {
    pub entry_id: String,
    pub name: String,
    pub address: String,
    pub status: &'static str,
    pub exit_code: Option<u32>,
    pub output: String,
    pub truncated: bool,
    pub error: String,
    pub duration_ms: u64,
    pub new_key: bool,
}

pub struct Options {
    pub command: String,
    pub sudo: bool,
    pub timeout: Duration,
    pub concurrency: usize,
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Command line sent to the server and whether the password goes to stdin.
pub fn command_line(command: &str, sudo: bool, has_password: bool) -> (String, bool) {
    match (sudo, has_password) {
        (false, _) => (command.to_owned(), false),
        (true, false) => (format!("sudo -n -- sh -c {}", shell_quote(command)), false),
        (true, true) => (format!("sudo -S -p '' -- sh -c {}", shell_quote(command)), true),
    }
}

/// Resolves the SSH parameters of `entry` (own credential or folder default).
/// Secrets are revealed once per credential thanks to `cache`.
fn params(
    src: &dyn DataSource,
    tree: &Tree,
    creds: &[Credential],
    entry: &Entry,
    known_hosts: &Path,
    cache: &mut HashMap<String, [Option<Zeroizing<String>>; 3]>,
) -> Result<ssh::Params, String> {
    if entry.host.trim().is_empty() {
        return Err("hôte non renseigné".into());
    }
    let cred = effective_credential(tree, entry).and_then(|id| creds.iter().find(|c| c.id == id));
    let secrets = match cred {
        Some(c) => match cache.get(&c.id) {
            Some(s) => s.clone(),
            None => {
                let reveal = |field, has: bool| -> Result<Option<Zeroizing<String>>, String> {
                    if has {
                        src.reveal(&c.id, field).map(|v| Some(Zeroizing::new(v))).map_err(|e| e.to_string())
                    } else {
                        Ok(None)
                    }
                };
                let s = [
                    reveal(SecretField::Password, c.has_password)?,
                    reveal(SecretField::PrivateKey, c.has_private_key)?,
                    reveal(SecretField::Passphrase, c.has_passphrase)?,
                ];
                cache.insert(c.id.clone(), s.clone());
                s
            }
        },
        None => Default::default(),
    };
    let username = cred
        .map(|c| c.username.clone())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| entry.username.clone());
    if username.is_empty() {
        return Err("aucun utilisateur : associez un identifiant à l'entrée ou au dossier".into());
    }
    let [password, private_key, passphrase] = secrets;
    Ok(ssh::Params {
        host: entry.host.trim().to_owned(),
        port: if entry.port == 0 { 22 } else { entry.port },
        username,
        password,
        private_key,
        passphrase,
        cols: 80,
        rows: 24,
        known_hosts: known_hosts.to_owned(),
    })
}

pub fn address(e: &Entry) -> String {
    format!("{}:{}", e.host.trim(), if e.port == 0 { 22 } else { e.port })
}

/// Runs the command on `entries`; blocks the calling (worker) thread.
/// `update(index, result)` reports every state change; `cancel` stops it.
#[allow(clippy::too_many_arguments)]
pub fn run(
    rt: &tokio::runtime::Handle,
    src: &dyn DataSource,
    tree: &Tree,
    creds: &[Credential],
    entries: &[Entry],
    opt: &Options,
    known_hosts: &Path,
    cancel: tokio::sync::watch::Receiver<bool>,
    update: impl Fn(usize, RunResult) + Send + Sync + 'static,
) {
    let update = Arc::new(update);
    let mut cache = HashMap::new();
    let mut jobs = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let base = RunResult {
            entry_id: e.id.clone(),
            name: e.name.clone(),
            address: address(e),
            status: STATUS_PENDING,
            ..Default::default()
        };
        match params(src, tree, creds, e, known_hosts, &mut cache) {
            Ok(p) => jobs.push((i, base, p)),
            Err(error) => update(
                i,
                RunResult {
                    status: STATUS_ERROR,
                    error,
                    ..base
                },
            ),
        }
    }
    drop(cache);
    let sem = Arc::new(tokio::sync::Semaphore::new(opt.concurrency.clamp(1, 32)));
    let (command, sudo, timeout) = (opt.command.clone(), opt.sudo, opt.timeout);
    rt.block_on(async move {
        let mut set = tokio::task::JoinSet::new();
        for (i, base, p) in jobs {
            let (sem, update, command, mut cancel) = (sem.clone(), update.clone(), command.clone(), cancel.clone());
            set.spawn(async move {
                let _permit = tokio::select! {
                    permit = sem.acquire_owned() => permit.ok(),
                    _ = cancel.wait_for(|c| *c) => None,
                };
                if _permit.is_none() || *cancel.borrow() {
                    return update(
                        i,
                        RunResult {
                            status: STATUS_CANCELED,
                            ..base
                        },
                    );
                }
                update(
                    i,
                    RunResult {
                        status: STATUS_RUNNING,
                        ..base.clone()
                    },
                );
                let started = Instant::now();
                let (line, feed) = command_line(&command, sudo, p.password.is_some());
                let stdin = if feed {
                    p.password.as_ref().map(|pw| Zeroizing::new(format!("{}\n", pw.as_str())))
                } else {
                    None
                };
                let outcome = tokio::select! {
                    r = tokio::time::timeout(timeout, ssh::exec(&p, &line, stdin, MAX_OUTPUT)) => Some(r),
                    _ = cancel.wait_for(|c| *c) => None,
                };
                let mut r = RunResult {
                    duration_ms: started.elapsed().as_millis() as u64,
                    ..base
                };
                match outcome {
                    None => (r.status, r.error) = (STATUS_CANCELED, "arrêt demandé".into()),
                    Some(Err(_)) => (r.status, r.error) = (STATUS_ERROR, "délai dépassé".into()),
                    Some(Ok(Err(e))) => (r.status, r.error) = (STATUS_ERROR, e),
                    Some(Ok(Ok(o))) => {
                        r.status = if o.exit_code == Some(0) { STATUS_OK } else { STATUS_FAILED };
                        if o.exit_code.is_none() {
                            r.error = "session fermée sans code de retour".into();
                        }
                        (r.exit_code, r.output, r.truncated, r.new_key) = (o.exit_code, o.output, o.truncated, o.new_key);
                    }
                }
                update(i, r);
            });
        }
        while set.join_next().await.is_some() {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sudo_lines() {
        assert_eq!(command_line("uptime", false, true), ("uptime".into(), false));
        assert_eq!(command_line("echo 'a'", true, false).0, r"sudo -n -- sh -c 'echo '\''a'\'''");
        assert_eq!(command_line("id", true, true), ("sudo -S -p '' -- sh -c 'id'".into(), true));
    }

    #[test]
    fn unresolved_targets_fail_fast() {
        let dir = tempfile::tempdir().unwrap();
        let src = crate::LocalSource::create(&dir.path().join("c.db"), "mot de passe maître").unwrap();
        let mut no_host = Entry::new(Kind::Ssh);
        no_host.name = "a".into();
        let mut no_user = Entry::new(Kind::Ssh);
        no_user.host = "127.0.0.1".into();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let s = seen.clone();
        let opt = Options {
            command: "true".into(),
            sudo: false,
            timeout: Duration::from_secs(1),
            concurrency: 4,
        };
        run(
            rt.handle(),
            &src,
            &Tree::default(),
            &[],
            &[no_host, no_user],
            &opt,
            &dir.path().join("kh.json"),
            rx,
            move |i, r| {
                s.lock().unwrap().push((i, r.status, r.error));
            },
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|(_, st, _)| *st == STATUS_ERROR));
        assert!(seen[0].2.contains("hôte") && seen[1].2.contains("utilisateur"));
    }
}
