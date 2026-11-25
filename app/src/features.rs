//! Ribbon actions and dialogs shared with the web version: quick connect,
//! connection tests and availability, credential manager, folder
//! credentials, audit log, workspaces, theme and navigation.

use super::*;
use relay_desktop_core::probe::{self, Probe};
use relay_desktop_core::{AuditEvent, Workspace};
use std::time::Duration;

pub fn status_of(s: &State, id: &str) -> i32 {
    s.probes.get(id).map(|p| if p.ok { 1 } else { 2 }).unwrap_or(0)
}

fn testable(e: &Entry) -> bool {
    matches!(e.kind, Kind::Rdp | Kind::Ssh | Kind::Vnc) && !e.host.trim().is_empty()
}

fn current_entry(s: &State) -> Option<Entry> {
    match &s.current {
        Some(Sel::Entry(id)) => s.tree.entries.iter().find(|e| &e.id == id).cloned(),
        _ => None,
    }
}

fn current_folder(s: &State) -> Option<Folder> {
    match &s.current {
        Some(Sel::Folder(id)) => s.tree.folders.iter().find(|f| &f.id == id).cloned(),
        _ => None,
    }
}

/// Ribbon enable states and status bar path for the current selection.
pub fn update_flags(ui: &AppWindow) {
    with(|s| {
        let entry = current_entry(s);
        ui.set_has_entry(entry.is_some());
        ui.set_has_folder(current_folder(s).is_some());
        ui.set_can_test(entry.as_ref().is_some_and(testable));
        ui.set_is_favorite(entry.as_ref().is_some_and(|e| e.favorite));
        ui.set_entry_kind(entry.as_ref().map(|e| e.kind.as_str()).unwrap_or("").into());
        let folder = match &s.current {
            Some(Sel::Folder(id)) => Some(id.clone()),
            Some(Sel::Entry(_)) => entry.as_ref().and_then(|e| e.folder_id.clone()),
            None => None,
        };
        ui.set_status_path(folder_path(&s.tree, folder.as_deref()).into());
        ui.set_ctx_ssh(ssh_targets(s).len() as i32);
        ui.set_marked_count(s.marked.len() as i32);
    });
}

pub fn update_counts(ui: &AppWindow) {
    let text = with(|s| {
        format!(
            "{} entrée(s) · {} dossier(s) · {} identifiant(s)",
            s.tree.entries.len(),
            s.tree.folders.len(),
            s.creds.len()
        )
    });
    ui.set_status_counts(text.into());
}

pub fn after_load(ui: &AppWindow) {
    update_counts(ui);
    update_flags(ui);
    probe_entries(ui, None);
}

/// Tests the TCP port of the entries (all, or one folder subtree) from this PC.
pub fn probe_entries(ui: &AppWindow, folder: Option<String>) {
    let targets: Vec<(String, String, u16)> = with(|s| {
        s.tree
            .entries
            .iter()
            .filter(|e| testable(e) && folder.as_ref().is_none_or(|f| in_folder(&s.tree, e, f)))
            .map(|e| (e.id.clone(), e.host.clone(), e.port))
            .collect()
    });
    if targets.is_empty() {
        if ui.get_dialog() == "availability" {
            ui.set_busy(false);
            fill_availability(ui);
        }
        return;
    }
    let source_at_start = source();
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let results = probe::many(targets, Duration::from_secs(3));
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            let same_source = with(|s| match (&source_at_start, &s.source) {
                (Some(start), Some(current)) => Arc::ptr_eq(start, current),
                _ => false,
            });
            if !same_source {
                return;
            }
            with(|s| s.probes.extend(results));
            rebuild(&ui);
            if ui.get_dialog() == "availability" {
                ui.set_busy(false);
                fill_availability(&ui);
            }
        });
    });
}

/// SSH entries targeted by a command for the selection (same rule as the web:
/// the entry itself, the folder subtree, or every SSH entry at the root).
pub fn ssh_targets(s: &State) -> Vec<String> {
    let ssh = s.tree.entries.iter().filter(|e| e.kind == Kind::Ssh);
    match &s.current {
        Some(Sel::Entry(id)) => ssh.filter(|e| &e.id == id).map(|e| e.id.clone()).collect(),
        Some(Sel::Folder(f)) => ssh.filter(|e| in_folder(&s.tree, e, f)).map(|e| e.id.clone()).collect(),
        None => ssh.map(|e| e.id.clone()).collect(),
    }
}

