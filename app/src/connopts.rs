//! "Connexion RDP" / "Terminal SSH" / "Paramètres VNC" section of the entry editor: per-entry
//! settings stored in the `settings` object of the entry (see core::conn).

use super::*;
use relay_desktop_core::charset::{Charset, CHARSETS};
use relay_desktop_core::conn::{Audio, Gateway, KeyHook, VncOptions, COLOR_DEPTHS, RDP_SECURITY, SSH_SCHEMES, TERM_FONTS, VNC_DEPTHS};
use relay_desktop_core::highlight::{self, Rule};
use relay_desktop_core::termtheme;

fn num(v: u32) -> SharedString {
    if v == 0 {
        SharedString::new()
    } else {
        v.to_string().into()
    }
}

fn parse(s: &str, what: &str) -> Result<u32, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(0);
    }
    s.parse::<u32>().map_err(|_| format!("{what} : nombre entier attendu."))
}

/// Empty width / height of a fixed resolution take the values shown as placeholders.
fn fixed_default(res: Resolution, w: u32, h: u32) -> (u32, u32) {
    if res == Resolution::Fixed {
        (if w == 0 { 1920 } else { w }, if h == 0 { 1080 } else { h })
    } else {
        (w, h)
    }
}

/// Fills the section from the entry settings.
pub fn load(ui: &AppWindow, e: &Entry) {
    ui.set_e_section(0);
    let r = RdpOptions::from_settings(&e.settings);
    ui.set_er_res(Resolution::ALL.iter().position(|x| *x == r.resolution).unwrap_or(0) as i32);
    ui.set_er_width(num(r.width));
    ui.set_er_height(num(r.height));
    ui.set_er_depth(COLOR_DEPTHS.iter().position(|d| *d == r.color_depth).unwrap_or(0) as i32);
    ui.set_er_multimon(r.multi_mon);
    ui.set_er_copy(!r.disable_copy);
    ui.set_er_paste(!r.disable_paste);
    ui.set_er_drives(r.drives);
    ui.set_er_printers(r.printers);
    ui.set_er_audio(Audio::ALL.iter().position(|a| *a == r.audio).unwrap_or(0) as i32);
    ui.set_er_keys(KeyHook::ALL.iter().position(|k| *k == r.keyboard).unwrap_or(0) as i32);
    ui.set_er_console(r.console);
    ui.set_er_security(RDP_SECURITY.iter().position(|s| *s == r.security).unwrap_or(0) as i32);
    ui.set_er_ignore_cert(r.ignore_cert == Some(true));
    ui.set_er_wallpaper(r.wallpaper);
    ui.set_er_gw(Gateway::ALL.iter().position(|g| *g == r.gateway).unwrap_or(0) as i32);
    ui.set_er_gw_host(r.gateway_host.into());
    ui.set_er_gw_same(r.gateway_same_creds);

    let s = SshOptions::from_settings(&e.settings);
    ui.set_es_font(
        TERM_FONTS
            .iter()
            .position(|(n, _)| n.eq_ignore_ascii_case(&s.font_name))
            .map(|i| i as i32 + 1)
            .unwrap_or(0),
    );
    ui.set_es_size(num(s.font_size));
    ui.set_es_scheme(SSH_SCHEMES.iter().position(|(k, _)| *k == s.color_scheme).unwrap_or(0) as i32);
    ui.set_es_scrollback(num(s.scrollback));
    ui.set_es_keepalive(s.keepalive.map(|v| SharedString::from(v.to_string())).unwrap_or_default());
    ui.set_es_agent(s.agent);
    ui.set_es_keyfile(s.key_file.into());
    ui.set_es_charset(s.charset.index() as i32);
    ui.set_es_send_locale(s.send_locale);
    ui.set_es_theme(if s.theme.is_empty() {
        0
    } else {
        termtheme::index_of(&s.theme) as i32 + 1
    });
    ui.set_es_sftp(!s.disable_sftp);
    ui.set_es_sftp_root(s.sftp_root.into());
    let v = VncOptions::from_settings(&e.settings);
    ui.set_ev_depth(VNC_DEPTHS.iter().position(|d| *d == v.color_depth).unwrap_or(0) as i32);
    ui.set_ev_read_only(v.read_only);
    ui.set_ev_remote_cursor(v.remote_cursor);
    ui.set_es_highlight(match s.highlight {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    });
}

