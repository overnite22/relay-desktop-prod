//! Per-entry connection settings (the `settings` JSON object of an entry).
//!
//! Keys shared with Relay web keep the web names and meaning (`console`,
//! `disableAudio`, `wallpaper`, `disableCopy`, `disablePaste`, `security`,
//! `ignoreCert`, `fontName`, `fontSize`, `colorScheme`, `scrollback`). Keys
//! only used by the desktop client are ignored by the server (it stores the
//! object as is) : `resolution`, `width`, `height`, `colorDepth`, `multiMon`,
//! `redirectDrives`, `redirectPrinters`, `audioMode`, `gatewayHost`,
//! `gatewayUsage`, `gatewaySameCreds`, `keyboardHook`, `keepalive`, `sshAgent`, `keyFile`, `charset`, `sendLocale`, `theme`, `highlight`.
//! Unknown keys (`drive`, `keyboardLayout`, `sftp`, Proxmox keys…) are kept.

use serde_json::{Map, Value};

type Settings = Map<String, Value>;

fn get_bool(s: &Settings, k: &str) -> Option<bool> {
    s.get(k).and_then(Value::as_bool)
}

fn get_u32(s: &Settings, k: &str) -> Option<u32> {
    s.get(k).and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok())
}

fn get_str<'a>(s: &'a Settings, k: &str) -> &'a str {
    s.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Writes `v` or removes the key when it equals the default.
fn set_bool(s: &mut Settings, k: &str, v: bool, default: bool) {
    if v == default {
        s.remove(k);
    } else {
        s.insert(k.into(), Value::Bool(v));
    }
}

fn set_u32(s: &mut Settings, k: &str, v: u32) {
    if v == 0 {
        s.remove(k);
    } else {
        s.insert(k.into(), Value::from(v));
    }
}

fn set_str(s: &mut Settings, k: &str, v: &str) {
    let v = v.trim();
    if v.is_empty() {
        s.remove(k);
    } else {
        s.insert(k.into(), Value::String(v.into()));
    }
}

/// Desktop size of an RDP session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resolution {
    /// Global choice of the Options window.
    #[default]
    Default,
    /// Follow the tab size (dynamic resolution).
    Fit,
    /// Fixed `width`×`height`, scaled to the tab.
    Fixed,
    /// Full screen on the monitor of the window.
    FullScreen,
}

impl Resolution {
    pub const ALL: [Resolution; 4] = [Resolution::Default, Resolution::Fit, Resolution::Fixed, Resolution::FullScreen];

    pub fn key(self) -> &'static str {
        match self {
            Resolution::Default => "",
            Resolution::Fit => "fit",
            Resolution::Fixed => "fixed",
            Resolution::FullScreen => "fullscreen",
        }
    }

    pub fn from_key(k: &str) -> Resolution {
        Resolution::ALL.into_iter().find(|r| r.key() == k).unwrap_or_default()
    }

    /// Resolves `Default` with the global choice (itself never `Default`).
    pub fn or(self, global: Resolution) -> Resolution {
        match (self, global) {
            (Resolution::Default, Resolution::Default) => Resolution::Fit,
            (Resolution::Default, g) => g,
            (r, _) => r,
        }
    }
}

/// Where the remote sound is played.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Audio {
    #[default]
    Local,
    Remote,
    Off,
}

impl Audio {
    pub const ALL: [Audio; 3] = [Audio::Local, Audio::Remote, Audio::Off];

    /// `AudioRedirectionMode` of the RDP control.
    pub fn mode(self) -> i32 {
        match self {
            Audio::Local => 0,
            Audio::Remote => 1,
            Audio::Off => 2,
        }
    }
}

/// Where Windows key combinations (Alt+Tab, Windows key…) go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyHook {
    /// To the session only in full screen (mstsc default).
    #[default]
    FullScreen,
    Remote,
    Local,
}

