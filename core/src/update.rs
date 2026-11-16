//! In-app updates: finds the newest published release of Relay Desktop on
//! GitHub, downloads its MSI and verifies it against the published SHA-256
//! checksums before anything is executed.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_API: &str = "https://api.github.com";
pub const DEFAULT_REPO: &str = "overnite22/relay-desktop-prod";
const MAX_MSI: u64 = 512 << 20;
const MAX_SUMS: u64 = 64 << 10;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("serveur de mises à jour injoignable : {0}")]
    Network(String),
    #[error("réponse inattendue du serveur de mises à jour : {0}")]
    Invalid(String),
    #[error("l'empreinte SHA-256 du paquet téléchargé ne correspond pas : téléchargement rejeté")]
    Checksum,
    #[error("téléchargement annulé")]
    Cancelled,
    #[error("paquet de mise à jour : {0}")]
    Io(String),
}

/// Semantic version `MAJOR.MINOR.PATCH[-PRERELEASE]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

impl Version {
    /// Accepts `1.2.3`, `v1.2.3`, `desktop-v1.2.3` and pre-releases (`1.2.3-beta.1`).
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix("desktop-").unwrap_or(s);
        let s = s.strip_prefix('v').unwrap_or(s);
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_owned())),
            Some(_) => return None,
            None => (s, None),
        };
        let mut it = core.split('.').map(|p| {
            if p.is_empty() || (p.len() > 1 && p.starts_with('0')) {
                None
            } else {
                p.parse::<u64>().ok()
            }
        });
        let v = Version {
            major: it.next()??,
            minor: it.next()??,
            patch: it.next()??,
            pre,
        };
        it.next().is_none().then_some(v)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