/// Writes the section into `e.settings` (only the options of its kind).
pub fn store(ui: &AppWindow, e: &mut Entry) -> Result<(), String> {
    match e.kind {
        Kind::Rdp => {
            let before = RdpOptions::from_settings(&e.settings);
            let pick = |i: i32| i.max(0) as usize;
            let resolution = Resolution::ALL.get(pick(ui.get_er_res())).copied().unwrap_or_default();
            let (width, height) = fixed_default(
                resolution,
                parse(&ui.get_er_width(), "Largeur")?,
                parse(&ui.get_er_height(), "Hauteur")?,
            );
            let r = RdpOptions {
                resolution,
                width,
                height,
                color_depth: COLOR_DEPTHS.get(pick(ui.get_er_depth())).copied().unwrap_or(0),
                multi_mon: ui.get_er_multimon(),
                disable_copy: !ui.get_er_copy(),
                disable_paste: !ui.get_er_paste(),
                drives: ui.get_er_drives(),
                printers: ui.get_er_printers(),
                audio: Audio::ALL.get(pick(ui.get_er_audio())).copied().unwrap_or_default(),
                keyboard: KeyHook::ALL.get(pick(ui.get_er_keys())).copied().unwrap_or_default(),
                console: ui.get_er_console(),
                security: RDP_SECURITY.get(pick(ui.get_er_security())).copied().unwrap_or("any").into(),
                // Unchecked keeps the web default (absent) unless the entry had a value.
                ignore_cert: if ui.get_er_ignore_cert() {
                    Some(true)
                } else {
                    before.ignore_cert.map(|_| false)
                },
                wallpaper: ui.get_er_wallpaper(),
                gateway: Gateway::ALL.get(pick(ui.get_er_gw())).copied().unwrap_or_default(),
                gateway_host: ui.get_er_gw_host().trim().into(),
                gateway_same_creds: ui.get_er_gw_same(),
            };
            r.validate()?;
            r.apply(&mut e.settings);
        }
        Kind::Ssh => {
            let keepalive = ui.get_es_keepalive();
            let s = SshOptions {
                font_name: match ui.get_es_font() {
                    i if i > 0 => TERM_FONTS.get(i as usize - 1).map(|(n, _)| n.to_string()).unwrap_or_default(),
                    _ => String::new(),
                },
                font_size: parse(&ui.get_es_size(), "Taille de police")?,
                color_scheme: SSH_SCHEMES
                    .get(ui.get_es_scheme().max(0) as usize)
                    .map(|(k, _)| k.to_string())
                    .unwrap_or_default(),
                scrollback: parse(&ui.get_es_scrollback(), "Historique")?.min(100_000),
                keepalive: if keepalive.trim().is_empty() {
                    None
                } else {
                    Some(parse(&keepalive, "Keepalive")?)
                },
                agent: ui.get_es_agent(),
                key_file: ui.get_es_keyfile().trim().into(),
                charset: CHARSETS.get(ui.get_es_charset().max(0) as usize).map_or(Charset::Utf8, |c| c.0),
                send_locale: ui.get_es_send_locale(),
                theme: match ui.get_es_theme() {
                    i if i > 0 => termtheme::THEMES.get(i as usize - 1).map(|t| t.key.to_string()).unwrap_or_default(),
                    _ => String::new(),
                },
                highlight: match ui.get_es_highlight() {
                    1 => Some(true),
                    2 => Some(false),
                    _ => None,
                },
                disable_sftp: !ui.get_es_sftp(),
                sftp_root: ui.get_es_sftp_root().trim().into(),
            };
            s.validate()?;
            s.apply(&mut e.settings);
        }
        Kind::Vnc => VncOptions {
            color_depth: VNC_DEPTHS.get(ui.get_ev_depth().max(0) as usize).copied().unwrap_or(0),
            read_only: ui.get_ev_read_only(),
            remote_cursor: ui.get_ev_remote_cursor(),
        }
        .apply(&mut e.settings),
        _ => {}
    }
    Ok(())
}

/// "Parcourir…" of the key file field.
pub fn pick_key_file(ui: &AppWindow) {
    #[cfg(windows)]
    {
        let mut dlg = rfd::FileDialog::new().set_title("Clé privée SSH");
        if let Some(home) = std::env::var_os("USERPROFILE") {
            dlg = dlg.set_directory(PathBuf::from(home).join(".ssh"));
        }
        if let Some(path) = dlg.pick_file() {
            ui.set_es_keyfile(path.display().to_string().into());
        }
    }
    #[cfg(not(windows))]
    let _ = ui;
}

const GLOBAL_RES: [Resolution; 3] = [Resolution::Fit, Resolution::Fixed, Resolution::FullScreen];