impl KeyHook {
    pub const ALL: [KeyHook; 3] = [KeyHook::FullScreen, KeyHook::Remote, KeyHook::Local];

    fn key(self) -> &'static str {
        match self {
            KeyHook::FullScreen => "",
            KeyHook::Remote => "remote",
            KeyHook::Local => "local",
        }
    }

    /// `KeyboardHookMode` of the RDP control.
    pub fn mode(self) -> i32 {
        match self {
            KeyHook::Local => 0,
            KeyHook::Remote => 1,
            KeyHook::FullScreen => 2,
        }
    }
}

/// Use of an RD Gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Gateway {
    #[default]
    None,
    /// Always through the gateway.
    Always,
    /// Gateway, except for local addresses.
    Auto,
}

impl Gateway {
    pub const ALL: [Gateway; 3] = [Gateway::None, Gateway::Always, Gateway::Auto];

    fn key(self) -> &'static str {
        match self {
            Gateway::None => "",
            Gateway::Always => "always",
            Gateway::Auto => "auto",
        }
    }

    /// `GatewayUsageMethod` of the RDP control.
    pub fn usage_method(self) -> i32 {
        match self {
            Gateway::None => 0,
            Gateway::Always => 1,
            Gateway::Auto => 2,
        }
    }
}

/// RDP security layer (web `security`).
pub const RDP_SECURITY: [&str; 4] = ["any", "nla", "tls", "rdp"];
/// Colour depths offered (0 = default, 32 bits).
pub const COLOR_DEPTHS: [u32; 5] = [0, 15, 16, 24, 32];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RdpOptions {
    pub resolution: Resolution,
    pub width: u32,
    pub height: u32,
    pub color_depth: u32,
    pub multi_mon: bool,
    pub disable_copy: bool,
    pub disable_paste: bool,
    pub drives: bool,
    pub printers: bool,
    pub audio: Audio,
    pub keyboard: KeyHook,
    pub console: bool,
    /// One of [`RDP_SECURITY`] (empty = any).
    pub security: String,
    /// Web semantics: absent = ignore. Desktop: only an explicit `true`
    /// skips the certificate warning of the control.
    pub ignore_cert: Option<bool>,
    pub wallpaper: bool,
    pub gateway: Gateway,
    pub gateway_host: String,
    pub gateway_same_creds: bool,
}

impl RdpOptions {
    pub fn from_settings(s: &Settings) -> Self {
        let width = get_u32(s, "width").unwrap_or(0);
        let height = get_u32(s, "height").unwrap_or(0);
        let gateway = match get_str(s, "gatewayUsage") {
            "always" => Gateway::Always,
            "auto" => Gateway::Auto,
            _ => Gateway::None,
        };
        RdpOptions {
            resolution: Resolution::from_key(get_str(s, "resolution")),
            width,
            height,
            color_depth: get_u32(s, "colorDepth").filter(|d| COLOR_DEPTHS.contains(d)).unwrap_or(0),
            multi_mon: get_bool(s, "multiMon").unwrap_or(false),
            disable_copy: get_bool(s, "disableCopy").unwrap_or(false),
            disable_paste: get_bool(s, "disablePaste").unwrap_or(false),
            drives: get_bool(s, "redirectDrives").unwrap_or(false),
            printers: get_bool(s, "redirectPrinters").unwrap_or(false),
            audio: if get_bool(s, "disableAudio").unwrap_or(false) {
                Audio::Off
            } else if get_str(s, "audioMode") == "remote" {
                Audio::Remote
            } else {
                Audio::Local
            },
            keyboard: KeyHook::ALL
                .into_iter()
                .find(|k| k.key() == get_str(s, "keyboardHook"))
                .unwrap_or_default(),
            console: get_bool(s, "console").unwrap_or(false),
            security: Some(get_str(s, "security"))
                .filter(|v| RDP_SECURITY.contains(v))
                .unwrap_or("any")
                .into(),
            ignore_cert: get_bool(s, "ignoreCert"),
            wallpaper: get_bool(s, "wallpaper").unwrap_or(false),
            gateway,
            gateway_host: get_str(s, "gatewayHost").into(),
            gateway_same_creds: get_bool(s, "gatewaySameCreds").unwrap_or(true),
        }
    }

