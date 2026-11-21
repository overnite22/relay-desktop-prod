//! Native administration dialogs: backups, NetBox / Proxmox sync sources,
//! duplicates, RDM import and recordings.

use super::*;
use relay_desktop_core::admin::{DuplicateGroup, ImportOptions, ImportPlan, Recording, SyncSource};

#[derive(Default, PartialEq, Clone, Copy)]
enum Mode {
    #[default]
    None,
    Sources,
    Duplicates,
    Recordings,
}

#[derive(Default)]
pub struct Admin {
    mode: Mode,
    sources: Vec<SyncSource>,
    groups: Vec<DuplicateGroup>,
    /// Duplicate rows: (group, entry) — `None` for the group header.
    dup_rows: Vec<(usize, Option<usize>)>,
    recordings: Vec<Recording>,
    plan: Option<ImportPlan>,
    plan_data: Vec<u8>,
    /// Sync source being edited (`None`: new source).
    src_edit: Option<String>,
}

fn date(s: &str) -> String {
    relay_desktop_core::model::local_time(s, false)
}

fn size(n: i64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} Mio", n as f64 / (1 << 20) as f64)
    } else {
        format!("{:.0} Kio", (n as f64 / 1024.0).ceil())
    }
}

fn rows(ui: &AppWindow, rows: Vec<ListRow>) {
    ui.set_adm_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_adm_selected(-1);
}

/// Opens an administration list; each action is (label, needs a selected row).
fn open_list(ui: &AppWindow, mode: Mode, title: &str, hint: &str, actions: &[(&str, bool)]) {
    with(|s| s.admin.mode = mode);
    ui.set_adm_title(title.into());
    ui.set_adm_hint(hint.into());
    ui.set_adm_detail("".into());
    let actions: Vec<AdmAction> = actions
        .iter()
        .map(|(label, sel)| AdmAction {
            label: (*label).into(),
            needs_selection: *sel,
        })
        .collect();
    ui.set_adm_actions(ModelRc::new(VecModel::from(actions)));
    rows(ui, Vec::new());
    ui.set_dialog_error("".into());
    ui.set_dialog("admin".into());
    reload(ui);
}

/// Reloads the list of the open administration dialog.
fn reload(ui: &AppWindow) {
    let Some(src) = source() else { return };
    match with(|s| s.admin.mode) {
        Mode::Sources => bg(
            ui,
            "Chargement des sources…",
            move || src.sync_sources(),
            |ui, r| match r {
                Ok(list) => {
                    let r = list
                        .iter()
                        .map(|x| ListRow {
                            name: x.name.clone().into(),
                            detail: format!("{} · {}", if x.kind == "proxmox" { "Proxmox" } else { "NetBox" }, x.url).into(),
                            extra: match &x.last_sync_at {
                                Some(at) => format!("{} · {}", date(at), x.last_status),
                                None if x.interval_min > 0 => format!("toutes les {} min", x.interval_min),
                                None => "jamais synchronisée".into(),
                            }
                            .into(),
                            status: match x.last_status.as_str() {
                                "" => 0,
                                s if s.starts_with("ok") || s.contains("réussi") => 1,
                                s if s.contains("erreur") || s.contains("échec") => 2,
                                _ => 0,
                            },
                        })
                        .collect();
                    with(|s| s.admin.sources = list);
                    rows(ui, r);
                    ui.set_adm_detail(if with(|s| s.admin.sources.is_empty()) {
                        "Aucune source : « Nouvelle… » pour ajouter un serveur Proxmox VE ou NetBox.".into()
                    } else {
                        "".into()
                    });
                }
                Err(e) => fail(ui, e),
            },
        ),
        Mode::Duplicates => bg(
            ui,
            "Recherche des doublons…",
            move || src.duplicates(),
            |ui, r| match r {
                Ok(groups) => {
                    let (mut list, mut map) = (Vec::new(), Vec::new());
                    with(|s| {
                        for (gi, g) in groups.iter().enumerate() {
                            list.push(ListRow {
                                name: g.label.clone().into(),
                                detail: format!("{} entrées", g.entries.len()).into(),
                                extra: "".into(),
                                status: 2,
                            });
                            map.push((gi, None));
                            for (ei, e) in g.entries.iter().enumerate() {
                                list.push(ListRow {
                                    name: format!("      {}", e.name).into(),
                                    detail: folder_path(&s.tree, e.folder_id.as_deref()).into(),
                                    extra: if e.source_id.is_some() { "synchronisée" } else { "" }.into(),
                                    status: 0,
                                });
                                map.push((gi, Some(ei)));
                            }
                        }
                        ui.set_adm_detail(
                            if groups.is_empty() {
                                "Aucun doublon : chaque cible n'apparaît qu'une fois.".to_string()
                            } else {
                                format!(
                                    "{} groupe(s). Sélectionnez l'entrée à garder puis « Garder et fusionner ».",
                                    groups.len()
                                )
                            }
                            .into(),
                        );
                        s.admin.groups = groups;
                        s.admin.dup_rows = map;
                    });
                    rows(ui, list);
                }
                Err(e) => fail(ui, e),
            },
        ),
        Mode::Recordings => bg(
            ui,
            "Chargement des enregistrements…",
            move || src.recordings(),
            |ui, r| match r {
                Ok(list) => {
                    let r = list
                        .iter()
                        .map(|x| ListRow {
                            name: x.name.clone().into(),
                            detail: format!("{} · {}", x.kind.to_uppercase(), date(&x.at)).into(),
                            extra: format!("{}{}", size(x.size), if x.limited { " · tronqué" } else { "" }).into(),
                            status: 0,
                        })
                        .collect();
                    ui.set_adm_detail(if list.is_empty() {
                        "Aucun enregistrement.".into()
                    } else {
                        "".into()
                    });
                    with(|s| s.admin.recordings = list);
                    rows(ui, r);
                }
                Err(e) => fail(ui, e),
            },
        ),
        Mode::None => {}
    }
}