/// Options window (Aide → Options).
pub fn open_options(ui: &AppWindow) {
    with(|s| {
        let o = &s.settings;
        ui.set_o_theme(i32::from(o.theme.as_deref() == Some("light")));
        ui.set_o_auto_update(o.auto_update);
        let font = o.term_font.as_deref().unwrap_or("Consolas");
        ui.set_o_font(TERM_FONTS.iter().position(|(n, _)| n.eq_ignore_ascii_case(font)).unwrap_or(0) as i32);
        ui.set_o_font_size(num(o.term_font_size));
        ui.set_o_copy_select(o.term_copy_on_select);
        ui.set_o_term_theme(termtheme::index_of(&o.term_theme) as i32);
        ui.set_o_bold_bright(o.term_bold_bright);
        ui.set_o_highlight(o.term_highlight);
        let res = Resolution::from_key(o.rdp_resolution.as_deref().unwrap_or("")).or(Resolution::Fit);
        ui.set_o_rdp_res(GLOBAL_RES.iter().position(|r| *r == res).unwrap_or(0) as i32);
        ui.set_o_rdp_width(num(o.rdp_width));
        ui.set_o_rdp_height(num(o.rdp_height));
    });
    ui.set_dialog_error("".into());
    ui.set_dialog("options".into());
}

pub fn submit_options(ui: &AppWindow) {
    let checked = (|| -> Result<(u32, Resolution, u32, u32), String> {
        let size = parse(&ui.get_o_font_size(), "Taille de police")?;
        if size != 0 && !(6..=48).contains(&size) {
            return Err("Taille de police : 6 à 48.".into());
        }
        let res = GLOBAL_RES
            .get(ui.get_o_rdp_res().max(0) as usize)
            .copied()
            .unwrap_or(Resolution::Fit);
        let (w, h) = (parse(&ui.get_o_rdp_width(), "Largeur")?, parse(&ui.get_o_rdp_height(), "Hauteur")?);
        let (w, h) = fixed_default(res, w, h);
        if res == Resolution::Fixed && !((200..=8192).contains(&w) && (200..=8192).contains(&h)) {
            return Err("Résolution fixe : largeur et hauteur entre 200 et 8192 pixels.".into());
        }
        Ok((size, res, w, h))
    })();
    let (size, res, w, h) = match checked {
        Ok(v) => v,
        Err(e) => return ui.set_dialog_error(e.into()),
    };
    let light = ui.get_o_theme() == 1;
    let saved = with(|s| {
        let o = &mut s.settings;
        o.theme = Some(if light { "light" } else { "dark" }.into());
        o.auto_update = ui.get_o_auto_update();
        o.term_font = TERM_FONTS.get(ui.get_o_font().max(0) as usize).map(|(n, _)| n.to_string());
        o.term_font_size = size;
        o.term_copy_on_select = ui.get_o_copy_select();
        o.term_theme = termtheme::THEMES
            .get(ui.get_o_term_theme().max(0) as usize)
            .map(|t| t.key.to_string())
            .unwrap_or_default();
        o.term_bold_bright = ui.get_o_bold_bright();
        o.term_highlight = ui.get_o_highlight();
        o.rdp_resolution = Some(res.key().into());
        (o.rdp_width, o.rdp_height) = if res == Resolution::Fixed { (w, h) } else { (0, 0) };
        o.save()
    });
    if let Err(e) = saved {
        return ui.set_dialog_error(format!("Enregistrement des options : {e}").into());
    }
    ui.set_dark_mode(!light);
    ui.set_upd_auto(ui.get_o_auto_update());
    ui.set_dialog("".into());
    ui.set_status("Options enregistrées (police et résolution : prochaines sessions).".into());
}

/// Lists of the theme, character set and colour pickers.
pub fn init_lists(ui: &AppWindow) {
    let names = |v: Vec<String>| ModelRc::new(VecModel::from(v.into_iter().map(SharedString::from).collect::<Vec<_>>()));
    let themes: Vec<String> = termtheme::THEMES.iter().map(|t| t.name.to_string()).collect();
    let mut entry = vec!["Selon les Options".to_string()];
    entry.extend(themes.iter().cloned());
    ui.set_term_theme_names(names(themes));
    ui.set_es_theme_names(names(entry));
    ui.set_charset_names(names(CHARSETS.iter().map(|c| c.2.to_string()).collect()));
    ui.set_hl_color_names(names(highlight::COLORS.iter().map(|c| c.0.to_string()).collect()));
}

thread_local! {
    /// Rules being edited in the "Règles de coloration" dialog.
    static RULES: RefCell<Vec<Rule>> = const { RefCell::new(Vec::new()) };
}

fn color_index(hex: &str) -> i32 {
    highlight::COLORS.iter().position(|c| c.1.eq_ignore_ascii_case(hex)).unwrap_or(0) as i32
}

fn color_name(hex: &str) -> &'static str {
    highlight::COLORS.iter().find(|c| c.1.eq_ignore_ascii_case(hex)).map_or("", |c| c.0)
}