    /// Writes these options into `s`, keeping the keys it does not manage.
    pub fn apply(&self, s: &mut Settings) {
        set_str(s, "resolution", self.resolution.key());
        let fixed = self.resolution == Resolution::Fixed;
        set_u32(s, "width", if fixed { self.width } else { 0 });
        set_u32(s, "height", if fixed { self.height } else { 0 });
        set_u32(s, "colorDepth", self.color_depth);
        set_bool(s, "multiMon", self.multi_mon, false);
        set_bool(s, "disableCopy", self.disable_copy, false);
        set_bool(s, "disablePaste", self.disable_paste, false);
        set_bool(s, "redirectDrives", self.drives, false);
        set_bool(s, "redirectPrinters", self.printers, false);
        set_bool(s, "disableAudio", self.audio == Audio::Off, false);
        set_str(s, "audioMode", if self.audio == Audio::Remote { "remote" } else { "" });
        set_str(s, "keyboardHook", self.keyboard.key());
        set_bool(s, "console", self.console, false);
        set_str(s, "security", if self.security == "any" { "" } else { &self.security });
        match self.ignore_cert {
            Some(v) => {
                s.insert("ignoreCert".into(), Value::Bool(v));
            }
            None => {
                s.remove("ignoreCert");
            }
        }
        set_bool(s, "wallpaper", self.wallpaper, false);
        set_str(s, "gatewayUsage", self.gateway.key());
        let gw = self.gateway != Gateway::None;
        set_str(s, "gatewayHost", if gw { &self.gateway_host } else { "" });
        set_bool(s, "gatewaySameCreds", !gw || self.gateway_same_creds, true);
    }

    /// Checks the values before saving.
    pub fn validate(&self) -> Result<(), String> {
        let ok = |v: u32| (200..=8192).contains(&v);
        if self.resolution == Resolution::Fixed && !(ok(self.width) && ok(self.height)) {
            return Err("Résolution fixe : largeur et hauteur entre 200 et 8192 pixels.".into());
        }
        if self.gateway != Gateway::None && self.gateway_host.trim().is_empty() {
            return Err("Passerelle RD : nom d'hôte obligatoire.".into());
        }
        Ok(())
    }

    /// Colour depth sent to the control.
    pub fn depth(&self) -> u32 {
        if self.color_depth == 0 {
            32
        } else {
            self.color_depth
        }
    }

    /// Whether the clipboard channel is opened (the control has no
    /// one-way mode: disabling copy or paste closes it).
    pub fn clipboard(&self) -> bool {
        !self.disable_copy && !self.disable_paste
    }
}

/// Terminal colour schemes (same keys as Relay web).
pub const SSH_SCHEMES: [(&str, &str); 4] = [
    ("gray-black", "Gris sur noir"),
    ("green-black", "Vert sur noir"),
    ("white-black", "Blanc sur noir"),
    ("black-white", "Noir sur blanc"),
];

/// Terminal fonts offered (Windows font files).
pub const TERM_FONTS: [(&str, &str); 4] = [
    ("Consolas", "consola.ttf"),
    ("Cascadia Mono", "CascadiaMono.ttf"),
    ("Lucida Console", "lucon.ttf"),
    ("Courier New", "cour.ttf"),
];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SshOptions {
    /// Empty = global choice.
    pub font_name: String,
    /// 0 = global choice; else 6 to 48.
    pub font_size: u32,
    /// Empty = default (gray-black).
    pub color_scheme: String,
    /// 0 = default (5000 lines).
    pub scrollback: u32,
    /// Seconds between keepalives; `None` = 30 s, `Some(0)` = off.
    pub keepalive: Option<u32>,
    /// Try the keys of the SSH agent (Windows OpenSSH agent, then Pageant).
    pub agent: bool,
    /// Private key file on this computer (used when the credential has none).
    pub key_file: String,
    /// Remote character set (`charset`, desktop only).
    pub charset: crate::charset::Charset,
    /// Send LANG and LC_ALL with the local language and that character set.
    pub send_locale: bool,
    /// Terminal colour theme (`theme`); empty = Options.
    pub theme: String,
    /// Syntax colouring (`highlight`): None = Options, else on / off.
    pub highlight: Option<bool>,
}