fn selected<T: Clone>(ui: &AppWindow, list: impl FnOnce(&State) -> &Vec<T>) -> Option<T> {
    let i = usize::try_from(ui.get_adm_selected()).ok()?;
    with(|s| list(s).get(i).cloned())
}

fn outcome(ui: &AppWindow, what: &str, r: Result<relay_desktop_core::admin::Outcome, Error>) {
    match r {
        Ok(o) => ui.set_adm_detail(format!("{what} : {}{}", if o.ok { "" } else { "échec — " }, o.message).into()),
        Err(e) => fail(ui, e),
    }
}

/// List dialog buttons, by index in `adm-actions`.
fn action(ui: &AppWindow, n: usize) {
    let Some(src) = source() else { return };
    ui.set_dialog_error("".into());
    match (with(|s| s.admin.mode), n) {
        (Mode::Sources, 0 | 1) => {
            let Some(x) = selected(ui, |s| &s.admin.sources) else {
                return fail(ui, "Sélectionnez une source.");
            };
            if n == 0 {
                bg(
                    ui,
                    &format!("Test de « {} »…", x.name),
                    move || src.test_sync_source(&x.id),
                    |ui, r| outcome(ui, "Test", r),
                );
            } else {
                bg(
                    ui,
                    &format!("Synchronisation de « {} »…", x.name),
                    move || src.run_sync_source(&x.id),
                    |ui, r| {
                        let ok = r.is_ok();
                        outcome(ui, "Synchronisation", r);
                        if ok {
                            load(ui);
                            reload_keep_detail(ui);
                        }
                    },
                );
            }
        }
        (Mode::Duplicates, 0) => {
            let pick = usize::try_from(ui.get_adm_selected())
                .ok()
                .and_then(|i| with(|s| s.admin.dup_rows.get(i).copied()));
            let Some((gi, Some(ei))) = pick else {
                return fail(ui, "Sélectionnez, dans un groupe, l'entrée à garder.");
            };
            let Some(g) = with(|s| s.admin.groups.get(gi).cloned()) else {
                return;
            };
            let keep = g.entries[ei].clone();
            let others: Vec<String> = g.entries.iter().filter(|e| e.id != keep.id).map(|e| e.id.clone()).collect();
            features::confirm_as(
                ui,
                format!(
                    "Garder « {} » et y fusionner {} autre(s) entrée(s) ? Les autres seront supprimées (identifiant, utilisateur et notes repris).",
                    keep.name,
                    others.len()
                ),
                "Fusionner",
                move |ui| {
                    bg(ui, "Fusion…", move || src.merge_entries(&keep.id, &others), |ui, r| match r {
                        Ok(e) => {
                            ui.set_dialog("admin".into());
                            ui.set_status(format!("Doublons fusionnés dans « {} »", e.name).into());
                            load(ui);
                            reload(ui);
                        }
                        Err(e) => {
                            ui.set_dialog("admin".into());
                            fail(ui, e);
                        }
                    });
                },
            );
        }
        (Mode::Duplicates, 1) | (Mode::Sources, 5) | (Mode::Recordings, 1) => reload(ui),
        (Mode::Sources, 2) => open_source_editor(ui, None),
        (Mode::Sources, 3 | 4) => {
            let Some(x) = selected(ui, |s| &s.admin.sources) else {
                return fail(ui, "Sélectionnez une source.");
            };
            if n == 3 {
                return open_source_editor(ui, Some(x));
            }
            features::confirm(
                ui,
                format!(
                    "Supprimer la source « {} » ? Les entrées qu'elle a créées restent dans l'arborescence.",
                    x.name
                ),
                move |ui| {
                    let Some(src) = source() else { return };
                    bg(
                        ui,
                        "Suppression de la source…",
                        move || src.delete_sync_source(&x.id),
                        |ui, r| match r {
                            Ok(()) => {
                                act(ui, "sources");
                            }
                            Err(e) => fail(ui, e),
                        },
                    );
                },
            );
        }
        (Mode::Recordings, 0) => {
            let Some(x) = selected(ui, |s| &s.admin.recordings) else {
                return fail(ui, "Sélectionnez un enregistrement.");
            };
            features::confirm(
                ui,
                format!("Supprimer définitivement l'enregistrement « {} » ?", x.name),
                move |ui| {
                    bg(
                        ui,
                        "Suppression…",
                        move || src.delete_recording(&x.id),
                        |ui, r| {
                            ui.set_dialog("admin".into());
                            match r {
                                Ok(()) => reload(ui),
                                Err(e) => fail(ui, e),
                            }
                        },
                    );
                },
            );
        }
        _ => {}
    }
}

