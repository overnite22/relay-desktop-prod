//! SFTP file browser of an SSH session (russh-sftp, pure Rust).
//!
//! The subsystem runs on a new channel of the terminal's connection
//! ([`crate::ssh::Input::OpenSftp`]): same host key check and credentials, no
//! second login. Paths are remote POSIX paths (`/` separators).

use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, StatusCode};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Message of a transfer stopped by [`Progress::cancel`].
pub const CANCELLED: &str = "transfert annulé";

const CHUNK: usize = 256 * 1024;

/// One line of a remote directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub path: String,
    /// Directory, or symbolic link to a directory.
    pub dir: bool,
    pub link: bool,
    pub size: u64,
    /// Modification time (Unix seconds).
    pub mtime: Option<i64>,
    /// `st_mode` (type and permission bits), 0 if unknown.
    pub mode: u32,
}

impl Item {
    /// `drwxr-xr-x` style permissions.
    pub fn perms(&self) -> String {
        perms(self.mode, self.dir, self.link)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
    Perms,
}

impl SortKey {
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Size,
            2 => Self::Modified,
            3 => Self::Perms,
            _ => Self::Name,
        }
    }
}

/// Folders first, then by `key` (name as tie-breaker, natural order: `f2` < `f10`).
pub fn sort(items: &mut [Item], key: SortKey, desc: bool) {
    items.sort_by(|a, b| {
        let by = match key {
            SortKey::Name => std::cmp::Ordering::Equal,
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Modified => a.mtime.cmp(&b.mtime),
            SortKey::Perms => a.perms()[1..].cmp(&b.perms()[1..]),
        };
        let by = by.then_with(|| natural(&a.name, &b.name));
        b.dir.cmp(&a.dir).then(if desc { by.reverse() } else { by })
    });
}

/// Case-insensitive comparison where digit runs compare as numbers.
pub fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (n, m) = (take(&mut x), take(&mut y));
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let o = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if o.is_ne() {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o.is_ne() {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// `drwxr-xr-x` from a `st_mode` (setuid, setgid and sticky bits included).
pub fn perms(mode: u32, dir: bool, link: bool) -> String {
    let kind = if link {
        'l'
    } else if dir {
        'd'
    } else {
        '-'
    };
    let mut s = String::from(kind);
    for (shift, special, set) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        let x = bits & 1 != 0;
        s.push(match (mode & special != 0, x) {
            (true, true) => set,
            (true, false) => set.to_ascii_uppercase(),
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// Size as in Relay web: `512 o`, `12 Ko`, `3.4 Mo`, `1.25 Go`.
pub fn fmt_size(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if n < 1024 {
        format!("{n} o")
    } else if f < K * K {
        format!("{:.0} Ko", f / K)
    } else if f < K * K * K {
        format!("{:.1} Mo", f / (K * K))
    } else {
        format!("{:.2} Go", f / (K * K * K))
    }
}

/// Local `AAAA-MM-JJ HH:MM` of a Unix time.
pub fn fmt_time(t: Option<i64>) -> String {
    use time::{macros::format_description, OffsetDateTime, UtcOffset};
    let Some(t) = t.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()) else {
        return String::new();
    };
    let t = UtcOffset::local_offset_at(t).map_or(t, |o| t.to_offset(o));
    t.format(format_description!("[year]-[month]-[day] [hour]:[minute]"))
        .unwrap_or_default()
}

/// Parent of a remote path (`/` stays `/`).
pub fn parent(path: &str) -> String {
    let t = path.trim_end_matches('/');
    match t.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => t[..i].into(),
    }
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Last component of a remote path.
pub fn basename(path: &str) -> &str {
    let t = path.trim_end_matches('/');
    t.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("/")
}

/// Checks a file or folder name typed by the user.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err("nom invalide".into());
    }
    if name.contains('/') || name.chars().any(char::is_control) {
        return Err("le nom ne doit contenir ni « / » ni caractère de contrôle".into());
    }
    Ok(())
}

/// Readable message of an SFTP error.
fn msg(e: SftpError) -> String {
    match e {
        SftpError::Status(s) => {
            let detail = s.error_message.trim();
            let base = match s.status_code {
                StatusCode::NoSuchFile => "fichier ou dossier introuvable",
                StatusCode::PermissionDenied => "accès refusé",
                StatusCode::OpUnsupported => "opération non prise en charge par le serveur",
                StatusCode::NoConnection | StatusCode::ConnectionLost => "connexion SFTP perdue",
                StatusCode::Failure => "échec de l'opération",
                _ => "erreur SFTP",
            };
            if detail.is_empty() || detail.eq_ignore_ascii_case(&s.status_code.to_string()) {
                base.into()
            } else {
                format!("{base} ({detail})")
            }
        }
        SftpError::Timeout => "le serveur SFTP ne répond pas".into(),
        other => format!("erreur SFTP : {other}"),
    }
}

fn io(e: std::io::Error) -> String {
    format!("erreur d'entrée / sortie : {e}")
}

/// Counters of one transfer, shared with the interface.
#[derive(Debug, Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    cancel: AtomicBool,
}

impl Progress {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
    /// Completion between 0 and 1 (0 while the size is unknown).
    pub fn ratio(&self) -> f32 {
        let total = self.total.load(Ordering::Acquire);
        if total == 0 {
            return 0.0;
        }
        (self.done.load(Ordering::Acquire) as f64 / total as f64).min(1.0) as f32
    }
}

/// SFTP session of an SSH connection.
pub struct Remote {
    sftp: SftpSession,
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Remote")
    }
}