impl SshOptions {
    pub fn from_settings(s: &Settings) -> Self {
        SshOptions {
            font_name: get_str(s, "fontName").into(),
            font_size: get_u32(s, "fontSize").filter(|v| (6..=48).contains(v)).unwrap_or(0),
            color_scheme: Some(get_str(s, "colorScheme"))
                .filter(|v| SSH_SCHEMES.iter().any(|(k, _)| k == v))
                .unwrap_or("")
                .into(),
            scrollback: get_u32(s, "scrollback").unwrap_or(0).min(100_000),
            keepalive: get_u32(s, "keepalive").map(|v| v.min(3600)),
            agent: get_bool(s, "sshAgent").unwrap_or(false),
            key_file: get_str(s, "keyFile").into(),
            charset: crate::charset::Charset::from_key(get_str(s, "charset")),
            send_locale: get_bool(s, "sendLocale").unwrap_or(false),
            theme: Some(get_str(s, "theme"))
                .filter(|v| crate::termtheme::THEMES.iter().any(|t| t.key == *v))
                .unwrap_or("")
                .into(),
            highlight: get_bool(s, "highlight"),
        }
    }

    pub fn apply(&self, s: &mut Settings) {
        set_str(s, "fontName", &self.font_name);
        set_u32(s, "fontSize", self.font_size);
        set_str(
            s,
            "colorScheme",
            if self.color_scheme == "gray-black" {
                ""
            } else {
                &self.color_scheme
            },
        );
        set_u32(s, "scrollback", self.scrollback);
        match self.keepalive {
            Some(v) if v != 30 => {
                s.insert("keepalive".into(), Value::from(v));
            }
            _ => {
                s.remove("keepalive");
            }
        }
        set_bool(s, "sshAgent", self.agent, false);
        set_str(s, "keyFile", &self.key_file);
        set_str(s, "charset", self.charset.key());
        set_bool(s, "sendLocale", self.send_locale, false);
        set_str(s, "theme", &self.theme);
        match self.highlight {
            Some(v) => {
                s.insert("highlight".into(), Value::from(v));
            }
            None => {
                s.remove("highlight");
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.font_size != 0 && !(6..=48).contains(&self.font_size) {
            return Err("Taille de police : 6 à 48.".into());
        }
        if self.keepalive.is_some_and(|v| v > 3600) {
            return Err("Keepalive : 0 à 3600 secondes.".into());
        }
        Ok(())
    }

    pub fn keepalive_secs(&self) -> u32 {
        self.keepalive.unwrap_or(30)
    }

    /// Default foreground / background of the colour scheme.
    pub fn colors(&self) -> ([u8; 3], [u8; 3]) {
        match self.color_scheme.as_str() {
            "green-black" => ([0x13, 0xc6, 0x0e], [0x0c, 0x0c, 0x0c]),
            "white-black" => ([0xff, 0xff, 0xff], [0x0c, 0x0c, 0x0c]),
            "black-white" => ([0x0c, 0x0c, 0x0c], [0xff, 0xff, 0xff]),
            _ => ([0xcc, 0xcc, 0xcc], [0x0c, 0x0c, 0x0c]),
        }
    }

    /// Windows font file of `name` (Consolas when unknown).
    pub fn font_file(name: &str) -> &'static str {
        TERM_FONTS
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name.trim()))
            .map(|(_, f)| *f)
            .unwrap_or("consola.ttf")
    }
}