pub fn in_folder(tree: &Tree, e: &Entry, folder: &str) -> bool {
    let mut cur = e.folder_id.clone();
    let mut guard = 0;
    while let Some(id) = cur {
        if id == folder {
            return true;
        }
        cur = tree.folders.iter().find(|f| f.id == id).and_then(|f| f.parent_id.clone());
        guard += 1;
        if guard > 64 {
            break;
        }
    }
    false
}

fn fill_availability(ui: &AppWindow) {
    let failed_only = ui.get_avail_failed_only();
    let rows: Vec<AvailRow> = with(|s| {
        let mut list: Vec<&Entry> = s
            .tree
            .entries
            .iter()
            .filter(|e| testable(e) && s.avail_folder.as_ref().is_none_or(|f| in_folder(&s.tree, e, f)))
            .filter(|e| !failed_only || status_of(s, &e.id) == 2)
            .collect();
        list.sort_by_key(|e| (status_of(s, &e.id) != 2, e.name.to_lowercase()));
        list.into_iter()
            .map(|e| {
                let p = s.probes.get(&e.id);
                AvailRow {
                    name: e.name.clone().into(),
                    path: folder_path(&s.tree, e.folder_id.as_deref()).into(),
                    kind: e.kind.as_str().into(),
                    endpoint: format!("{}:{}", e.host, e.port).into(),
                    status: status_of(s, &e.id),
                    detail: match p {
                        Some(p) if p.ok => format!("répond en {} ms ({})", p.millis, p.address),
                        Some(p) => p.error.clone(),
                        None => "pas encore testé".into(),
                    }
                    .into(),
                }
            })
            .collect()
    });
    ui.set_avail_rows(ModelRc::new(VecModel::from(rows)));
}

fn describe(p: &Probe, host: &str, port: u16) -> String {
    if p.ok {
        format!(
            "✅ Le port {port} de {host} répond depuis ce poste en {} ms (adresse {}).",
            p.millis, p.address
        )
    } else {
        format!("❌ Le port {port} de {host} ne répond pas depuis ce poste : {}.", p.error)
    }
}

fn fill_creds(ui: &AppWindow) {
    let rows: Vec<CredRow> = with(|s| {
        s.creds
            .iter()
            .map(|c| {
                let mut tags = Vec::new();
                if c.has_password {
                    tags.push("mot de passe");
                }
                if c.has_private_key {
                    tags.push("clé privée");
                }
                if c.has_totp {
                    tags.push("TOTP");
                }
                let user = if c.domain.is_empty() {
                    c.username.clone()
                } else {
                    format!("{}\\{}", c.domain, c.username)
                };
                CredRow {
                    name: c.name.clone().into(),
                    username: user.into(),
                    detail: tags.join(" · ").into(),
                }
            })
            .collect()
    });
    ui.set_cred_rows(ModelRc::new(VecModel::from(rows)));
}

fn open_creds(ui: &AppWindow) {
    fill_creds(ui);
    ui.set_cred_selected(-1);
    ui.set_cred_info("".into());
    ui.set_dialog_error("".into());
    ui.set_dialog("creds".into());
}

fn selected_cred(ui: &AppWindow) -> Option<Credential> {
    let i = ui.get_cred_selected();
    with(|s| if i >= 0 { s.creds.get(i as usize).cloned() } else { None })
}

fn web_page(ui: &AppWindow, what: &str) {
    let Some(url) = source().and_then(|s| s.web_url()) else {
        ui.set_status(format!("{what} : disponible avec un serveur Relay.").into());
        return;
    };
    let msg = session::open(session::Target {
        kind: Kind::Web,
        host: String::new(),
        port: 0,
        url,
        username: String::new(),
        domain: String::new(),
        password: None,
    });
    ui.set_status(
        match msg {
            Ok(_) => format!("{what} : ouvert dans Relay web (version native à venir)."),
            Err(e) => e,
        }
        .into(),
    );
}