fn item(dir: &str, name: String, a: &FileAttributes) -> Item {
    let ft = a.file_type();
    Item {
        path: join(dir, &name),
        name,
        dir: ft.is_dir(),
        link: ft.is_symlink(),
        size: a.size.unwrap_or(0),
        mtime: a.mtime.map(i64::from),
        mode: a.permissions.unwrap_or(0),
    }
}

impl Remote {
    /// Starts the SFTP protocol on an opened `sftp` subsystem stream.
    pub async fn start<S>(stream: S) -> Result<Self, String>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let sftp = SftpSession::new(stream).await.map_err(msg)?;
        sftp.set_timeout(30);
        Ok(Self { sftp })
    }

    /// Absolute form of `path` (`.` = home folder of the account).
    pub async fn canonical(&self, path: &str) -> Result<String, String> {
        self.sftp.canonicalize(path).await.map_err(msg)
    }

    /// Lists `dir` (without `.` and `..`), sorted by name, folders first;
    /// symbolic links to folders count as folders.
    pub async fn list(&self, dir: &str) -> Result<Vec<Item>, String> {
        let entries = self.sftp.read_dir(dir).await.map_err(msg)?;
        let mut out = Vec::new();
        for e in entries {
            let mut it = item(dir, e.file_name(), &e.metadata());
            if it.link {
                if let Ok(target) = self.sftp.metadata(it.path.clone()).await {
                    it.dir = target.file_type().is_dir();
                    if !it.dir {
                        it.size = target.size.unwrap_or(it.size);
                    }
                }
            }
            out.push(it);
        }
        sort(&mut out, SortKey::Name, false);
        Ok(out)
    }

    /// Attributes of `path` (links followed).
    pub async fn stat(&self, path: &str) -> Result<Item, String> {
        let a = self.sftp.metadata(path).await.map_err(msg)?;
        Ok(item(&parent(path), basename(path).to_string(), &a))
    }

    pub async fn exists(&self, path: &str) -> bool {
        self.sftp.symlink_metadata(path).await.is_ok()
    }

    pub async fn mkdir(&self, path: &str) -> Result<(), String> {
        check_name(basename(path))?;
        self.sftp.create_dir(path).await.map_err(msg)
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        check_name(basename(to))?;
        if self.exists(to).await {
            return Err(format!("« {} » existe déjà", basename(to)));
        }
        self.sftp.rename(from, to).await.map_err(msg)
    }

    /// Deletes a file, a link or a folder with its whole content (links are
    /// removed, never followed).
    pub async fn remove(&self, path: &str) -> Result<(), String> {
        if path.trim_end_matches('/').is_empty() {
            return Err("le dossier racine ne peut pas être supprimé".into());
        }
        let a = self.sftp.symlink_metadata(path).await.map_err(msg)?;
        if !a.file_type().is_dir() {
            return self.sftp.remove_file(path).await.map_err(msg);
        }
        // Depth first without recursion of futures.
        let mut stack = vec![(path.to_string(), false)];
        while let Some((dir, listed)) = stack.pop() {
            if listed {
                self.sftp.remove_dir(dir.clone()).await.map_err(msg)?;
                continue;
            }
            stack.push((dir.clone(), true));
            for e in self.sftp.read_dir(dir.clone()).await.map_err(msg)? {
                let p = join(&dir, &e.file_name());
                if e.metadata().file_type().is_dir() {
                    stack.push((p, false));
                } else {
                    self.sftp.remove_file(p).await.map_err(msg)?;
                }
            }
        }
        Ok(())
    }

    /// Sends the local file `local` to `remote` (replaced if it exists). A
    /// cancelled or failed upload removes the partial remote file.
    pub async fn upload(&self, local: &Path, remote: &str, p: &Progress) -> Result<(), String> {
        let mut src = tokio::fs::File::open(local).await.map_err(io)?;
        let len = src.metadata().await.map_err(io)?.len();
        p.total.store(len, Ordering::Release);
        let mut dst = self.sftp.create(remote).await.map_err(msg)?;
        let mut buf = vec![0u8; CHUNK];
        let copied: Result<(), String> = async {
            loop {
                if p.cancelled() {
                    return Err(CANCELLED.to_string());
                }
                let n = src.read(&mut buf).await.map_err(io)?;
                if n == 0 {
                    break;
                }
                dst.write_all(&buf[..n]).await.map_err(io)?;
                p.done.fetch_add(n as u64, Ordering::AcqRel);
            }
            dst.flush().await.map_err(io)?;
            Ok(())
        }
        .await;
        let closed = dst.shutdown().await.map_err(io);
        if let Err(e) = copied.and(closed) {
            let _ = self.sftp.remove_file(remote).await;
            return Err(e);
        }
        Ok(())
    }

    /// Saves `remote` as `local` through a `.part` file renamed at the end
    /// (removed when cancelled or failed).
    pub async fn download(&self, remote: &str, local: &Path, p: &Progress) -> Result<(), String> {
        let mut src = self.sftp.open(remote).await.map_err(msg)?;
        if let Ok(a) = src.metadata().await {
            p.total.store(a.size.unwrap_or(0), Ordering::Release);
        }
        let mut part = local.as_os_str().to_owned();
        part.push(".part");
        let part = PathBuf::from(part);
        let mut dst = tokio::fs::File::create(&part).await.map_err(io)?;
        let mut buf = vec![0u8; CHUNK];
        let copied: Result<(), String> = async {
            loop {
                if p.cancelled() {
                    return Err(CANCELLED.to_string());
                }
                let n = src.read(&mut buf).await.map_err(io)?;
                if n == 0 {
                    break;
                }
                dst.write_all(&buf[..n]).await.map_err(io)?;
                p.done.fetch_add(n as u64, Ordering::AcqRel);
            }
            dst.flush().await.map_err(io)?;
            Ok(())
        }
        .await;
        drop(dst);
        let _ = src.shutdown().await;
        let done = copied.and_then(|_| {
            let _ = std::fs::remove_file(local);
            std::fs::rename(&part, local).map_err(io)
        });
        if done.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        done
    }

    pub async fn close(&self) {
        let _ = self.sftp.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(name: &str, dir: bool, size: u64, mtime: i64) -> Item {
        Item {
            name: name.into(),
            path: join("/srv", name),
            dir,
            link: false,
            size,
            mtime: Some(mtime),
            mode: if dir { 0o40755 } else { 0o100644 },
        }
    }

    #[test]
    fn sorting() {
        let mut v = vec![
            it("f10.txt", false, 5, 3),
            it("B", true, 0, 1),
            it("f2.txt", false, 50, 1),
            it("a", true, 0, 2),
            it("F1.txt", false, 500, 2),
        ];
        let names = |v: &[Item]| v.iter().map(|i| i.name.clone()).collect::<Vec<_>>().join(",");
        sort(&mut v, SortKey::Name, false);
        assert_eq!(names(&v), "a,B,F1.txt,f2.txt,f10.txt");
        sort(&mut v, SortKey::Name, true);
        assert_eq!(names(&v), "B,a,f10.txt,f2.txt,F1.txt");
        sort(&mut v, SortKey::Size, true);
        assert_eq!(names(&v), "B,a,F1.txt,f2.txt,f10.txt");
        sort(&mut v, SortKey::Modified, false);
        assert_eq!(names(&v), "B,a,f2.txt,F1.txt,f10.txt");
        assert_eq!(SortKey::from_index(2), SortKey::Modified);
    }

    #[test]
    fn formatting() {
        assert_eq!(perms(0o40755, true, false), "drwxr-xr-x");
        assert_eq!(perms(0o100640, false, false), "-rw-r-----");
        assert_eq!(perms(0o104755, false, false), "-rwsr-xr-x");
        assert_eq!(perms(0o41777, true, false), "drwxrwxrwt");
        assert_eq!(perms(0o120777, false, true), "lrwxrwxrwx");
        assert_eq!(perms(0o2644, false, false), "-rw-r-Sr--");
        assert_eq!(fmt_size(512), "512 o");
        assert_eq!(fmt_size(12 * 1024), "12 Ko");
        assert_eq!(fmt_size(3 * 1024 * 1024 + 400 * 1024), "3.4 Mo");
        assert_eq!(fmt_size(5 * 1024 * 1024 * 1024 / 4), "1.25 Go");
        assert_eq!(fmt_time(None), "");
        assert_eq!(fmt_time(Some(1_790_000_000)).len(), 16);
    }

    #[test]
    fn paths() {
        assert_eq!(parent("/srv/www/"), "/srv");
        assert_eq!(parent("/srv"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/srv", "a"), "/srv/a");
        assert_eq!(basename("/srv/www/"), "www");
        assert_eq!(basename("/"), "/");
        assert!(check_name("rapport.txt").is_ok());
        for bad in ["", ".", "..", "a/b", "a\nb"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn progress() {
        let p = Progress::default();
        assert_eq!(p.ratio(), 0.0);
        p.total.store(200, Ordering::Release);
        p.done.store(50, Ordering::Release);
        assert_eq!(p.ratio(), 0.25);
        assert!(!p.cancelled());
        p.cancel();
        assert!(p.cancelled());
    }
}