/// Readable reason of an RDP disconnection (`discReason` of the
/// `OnDisconnected` event, `ExtendedDisconnectReason` of the control).
/// `None`: normal end of session, or a code this table does not know (the
/// caller then asks the control's `GetErrorDescription`).
pub fn rdp_disconnect_text(disc: i32, ext: i32) -> Option<String> {
    let ext_text = match ext {
        3 => Some("délai d'inactivité du serveur dépassé"),
        4 => Some("délai d'ouverture de session dépassé"),
        5 => Some("session reprise depuis un autre poste"),
        6 => Some("mémoire insuffisante sur le serveur"),
        7 => Some("connexion refusée par le serveur"),
        8 => Some("connexion refusée par le serveur (FIPS)"),
        9 => Some("droits insuffisants sur le serveur"),
        10 => Some("le serveur exige de ressaisir les identifiants"),
        11 => Some("déconnexion demandée par l'utilisateur distant"),
        12 => Some("fermeture de session par l'utilisateur"),
        256..=271 => Some("erreur de licence Bureau à distance"),
        _ => None,
    };
    let disc_text = match disc {
        1..=3 => None,
        260 | 520 | 1288 | 1540 => Some("nom d'hôte introuvable (DNS)"),
        264 | 1796 => Some("délai de connexion dépassé"),
        516 => Some("connexion impossible : hôte injoignable ou port fermé"),
        776 | 2052 => Some("adresse IP invalide"),
        1030 | 1542 | 2310 | 2566 => Some("erreur de sécurité de la connexion"),
        1798 => Some("certificat du serveur illisible"),
        2055 => Some("identifiants refusés"),
        2056 | 2312 => Some("négociation de licence impossible"),
        2308 => Some("connexion interrompue par le réseau ou le serveur"),
        2567 => Some("compte utilisateur inconnu"),
        2823 => Some("compte utilisateur désactivé"),
        2825 => Some("le serveur exige l'authentification réseau (NLA)"),
        3079 => Some("restrictions du compte utilisateur"),
        3335 => Some("compte utilisateur verrouillé"),
        3591 => Some("compte utilisateur expiré"),
        3847 => Some("mot de passe expiré"),
        4615 => Some("le mot de passe doit être changé avant la première connexion"),
        _ => None,
    };
    match (disc_text, ext_text) {
        (Some(d), _) => Some(format!("Déconnecté : {d}")),
        (None, Some(e)) if !(1..=3).contains(&disc) || !matches!(ext, 11 | 12) => Some(format!("Déconnecté : {e}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Settings {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn rdp_reads_web_keys_and_defaults() {
        let s = map(json!({"console": true, "disableAudio": true, "security": "nla", "drive": false, "keyboardLayout": "fr-fr-azerty"}));
        let o = RdpOptions::from_settings(&s);
        assert!(o.console);
        assert_eq!(o.audio, Audio::Off);
        assert_eq!(o.security, "nla");
        assert_eq!(o.resolution, Resolution::Default);
        assert_eq!(o.depth(), 32);
        assert!(o.clipboard());
        assert!(o.gateway_same_creds);
        let d = RdpOptions::from_settings(&Map::new());
        assert_eq!(d.security, "any");
        assert_eq!(d.ignore_cert, None);
    }

    #[test]
    fn rdp_roundtrip_keeps_foreign_keys() {
        let mut s = map(json!({"drive": false, "keyboardLayout": "de-de-qwertz", "console": true}));
        let mut o = RdpOptions::from_settings(&s);
        o.console = false;
        o.resolution = Resolution::Fixed;
        o.width = 1600;
        o.height = 900;
        o.audio = Audio::Remote;
        o.gateway = Gateway::Auto;
        o.gateway_host = " gw.lan ".into();
        o.gateway_same_creds = false;
        o.disable_copy = true;
        o.keyboard = KeyHook::Remote;
        o.apply(&mut s);
        assert_eq!(s["keyboardHook"], json!("remote"));
        assert_eq!(KeyHook::Remote.mode(), 1);
        assert_eq!(KeyHook::default().mode(), 2);
        assert_eq!(s["drive"], json!(false));
        assert_eq!(s["keyboardLayout"], json!("de-de-qwertz"));
        assert!(!s.contains_key("console"));
        assert_eq!(s["resolution"], json!("fixed"));
        assert_eq!(s["audioMode"], json!("remote"));
        assert_eq!(s["gatewayHost"], json!("gw.lan"));
        let back = RdpOptions::from_settings(&s);
        assert_eq!(back.width, 1600);
        assert_eq!(back.gateway, Gateway::Auto);
        assert_eq!(back.keyboard, KeyHook::Remote);
        assert!(!back.gateway_same_creds);
        assert!(!back.clipboard());
        assert_eq!(back.gateway_host, "gw.lan");
        // Back to defaults: desktop keys disappear.
        RdpOptions::from_settings(&Map::new()).apply(&mut s);
        let keys: Vec<_> = s.keys().cloned().collect();
        assert_eq!(keys, vec!["drive".to_string(), "keyboardLayout".to_string()]);
    }

    #[test]
    fn rdp_validation_and_resolution() {
        let mut o = RdpOptions {
            resolution: Resolution::Fixed,
            width: 100,
            height: 900,
            ..Default::default()
        };
        assert!(o.validate().is_err());
        o.width = 1280;
        assert!(o.validate().is_ok());
        o.gateway = Gateway::Always;
        assert!(o.validate().is_err());
        assert_eq!(Resolution::Default.or(Resolution::Default), Resolution::Fit);
        assert_eq!(Resolution::Default.or(Resolution::FullScreen), Resolution::FullScreen);
        assert_eq!(Resolution::Fixed.or(Resolution::FullScreen), Resolution::Fixed);
    }

    #[test]
    fn ssh_roundtrip() {
        let mut s = map(json!({"fontSize": 99, "colorScheme": "nope", "sftp": false, "keepalive": 0}));
        let o = SshOptions::from_settings(&s);
        assert_eq!(o.font_size, 0);
        assert_eq!(o.color_scheme, "");
        assert_eq!(o.keepalive, Some(0));
        assert_eq!(o.keepalive_secs(), 0);
        let o = SshOptions {
            font_name: "Cascadia Mono".into(),
            font_size: 16,
            color_scheme: "black-white".into(),
            agent: true,
            key_file: r"C:\Users\me\.ssh\id_ed25519".into(),
            keepalive: Some(30),
            ..Default::default()
        };
        o.apply(&mut s);
        assert_eq!(s["sftp"], json!(false));
        assert!(!s.contains_key("keepalive"));
        assert_eq!(
            SshOptions::from_settings(&s),
            SshOptions {
                keepalive: None,
                ..o.clone()
            }
        );
        assert_eq!(SshOptions::font_file("cascadia mono"), "CascadiaMono.ttf");
        assert_eq!(SshOptions::font_file("monospace"), "consola.ttf");
        assert_eq!(o.colors().1, [0xff, 0xff, 0xff]);
    }

    #[test]
    fn rdp_disconnect_texts() {
        assert_eq!(rdp_disconnect_text(1, 0), None);
        assert_eq!(rdp_disconnect_text(2, 12), None);
        assert_eq!(
            rdp_disconnect_text(3, 3).as_deref(),
            Some("Déconnecté : délai d'inactivité du serveur dépassé")
        );
        assert_eq!(
            rdp_disconnect_text(516, 0).as_deref(),
            Some("Déconnecté : connexion impossible : hôte injoignable ou port fermé")
        );
        assert_eq!(rdp_disconnect_text(2055, 0).as_deref(), Some("Déconnecté : identifiants refusés"));
        assert_eq!(rdp_disconnect_text(99999, 0), None);
    }
}