pub fn act(ui: &AppWindow, id: &str) {
    if notes::act(ui, id) || bulk::act(ui, id) || admin::act(ui, id) {
        return;
    }
    match id {
        "open" if multisel::active() => multisel::open_all(ui),
        "open" => open_session(ui, false),
        "move" => multisel::move_dialog(ui),
        "new" => open_entry_dialog(ui, None),
        "new-rdp" | "new-ssh" => {
            open_entry_dialog(ui, None);
            ui.set_e_kind(if id == "new-rdp" { 0 } else { 1 });
        }
        "folder" => open_folder_dialog(ui, None),
        "edit" => ui.invoke_edit_selected(),
        "del" => ui.invoke_delete_selected(),
        "reload" => load(ui),
        "dup" | "fav" => {
            let (Some(src), Some(mut e)) = (source(), with(|s| current_entry(s))) else {
                return;
            };
            if id == "dup" {
                e.id.clear();
                e.name = format!("{} (copie)", e.name);
            } else {
                e.favorite = !e.favorite;
            }
            bg(
                ui,
                "Enregistrement…",
                move || src.save_entry(&e),
                |ui, r| match r {
                    Ok(saved) => {
                        with(|s| s.current = Some(Sel::Entry(saved.id)));
                        load(ui);
                    }
                    Err(e) => ui.set_status(e.to_string().into()),
                },
            );
        }
        "options" => connopts::open_options(ui),
        "dark" | "light" => {
            ui.set_dark_mode(id == "dark");
            with(|s| {
                s.settings.theme = Some(id.to_string());
                let _ = s.settings.save();
            });
        }
        "expand" => {
            with(|s| s.expanded = s.tree.folders.iter().map(|f| f.id.clone()).collect());
            rebuild(ui);
        }
        "collapse" => {
            with(|s| s.expanded.clear());
            rebuild(ui);
        }
        "dash" => ui.set_active_tab(-1),
        "sftp" => files::open(ui),
        "hl-rules" => connopts::open_rules(ui),
        "cad" => {
            let sent = active_session(ui, |x| match (&x.rdp, &x.pve) {
                (Some(r), _) => r.send_cad(),
                (None, Some(c)) => {
                    c.send_cad();
                    Ok(())
                }
                _ => Err("ouvrez d'abord une session RDP ou VNC".into()),
            });
            if let Some(Err(e)) = sent {
                ui.set_status(format!("Ctrl+Alt+Suppr : {e}").into());
            }
        }
        "fullscreen" => {
            if active_session(ui, |x| x.rdp.as_mut().map(|r| r.enter_full_screen())).flatten() != Some(true) {
                ui.set_status("Plein écran : ouvrez d'abord une session RDP connectée.".into());
            }
        }
        "about" => ui.set_dialog("about".into()),
        "test-connection" => {
            let Some(e) = with(|s| current_entry(s)).filter(testable) else {
                return;
            };
            ui.set_test_text("".into());
            ui.set_dialog_error("".into());
            ui.set_dialog("test".into());
            let (host, port, eid) = (e.host.clone(), e.port, e.id.clone());
            bg(
                ui,
                &format!("Test de {host}:{port}…"),
                move || Ok(probe::tcp(&host, port, Duration::from_secs(5))),
                move |ui, r| {
                    if let Ok(p) = r {
                        ui.set_test_text(describe(&p, &e.host, e.port).into());
                        with(|s| s.probes.insert(eid, p));
                        rebuild(ui);
                    }
                },
            );
        }
        "availability" => {
            let folder = with(|s| match &s.current {
                Some(Sel::Folder(f)) => Some(f.clone()),
                _ => None,
            });
            with(|s| s.avail_folder = folder.clone());
            ui.set_dialog_error("".into());
            ui.set_dialog("availability".into());
            fill_availability(ui);
            ui.set_busy(true);
            probe_entries(ui, folder);
        }
        "availability-run" => {
            ui.set_busy(true);
            probe_entries(ui, with(|s| s.avail_folder.clone()));
        }
        "availability-filter" => fill_availability(ui),
        "quick" => {
            fill_choices(ui, None, None);
            ui.set_q_kind(0);
            ui.set_q_cred(0);
            for set in [
                AppWindow::set_q_host,
                AppWindow::set_q_port,
                AppWindow::set_q_username,
                AppWindow::set_q_password,
            ] {
                set(ui, "".into());
            }
            ui.set_dialog_error("".into());
            ui.set_dialog("quick".into());
        }
        "creds" => open_creds(ui),
        "cred-new" | "cred-edit" => {
            let edit = if id == "cred-edit" { selected_cred(ui) } else { None };
            with(|s| {
                s.cred_edit = edit.as_ref().map(|c| c.id.clone());
                s.back_to_creds = true;
            });
            let c = edit.unwrap_or_default();
            ui.set_c_title(
                if c.id.is_empty() {
                    "Nouvel identifiant"
                } else {
                    "Modifier l'identifiant"
                }
                .into(),
            );
            ui.set_c_name(c.name.into());
            ui.set_c_username(c.username.into());
            ui.set_c_domain(c.domain.into());
            ui.set_c_notes(c.notes.into());
            ui.set_c_password("".into());
            ui.set_c_totp("".into());
            ui.set_dialog_error("".into());
            ui.set_dialog("credential".into());
        }
        "cred-copy" | "cred-totp" => {
            let (Some(src), Some(c)) = (source(), selected_cred(ui)) else {
                return;
            };
            if id == "cred-copy" {
                if !c.has_password {
                    ui.set_cred_info("Cet identifiant n'a pas de mot de passe.".into());
                    return;
                }
                bg(
                    ui,
                    "Lecture du coffre…",
                    move || src.reveal(&c.id, SecretField::Password).map(Zeroizing::new),
                    |ui, r| match r {
                        Ok(pw) => match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(pw.as_str())) {
                            Ok(()) => {
                                ui.set_cred_info("Mot de passe copié. Le presse-papiers sera vidé dans 30 secondes.".into());
                                std::thread::spawn(move || {
                                    std::thread::sleep(Duration::from_secs(30));
                                    if let Ok(mut cb) = arboard::Clipboard::new() {
                                        if cb.get_text().is_ok_and(|t| t == pw.as_str()) {
                                            let _ = cb.clear();
                                        }
                                    }
                                });
                            }
                            Err(e) => ui.set_cred_info(format!("Presse-papiers indisponible : {e}").into()),
                        },
                        Err(e) => ui.set_cred_info(e.to_string().into()),
                    },
                );
            } else {
                if !c.has_totp {
                    ui.set_cred_info("Cet identifiant n'a pas de secret TOTP.".into());
                    return;
                }
                bg(
                    ui,
                    "Calcul du code…",
                    move || src.totp(&c.id),
                    |ui, r| {
                        ui.set_cred_info(
                            match r {
                                Ok(t) => format!(
                                    "Code TOTP : {} {} (encore {} s)",
                                    &t.code[..3.min(t.code.len())],
                                    &t.code[3.min(t.code.len())..],
                                    t.remaining
                                ),
                                Err(e) => e.to_string(),
                            }
                            .into(),
                        )
                    },
                );
            }
        }
        "cred-del" => {
            let Some(c) = selected_cred(ui) else { return };
            with(|s| {
                s.pending = Pending::DeleteCredential(c.id.clone());
                s.back_to_creds = true;
            });
            ui.set_confirm_ok("".into());
            ui.set_confirm_text(format!("Supprimer définitivement l'identifiant « {} » ?", c.name).into());
            ui.set_dialog_error("".into());
            ui.set_dialog("confirm".into());
        }
        "folder-creds" => {
            let Some(f) = with(|s| current_folder(s)) else { return };
            fill_choices(ui, None, None);
            let idx = |kind: &str| {
                let cred = f.credentials.get(kind).cloned().flatten();
                with(|s| s.cred_choices.iter().position(|c| c.is_some() && *c == cred).unwrap_or(0) as i32)
            };
            ui.set_fc_rdp(idx("rdp"));
            ui.set_fc_ssh(idx("ssh"));
            ui.set_fc_vnc(idx("vnc"));
            ui.set_fc_web(idx("web"));
            ui.set_dialog_error("".into());
            ui.set_dialog("folder-creds".into());
        }
        "audit" => {
            let Some(src) = source() else { return };
            bg(
                ui,
                "Chargement du journal…",
                move || src.audit(300),
                |ui, r| match r {
                    Ok(events) => {
                        let rows: Vec<AuditRow> = events.into_iter().map(audit_row).collect();
                        ui.set_audit_rows(ModelRc::new(VecModel::from(rows)));
                        ui.set_dialog_error("".into());
                        ui.set_dialog("audit".into());
                    }
                    Err(e) => ui.set_status(e.to_string().into()),
                },
            );
        }
        "workspaces" => load_workspaces(ui, true),
        "ws-open" => {
            let i = ui.get_ws_selected();
            let Some(w) = with(|s| usize::try_from(i).ok().and_then(|i| s.workspaces.get(i).cloned())) else {
                return;
            };
            close_dialog(ui);
            for id in &w.entry_ids {
                if with(|s| s.tree.entries.iter().any(|e| &e.id == id)) {
                    with(|s| s.current = Some(Sel::Entry(id.clone())));
                    open_session(ui, false);
                }
            }
            ui.set_status(format!("Espace « {} » : {} session(s) ouverte(s)", w.name, w.entry_ids.len()).into());
        }
        "ws-save" => {
            let name = ui.get_ws_name().trim().to_string();
            if name.is_empty() {
                return fail(ui, "Donnez un nom à l'espace de travail.");
            }
            let ids: Vec<String> = with(|s| s.sessions.iter().map(|x| x.entry_id.clone()).filter(|id| !id.is_empty()).collect());
            let Some(src) = source() else { return };
            let w = Workspace {
                id: String::new(),
                name,
                entry_ids: ids,
            };
            bg(
                ui,
                "Enregistrement…",
                move || src.save_workspace(&w),
                |ui, r| match r {
                    Ok(_) => {
                        ui.set_ws_name("".into());
                        load_workspaces(ui, false);
                    }
                    Err(e) => fail(ui, e),
                },
            );
        }
        "ws-del" => {
            let i = ui.get_ws_selected();
            let (Some(src), Some(w)) = (
                source(),
                with(|s| usize::try_from(i).ok().and_then(|i| s.workspaces.get(i).cloned())),
            ) else {
                return;
            };
            bg(
                ui,
                "Suppression…",
                move || src.delete_workspace(&w.id),
                |ui, r| match r {
                    Ok(()) => load_workspaces(ui, false),
                    Err(e) => fail(ui, e),
                },
            );
        }
        "record" => recorder::record(ui),
        "play-cast" => recorder::play(ui),
        "devices" => web_page(ui, "Appareils"),
        _ => {}
    }
}

