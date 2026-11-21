//! Administration data shared by the native dialogs: NetBox / Proxmox sync
//! sources, duplicate detection and merge, RDM import plans, recordings.

use crate::model::*;
use crate::source::{DataSource, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncSource {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub url: String,
    pub token_id: String,
    pub has_secret: bool,
    pub verify_tls: bool,
    /// `sshCredentialId`, `rdpCredentialId`, `guestEntries`, `nodeEntries`, `devices`, `vms`.
    #[serde(deserialize_with = "crate::model::nullable")]
    pub options: serde_json::Map<String, serde_json::Value>,
    pub interval_min: i64,
    pub last_sync_at: Option<String>,
    pub last_status: String,
}

impl SyncSource {
    /// Boolean option, `true` when absent (server rule).
    pub fn option_on(&self, key: &str) -> bool {
        self.options.get(key).and_then(serde_json::Value::as_bool).unwrap_or(true)
    }

    pub fn option_str(&self, key: &str) -> &str {
        self.options.get(key).and_then(serde_json::Value::as_str).unwrap_or("")
    }
}

/// Exact body of `POST /api/sources` and `PUT /api/sources/{id}` (unknown
/// fields are refused). `secret: None` keeps the stored secret.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSourceInput {
    pub kind: String,
    pub name: String,
    pub url: String,
    pub token_id: String,
    pub secret: Option<String>,
    pub verify_tls: bool,
    pub options: serde_json::Map<String, serde_json::Value>,
    pub interval_min: i64,
}

impl SyncSourceInput {
    /// Checks the rules of the server before sending.
    pub fn validate(&self, creating: bool) -> std::result::Result<(), String> {
        if self.kind != "proxmox" && self.kind != "netbox" {
            return Err("type de source inconnu".into());
        }
        if self.name.trim().is_empty() {
            return Err("le nom de la source est obligatoire".into());
        }
        if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
            return Err("l'URL doit commencer par https:// ou http://".into());
        }
        if self.kind == "proxmox" && !self.token_id.contains('!') {
            return Err("identifiant de jeton Proxmox attendu au format utilisateur@royaume!nom-du-jeton".into());
        }
        if !(0..=7 * 24 * 60).contains(&self.interval_min) {
            return Err("intervalle de synchronisation invalide (0 à 10080 minutes)".into());
        }
        if creating && self.secret.as_deref().is_none_or(|s| s.trim().is_empty()) {
            return Err("le secret du jeton d'API est obligatoire".into());
        }
        Ok(())
    }
}