/// Reloads the list after a sync without clearing its result message.
fn reload_keep_detail(ui: &AppWindow) {
    let detail = ui.get_adm_detail();
    reload(ui);
    ui.set_adm_detail(detail);
}

#[cfg(windows)]
fn save_as(title: &str, name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().set_title(title).set_file_name(name).save_file()
}

#[cfg(not(windows))]
fn save_as(_: &str, _: &str) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn pick(title: &str, filter: (&str, &[&str])) -> Option<PathBuf> {
    rfd::FileDialog::new().set_title(title).add_filter(filter.0, filter.1).pick_file()
}

#[cfg(not(windows))]
fn pick(_: &str, _: (&str, &[&str])) -> Option<PathBuf> {
    None
}

fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("{secs}")
}

fn backup_export(ui: &AppWindow) {
    let pw = Zeroizing::new(ui.get_bk_password().to_string());
    if *pw != ui.get_bk_password2().as_str() {
        return fail(ui, "Les deux mots de passe ne correspondent pas.");
    }
    if pw.chars().count() < relay_desktop_core::backup::MIN_PASSWORD {
        return fail(
            ui,
            format!(
                "Le mot de passe de sauvegarde doit faire au moins {} caractères.",
                relay_desktop_core::backup::MIN_PASSWORD
            ),
        );
    }
    let server = ui.get_server_mode();
    let name = if server {
        format!("relay-{}.relay", stamp())
    } else {
        format!("coffre-local-{}.relaybak", stamp())
    };
    let (Some(src), Some(path)) = (source(), save_as("Enregistrer la sauvegarde", &name)) else {
        return;
    };
    bg(
        ui,
        "Sauvegarde…",
        move || {
            let data = src.export_backup(&pw)?;
            std::fs::write(&path, data).map_err(|e| Error::Invalid(e.to_string()))?;
            Ok(path)
        },
        |ui, r| match r {
            Ok(p) => {
                ui.set_bk_password("".into());
                ui.set_bk_password2("".into());
                ui.set_status(format!("Sauvegarde enregistrée : {}", p.display()).into());
            }
            Err(e) => fail(ui, e),
        },
    );
}

