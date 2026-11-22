//! "Connexion RDP" / "Terminal SSH" section of the entry editor: per-entry
//! settings stored in the `settings` object of the entry (see core::conn).

use super::*;
use relay_desktop_core::conn::{Audio, Gateway, KeyHook, COLOR_DEPTHS, RDP_SECURITY, SSH_SCHEMES, TERM_FONTS};

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
            };
            s.validate()?;
            s.apply(&mut e.settings);
        }
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