/// Answer of the test and sync endpoints.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Outcome {
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DuplicateGroup {
    pub key: String,
    pub label: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Recording {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub at: String,
    pub size: i64,
    pub limited: bool,
}

/// Normalised target of an entry: same rule as the server, without DNS
/// resolution (host names are compared case-insensitively).
pub fn target_key(e: &Entry) -> Option<(String, String)> {
    let host = |h: &str| h.trim().trim_matches(['[', ']']).trim_end_matches('.').to_lowercase();
    match e.kind {
        Kind::Rdp | Kind::Ssh | Kind::Vnc if !e.host.trim().is_empty() => {
            let (k, h) = (e.kind.as_str(), host(&e.host));
            Some((format!("{k}|{h}|{}", e.port), format!("{} {h}:{}", k.to_uppercase(), e.port)))
        }
        Kind::Web => {
            let u = url::Url::parse(e.url.trim()).ok()?;
            let h = host(u.host_str()?);
            let scheme = u.scheme().to_lowercase();
            let port = u.port_or_known_default().map(|p| p.to_string()).unwrap_or_default();
            let path = u.path().trim_end_matches('/').to_string();
            Some((
                format!("web|{scheme}|{h}|{port}|{path}"),
                format!("Web {scheme}://{h}:{port}{path}"),
            ))
        }
        _ => None,
    }
}

/// Groups of entries pointing at the same target, synchronised entries first.
pub fn find_duplicates(tree: &Tree) -> Vec<DuplicateGroup> {
    let mut groups: BTreeMap<String, DuplicateGroup> = BTreeMap::new();
    for e in &tree.entries {
        if let Some((key, label)) = target_key(e) {
            let g = groups.entry(key.clone()).or_insert_with(|| DuplicateGroup {
                key,
                label,
                entries: Vec::new(),
            });
            g.entries.push(e.clone());
        }
    }
    let mut out: Vec<DuplicateGroup> = groups.into_values().filter(|g| g.entries.len() > 1).collect();
    for g in &mut out {
        g.entries
            .sort_by(|a, b| b.source_id.is_some().cmp(&a.source_id.is_some()).then_with(|| a.name.cmp(&b.name)));
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Merges `others` into `keep` like the server: credential, user name and
/// notes are taken over when missing, favourite is kept, others are deleted.
pub fn merge(src: &dyn DataSource, keep_id: &str, others: &[String]) -> Result<Entry> {
    let tree = src.tree()?;
    let find = |id: &str| tree.entries.iter().find(|e| e.id == id).cloned().ok_or(Error::NotFound);
    let mut keep = find(keep_id)?;
    let mut merged = Vec::new();
    for id in others.iter().filter(|id| id.as_str() != keep_id) {
        let o = find(id)?;
        if keep.credential_id.as_deref().is_none_or(str::is_empty) && o.credential_id.as_deref().is_some_and(|c| !c.is_empty()) {
            keep.credential_id = o.credential_id.clone();
        }
        if keep.username.is_empty() && !o.username.is_empty() {
            keep.username = o.username.clone();
        }
        let n = o.notes.trim();
        if !n.is_empty() && !keep.notes.contains(n) {
            if !keep.notes.is_empty() {
                keep.notes.push_str("\n\n");
            }
            keep.notes.push_str(&format!("— Notes reprises de « {} » —\n{n}", o.name));
        }
        keep.favorite |= o.favorite;
        merged.push(o.id);
    }
    if merged.is_empty() {
        return Err(Error::Invalid("choisissez au moins une autre entrée à fusionner".into()));
    }
    let saved = src.save_entry(&keep)?;
    for id in merged {
        src.delete_entry(&id)?;
    }
    Ok(saved)
}

/// Content of an RDM export, as analysed by the server (or locally).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportPlan {
    pub folders: Vec<ImportFolder>,
    pub entries: Vec<ImportEntry>,
    pub credentials: Vec<ImportCredential>,
    pub skipped: Vec<ImportSkipped>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportFolder {
    pub path: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportEntry {
    pub name: String,
    pub kind: String,
    pub host: String,
    pub port: u16,
    pub url: String,
    pub username: String,
    pub path: Vec<String>,
    pub credential: String,
    pub inline_secret: bool,
    pub rdm_type: String,
    #[serde(skip)]
    pub source_id: String,
    #[serde(skip)]
    pub credential_ref: String,
    #[serde(skip)]
    pub notes: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportCredential {
    #[serde(skip)]
    pub source_id: String,
    pub name: String,
    pub username: String,
    pub domain: String,
    pub has_password: bool,
    pub encrypted_secret: bool,
    pub has_private_key: bool,
    #[serde(skip)]
    pub password: String,
    #[serde(skip)]
    pub private_key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportSkipped {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportResult {
    pub folders: usize,
    pub entries: usize,
    pub credentials: usize,
    pub root_id: String,
}

/// Import options: root folder name and default credentials (by name in the
/// plan) for SSH and RDP entries imported without one.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub root: String,
    pub ssh_default: String,
    pub rdp_default: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: Kind, host: &str, port: u16, url: &str) -> Entry {
        let mut e = Entry::new(kind);
        e.id = name.into();
        e.name = name.into();
        e.host = host.into();
        e.port = port;
        e.url = url.into();
        e
    }

    #[test]
    fn duplicate_groups() {
        let mut synced = entry("b", Kind::Ssh, "SRV.lab.", 22, "");
        synced.source_id = Some("nb".into());
        let tree = Tree {
            entries: vec![
                entry("a", Kind::Ssh, "srv.lab", 22, ""),
                synced,
                entry("c", Kind::Ssh, "srv.lab", 2222, ""),
                entry("d", Kind::Rdp, "srv.lab", 22, ""),
                entry("w1", Kind::Web, "", 0, "https://Portail.lab/app/"),
                entry("w2", Kind::Web, "", 0, "https://portail.lab:443/app"),
            ],
            ..Default::default()
        };
        let g = find_duplicates(&tree);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].label, "SSH srv.lab:22");
        assert_eq!(g[0].entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(g[1].label, "Web https://portail.lab:443/app");
    }

    #[test]
    fn merge_local() {
        let dir = tempfile::tempdir().unwrap();
        let src = crate::LocalSource::create(&dir.path().join("m.rdb"), "mot de passe maître").unwrap();
        let mut a = entry("", Kind::Ssh, "h", 22, "");
        a.name = "garder".into();
        let a = src.save_entry(&a).unwrap();
        let mut b = entry("", Kind::Ssh, "h", 22, "");
        b.name = "doublon".into();
        b.username = "root".into();
        b.notes = "accès console".into();
        b.favorite = true;
        let b = src.save_entry(&b).unwrap();
        let kept = merge(&src, &a.id, std::slice::from_ref(&b.id)).unwrap();
        assert_eq!(kept.username, "root");
        assert!(kept.favorite && kept.notes.contains("« doublon »") && kept.notes.contains("accès console"));
        assert_eq!(src.tree().unwrap().entries.len(), 1);
        assert!(merge(&src, &a.id, std::slice::from_ref(&a.id)).is_err());
    }
}