fn backup_restore(ui: &AppWindow) {
    let pw = Zeroizing::new(ui.get_bk_restore_password().to_string());
    let server = ui.get_server_mode();
    let filter: (&str, &[&str]) = if server {
        ("Sauvegarde Relay", &["relay"])
    } else {
        ("Sauvegarde de coffre local", &["relaybak"])
    };
    let (Some(src), Some(file)) = (source(), pick("Choisir la sauvegarde", filter)) else {
        return;
    };
    if server {
        features::confirm_as(
            ui,
            "Restaurer cette sauvegarde ? Tout le contenu actuel du serveur Relay (entrées, identifiants, réglages, enregistrements) sera remplacé.".into(),
            "Restaurer",
            move |ui| {
                bg(
                    ui,
                    "Restauration…",
                    move || {
                        let data = std::fs::read(&file).map_err(|e| Error::Invalid(e.to_string()))?;
                        src.restore_backup(&data, &pw)
                    },
                    |ui, r| match r {
                        Ok(()) => {
                            close_dialog(ui);
                            ui.set_status("Sauvegarde restaurée".into());
                            load(ui);
                        }
                        Err(e) => {
                            ui.set_dialog("backup".into());
                            fail(ui, e);
                        }
                    },
                );
            },
        );
        return;
    }
    let Some(out) = save_as("Fichier du coffre restauré", "Relay-restauré.rdb") else {
        return;
    };
    bg(
        ui,
        "Restauration…",
        move || {
            let data = std::fs::read(&file).map_err(|e| Error::Invalid(e.to_string()))?;
            let db = relay_desktop_core::backup::open(&data, &pw)?;
            if out.exists() {
                return Err(Error::Invalid("ce fichier existe déjà : choisissez un nouveau nom".into()));
            }
            std::fs::write(&out, db.as_slice()).map_err(|e| Error::Invalid(e.to_string()))?;
            Ok(out)
        },
        |ui, r| match r {
            Ok(p) => {
                ui.set_bk_restore_password("".into());
                ui.set_status(
                    format!(
                        "Coffre restauré dans {} : ajoutez-le comme source (mot de passe maître d'origine).",
                        p.display()
                    )
                    .into(),
                );
            }
            Err(e) => fail(ui, e),
        },
    );
}

fn import_preview(ui: &AppWindow) {
    let path = PathBuf::from(ui.get_imp_path().trim());
    let Some(src) = source() else { return };
    ui.set_imp_summary("".into());
    bg(
        ui,
        "Analyse de l'export RDM…",
        move || {
            let data = std::fs::read(&path).map_err(|e| Error::Invalid(e.to_string()))?;
            let plan = src.import_preview(&data)?;
            Ok((data, plan))
        },
        |ui, r| match r {
            Ok((data, plan)) => show_plan(ui, data, plan),
            Err(e) => fail(ui, e),
        },
    );
}

fn show_plan(ui: &AppWindow, data: Vec<u8>, plan: ImportPlan) {
    let mut list: Vec<ListRow> = plan
        .entries
        .iter()
        .map(|e| ListRow {
            name: e.name.clone().into(),
            detail: if e.kind == "web" {
                format!("Web · {}", e.url)
            } else {
                format!("{} · {}:{}", e.kind.to_uppercase(), e.host, e.port)
            }
            .into(),
            extra: format!(
                "{}{}",
                e.path.join(" / "),
                if e.inline_secret { " · mot de passe à ressaisir" } else { "" }
            )
            .into(),
            status: if e.inline_secret { 2 } else { 1 },
        })
        .collect();
    list.extend(plan.skipped.iter().map(|s| ListRow {
        name: s.name.clone().into(),
        detail: format!("ignorée ({})", s.kind).into(),
        extra: s.reason.clone().into(),
        status: 0,
    }));
    let mut summary = format!(
        "{} entrée(s), {} identifiant(s), {} dossier(s), {} ignorée(s).",
        plan.entries.len(),
        plan.credentials.len(),
        plan.folders.len(),
        plan.skipped.len()
    );
    for w in &plan.warnings {
        summary.push_str(&format!("\n⚠ {w}"));
    }
    ui.set_imp_cred_names(strings(
        std::iter::once("(aucun)".to_string())
            .chain(plan.credentials.iter().map(|c| c.name.clone()))
            .collect(),
    ));
    ui.set_imp_ssh(0);
    ui.set_imp_rdp(0);
    ui.set_imp_summary(summary.into());
    ui.set_imp_rows(ModelRc::new(VecModel::from(list)));
    ui.set_imp_ready(true);
    ui.set_dialog_error("".into());
    with(|s| {
        s.admin.plan = Some(plan);
        s.admin.plan_data = data;
    });
}

