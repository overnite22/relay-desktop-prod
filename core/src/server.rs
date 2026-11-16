//! Relay server data source, authenticated with a device token
//! (`Authorization: Bearer rlt_…`) created in Administration → Appareils.

use crate::admin::*;
use crate::command::SavedCommand;
use crate::model::*;
use crate::source::{DataSource, Error, PveConsole, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::time::Duration;
use zeroize::Zeroizing;

pub struct ServerSource {
    base: String,
    token: Zeroizing<String>,
    agent: ureq::Agent,
    user: String,
}

fn seg(id: &str) -> String {
    url::form_urlencoded::byte_serialize(id.as_bytes()).collect()
}

impl ServerSource {
    /// Validates the token against `/api/me` and returns a ready source.
    pub fn connect(server: &str, token: &str, user_agent: &str) -> Result<Self> {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(user_agent)
            .tls_config(tls)
            .build()
            .into();
        let mut s = ServerSource {
            base: server.trim_end_matches('/').to_owned(),
            token: Zeroizing::new(token.to_owned()),
            agent,
            user: String::new(),
        };
        #[derive(serde::Deserialize)]
        struct Me {
            user: String,
        }
        s.user = s.call::<Me>("GET", "/api/me", None::<&()>)?.user;
        Ok(s)
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn server(&self) -> &str {
        &self.base
    }

    fn call<T: DeserializeOwned>(&self, method: &str, path: &str, body: Option<&impl Serialize>) -> Result<T> {
        let text = self.send(method, path, body)?;
        let text = if text.trim().is_empty() { "null" } else { text.as_str() };
        serde_json::from_str(text).map_err(|e| Error::Server(format!("réponse illisible du serveur : {e}")))
    }

    fn send(&self, method: &str, path: &str, body: Option<&impl Serialize>) -> Result<String> {
        let body = match body {
            Some(b) => Some((
                serde_json::to_vec(b).map_err(|e| Error::Invalid(e.to_string()))?,
                "application/json",
            )),
            None => None,
        };
        let bytes = self.raw(method, path, body, &[])?;
        String::from_utf8(bytes).map_err(|e| Error::Server(format!("réponse illisible du serveur : {e}")))
    }

    /// Sends an optional body with its content type and returns the raw response.
    fn raw(&self, method: &str, path: &str, body: Option<(Vec<u8>, &str)>, headers: &[(&str, &str)]) -> Result<Vec<u8>> {
        let req = ureq::http::Request::builder()
            .method(method)
            .uri(format!("{}{}", self.base, path))
            .header("Authorization", format!("Bearer {}", self.token.as_str()))
            .header("X-Relay", "1")
            .header("Accept", "application/json");
        let req = headers.iter().fold(req, |req, (k, v)| req.header(*k, *v));
        let resp = match body {
            Some((bytes, kind)) => {
                let req = req
                    .header("Content-Type", kind)
                    .body(bytes)
                    .map_err(|e| Error::Invalid(e.to_string()))?;
                self.agent.run(req)
            }
            None => self.agent.run(req.body(()).map_err(|e| Error::Invalid(e.to_string()))?),
        };
        let mut resp = resp.map_err(|e| Error::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let data = resp.body_mut().with_config().limit(64 << 20).read_to_vec().unwrap_or_default();
        if (200..300).contains(&status) {
            return Ok(data);
        }
        let text = String::from_utf8_lossy(&data);
        match status {
            401 => Err(Error::Unauthorized),
            404 => Err(Error::NotFound),
            300..=399 | 403 if !text.trim_start().starts_with('{') => Err(Error::Server(
                "accès bloqué avant Relay : si un SSO protège le serveur, il doit laisser passer /api/ avec un jeton d'appareil".into(),
            )),
            _ => {
                let msg = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| format!("erreur HTTP {status}"));
                Err(Error::Server(msg))
            }
        }
    }
}

impl DataSource for ServerSource {
    fn pve_console(&self, id: &str) -> Result<PveConsole> {
        #[derive(serde::Deserialize)]
        struct Ticket {
            token: String,
            password: String,
        }
        let t: Ticket = self.call(
            "POST",
            &format!("/api/entries/{}/connect", seg(id)),
            Some(&serde_json::json!({"native": false})),
        )?;
        let base = self.base.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
        Ok(PveConsole {
            url: Zeroizing::new(format!("{base}/api/vnc-tunnel?token={}", seg(&t.token))),
            bearer: self.token.clone(),
            password: Zeroizing::new(t.password),
        })
    }

    fn tree(&self) -> Result<Tree> {
        self.call("GET", "/api/tree", None::<&()>)
    }

    fn save_folder(&self, f: &Folder) -> Result<Folder> {
        let body = serde_json::json!({ "name": f.name.trim(), "parentId": f.parent_id });
        if f.id.is_empty() {
            self.call("POST", "/api/folders", Some(&body))
        } else {
            self.call("PUT", &format!("/api/folders/{}", seg(&f.id)), Some(&body))
        }
    }

    fn delete_folder(&self, id: &str) -> Result<()> {
        self.send("DELETE", &format!("/api/folders/{}", seg(id)), None::<&()>).map(|_| ())
    }

    fn save_entry(&self, e: &Entry) -> Result<Entry> {
        let body = EntryInput::from(e);
        if e.id.is_empty() {
            self.call("POST", "/api/entries", Some(&body))
        } else {
            self.call("PUT", &format!("/api/entries/{}", seg(&e.id)), Some(&body))
        }
    }

    fn delete_entry(&self, id: &str) -> Result<()> {
        self.send("DELETE", &format!("/api/entries/{}", seg(id)), None::<&()>).map(|_| ())
    }

    fn credentials(&self) -> Result<Vec<Credential>> {
        self.call("GET", "/api/credentials", None::<&()>)
    }

    fn save_credential(&self, id: Option<&str>, input: &CredentialInput) -> Result<Credential> {
        match id {
            None => self.call("POST", "/api/credentials", Some(input)),
            Some(id) => self.call("PUT", &format!("/api/credentials/{}", seg(id)), Some(input)),
        }
    }

    fn delete_credential(&self, id: &str) -> Result<()> {
        self.send("DELETE", &format!("/api/credentials/{}", seg(id)), None::<&()>)
            .map(|_| ())
    }

    fn set_folder_credentials(&self, folder_id: &str, creds: &std::collections::BTreeMap<String, Option<String>>) -> Result<()> {
        let body = serde_json::json!({ "credentials": creds });
        self.send("PUT", &format!("/api/folders/{}/credentials", seg(folder_id)), Some(&body))
            .map(|_| ())
    }

    fn totp(&self, credential_id: &str) -> Result<TotpCode> {
        self.call("GET", &format!("/api/credentials/{}/totp", seg(credential_id)), None::<&()>)
    }

    fn audit(&self, limit: u32) -> Result<Vec<AuditEvent>> {
        self.call("GET", &format!("/api/audit?limit={limit}"), None::<&()>)
    }

    fn workspaces(&self) -> Result<Vec<Workspace>> {
        self.call::<Option<Vec<Workspace>>>("GET", "/api/workspaces", None::<&()>)
            .map(Option::unwrap_or_default)
    }

    fn save_workspace(&self, w: &Workspace) -> Result<Workspace> {
        let body = serde_json::json!({ "name": w.name.trim(), "entryIds": w.entry_ids });
        if w.id.is_empty() {
            self.call("POST", "/api/workspaces", Some(&body))
        } else {
            self.call("PUT", &format!("/api/workspaces/{}", seg(&w.id)), Some(&body))
        }
    }

    fn delete_workspace(&self, id: &str) -> Result<()> {
        self.send("DELETE", &format!("/api/workspaces/{}", seg(id)), None::<&()>)
            .map(|_| ())
    }

    fn attachments(&self, entry_id: &str) -> Result<Vec<Attachment>> {
        self.call::<Option<Vec<Attachment>>>("GET", &format!("/api/entries/{}/attachments", seg(entry_id)), None::<&()>)
            .map(Option::unwrap_or_default)
    }

    fn add_attachment(&self, entry_id: &str, name: &str, data: &[u8]) -> Result<Attachment> {
        let path = format!("/api/entries/{}/attachments?name={}", seg(entry_id), seg(name));
        let bytes = self.raw("POST", &path, Some((data.to_vec(), "application/octet-stream")), &[])?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Server(format!("réponse illisible du serveur : {e}")))
    }

    fn attachment_data(&self, entry_id: &str, attachment_id: &str) -> Result<Vec<u8>> {
        let path = format!("/api/entries/{}/attachments/{}", seg(entry_id), seg(attachment_id));
        self.raw("GET", &path, None, &[])
    }

    fn delete_attachment(&self, entry_id: &str, attachment_id: &str) -> Result<()> {
        let path = format!("/api/entries/{}/attachments/{}", seg(entry_id), seg(attachment_id));
        self.raw("DELETE", &path, None, &[]).map(|_| ())
    }

    fn saved_commands(&self) -> Result<Vec<SavedCommand>> {
        self.call::<Option<Vec<SavedCommand>>>("GET", "/api/commands/saved", None::<&()>)
            .map(Option::unwrap_or_default)
    }

    fn save_command(&self, c: &SavedCommand) -> Result<SavedCommand> {
        let body = serde_json::json!({ "name": c.name.trim(), "command": c.command, "sudo": c.sudo });
        if c.id.is_empty() {
            self.call("POST", "/api/commands/saved", Some(&body))
        } else {
            self.call("PUT", &format!("/api/commands/saved/{}", seg(&c.id)), Some(&body))
        }
    }

    fn delete_command(&self, id: &str) -> Result<()> {
        self.send("DELETE", &format!("/api/commands/saved/{}", seg(id)), None::<&()>)
            .map(|_| ())
    }

    fn export_backup(&self, password: &str) -> Result<Vec<u8>> {
        let body = serde_json::to_vec(&serde_json::json!({ "password": password })).map_err(|e| Error::Invalid(e.to_string()))?;
        self.raw("POST", "/api/backups/export", Some((body, "application/json")), &[])
    }

    fn restore_backup(&self, data: &[u8], password: &str) -> Result<()> {
        let header = crate::backup::base64(password.as_bytes());
        self.raw(
            "POST",
            "/api/backups/restore",
            Some((data.to_vec(), "application/octet-stream")),
            &[("X-Backup-Password", header.as_str())],
        )
        .map(|_| ())
    }

    fn sync_sources(&self) -> Result<Vec<SyncSource>> {
        self.call::<Option<Vec<SyncSource>>>("GET", "/api/sources", None::<&()>)
            .map(Option::unwrap_or_default)
    }

    fn test_sync_source(&self, id: &str) -> Result<Outcome> {
        self.call("POST", &format!("/api/sources/{}/test", seg(id)), None::<&()>)
    }

    fn run_sync_source(&self, id: &str) -> Result<Outcome> {
        self.call("POST", &format!("/api/sources/{}/sync", seg(id)), None::<&()>)
    }

    fn duplicates(&self) -> Result<Vec<DuplicateGroup>> {
        self.call::<Option<Vec<DuplicateGroup>>>("GET", "/api/duplicates", None::<&()>)
            .map(Option::unwrap_or_default)
    }

    fn merge_entries(&self, keep: &str, others: &[String]) -> Result<Entry> {
        let body = serde_json::json!({ "ids": others });
        self.call("POST", &format!("/api/entries/{}/merge", seg(keep)), Some(&body))
    }

    fn import_preview(&self, data: &[u8]) -> Result<ImportPlan> {
        let bytes = self.raw("POST", "/api/import/rdm/preview", Some((data.to_vec(), "application/xml")), &[])?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Server(format!("réponse illisible du serveur : {e}")))
    }

    fn import_apply(&self, data: &[u8], opt: &ImportOptions) -> Result<ImportResult> {
        let path = format!(
            "/api/import/rdm/apply?folder={}&sshDefault={}&rdpDefault={}",
            seg(&opt.root),
            seg(&opt.ssh_default),
            seg(&opt.rdp_default)
        );
        let bytes = self.raw("POST", &path, Some((data.to_vec(), "application/xml")), &[])?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Server(format!("réponse illisible du serveur : {e}")))
    }

    fn recordings(&self) -> Result<Vec<Recording>> {
        self.call::<Option<Vec<Recording>>>("GET", "/api/recordings", None::<&()>)
            .map(Option::unwrap_or_default)
    }

    fn delete_recording(&self, id: &str) -> Result<()> {
        self.send("DELETE", &format!("/api/recordings/{}", seg(id)), None::<&()>)
            .map(|_| ())
    }

    fn web_url(&self) -> Option<String> {
        Some(self.base.clone())
    }

    fn reveal(&self, credential_id: &str, field: SecretField) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct V {
            value: String,
        }
        let body = serde_json::json!({ "field": field.as_str() });
        Ok(self
            .call::<V>("POST", &format!("/api/credentials/{}/reveal", seg(credential_id)), Some(&body))?
            .value)
    }
}