fn audit_row(a: AuditEvent) -> AuditRow {
    AuditRow {
        at: a.at.replace('T', " ").chars().take(19).collect::<String>().into(),
        user: a.user.into(),
        action: a.action.into(),
        target: a.target.into(),
        detail: a.detail.into(),
    }
}

fn load_workspaces(ui: &AppWindow, open: bool) {
    let Some(src) = source() else { return };
    bg(
        ui,
        "Chargement des espaces…",
        move || src.workspaces(),
        move |ui, r| match r {
            Ok(list) => {
                let rows: Vec<WsRow> = list
                    .iter()
                    .map(|w| WsRow {
                        name: w.name.clone().into(),
                        detail: format!("{} entrée(s)", w.entry_ids.len()).into(),
                    })
                    .collect();
                with(|s| s.workspaces = list);
                ui.set_ws_rows(ModelRc::new(VecModel::from(rows)));
                ui.set_ws_selected(-1);
                if open {
                    ui.set_ws_name("".into());
                    ui.set_dialog_error("".into());
                    ui.set_dialog("workspaces".into());
                }
            }
            Err(e) => ui.set_status(e.to_string().into()),
        },
    );
}

/// Dialogs handled here; returns false for the ones handled by `submit`.
pub fn submit(ui: &AppWindow) -> bool {
    match ui.get_dialog().as_str() {
        "quick" => submit_quick(ui),
        "options" => connopts::submit_options(ui),
        "hl-rules" => connopts::submit_rules(ui),
        "sync-src" => admin::submit_source(ui),
        "folder-creds" => {
            let (Some(src), Some(f)) = (source(), with(|s| current_folder(s))) else {
                return true;
            };
            let mut map = std::collections::BTreeMap::new();
            with(|s| {
                for (kind, i) in [
                    ("rdp", ui.get_fc_rdp()),
                    ("ssh", ui.get_fc_ssh()),
                    ("vnc", ui.get_fc_vnc()),
                    ("web", ui.get_fc_web()),
                ] {
                    if let Some(Some(id)) = s.cred_choices.get(i as usize) {
                        map.insert(kind.to_string(), Some(id.clone()));
                    }
                }
            });
            bg(
                ui,
                "Enregistrement…",
                move || src.set_folder_credentials(&f.id, &map),
                |ui, r| match r {
                    Ok(()) => {
                        close_dialog(ui);
                        load(ui);
                    }
                    Err(e) => fail(ui, e),
                },
            );
        }
        "credential" => {
            let Some(src) = source() else { return true };
            let pw = ui.get_c_password().to_string();
            let totp = ui.get_c_totp().trim().to_string();
            let id = with(|s| s.cred_edit.clone());
            let input = CredentialInput {
                name: ui.get_c_name().trim().into(),
                username: ui.get_c_username().trim().into(),
                domain: ui.get_c_domain().trim().into(),
                notes: ui.get_c_notes().into(),
                password: (!pw.is_empty()).then_some(pw),
                totp: (!totp.is_empty()).then_some(totp),
                ..Default::default()
            };
            bg(
                ui,
                "Enregistrement de l'identifiant…",
                move || {
                    let mut input = input;
                    let r = src.save_credential(id.as_deref(), &input);
                    zeroize::Zeroize::zeroize(&mut input);
                    r
                },
                |ui, r| match r {
                    Ok(c) => {
                        let back = with(|s| std::mem::take(&mut s.back_to_creds));
                        close_dialog(ui);
                        ui.set_status(format!("Identifiant « {} » enregistré", c.name).into());
                        reload_then(ui, back);
                    }
                    Err(e) => fail(ui, e),
                },
            );
        }
        "confirm" if with(|s| matches!(s.pending, Pending::Run(_))) => {
            if let Pending::Run(f) = with(|s| std::mem::take(&mut s.pending)) {
                f(ui);
            }
        }
        "confirm" => {
            let id = with(|s| match &s.pending {
                Pending::DeleteCredential(id) => Some(id.clone()),
                _ => None,
            });
            let Some(id) = id else { return false };
            with(|s| s.pending = Pending::None);
            let Some(src) = source() else { return true };
            bg(
                ui,
                "Suppression…",
                move || src.delete_credential(&id),
                |ui, r| match r {
                    Ok(()) => {
                        let back = with(|s| std::mem::take(&mut s.back_to_creds));
                        close_dialog(ui);
                        reload_then(ui, back);
                    }
                    Err(e) => fail(ui, e),
                },
            );
        }
        _ => return false,
    }
    true
}