fn import_apply(ui: &AppWindow) {
    let (Some(src), Some(plan), data) = (source(), with(|s| s.admin.plan.clone()), with(|s| s.admin.plan_data.clone())) else {
        return fail(ui, "Analysez d'abord le fichier.");
    };
    let name = |i: i32| {
        usize::try_from(i - 1)
            .ok()
            .and_then(|i| plan.credentials.get(i))
            .map(|c| c.name.clone())
            .unwrap_or_default()
    };
    let opt = ImportOptions {
        root: ui.get_imp_root().trim().to_string(),
        ssh_default: name(ui.get_imp_ssh()),
        rdp_default: name(ui.get_imp_rdp()),
    };
    bg(
        ui,
        "Import…",
        move || src.import_apply(&data, &opt),
        |ui, r| match r {
            Ok(res) => {
                close_dialog(ui);
                with(|s| {
                    s.admin.plan = None;
                    s.admin.plan_data.clear();
                    s.expanded.insert(res.root_id.clone());
                    s.current = Some(Sel::Folder(res.root_id.clone()));
                });
                ui.set_status(
                    format!(
                        "Import terminé : {} entrée(s), {} identifiant(s), {} dossier(s)",
                        res.entries, res.credentials, res.folders
                    )
                    .into(),
                );
                load(ui);
            }
            Err(e) => fail(ui, e),
        },
    );
}

/// Handles the administration actions; returns false for other ids.
pub fn act(ui: &AppWindow, id: &str) -> bool {
    match id {
        "backups" => {
            for set in [
                AppWindow::set_bk_password,
                AppWindow::set_bk_password2,
                AppWindow::set_bk_restore_password,
            ] {
                set(ui, "".into());
            }
            ui.set_dialog_error("".into());
            ui.set_dialog("backup".into());
        }
        "bk-export" => backup_export(ui),
        "bk-restore" => backup_restore(ui),
        "sources" => open_list(
            ui,
            Mode::Sources,
            "Sources de synchronisation",
            "Inventaires NetBox et Proxmox importés par le serveur Relay.",
            &[
                ("Tester la connexion", true),
                ("Synchroniser maintenant", true),
                ("Nouvelle…", false),
                ("Modifier…", true),
                ("Supprimer", true),
                ("Actualiser", false),
            ],
        ),
        "dups" => open_list(
            ui,
            Mode::Duplicates,
            "Détection des doublons",
            if ui.get_server_mode() {
                "Entrées visant la même cible (protocole, adresse IP résolue par le serveur, port)."
            } else {
                "Entrées visant la même cible (protocole, hôte, port ; sans résolution DNS en mode local)."
            },
            &[("Garder et fusionner", true), ("Actualiser", false)],
        ),
        "recordings" => open_list(
            ui,
            Mode::Recordings,
            "Enregistrements de sessions",
            "Sessions enregistrées par le serveur Relay. La relecture se fait encore dans Relay web.",
            &[("Supprimer", true), ("Actualiser", false)],
        ),
        "adm-select" => {}
        "import" => {
            with(|s| {
                s.admin.plan = None;
                s.admin.plan_data.clear();
            });
            ui.set_imp_path("".into());
            ui.set_imp_root("Import RDM".into());
            ui.set_imp_summary("".into());
            ui.set_imp_rows(ModelRc::new(VecModel::from(Vec::<ListRow>::new())));
            ui.set_imp_cred_names(strings(vec!["(aucun)".into()]));
            ui.set_imp_ssh(0);
            ui.set_imp_rdp(0);
            ui.set_imp_ready(false);
            ui.set_dialog_error("".into());
            ui.set_dialog("import".into());
        }
        "imp-browse" => {
            if let Some(p) = pick("Export Remote Desktop Manager", ("Export RDM", &["rdm", "xml"])) {
                ui.set_imp_path(p.to_string_lossy().to_string().into());
                import_preview(ui);
            }
        }
        "imp-preview" => import_preview(ui),
        "imp-apply" => import_apply(ui),
        _ => match id.strip_prefix("adm-").and_then(|n| n.parse().ok()) {
            Some(n) => action(ui, n),
            None => return false,
        },
    }
    true
}

