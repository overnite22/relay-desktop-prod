//! Data source list (`%APPDATA%\Relay\settings.json`) and device tokens
//! (Windows Credential Manager, never written to the settings file).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceCfg {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub kind: SourceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceKind {
    Server { url: String },
    Local { path: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub sources: Vec<SourceCfg>,
    #[serde(default)]
    pub last: Option<String>,
    /// Look for a new version at startup (at most once a day).
    #[serde(default = "yes")]
    pub auto_update: bool,
    #[serde(default)]
    pub last_update_check: u64,
    /// "dark" or "light" (ribbon Affichage → Thème).
    #[serde(default)]
    pub theme: Option<String>,
    /// Terminal font of SSH tabs (Options; an entry may override it).
    #[serde(default)]
    pub term_font: Option<String>,
    /// Terminal font size in logical pixels (0 = 14).
    #[serde(default)]
    pub term_font_size: u32,
    /// Copy the terminal selection as soon as it is made (PuTTY).
    #[serde(default)]
    pub term_copy_on_select: bool,
    /// Default desktop size of RDP tabs: "fit", "fixed" or "fullscreen".
    #[serde(default)]
    pub rdp_resolution: Option<String>,
    /// Fixed RDP size used when `rdp_resolution` is "fixed".
    #[serde(default)]
    pub rdp_width: u32,
    #[serde(default)]
    pub rdp_height: u32,
}

fn yes() -> bool {
    true
}

pub fn dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Relay")
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sources: Vec::new(),
            last: None,
            auto_update: true,
            last_update_check: 0,
            theme: None,
            term_font: None,
            term_font_size: 0,
            term_copy_on_select: false,
            rdp_resolution: None,
            rdp_width: 0,
            rdp_height: 0,
        }
    }
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read(dir().join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let d = dir();
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        let tmp = d.join("settings.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, d.join("settings.json")).map_err(|e| e.to_string())
    }
}

fn token_entry(id: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new("Relay Desktop", &format!("server:{id}"))
}

pub fn save_token(id: &str, token: &str) -> Result<(), String> {
    token_entry(id)
        .and_then(|e| e.set_password(token))
        .map_err(|e| format!("Gestionnaire d'identification : {e}"))
}

pub fn load_token(id: &str) -> Option<String> {
    token_entry(id).and_then(|e| e.get_password()).ok()
}

pub fn delete_token(id: &str) {
    if let Ok(e) = token_entry(id) {
        let _ = e.delete_credential();
    }
}