fn cmp_pre(a: &str, b: &str) -> Ordering {
    for (x, y) in a.split('.').zip(b.split('.')) {
        let o = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            _ => x.cmp(y),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    a.split('.').count().cmp(&b.split('.').count())
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(o.major, o.minor, o.patch))
            .then_with(|| match (&self.pre, &o.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => cmp_pre(a, b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    pub notes: String,
    pub page: String,
    pub msi_name: String,
    pub msi_url: String,
    pub msi_size: u64,
    pub sums_url: String,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

/// Picks the newest usable release from a GitHub `/releases` listing:
/// published, not a pre-release, with an x64 MSI named after its version and
/// a `SHA256SUMS` asset.
pub fn parse_releases(json: &str) -> Result<Option<Release>, UpdateError> {
    let list: Vec<GhRelease> = serde_json::from_str(json).map_err(|e| UpdateError::Invalid(e.to_string()))?;
    let mut best: Option<Release> = None;
    for r in list.into_iter().filter(|r| !r.draft && !r.prerelease) {
        let Some(version) = Version::parse(&r.tag_name) else { continue };
        if version.pre.is_some() {
            continue;
        }
        let msi_name = format!("Relay-{version}-x64.msi");
        let (Some(msi), Some(sums)) = (
            r.assets.iter().find(|a| a.name == msi_name),
            r.assets.iter().find(|a| a.name == "SHA256SUMS"),
        ) else {
            continue;
        };
        let candidate = Release {
            version,
            tag: r.tag_name.clone(),
            notes: r.body.clone().unwrap_or_default(),
            page: r.html_url.clone(),
            msi_name,
            msi_url: msi.browser_download_url.clone(),
            msi_size: msi.size,
            sums_url: sums.browser_download_url.clone(),
        };
        if best.as_ref().is_none_or(|b| candidate.version > b.version) {
            best = Some(candidate);
        }
    }
    Ok(best)
}

/// Expected hex digest of `file` in a `sha256sum`-style listing.
pub fn expected_sum(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (hash, name) = (parts.next()?, parts.next()?);
        (name.trim_start_matches('*') == file && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Only HTTPS, except plain HTTP on loopback for tests.
fn check_url(url: &str) -> Result<(), UpdateError> {
    let u = url::Url::parse(url).map_err(|_| UpdateError::Invalid(format!("adresse invalide : {url}")))?;
    let loopback = matches!(u.host_str(), Some("127.0.0.1") | Some("localhost") | Some("[::1]"));
    match u.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        _ => Err(UpdateError::Invalid(format!("adresse non sécurisée refusée : {url}"))),
    }
}

pub struct Updater {
    api: String,
    repo: String,
    agent: ureq::Agent,
}

impl Updater {
    pub fn new(api: &str, repo: &str, user_agent: &str) -> Updater {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .http_status_as_error(false)
            .user_agent(user_agent)
            .tls_config(tls)
            .build()
            .into();
        Updater {
            api: api.trim_end_matches('/').to_owned(),
            repo: repo.to_owned(),
            agent,
        }
    }

    fn get(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, UpdateError> {
        check_url(url)?;
        let resp = self
            .agent
            .get(url)
            .header("Accept", "application/vnd.github+json, application/octet-stream")
            .call()
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        match resp.status().as_u16() {
            200..=299 => Ok(resp),
            403 | 429 => Err(UpdateError::Network(
                "limite de requêtes GitHub atteinte, réessayez plus tard".into(),
            )),
            s => Err(UpdateError::Invalid(format!("HTTP {s} pour {url}"))),
        }
    }

    /// Newest release, whatever the installed version.
    pub fn latest(&self) -> Result<Option<Release>, UpdateError> {
        let url = format!("{}/repos/{}/releases?per_page=20", self.api, self.repo);
        let body = self
            .get(&url)?
            .body_mut()
            .with_config()
            .limit(4 << 20)
            .read_to_string()
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        parse_releases(&body)
    }

    /// Release strictly newer than `current`, if any.
    pub fn available(&self, current: &str) -> Result<Option<Release>, UpdateError> {
        let current = Version::parse(current).ok_or_else(|| UpdateError::Invalid(format!("version locale illisible : {current}")))?;
        Ok(self.latest()?.filter(|r| r.version > current))
    }

    /// Downloads the MSI into `dir`, verifying its SHA-256 while streaming.
    /// The file only gets its final name once the checksum matches.
    pub fn download(&self, r: &Release, dir: &Path, progress: impl Fn(u64, u64)) -> Result<PathBuf, UpdateError> {
        self.download_cancellable(r, dir, progress, || false)
    }

    /// Stops between received chunks when cancelled, removing the partial file.
    pub fn download_cancellable(
        &self,
        r: &Release,
        dir: &Path,
        progress: impl Fn(u64, u64),
        cancelled: impl Fn() -> bool,
    ) -> Result<PathBuf, UpdateError> {
        if cancelled() {
            return Err(UpdateError::Cancelled);
        }
        let sums = self
            .get(&r.sums_url)?
            .body_mut()
            .with_config()
            .limit(MAX_SUMS)
            .read_to_string()
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        let expected =
            expected_sum(&sums, &r.msi_name).ok_or_else(|| UpdateError::Invalid(format!("{} absent de SHA256SUMS", r.msi_name)))?;
        std::fs::create_dir_all(dir).map_err(|e| UpdateError::Io(e.to_string()))?;
        let final_path = dir.join(&r.msi_name);
        let part = dir.join(format!("{}.part", r.msi_name));
        let mut resp = self.get(&r.msi_url)?;
        let mut reader = resp.body_mut().with_config().limit(MAX_MSI).reader();
        // Drop the file before the guard so cleanup works on Windows too.
        struct PartialFile(PathBuf);
        impl Drop for PartialFile {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = PartialFile(part.clone());
        let mut file = std::fs::File::create(&part).map_err(|e| UpdateError::Io(e.to_string()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 << 10];
        let mut done = 0u64;
        loop {
            if cancelled() {
                return Err(UpdateError::Cancelled);
            }
            let n = reader.read(&mut buf).map_err(|e| UpdateError::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| UpdateError::Io(e.to_string()))?;
            done += n as u64;
            progress(done, r.msi_size);
        }
        if cancelled() {
            return Err(UpdateError::Cancelled);
        }
        file.sync_all().map_err(|e| UpdateError::Io(e.to_string()))?;
        drop(file);
        if hex(&hasher.finalize()) != expected {
            let _ = std::fs::remove_file(&part);
            return Err(UpdateError::Checksum);
        }
        std::fs::rename(&part, &final_path).map_err(|e| UpdateError::Io(e.to_string()))?;
        Ok(final_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("desktop-v1.0.0") > v("1.0.0-beta.2"));
        assert!(v("1.0.0-beta.10") > v("1.0.0-beta.2"));
        assert!(v("1.0.0-beta") < v("1.0.0-beta.1"));
        assert_eq!(v("v2.3.4").to_string(), "2.3.4");
        for bad in ["1.2", "1.2.3.4", "01.2.3", "x.y.z", "1.2.3-", ""] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn picks_newest_complete_release() {
        let json = r#"[
          {"tag_name":"desktop-v0.3.0","draft":true,"assets":[]},
          {"tag_name":"desktop-v0.2.5","prerelease":true,"assets":[]},
          {"tag_name":"desktop-v0.2.1","assets":[{"name":"Relay-0.2.1-x64.msi","browser_download_url":"https://x/a.msi","size":5}]},
          {"tag_name":"desktop-v0.2.0","body":"Notes","html_url":"https://x/r","assets":[
             {"name":"Relay-0.2.0-x64.msi","browser_download_url":"https://x/b.msi","size":7},
             {"name":"SHA256SUMS","browser_download_url":"https://x/s"}]},
          {"tag_name":"desktop-v0.1.0","assets":[
             {"name":"Relay-0.1.0-x64.msi","browser_download_url":"https://x/c.msi"},
             {"name":"SHA256SUMS","browser_download_url":"https://x/s0"}]}
        ]"#;
        let r = parse_releases(json).unwrap().unwrap();
        assert_eq!(r.version.to_string(), "0.2.0");
        assert_eq!((r.msi_url.as_str(), r.msi_size, r.notes.as_str()), ("https://x/b.msi", 7, "Notes"));
        assert!(parse_releases("[]").unwrap().is_none());
        assert!(parse_releases("{").is_err());
    }

    #[test]
    fn checksums() {
        let h = "a".repeat(64);
        let sums = format!("{h}  Relay-1.0.0-x64.msi\n{}  *other.msi\n", "b".repeat(64));
        assert_eq!(expected_sum(&sums, "Relay-1.0.0-x64.msi"), Some(h));
        assert_eq!(expected_sum(&sums, "other.msi"), Some("b".repeat(64)));
        assert_eq!(expected_sum(&sums, "missing.msi"), None);
        assert_eq!(expected_sum("zz  Relay-1.0.0-x64.msi", "Relay-1.0.0-x64.msi"), None);
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rejects_insecure_urls() {
        assert!(check_url("http://evil.example/x.msi").is_err());
        assert!(check_url("file:///C:/x.msi").is_err());
        assert!(check_url("https://github.com/x").is_ok());
        assert!(check_url("http://127.0.0.1:9/x").is_ok());
    }
}