fn show_rules(ui: &AppWindow) {
    let rows: Vec<ListRow> = RULES.with(|r| {
        r.borrow()
            .iter()
            .map(|r| {
                let mut colors = Vec::new();
                if !r.fg.is_empty() {
                    colors.push(color_name(&r.fg).to_string());
                }
                if !r.bg.is_empty() {
                    colors.push(format!("fond {}", color_name(&r.bg).to_lowercase()));
                }
                if r.bold {
                    colors.push("gras".into());
                }
                ListRow {
                    name: r.pattern.clone().into(),
                    detail: if r.regex { "Expression" } else { "Mot" }.into(),
                    extra: colors.join(", ").into(),
                    status: if r.enabled { 1 } else { 0 },
                }
            })
            .collect()
    });
    ui.set_hl_rows(ModelRc::new(VecModel::from(rows)));
}

fn load_rule(ui: &AppWindow) {
    let i = ui.get_hl_sel();
    let Some(r) = RULES.with(|r| r.borrow().get(usize::try_from(i).ok()?).cloned()) else {
        ui.set_hl_sel(-1);
        return;
    };
    ui.set_hl_pattern(r.pattern.into());
    ui.set_hl_regex(r.regex);
    ui.set_hl_case(r.case_sensitive);
    ui.set_hl_fg(color_index(&r.fg));
    ui.set_hl_bg(color_index(&r.bg));
    ui.set_hl_bold(r.bold);
    ui.set_hl_enabled(r.enabled);
}

/// Options → "Règles…": edits a copy of the rules (Options fields kept).
pub fn open_rules(ui: &AppWindow) {
    let rules = with(|s| s.settings.term_rules.clone()).unwrap_or_else(highlight::default_rules);
    RULES.with(|r| *r.borrow_mut() = rules);
    ui.set_hl_sel(-1);
    show_rules(ui);
    ui.set_dialog_error("".into());
    ui.set_dialog("hl-rules".into());
}

pub fn rules_act(ui: &AppWindow, action: &str) {
    let sel = ui.get_hl_sel();
    let idx = usize::try_from(sel).ok();
    match action {
        "select" => return load_rule(ui),
        "cancel" => {
            ui.set_dialog_error("".into());
            ui.set_dialog("options".into());
            return;
        }
        "apply" => {
            let color = |i: i32| highlight::COLORS.get(i.max(0) as usize).map_or(String::new(), |c| c.1.to_string());
            let rule = Rule {
                pattern: ui.get_hl_pattern().to_string(),
                regex: ui.get_hl_regex(),
                case_sensitive: ui.get_hl_case(),
                fg: color(ui.get_hl_fg()),
                bg: color(ui.get_hl_bg()),
                bold: ui.get_hl_bold(),
                enabled: ui.get_hl_enabled(),
            };
            let (_, errors) = highlight::Highlighter::new(std::slice::from_ref(&rule));
            ui.set_dialog_error(errors.join(" ; ").into());
            RULES.with(|r| {
                let mut r = r.borrow_mut();
                if let Some(slot) = idx.and_then(|i| r.get_mut(i)) {
                    *slot = rule;
                }
            });
        }
        "add" => {
            let n = RULES.with(|r| {
                let mut r = r.borrow_mut();
                r.push(Rule {
                    pattern: "motif".into(),
                    fg: highlight::COLORS[1].1.into(),
                    ..Default::default()
                });
                r.len() - 1
            });
            ui.set_hl_sel(n as i32);
            load_rule(ui);
        }
        "delete" => {
            RULES.with(|r| {
                if let Some(i) = idx.filter(|i| *i < r.borrow().len()) {
                    r.borrow_mut().remove(i);
                }
            });
            ui.set_hl_sel(-1);
        }
        "up" | "down" => {
            let moved = RULES.with(|r| {
                let mut r = r.borrow_mut();
                let i = idx?;
                let j = if action == "up" { i.checked_sub(1)? } else { i + 1 };
                (j < r.len()).then(|| r.swap(i, j)).map(|_| j)
            });
            if let Some(j) = moved {
                ui.set_hl_sel(j as i32);
            }
        }
        "defaults" => {
            RULES.with(|r| *r.borrow_mut() = highlight::default_rules());
            ui.set_hl_sel(-1);
        }
        _ => {}
    }
    show_rules(ui);
}

/// "Enregistrer" of the rules: refused while a rule does not compile.
pub fn submit_rules(ui: &AppWindow) {
    let rules = RULES.with(|r| r.borrow().clone());
    let (_, errors) = highlight::Highlighter::new(&rules);
    if !errors.is_empty() {
        return ui.set_dialog_error(format!("Règle invalide : {}", errors.join(" ; ")).into());
    }
    let saved = with(|s| {
        s.settings.term_rules = Some(rules);
        s.settings.save()
    });
    if let Err(e) = saved {
        return ui.set_dialog_error(format!("Enregistrement des règles : {e}").into());
    }
    ui.set_dialog_error("".into());
    ui.set_dialog("options".into());
    ui.set_status("Règles enregistrées : elles s'appliquent aux prochaines sessions SSH.".into());
}
