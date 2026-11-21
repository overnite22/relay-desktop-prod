use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Connection protocol, serialized like the Relay server (`"rdp"`, `"ssh"`…).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Rdp,
    Ssh,
    Vnc,
    Web,
    Pve,
}

impl Kind {
    pub const EDITABLE: [Kind; 4] = [Kind::Rdp, Kind::Ssh, Kind::Vnc, Kind::Web];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Rdp => "rdp",
            Kind::Ssh => "ssh",
            Kind::Vnc => "vnc",
            Kind::Web => "web",
            Kind::Pve => "pve",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::Rdp => "RDP",
            Kind::Ssh => "SSH",
            Kind::Vnc => "VNC",
            Kind::Web => "Web",
            Kind::Pve => "Proxmox",
        }
    }
    pub fn default_port(self) -> u16 {
        match self {
            Kind::Rdp => 3389,
            Kind::Ssh => 22,
            Kind::Vnc => 5900,
            Kind::Web => 443,
            Kind::Pve => 8006,
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        [Kind::Rdp, Kind::Ssh, Kind::Vnc, Kind::Web, Kind::Pve]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Folder {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    /// Default credential per protocol inherited by the folder content.
    #[serde(deserialize_with = "nullable")]
    pub credentials: BTreeMap<String, Option<String>>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub folder_id: Option<String>,
    pub name: String,
    pub kind: Kind,
    #[serde(default, deserialize_with = "nullable")]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default, deserialize_with = "nullable")]
    pub url: String,
    #[serde(default, deserialize_with = "nullable")]
    pub username: String,
    #[serde(default)]
    pub credential_id: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub settings: serde_json::Map<String, serde_json::Value>,
    #[serde(default, deserialize_with = "nullable")]
    pub notes: String,
    #[serde(default)]
    pub favorite: bool,
    /// Set when the entry is synchronized from NetBox/Proxmox (read-only).
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

impl Entry {
    pub fn new(kind: Kind) -> Self {
        Entry {
            id: String::new(),
            folder_id: None,
            name: String::new(),
            kind,
            host: String::new(),
            port: kind.default_port(),
            url: String::new(),
            username: String::new(),
            credential_id: None,
            settings: Default::default(),
            notes: String::new(),
            favorite: false,
            source_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

/// Exact body accepted by `POST/PUT /api/entries` (unknown fields are refused).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryInput<'a> {
    pub folder_id: &'a Option<String>,
    pub name: &'a str,
    pub kind: Kind,
    pub host: &'a str,
    pub port: u16,
    pub url: &'a str,
    pub username: &'a str,
    pub credential_id: &'a Option<String>,
    pub settings: &'a serde_json::Map<String, serde_json::Value>,
    pub notes: &'a str,
    pub favorite: bool,
}

impl<'a> From<&'a Entry> for EntryInput<'a> {
    fn from(e: &'a Entry) -> Self {
        EntryInput {
            folder_id: &e.folder_id,
            name: &e.name,
            kind: e.kind,
            host: &e.host,
            port: e.port,
            url: &e.url,
            username: &e.username,
            credential_id: &e.credential_id,
            settings: &e.settings,
            notes: &e.notes,
            favorite: e.favorite,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Credential {
    pub id: String,
    pub name: String,
    pub username: String,
    pub domain: String,
    pub notes: String,
    pub has_password: bool,
    pub has_private_key: bool,
    pub has_passphrase: bool,
    pub has_totp: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Secrets: `None` keeps the stored value, `Some("")` clears it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, zeroize::Zeroize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInput {
    pub name: String,
    pub username: String,
    pub domain: String,
    pub notes: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub totp: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretField {
    Password,
    PrivateKey,
    Passphrase,
    Totp,
}

impl SecretField {
    pub fn as_str(self) -> &'static str {
        match self {
            SecretField::Password => "password",
            SecretField::PrivateKey => "privateKey",
            SecretField::Passphrase => "passphrase",
            SecretField::Totp => "totp",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tree {
    #[serde(deserialize_with = "nullable")]
    pub folders: Vec<Folder>,
    #[serde(deserialize_with = "nullable")]
    pub entries: Vec<Entry>,
}

/// Go encodes empty slices and maps as `null`.
pub(crate) fn nullable<'de, D: serde::Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AuditEvent {
    pub id: i64,
    pub at: String,
    pub user: String,
    pub action: String,
    pub target: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub entry_ids: Vec<String>,
}

/// Current TOTP code of a credential.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TotpCode {
    pub code: String,
    pub remaining: u32,
    pub period: u32,
}

/// File attached to an entry (content fetched separately).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub size: i64,
    pub created_at: String,
}

/// Same limit as the Relay server.
pub const MAX_ATTACHMENT_SIZE: usize = 10 << 20;

/// Validates an attachment file name like the Relay server does.
pub fn attachment_name(raw: &str) -> Option<String> {
    let base = raw.replace('\\', "/");
    let name = base.rsplit('/').next().unwrap_or("").trim();
    if name.is_empty() || name == "." || name == ".." || name.len() > 255 || name.chars().any(char::is_control) {
        None
    } else {
        Some(name.to_owned())
    }
}

/// Formats a server / database timestamp (RFC 3339, UTC) in local time:
/// `AAAA-MM-JJ HH:MM` (`:SS` with `seconds`). Unknown formats are shown as is.
pub fn local_time(s: &str, seconds: bool) -> String {
    use time::{format_description::well_known::Rfc3339, macros::format_description, OffsetDateTime, UtcOffset};
    let raw = || s.replace('T', " ").chars().take(if seconds { 19 } else { 16 }).collect();
    let Ok(t) = OffsetDateTime::parse(s.trim(), &Rfc3339) else {
        return raw();
    };
    let t = UtcOffset::local_offset_at(t).map_or(t, |o| t.to_offset(o));
    let out = if seconds {
        t.format(format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"))
    } else {
        t.format(format_description!("[year]-[month]-[day] [hour]:[minute]"))
    };
    out.unwrap_or_else(|_| raw())
}

#[cfg(test)]
mod time_tests {
    use super::local_time;

    #[test]
    fn local_time_formats() {
        // The offset depends on the machine: only the shape is checked.
        let t = local_time("2026-11-18T07:52:09.437Z", true);
        assert_eq!(t.len(), 19, "{t}");
        assert!(t.starts_with("2026-11-1"), "{t}");
        assert_eq!(local_time("2026-11-18T07:52:09Z", false).len(), 16);
        assert_eq!(local_time("hier", false), "hier");
        assert_eq!(local_time("", true), "");
    }
}
