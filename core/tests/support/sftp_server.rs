//! SSH + SFTP test server (russh, russh-sftp) serving a folder as `/`:
//! password `secret` for the user `test`, a shell that echoes its input.
//! Shared by `tests/sftp.rs` and `examples/sftp_test_server.rs`.
use russh::server::{Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, SftpHandle, Status, StatusCode, Version};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

struct Ssh {
    root: PathBuf,
    channels: HashMap<ChannelId, Channel<Msg>>,
    /// Shell channels (echoed), the others carry SFTP.
    shells: std::collections::HashSet<ChannelId>,
    /// Current line of the test shell.
    line: Vec<u8>,
}

impl russh::server::Handler for Ssh {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == "test" && password == "secret" {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        match (name, self.channels.remove(&id)) {
            ("sftp", Some(channel)) => {
                session.channel_success(id)?;
                let fs = Fs {
                    root: self.root.clone(),
                    ..Default::default()
                };
                russh_sftp::server::run(channel.into_stream(), fs).await;
            }
            _ => session.channel_failure(id)?,
        }
        Ok(())
    }

    async fn shell_request(&mut self, id: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        self.shells.insert(id);
        let banner = [
            "Shell de test srv-lab-sftp (écho ; « demo » affiche un échantillon, « latin1 » du texte ISO-8859-1)",
            "",
        ]
        .join("\r\n");
        session.data(id, format!("{banner}$ ").into_bytes())
    }

    async fn data(&mut self, id: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if !self.shells.contains(&id) {
            return Ok(());
        }
        // Bytes received, for manual checks of the keyboard and charsets.
        eprintln!("reçu : {data:02x?}");
        let mut out = Vec::new();
        for &b in data {
            if b != b'\r' {
                self.line.push(b);
                out.push(b);
                continue;
            }
            out.extend_from_slice(b"\r\n");
            match std::mem::take(&mut self.line).as_slice() {
                b"demo" => out.extend_from_slice(DEMO.replace('\n', "\r\n").as_bytes()),
                b"latin1" => out.extend_from_slice(b"Latin-1 : caf\xe9, \xe0 l'\xe9t\xe9, na\xefve, \xa9 \xb0C\r\n"),
                _ => {}
            }
            out.extend_from_slice(b"$ ");
        }
        session.data(id, out)
    }
}

/// Sample shown by the "demo" command of the test shell.
const DEMO: &str = "Accents : é è à ç ê ë ï ô ù œ € ß ñ
Symboles : ✓ ✗ ⚠ → ← ↑ ★ ♥ © ® ° ±
Cadres : ┌──┬──┐ ╔══╗ ░▒▓█
         └──┴──┘ ╚══╝
