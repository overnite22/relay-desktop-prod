use crate::admin::*;
use crate::model::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("serveur injoignable : {0}")]
    Network(String),
    #[error("{0}")]
    Server(String),
    #[error("accès refusé : le jeton d'appareil a peut-être été révoqué")]
    Unauthorized,
    #[error("élément introuvable")]
    NotFound,
    #[error("mot de passe maître incorrect")]
    BadPassword,
    #[error("{0}")]
    Invalid(String),
    #[error("base locale : {0}")]
    Database(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A place where folders, entries and credentials live. Calls block and are
/// made from worker threads, never from the UI thread.
pub trait DataSource: Send + Sync {
    fn tree(&self) -> Result<Tree>;
    /// Creates the folder when `id` is empty, updates it otherwise.
    fn save_folder(&self, folder: &Folder) -> Result<Folder>;
    fn delete_folder(&self, id: &str) -> Result<()>;
    /// Creates the entry when `id` is empty, updates it otherwise.
    fn save_entry(&self, entry: &Entry) -> Result<Entry>;
    fn delete_entry(&self, id: &str) -> Result<()>;
    fn credentials(&self) -> Result<Vec<Credential>>;
    fn save_credential(&self, id: Option<&str>, input: &CredentialInput) -> Result<Credential>;
    fn delete_credential(&self, id: &str) -> Result<()>;
    fn reveal(&self, credential_id: &str, field: SecretField) -> Result<String>;

    /// Default credential per protocol for a folder (inherited by its content).
    fn set_folder_credentials(&self, folder_id: &str, creds: &std::collections::BTreeMap<String, Option<String>>) -> Result<()> {
        let mut f = self
            .tree()?
            .folders
            .into_iter()
            .find(|f| f.id == folder_id)
            .ok_or(Error::NotFound)?;
        f.credentials = creds.clone();
        self.save_folder(&f).map(|_| ())
    }

    fn totp(&self, credential_id: &str) -> Result<TotpCode> {
        let secret = zeroize::Zeroizing::new(self.reveal(credential_id, SecretField::Totp)?);
        let key = crate::totp::parse_secret(&secret).ok_or_else(|| Error::Invalid("secret TOTP invalide".into()))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let (code, remaining) = crate::totp::code_at(&key, now);
        Ok(TotpCode {
            code,
            remaining,
            period: 30,
        })
    }

    fn audit(&self, _limit: u32) -> Result<Vec<AuditEvent>> {
        Err(Error::Invalid("le journal d'activité est tenu par le serveur Relay".into()))
    }

    fn attachments(&self, entry_id: &str) -> Result<Vec<Attachment>>;
    fn add_attachment(&self, entry_id: &str, name: &str, data: &[u8]) -> Result<Attachment>;
    fn attachment_data(&self, entry_id: &str, attachment_id: &str) -> Result<Vec<u8>>;
    fn delete_attachment(&self, entry_id: &str, attachment_id: &str) -> Result<()>;

    /// Library of saved bulk commands.
    fn saved_commands(&self) -> Result<Vec<crate::command::SavedCommand>>;
    /// Creates the command when `id` is empty, updates it otherwise.
    fn save_command(&self, c: &crate::command::SavedCommand) -> Result<crate::command::SavedCommand>;
    fn delete_command(&self, id: &str) -> Result<()>;

    /// Encrypted backup of the whole source, protected by `password`.
    fn export_backup(&self, password: &str) -> Result<Vec<u8>>;
    /// Replaces the server content with a backup (server sources only).
    fn restore_backup(&self, _data: &[u8], _password: &str) -> Result<()> {
        Err(Error::Invalid(
            "un coffre local se restaure dans un nouveau fichier, ajouté ensuite comme source".into(),
        ))
    }

    /// NetBox / Proxmox synchronisation sources (server only).
    fn sync_sources(&self) -> Result<Vec<SyncSource>> {
        Err(server_only())
    }
    /// Creates (`id: None`) or updates a synchronisation source.
    fn save_sync_source(&self, _id: Option<&str>, _input: &crate::admin::SyncSourceInput) -> Result<SyncSource> {
        Err(server_only())
    }
    fn delete_sync_source(&self, _id: &str) -> Result<()> {
        Err(server_only())
    }
    fn test_sync_source(&self, _id: &str) -> Result<Outcome> {
        Err(server_only())
    }
    fn run_sync_source(&self, _id: &str) -> Result<Outcome> {
        Err(server_only())
    }
    /// Entries pointing at the same target.
    fn duplicates(&self) -> Result<Vec<DuplicateGroup>> {
        Ok(crate::admin::find_duplicates(&self.tree()?))
    }
    /// Merges `others` into `keep` and deletes them.
    fn merge_entries(&self, keep: &str, others: &[String]) -> Result<Entry>;
    /// Analyses an RDM export (.rdm XML).
    fn import_preview(&self, _data: &[u8]) -> Result<ImportPlan> {
        Err(server_only())
    }
    fn import_apply(&self, _data: &[u8], _opt: &ImportOptions) -> Result<ImportResult> {
        Err(server_only())
    }
    fn recordings(&self) -> Result<Vec<Recording>> {
        Err(server_only())
    }
    /// Raw Guacamole protocol of a recording (see [`crate::guac`]).
    fn recording_data(&self, _id: &str) -> Result<String> {
        Err(server_only())
    }
    fn delete_recording(&self, _id: &str) -> Result<()> {
        Err(server_only())
    }

    fn workspaces(&self) -> Result<Vec<Workspace>>;
    fn save_workspace(&self, workspace: &Workspace) -> Result<Workspace>;
    fn delete_workspace(&self, id: &str) -> Result<()>;

    fn pve_console(&self, _id: &str) -> Result<PveConsole> {
        Err(Error::Invalid("Une console Proxmox nécessite une source serveur Relay.".into()))
    }

    /// Base URL of the Relay web interface, for server-side administration.
    fn web_url(&self) -> Option<String> {
        None
    }
}

/// Credential used to open `entry`: its own, else the nearest folder default
/// for its protocol (same rule as the Relay server).
pub fn effective_credential(tree: &Tree, entry: &Entry) -> Option<String> {
    if let Some(id) = entry.credential_id.as_ref().filter(|s| !s.is_empty()) {
        return Some(id.clone());
    }
    let mut folder_id = entry.folder_id.clone();
    let mut guard = 0;
    while let Some(id) = folder_id {
        let folder = tree.folders.iter().find(|f| f.id == id)?;
        if let Some(Some(cred)) = folder.credentials.get(entry.kind.as_str()) {
            if !cred.is_empty() {
                return Some(cred.clone());
            }
        }
        folder_id = folder.parent_id.clone();
        guard += 1;
        if guard > 64 {
            return None;
        }
    }
    None
}

/// Short-lived console credentials. Never persist or log these values.
pub struct PveConsole {
    pub url: zeroize::Zeroizing<String>,
    pub bearer: zeroize::Zeroizing<String>,
    pub password: zeroize::Zeroizing<String>,
}

fn server_only() -> Error {
    Error::Invalid("fonction disponible avec une source serveur Relay".into())
}