/// Reloads the inventory, then reopens the credential manager if asked.
fn reload_then(ui: &AppWindow, creds: bool) {
    let Some(src) = source() else { return };
    bg(
        ui,
        "Chargement…",
        move || Ok((src.tree()?, src.credentials()?)),
        move |ui, r| {
            if let Ok((tree, list)) = r {
                with(|s| {
                    s.tree = tree;
                    s.creds = list;
                });
                rebuild(ui);
                update_counts(ui);
            }
            if creds {
                open_creds(ui);
            }
        },
    );
}

fn submit_quick(ui: &AppWindow) {
    let kind = [Kind::Rdp, Kind::Ssh, Kind::Web][ui.get_q_kind().clamp(0, 2) as usize];
    let host = ui.get_q_host().trim().to_string();
    if host.is_empty() {
        return fail(ui, "Indiquez l'hôte ou l'URL.");
    }
    let port = match ui.get_q_port().trim() {
        "" => kind.default_port(),
        p => match p.parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => return fail(ui, "Port invalide (1 à 65535)."),
        },
    };
    let mut entry = Entry::new(kind);
    entry.name = host.clone();
    if kind == Kind::Web {
        entry.url = if host.starts_with("http") {
            host.clone()
        } else {
            format!("https://{host}")
        };
    } else {
        entry.host = host;
        entry.port = port;
    }
    let cred = with(|s| s.cred_choices.get(ui.get_q_cred() as usize).cloned().flatten());
    let typed_user = ui.get_q_username().trim().to_string();
    let typed_pw = Zeroizing::new(ui.get_q_password().to_string());
    ui.set_q_password("".into());
    let Some(src) = source() else { return };
    let creds = with(|s| s.creds.clone());
    let launched = entry.clone();
    bg(
        ui,
        "Connexion rapide…",
        move || {
            let c = cred.as_ref().and_then(|id| creds.iter().find(|c| &c.id == id)).cloned();
            let reveal = |f: SecretField, has: bool| -> Result<Option<Zeroizing<String>>, Error> {
                match &c {
                    Some(c) if has => Ok(Some(Zeroizing::new(src.reveal(&c.id, f)?))),
                    _ => Ok(None),
                }
            };
            let (username, domain, password) = match &c {
                Some(cr) => (
                    cr.username.clone(),
                    cr.domain.clone(),
                    reveal(SecretField::Password, cr.has_password)?,
                ),
                None => (typed_user, String::new(), (!typed_pw.is_empty()).then(|| typed_pw.clone())),
            };
            if entry.kind == Kind::Ssh {
                let cc = c.clone().unwrap_or_default();
                return Ok(Launch::Ssh(SshCreds {
                    username,
                    password,
                    private_key: reveal(SecretField::PrivateKey, cc.has_private_key)?,
                    passphrase: reveal(SecretField::Passphrase, cc.has_passphrase)?,
                }));
            }
            if entry.kind == Kind::Rdp {
                return Ok(Launch::Rdp(RdpCreds {
                    username,
                    domain,
                    password,
                }));
            }
            session::open(session::Target {
                kind: entry.kind,
                host: entry.host.clone(),
                port: entry.port,
                url: entry.url.clone(),
                username,
                domain,
                password,
            })
            .map(Launch::External)
            .map_err(Error::Invalid)
        },
        move |ui, r| match r {
            Ok(Launch::External(msg)) => {
                close_dialog(ui);
                ui.set_status(msg.into());
            }
            Ok(Launch::Ssh(c)) => {
                close_dialog(ui);
                open_ssh(ui, &launched, c);
            }
            Ok(Launch::Rdp(c)) => {
                close_dialog(ui);
                open_rdp(ui, &launched, c);
            }
            Ok(Launch::Pve(c)) => {
                close_dialog(ui);
                open_pve(ui, &launched, pve::Target::Relay(c));
            }
            Ok(Launch::Vnc(password)) => {
                close_dialog(ui);
                let target = pve::Target::Direct {
                    host: launched.host.clone(),
                    port: launched.port,
                    password: password.unwrap_or_default(),
                    opts: relay_desktop_core::conn::VncOptions::from_settings(&launched.settings),
                };
                open_pve(ui, &launched, target);
            }
            Err(e) => fail(ui, e),
        },
    );
}

/// Asks for confirmation, then runs `then` (which closes or replaces the dialog).
pub fn confirm(ui: &AppWindow, text: String, then: impl FnOnce(&AppWindow) + 'static) {
    confirm_as(ui, text, "", then);
}

/// Same as [`confirm`] with the label of the confirm button ("" = "Supprimer").
pub fn confirm_as(ui: &AppWindow, text: String, ok: &str, then: impl FnOnce(&AppWindow) + 'static) {
    with(|s| s.pending = Pending::Run(Box::new(then)));
    ui.set_confirm_ok(ok.into());
    ui.set_confirm_text(text.into());
    ui.set_dialog_error("".into());
    ui.set_dialog("confirm".into());
}