Powerline : \u{e0b0} main \u{e0a0} \u{e0b2}
Emoji : 😀 🚀 👍
CJK : 日本語 中文 한국어
Couleurs : \x1b[31mrouge\x1b[0m \x1b[32mvert\x1b[0m \x1b[33mjaune\x1b[0m \x1b[34mbleu\x1b[0m \x1b[35mmagenta\x1b[0m \x1b[36mcyan\x1b[0m \x1b[1;31mgras rouge\x1b[0m \x1b[7minverse\x1b[0m
ERROR: connection refused by srv-lab-db
warning: disk /var at 91%
nginx.service: active (running), status OK
ping 203.0.113.7, fe80::1, https://example.com/doc
";

#[derive(Default)]
struct Fs {
    root: PathBuf,
    next: u32,
    files: HashMap<String, std::fs::File>,
    dirs: HashMap<String, Option<Vec<File>>>,
}

fn code(e: std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

/// Lexical `/`-rooted form of a client path.
fn normal(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in Path::new(path).components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str().unwrap_or("")),
            Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    format!("/{}", parts.join("/"))
}

impl Fs {
    fn real(&self, path: &str) -> PathBuf {
        self.root.join(normal(path).trim_start_matches('/'))
    }
    fn handle(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }
}

impl russh_sftp::server::Handler for Fs {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![File::dummy(normal(&path))],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let mut files = Vec::new();
        for e in std::fs::read_dir(self.real(&path)).map_err(code)? {
            let e = e.map_err(code)?;
            let meta = e.metadata().map_err(code)?;
            files.push(File::new(e.file_name().to_string_lossy().into_owned(), FileAttributes::from(&meta)));
        }
        let h = self.handle("d");
        self.dirs.insert(h.clone(), Some(files));
        Ok(Handle { id, handle: h.into() })
    }

    async fn readdir(&mut self, id: u32, handle: SftpHandle) -> Result<Name, Self::Error> {
        let key = String::from_utf8_lossy(handle.as_bytes()).into_owned();
        match self.dirs.get_mut(&key).and_then(Option::take) {
            Some(files) => Ok(Name { id, files }),
            None => Err(StatusCode::Eof),
        }
    }

    async fn open(&mut self, id: u32, name: String, flags: OpenFlags, _: FileAttributes) -> Result<Handle, Self::Error> {
        let f = std::fs::OpenOptions::new()
            .read(flags.contains(OpenFlags::READ))
            .write(flags.contains(OpenFlags::WRITE))
            .create(flags.contains(OpenFlags::CREATE))
            .truncate(flags.contains(OpenFlags::TRUNCATE))
            .open(self.real(&name))
            .map_err(code)?;
        let h = self.handle("f");
        self.files.insert(h.clone(), f);
        Ok(Handle { id, handle: h.into() })
    }

    async fn read(&mut self, id: u32, handle: SftpHandle, offset: u64, len: u32) -> Result<Data, Self::Error> {
        let key = String::from_utf8_lossy(handle.as_bytes()).into_owned();
        let f = self.files.get_mut(&key).ok_or(StatusCode::Failure)?;
        f.seek(SeekFrom::Start(offset)).map_err(code)?;
        let mut data = vec![0; len as usize];
        let n = f.read(&mut data).map_err(code)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        data.truncate(n);
        Ok(Data { id, data })
    }

    async fn write(&mut self, id: u32, handle: SftpHandle, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
        let key = String::from_utf8_lossy(handle.as_bytes()).into_owned();
        let f = self.files.get_mut(&key).ok_or(StatusCode::Failure)?;
        f.seek(SeekFrom::Start(offset)).map_err(code)?;
        f.write_all(&data).map_err(code)?;
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: SftpHandle) -> Result<Status, Self::Error> {
        let key = String::from_utf8_lossy(handle.as_bytes()).into_owned();
        self.files.remove(&key);
        self.dirs.remove(&key);
        Ok(ok(id))
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::metadata(self.real(&path)).map_err(code)?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::symlink_metadata(self.real(&path)).map_err(code)?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn fstat(&mut self, id: u32, handle: SftpHandle) -> Result<Attrs, Self::Error> {
        let key = String::from_utf8_lossy(handle.as_bytes()).into_owned();
        let meta = self.files.get(&key).ok_or(StatusCode::Failure)?.metadata().map_err(code)?;
        Ok(Attrs {
            id,
            attrs: FileAttributes::from(&meta),
        })
    }

    async fn setstat(&mut self, id: u32, _: String, _: FileAttributes) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }

    async fn fsetstat(&mut self, id: u32, _: SftpHandle, _: FileAttributes) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, _: FileAttributes) -> Result<Status, Self::Error> {
        std::fs::create_dir(self.real(&path)).map_err(code)?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        std::fs::remove_dir(self.real(&path)).map_err(code)?;
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        std::fs::remove_file(self.real(&path)).map_err(code)?;
        Ok(ok(id))
    }

    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, Self::Error> {
        std::fs::rename(self.real(&from), self.real(&to)).map_err(code)?;
        Ok(ok(id))
    }
}

/// Starts the server on `addr` (port 0 = free port) and returns its port.
pub async fn serve(root: PathBuf, addr: &str) -> u16 {
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Arc::new(russh::server::Config {
        keys: vec![russh::keys::PrivateKey::from(
            russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]),
        )],
        auth_rejection_time: Duration::from_millis(10),
        ..Default::default()
    });
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let handler = Ssh {
                root: root.clone(),
                channels: HashMap::new(),
                shells: Default::default(),
                line: Vec::new(),
            };
            let config = config.clone();
            tokio::spawn(async move {
                if let Ok(running) = russh::server::run_stream(config, socket, handler).await {
                    let _ = running.await;
                }
            });
        }
    });
    port
}