/// Creation / edition form of a NetBox or Proxmox source (same fields as Relay web).
fn open_source_editor(ui: &AppWindow, x: Option<SyncSource>) {
    let creds = with(|s| s.creds.clone());
    let cred_index = |id: &str| creds.iter().position(|c| !id.is_empty() && c.id == id).map_or(0, |i| i as i32 + 1);
    let mut names = vec!["(aucun)".to_string()];
    names.extend(creds.iter().map(|c| c.name.clone()));
    ui.set_ss_cred_names(strings(names));
    let v = x.clone().unwrap_or_else(|| SyncSource {
        kind: "proxmox".into(),
        verify_tls: true,
        ..Default::default()
    });
    with(|s| s.admin.src_edit = x.map(|x| x.id));
    ui.set_ss_title(if v.id.is_empty() { "Nouvelle source" } else { "Modifier la source" }.into());
    ui.set_ss_kind(i32::from(v.kind == "netbox"));
    ui.set_ss_name(v.name.clone().into());
    ui.set_ss_url(v.url.clone().into());
    ui.set_ss_token_id(v.token_id.clone().into());
    ui.set_ss_secret("".into());
    ui.set_ss_has_secret(v.has_secret);
    ui.set_ss_verify_tls(v.verify_tls);
    ui.set_ss_interval(
        if v.interval_min > 0 {
            v.interval_min.to_string()
        } else {
            String::new()
        }
        .into(),
    );
    ui.set_ss_ssh_cred(cred_index(v.option_str("sshCredentialId")));
    ui.set_ss_rdp_cred(cred_index(v.option_str("rdpCredentialId")));
    let netbox = v.kind == "netbox";
    ui.set_ss_opt_a(v.option_on(if netbox { "devices" } else { "guestEntries" }));
    ui.set_ss_opt_b(v.option_on(if netbox { "vms" } else { "nodeEntries" }));
    ui.set_dialog_error("".into());
    ui.set_dialog("sync-src".into());
}

pub fn submit_source(ui: &AppWindow) {
    let Some(src) = source() else { return };
    let (id, creds) = with(|s| (s.admin.src_edit.clone(), s.creds.clone()));
    let netbox = ui.get_ss_kind() == 1;
    let interval = ui.get_ss_interval();
    let interval_min = if interval.trim().is_empty() {
        0
    } else {
        match interval.trim().parse::<i64>() {
            Ok(v) => v,
            Err(_) => return fail(ui, "Intervalle : nombre de minutes attendu."),
        }
    };
    let cred = |i: i32| usize::try_from(i - 1).ok().and_then(|i| creds.get(i)).map(|c| c.id.clone());
    let mut options = serde_json::Map::new();
    // Keys of the other kind are dropped, like the web form.
    for (key, value) in [
        ("sshCredentialId", cred(ui.get_ss_ssh_cred())),
        ("rdpCredentialId", cred(ui.get_ss_rdp_cred())),
    ] {
        if let Some(v) = value {
            options.insert(key.into(), v.into());
        }
    }
    let (a, b) = if netbox {
        ("devices", "vms")
    } else {
        ("guestEntries", "nodeEntries")
    };
    options.insert(a.into(), ui.get_ss_opt_a().into());
    options.insert(b.into(), ui.get_ss_opt_b().into());
    let secret = ui.get_ss_secret().trim().to_string();
    let input = relay_desktop_core::admin::SyncSourceInput {
        kind: if netbox { "netbox" } else { "proxmox" }.into(),
        name: ui.get_ss_name().trim().into(),
        url: ui.get_ss_url().trim().trim_end_matches('/').into(),
        token_id: if netbox {
            String::new()
        } else {
            ui.get_ss_token_id().trim().into()
        },
        secret: (!secret.is_empty()).then_some(secret),
        verify_tls: ui.get_ss_verify_tls(),
        options,
        interval_min,
    };
    if let Err(e) = input.validate(id.is_none()) {
        return fail(ui, e);
    }
    ui.set_ss_secret("".into());
    bg(
        ui,
        "Enregistrement de la source…",
        move || src.save_sync_source(id.as_deref(), &input),
        |ui, r| match r {
            Ok(saved) => {
                act(ui, "sources");
                ui.set_status(format!("Source « {} » enregistrée.", saved.name).into());
            }
            Err(e) => fail(ui, e),
        },
    );
}
