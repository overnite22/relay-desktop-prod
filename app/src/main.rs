// Native Relay client. UI thread owns the state; data source calls run on
// worker threads and post their result back with `invoke_from_event_loop`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod admin;
mod bulk;
mod features;
mod notes;
mod pve;
mod rdp;
mod session;
mod settings;
mod term;
mod update;

use relay_desktop_core::ssh;
use relay_desktop_core::{
    effective_credential, pairing, Credential, CredentialInput, DataSource, Entry, Error, Folder, Kind, LocalSource, SecretField,
    ServerSource, Tree,
};
use settings::{Settings, SourceCfg, SourceKind};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use zeroize::Zeroizing;

slint::include_modules!();

const AGENT: &str = concat!("Relay-Desktop/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, PartialEq)]
enum Sel {
    Folder(String),
    Entry(String),
}

#[derive(Default)]
enum Pending {
    #[default]
    None,
    Unlock(usize),
    Delete(Sel),
    RemoveSource(usize),
    DeleteCredential(String),
    /// Confirmed action of a native administration dialog.
    Run(Box<dyn FnOnce(&AppWindow)>),
}

static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static FONTS: OnceLock<Option<Arc<term::Fonts>>> = OnceLock::new();

fn rt() -> &'static tokio::runtime::Runtime {
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

/// An embedded session tab.
struct Session {
    id: u64,
    entry_id: String,
    title: String,
    icon: &'static str,
    state: &'static str,
    banner: String,
    kind: &'static str,
    term: Option<Arc<Mutex<term::Term>>>,
    input: Option<tokio::sync::mpsc::UnboundedSender<ssh::Input>>,
    rdp: Option<rdp::RdpSession>,
    pve: Option<pve::Console>,
}

struct RdpCreds {
    username: String,
    domain: String,
    password: Option<Zeroizing<String>>,
}

struct SshCreds {
    username: String,
    password: Option<Zeroizing<String>>,
    private_key: Option<Zeroizing<String>>,
    passphrase: Option<Zeroizing<String>>,
}

enum Launch {
    External(String),
    Ssh(SshCreds),
    Rdp(RdpCreds),
    Pve(relay_desktop_core::source::PveConsole),
}

#[derive(Default)]
struct State {
    probes: std::collections::HashMap<String, relay_desktop_core::probe::Probe>,
    cred_edit: Option<String>,
    back_to_creds: bool,
    avail_folder: Option<String>,
    workspaces: Vec<relay_desktop_core::Workspace>,
    sessions: Vec<Session>,
    next_session: u64,
    term_area: (f32, f32),
    session_area: (f32, f32, f32, f32),
    rdp_rect: (i32, i32, i32, i32),
    settings: Settings,
    source: Option<Arc<dyn DataSource>>,
    tree: Tree,
    creds: Vec<Credential>,
    expanded: HashSet<String>,
    rows: Vec<Sel>,
    search: String,
    current: Option<Sel>,
    editing: Option<Sel>,
    folder_choices: Vec<Option<String>>,
    cred_choices: Vec<Option<String>>,
    pending: Pending,
    attachments: Vec<relay_desktop_core::Attachment>,
    att_entry: Option<String>,
    cmd: bulk::Run,
    admin: admin::Admin,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

fn strings(v: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(v.into_iter().map(SharedString::from).collect::<Vec<_>>()))
}

/// Runs `work` on a worker thread and `done` back on the UI thread.
fn bg<T: Send + 'static>(
    ui: &AppWindow,
    status: &str,
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
    done: impl FnOnce(&AppWindow, Result<T, Error>) + Send + 'static,
) {
    ui.set_busy(true);
    ui.set_status(status.into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = work();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_busy(false);
                done(&ui, result);
            }
        });
    });
}

fn source() -> Option<Arc<dyn DataSource>> {
    with(|s| s.source.clone())
}

fn folder_path(tree: &Tree, id: Option<&str>) -> String {
    let mut parts = Vec::new();
    let mut cur = id.map(str::to_owned);
    while let Some(fid) = cur {
        let Some(f) = tree.folders.iter().find(|f| f.id == fid) else {
            break;
        };
        parts.push(f.name.clone());
        cur = f.parent_id.clone();
        if parts.len() > 64 {
            break;
        }
    }
    parts.reverse();
    parts.join(" / ")
}

fn sorted<'a, T>(items: impl Iterator<Item = &'a T>, name: impl Fn(&T) -> &str) -> Vec<&'a T> {
    let mut v: Vec<&T> = items.collect();
    v.sort_by_key(|x| name(x).to_lowercase());
    v
}

fn rebuild(ui: &AppWindow) {
    let (rows, sel) = with(|s| {
        let mut rows = Vec::new();
        let mut refs = Vec::new();
        let q = s.search.trim().to_lowercase();
        if q.is_empty() {
            walk(s, None, 0, &mut rows, &mut refs);
        } else {
            let hits = s
                .tree
                .entries
                .iter()
                .filter(|e| e.name.to_lowercase().contains(&q) || e.host.to_lowercase().contains(&q));
            for e in sorted(hits, |e| &e.name) {
                rows.push(TreeRow {
                    folder: false,
                    depth: 0,
                    expanded: false,
                    children: false,
                    name: e.name.clone().into(),
                    kind: e.kind.as_str().into(),
                    detail: folder_path(&s.tree, e.folder_id.as_deref()).into(),
                    status: features::status_of(s, &e.id),
                    favorite: e.favorite,
                });
                refs.push(Sel::Entry(e.id.clone()));
            }
        }
        let sel = s
            .current
            .as_ref()
            .and_then(|c| refs.iter().position(|r| r == c))
            .map(|i| i as i32)
            .unwrap_or(-1);
        s.rows = refs;
        (rows, sel)
    });
    ui.set_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_selected(sel);
    update_detail(ui);
}

fn walk(s: &State, parent: Option<&str>, depth: i32, rows: &mut Vec<TreeRow>, refs: &mut Vec<Sel>) {
    let tree = &s.tree;
    for f in sorted(tree.folders.iter().filter(|f| f.parent_id.as_deref() == parent), |f| &f.name) {
        let expanded = s.expanded.contains(&f.id);
        let children = tree.folders.iter().any(|c| c.parent_id.as_deref() == Some(&f.id))
            || tree.entries.iter().any(|e| e.folder_id.as_deref() == Some(&f.id));
        let count = tree.entries.iter().filter(|e| e.folder_id.as_deref() == Some(&f.id)).count();
        rows.push(TreeRow {
            folder: true,
            depth,
            expanded,
            children,
            name: f.name.clone().into(),
            kind: if expanded { "folder-open" } else { "folder" }.into(),
            status: 0,
            favorite: false,
            detail: if count > 0 { count.to_string().into() } else { SharedString::new() },
        });
        refs.push(Sel::Folder(f.id.clone()));
        if expanded {
            walk(s, Some(&f.id), depth + 1, rows, refs);
        }
    }
    for e in sorted(tree.entries.iter().filter(|e| e.folder_id.as_deref() == parent), |e| &e.name) {
        rows.push(TreeRow {
            folder: false,
            depth,
            expanded: false,
            children: false,
            name: e.name.clone().into(),
            kind: e.kind.as_str().into(),
            detail: e.host.clone().into(),
            status: features::status_of(s, &e.id),
            favorite: e.favorite,
        });
        refs.push(Sel::Entry(e.id.clone()));
    }
}

fn field(label: &str, value: impl Into<String>) -> Field {
    Field {
        label: label.into(),
        value: value.into().into(),
    }
}

fn update_detail(ui: &AppWindow) {
    with(|s| {
        let cred_name = |id: &str| {
            s.creds
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "(inaccessible)".into())
        };
        let (icon, title, subtitle, fields, notes, can_open, can_edit) = match &s.current {
            Some(Sel::Entry(id)) => match s.tree.entries.iter().find(|e| &e.id == id) {
                Some(e) => {
                    let addr = if e.kind == Kind::Web {
                        e.url.clone()
                    } else {
                        format!("{}:{}", e.host, e.port)
                    };
                    let cred = match (&e.credential_id, effective_credential(&s.tree, e)) {
                        (Some(c), _) if !c.is_empty() => cred_name(c),
                        (_, Some(c)) => format!("Hérité du dossier : {}", cred_name(&c)),
                        _ => "Aucun".into(),
                    };
                    let user = effective_credential(&s.tree, e)
                        .and_then(|c| s.creds.iter().find(|x| x.id == c))
                        .map(|c| {
                            if c.domain.is_empty() {
                                c.username.clone()
                            } else {
                                format!("{}\\{}", c.domain, c.username)
                            }
                        })
                        .filter(|u| !u.is_empty())
                        .unwrap_or_else(|| e.username.clone());
                    let mut fields = vec![
                        field("Protocole", e.kind.label()),
                        field("Adresse", addr),
                        field("Utilisateur", if user.is_empty() { "—".into() } else { user }),
                        field("Identifiant", cred),
                        field("Dossier", {
                            let p = folder_path(&s.tree, e.folder_id.as_deref());
                            if p.is_empty() {
                                "(racine)".into()
                            } else {
                                p
                            }
                        }),
                    ];
                    if e.source_id.is_some() {
                        fields.push(field("Origine", "Synchronisée (NetBox/Proxmox), lecture seule"));
                    }
                    if !e.updated_at.is_empty() {
                        fields.push(field(
                            "Modifiée le",
                            e.updated_at.replace('T', " ").chars().take(19).collect::<String>(),
                        ));
                    }
                    (
                        e.kind.as_str(),
                        e.name.clone(),
                        format!("{} · {}", e.kind.label(), e.host),
                        fields,
                        e.notes.clone(),
                        true,
                        e.source_id.is_none(),
                    )
                }
                None => Default::default(),
            },
            Some(Sel::Folder(id)) => match s.tree.folders.iter().find(|f| &f.id == id) {
                Some(f) => {
                    let subs = s.tree.folders.iter().filter(|c| c.parent_id.as_deref() == Some(id)).count();
                    let entries = s.tree.entries.iter().filter(|e| e.folder_id.as_deref() == Some(id)).count();
                    let defaults: Vec<String> = f
                        .credentials
                        .iter()
                        .filter_map(|(k, v)| {
                            v.as_ref()
                                .map(|c| format!("{} : {}", Kind::parse(k).map(|k| k.label()).unwrap_or(k), cred_name(c)))
                        })
                        .collect();
                    let fields = vec![
                        field("Chemin", folder_path(&s.tree, Some(id))),
                        field("Sous-dossiers", subs.to_string()),
                        field("Entrées", entries.to_string()),
                        field(
                            "Identifiants par défaut",
                            if defaults.is_empty() { "Aucun".into() } else { defaults.join(", ") },
                        ),
                    ];
                    ("folder", f.name.clone(), "Dossier".into(), fields, String::new(), false, true)
                }
                None => Default::default(),
            },
            None => Default::default(),
        };
        ui.set_detail_kind(icon.into());
        ui.set_detail_title(title.into());
        ui.set_detail_subtitle(subtitle.into());
        ui.set_detail_fields(ModelRc::new(VecModel::from(fields)));
        ui.set_detail_notes(notes.into());
        ui.set_can_open(can_open);
        ui.set_can_edit(can_edit);
        let rdp_entry = matches!(&s.current, Some(Sel::Entry(id)) if s.tree.entries.iter().any(|e| &e.id == id && e.kind == Kind::Rdp));
        ui.set_can_open_external(rdp_entry);
    });
    notes::show(ui, &ui.get_detail_notes());
    features::update_flags(ui);
}

fn show_sources(ui: &AppWindow, active: Option<&str>) {
    let (names, idx) = with(|s| {
        let names = s.settings.sources.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        let idx = active
            .and_then(|id| s.settings.sources.iter().position(|c| c.id == id))
            .map(|i| i as i32)
            .unwrap_or(-1);
        (names, idx)
    });
    ui.set_source_names(strings(names));
    ui.set_source_index(idx);
}

fn disconnect(ui: &AppWindow) {
    with(|s| {
        s.source = None;
        s.tree = Tree::default();
        s.creds.clear();
        s.current = None;
        s.expanded.clear();
        s.probes.clear();
    });
    ui.set_connected(false);
    ui.set_server_mode(false);
    ui.set_status_counts("".into());
    ui.set_user_label("".into());
    rebuild(ui);
}

fn attach(ui: &AppWindow, cfg: &SourceCfg, src: Arc<dyn DataSource>, label: String) {
    with(|s| {
        s.source = Some(src);
        s.settings.last = Some(cfg.id.clone());
        let _ = s.settings.save();
    });
    ui.set_connected(true);
    ui.set_server_mode(with(|s| s.source.as_ref().is_some_and(|x| x.web_url().is_some())));
    ui.set_user_label(label.into());
    ui.set_dialog("".into());
    ui.set_dialog_error("".into());
    load(ui);
}

fn activate(ui: &AppWindow, idx: usize) {
    let Some(cfg) = with(|s| s.settings.sources.get(idx).cloned()) else {
        return;
    };
    disconnect(ui);
    show_sources(ui, Some(&cfg.id));
    match &cfg.kind {
        SourceKind::Server { url } => {
            let Some(token) = settings::load_token(&cfg.id) else {
                ui.set_status("Jeton d'appareil absent : retirez puis ajoutez de nouveau cette source.".into());
                return;
            };
            let url = url.clone();
            let token = Zeroizing::new(token);
            bg(
                ui,
                &format!("Connexion à {url}…"),
                move || ServerSource::connect(&url, &token, AGENT),
                move |ui, r| match r {
                    Ok(src) => {
                        let label = format!("{} · {}", src.user(), src.server());
                        attach(ui, &cfg, Arc::new(src), label);
                    }
                    Err(e) => ui.set_status(format!("{} : {e}", cfg.name).into()),
                },
            );
        }
        SourceKind::Local { .. } => {
            with(|s| s.pending = Pending::Unlock(idx));
            ui.set_unlock_name(cfg.name.clone().into());
            ui.set_unlock_password("".into());
            ui.set_dialog_error("".into());
            ui.set_dialog("unlock".into());
            ui.set_status("Base locale verrouillée".into());
        }
    }
}

fn load(ui: &AppWindow) {
    let Some(src) = source() else { return };
    bg(
        ui,
        "Chargement de l'inventaire…",
        move || Ok((src.tree()?, src.credentials()?)),
        |ui, r| match r {
            Ok((tree, creds)) => {
                let n = tree.entries.len();
                with(|s| {
                    s.tree = tree;
                    s.creds = creds;
                    let valid = match &s.current {
                        Some(Sel::Entry(id)) => s.tree.entries.iter().any(|e| &e.id == id),
                        Some(Sel::Folder(id)) => s.tree.folders.iter().any(|f| &f.id == id),
                        None => true,
                    };
                    if !valid {
                        s.current = None;
                    }
                });
                rebuild(ui);
                features::after_load(ui);
                ui.set_status(format!("{n} entrée(s)").into());
            }
            Err(e) => ui.set_status(format!("Chargement impossible : {e}").into()),
        },
    );
}

fn selected_folder(s: &State) -> Option<String> {
    match &s.current {
        Some(Sel::Folder(id)) => Some(id.clone()),
        Some(Sel::Entry(id)) => s.tree.entries.iter().find(|e| &e.id == id).and_then(|e| e.folder_id.clone()),
        None => None,
    }
}

/// Fills the folder and credential pickers; returns the index of `folder`.
fn fill_choices(ui: &AppWindow, folder: Option<&str>, exclude: Option<&str>) -> i32 {
    with(|s| {
        let mut folders: Vec<(String, String)> = s
            .tree
            .folders
            .iter()
            .filter(|f| {
                exclude.is_none_or(|x| f.id != x && !folder_path(&s.tree, Some(&f.id)).starts_with(&(folder_path(&s.tree, Some(x)) + " /")))
            })
            .map(|f| (f.id.clone(), folder_path(&s.tree, Some(&f.id))))
            .collect();
        folders.sort_by_key(|(_, p)| p.to_lowercase());
        s.folder_choices = std::iter::once(None)
            .chain(folders.iter().map(|(id, _)| Some(id.clone())))
            .collect();
        ui.set_folder_names(strings(
            std::iter::once("(racine)".to_string())
                .chain(folders.into_iter().map(|(_, p)| p))
                .collect(),
        ));
        s.cred_choices = std::iter::once(None).chain(s.creds.iter().map(|c| Some(c.id.clone()))).collect();
        ui.set_credential_names(strings(
            std::iter::once("(aucun — hérité du dossier)".to_string())
                .chain(s.creds.iter().map(|c| {
                    if c.username.is_empty() {
                        c.name.clone()
                    } else {
                        format!("{} ({})", c.name, c.username)
                    }
                }))
                .collect(),
        ));
        s.folder_choices.iter().position(|c| c.as_deref() == folder).unwrap_or(0) as i32
    })
}

fn open_entry_dialog(ui: &AppWindow, entry: Option<Entry>) {
    let folder = entry
        .as_ref()
        .map(|e| e.folder_id.clone())
        .unwrap_or_else(|| with(|s| selected_folder(s)));
    let fi = fill_choices(ui, folder.as_deref(), None);
    let e = entry.clone().unwrap_or_else(|| Entry::new(Kind::Rdp));
    let ci = with(|s| {
        s.cred_choices
            .iter()
            .position(|c| c.is_some() && *c == e.credential_id)
            .unwrap_or(0) as i32
    });
    with(|s| s.editing = entry.as_ref().map(|e| Sel::Entry(e.id.clone())));
    ui.set_e_title(
        if entry.is_some() {
            "Modifier l'entrée"
        } else {
            "Nouvelle entrée"
        }
        .into(),
    );
    ui.set_e_name(e.name.clone().into());
    ui.set_e_kind(Kind::EDITABLE.iter().position(|k| *k == e.kind).unwrap_or(0) as i32);
    ui.set_e_host(e.host.clone().into());
    ui.set_e_port(if entry.is_some() { e.port.to_string() } else { String::new() }.into());
    ui.set_e_url(e.url.clone().into());
    ui.set_e_username(e.username.clone().into());
    ui.set_e_cred(ci);
    ui.set_e_folder(fi);
    ui.set_e_notes(e.notes.clone().into());
    ui.set_dialog_error("".into());
    ui.set_dialog("entry".into());
}

fn open_folder_dialog(ui: &AppWindow, folder: Option<Folder>) {
    let parent = folder
        .as_ref()
        .map(|f| f.parent_id.clone())
        .unwrap_or_else(|| with(|s| selected_folder(s)));
    let exclude = folder.as_ref().map(|f| f.id.clone());
    let pi = fill_choices(ui, parent.as_deref(), exclude.as_deref());
    with(|s| s.editing = folder.as_ref().map(|f| Sel::Folder(f.id.clone())));
    ui.set_f_title(
        if folder.is_some() {
            "Modifier le dossier"
        } else {
            "Nouveau dossier"
        }
        .into(),
    );
    ui.set_f_name(folder.map(|f| f.name).unwrap_or_default().into());
    ui.set_f_parent(pi);
    ui.set_dialog_error("".into());
    ui.set_dialog("folder".into());
}

fn close_dialog(ui: &AppWindow) {
    if ui.get_dialog() == "cmd" {
        bulk::stop();
    }
    if ui.get_dialog() == "update" {
        update::cancel();
    }
    with(|s| {
        s.pending = Pending::None;
        s.editing = None;
        s.cred_edit = None;
        s.back_to_creds = false;
    });
    for clear in [
        AppWindow::set_src_password,
        AppWindow::set_src_password2,
        AppWindow::set_src_token,
        AppWindow::set_unlock_password,
        AppWindow::set_c_password,
        AppWindow::set_c_totp,
        AppWindow::set_q_password,
    ] {
        clear(ui, "".into());
    }
    ui.set_dialog_error("".into());
    ui.set_dialog("".into());
}

fn fail(ui: &AppWindow, msg: impl ToString) {
    ui.set_dialog_error(msg.to_string().into());
}

fn submit(ui: &AppWindow) {
    if features::submit(ui) || notes::submit(ui) {
        return;
    }
    match ui.get_dialog().as_str() {
        "update" => update::install(ui),
        "source" => submit_source(ui),
        "unlock" => {
            let Pending::Unlock(idx) = with(|s| std::mem::take(&mut s.pending)) else {
                return;
            };
            let Some(cfg) = with(|s| s.settings.sources.get(idx).cloned()) else {
                return;
            };
            let SourceKind::Local { path } = cfg.kind.clone() else { return };
            with(|s| s.pending = Pending::Unlock(idx));
            let pw = Zeroizing::new(ui.get_unlock_password().to_string());
            let p = PathBuf::from(&path);
            bg(
                ui,
                "Déverrouillage…",
                move || LocalSource::open(&p, &pw),
                move |ui, r| match r {
                    Ok(src) => {
                        close_dialog(ui);
                        attach(ui, &cfg, Arc::new(src), format!("Base locale · {path}"));
                    }
                    Err(e) => fail(ui, e),
                },
            );
        }
        "entry" => {
            let Some(src) = source() else { return };
            let (mut e, folder, cred) = with(|s| {
                let base = match &s.editing {
                    Some(Sel::Entry(id)) => s.tree.entries.iter().find(|e| &e.id == id).cloned(),
                    _ => None,
                };
                let folder = s.folder_choices.get(ui.get_e_folder() as usize).cloned().flatten();
                let cred = s.cred_choices.get(ui.get_e_cred() as usize).cloned().flatten();
                (base.unwrap_or_else(|| Entry::new(Kind::Rdp)), folder, cred)
            });
            let kind = Kind::EDITABLE.get(ui.get_e_kind() as usize).copied().unwrap_or(Kind::Rdp);
            let port = ui.get_e_port().trim().to_string();
            e.port = if port.is_empty() {
                kind.default_port()
            } else {
                match port.parse::<u16>() {
                    Ok(p) if p > 0 => p,
                    _ => return fail(ui, "Port invalide (1 à 65535)."),
                }
            };
            e.kind = kind;
            e.name = ui.get_e_name().trim().into();
            e.host = ui.get_e_host().trim().into();
            e.url = ui.get_e_url().trim().into();
            e.username = ui.get_e_username().trim().into();
            e.notes = ui.get_e_notes().into();
            e.folder_id = folder;
            e.credential_id = cred;
            bg(
                ui,
                "Enregistrement…",
                move || src.save_entry(&e),
                |ui, r| match r {
                    Ok(saved) => {
                        with(|s| {
                            if let Some(f) = &saved.folder_id {
                                expand_to(s, f);
                            }
                            s.current = Some(Sel::Entry(saved.id.clone()));
                        });
                        close_dialog(ui);
                        load(ui);
                    }
                    Err(e) => fail(ui, e),
                },
            );
        }
        "folder" => {
            let Some(src) = source() else { return };
            let f = with(|s| {
                let mut f = match &s.editing {
                    Some(Sel::Folder(id)) => s.tree.folders.iter().find(|f| &f.id == id).cloned().unwrap_or_default(),
                    _ => Folder::default(),
                };
                f.parent_id = s.folder_choices.get(ui.get_f_parent() as usize).cloned().flatten();
                f
            });
            let f = Folder {
                name: ui.get_f_name().trim().into(),
                ..f
            };
            bg(
                ui,
                "Enregistrement…",
                move || src.save_folder(&f),
                |ui, r| match r {
                    Ok(saved) => {
                        with(|s| {
                            if let Some(p) = &saved.parent_id {
                                expand_to(s, p);
                            }
                            s.current = Some(Sel::Folder(saved.id.clone()));
                        });
                        close_dialog(ui);
                        load(ui);
                    }
                    Err(e) => fail(ui, e),
                },
            );
        }
        "credential" => {
            let Some(src) = source() else { return };
            let pw = ui.get_c_password().to_string();
            let input = CredentialInput {
                name: ui.get_c_name().trim().into(),
                username: ui.get_c_username().trim().into(),
                domain: ui.get_c_domain().trim().into(),
                notes: ui.get_c_notes().into(),
                password: if pw.is_empty() { None } else { Some(pw) },
                ..Default::default()
            };
            bg(
                ui,
                "Enregistrement de l'identifiant…",
                move || {
                    let mut input = input;
                    let r = src.save_credential(None, &input);
                    zeroize::Zeroize::zeroize(&mut input);
                    r
                },
                |ui, r| match r {
                    Ok(c) => {
                        close_dialog(ui);
                        ui.set_status(format!("Identifiant « {} » enregistré", c.name).into());
                        load(ui);
                    }
                    Err(e) => fail(ui, e),
                },
            );
        }
        "confirm" => match with(|s| std::mem::take(&mut s.pending)) {
            Pending::Delete(sel) => {
                let Some(src) = source() else { return };
                bg(
                    ui,
                    "Suppression…",
                    move || match &sel {
                        Sel::Entry(id) => src.delete_entry(id),
                        Sel::Folder(id) => src.delete_folder(id),
                    },
                    |ui, r| match r {
                        Ok(()) => {
                            with(|s| s.current = None);
                            close_dialog(ui);
                            load(ui);
                        }
                        Err(e) => fail(ui, e),
                    },
                );
            }
            Pending::RemoveSource(idx) => {
                let removed = with(|s| {
                    let cfg = s.settings.sources.remove(idx);
                    if s.settings.last.as_deref() == Some(&cfg.id) {
                        s.settings.last = None;
                    }
                    let _ = s.settings.save();
                    cfg
                });
                if matches!(removed.kind, SourceKind::Server { .. }) {
                    settings::delete_token(&removed.id);
                }
                close_dialog(ui);
                disconnect(ui);
                show_sources(ui, None);
                ui.set_status(format!("Source « {} » retirée (aucun fichier supprimé)", removed.name).into());
            }
            _ => close_dialog(ui),
        },
        _ => {}
    }
}

fn submit_source(ui: &AppWindow) {
    let name = ui.get_src_name().trim().to_string();
    if ui.get_src_type() == 0 {
        let p = match pairing::parse(&ui.get_src_url(), &ui.get_src_token()) {
            Ok(p) => p,
            Err(e) => return fail(ui, e),
        };
        let name = if name.is_empty() {
            p.server.trim_start_matches("https://").to_string()
        } else {
            name
        };
        let token = Zeroizing::new(p.token.clone());
        let server = p.server.clone();
        bg(
            ui,
            &format!("Connexion à {server}…"),
            move || ServerSource::connect(&server, &token, AGENT),
            move |ui, r| match r {
                Ok(src) => {
                    let cfg = SourceCfg {
                        id: uuid::Uuid::new_v4().to_string(),
                        name,
                        kind: SourceKind::Server { url: p.server.clone() },
                    };
                    if let Err(e) = settings::save_token(&cfg.id, &p.token) {
                        return fail(ui, e);
                    }
                    with(|s| s.settings.sources.push(cfg.clone()));
                    show_sources(ui, Some(&cfg.id));
                    let label = format!("{} · {}", src.user(), src.server());
                    close_dialog(ui);
                    attach(ui, &cfg, Arc::new(src), label);
                }
                Err(e) => fail(ui, e),
            },
        );
    } else {
        let path = ui.get_src_path().trim().to_string();
        if path.is_empty() {
            return fail(ui, "Choisissez le fichier de la base.");
        }
        let create = ui.get_src_create();
        let pw = Zeroizing::new(ui.get_src_password().to_string());
        if create && *pw != ui.get_src_password2().as_str() {
            return fail(ui, "Les deux mots de passe ne correspondent pas.");
        }
        let name = if name.is_empty() { "Base locale".to_string() } else { name };
        let p = path.clone();
        bg(
            ui,
            if create {
                "Création de la base…"
            } else {
                "Ouverture de la base…"
            },
            move || {
                if create {
                    LocalSource::create(&PathBuf::from(&p), &pw)
                } else {
                    LocalSource::open(&PathBuf::from(&p), &pw)
                }
            },
            move |ui, r| match r {
                Ok(src) => {
                    let cfg = SourceCfg {
                        id: uuid::Uuid::new_v4().to_string(),
                        name,
                        kind: SourceKind::Local { path: path.clone() },
                    };
                    with(|s| s.settings.sources.push(cfg.clone()));
                    show_sources(ui, Some(&cfg.id));
                    close_dialog(ui);
                    attach(ui, &cfg, Arc::new(src), format!("Base locale · {path}"));
                }
                Err(e) => fail(ui, e),
            },
        );
    }
}

fn expand_to(s: &mut State, folder: &str) {
    let mut cur = Some(folder.to_owned());
    while let Some(id) = cur {
        cur = s.tree.folders.iter().find(|f| f.id == id).and_then(|f| f.parent_id.clone());
        s.expanded.insert(id);
        if s.expanded.len() > 10_000 {
            break;
        }
    }
}

fn open_session(ui: &AppWindow, external: bool) {
    let Some(src) = source() else { return };
    let Some((entry, tree, creds)) = with(|s| match &s.current {
        Some(Sel::Entry(id)) => s
            .tree
            .entries
            .iter()
            .find(|e| &e.id == id)
            .cloned()
            .map(|e| (e, s.tree.clone(), s.creds.clone())),
        _ => None,
    }) else {
        return;
    };
    let name = entry.name.clone();
    let launched = entry.clone();
    bg(
        ui,
        &format!("Ouverture de {name}…"),
        move || {
            if entry.kind == Kind::Pve {
                return src.pve_console(&entry.id).map(Launch::Pve);
            }
            let cred_id = effective_credential(&tree, &entry);
            let cred = cred_id.as_ref().and_then(|id| creds.iter().find(|c| &c.id == id));
            let reveal = |field: SecretField, has: bool| -> Result<Option<Zeroizing<String>>, Error> {
                match cred {
                    Some(c) if has => Ok(Some(Zeroizing::new(src.reveal(&c.id, field)?))),
                    _ => Ok(None),
                }
            };
            let username = cred
                .map(|c| c.username.clone())
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| entry.username.clone());
            if entry.kind == Kind::Rdp && !external {
                return Ok(Launch::Rdp(RdpCreds {
                    username,
                    domain: cred.map(|c| c.domain.clone()).unwrap_or_default(),
                    password: reveal(SecretField::Password, cred.is_some_and(|c| c.has_password))?,
                }));
            }
            if entry.kind == Kind::Ssh {
                let c = cred.cloned().unwrap_or_default();
                return Ok(Launch::Ssh(SshCreds {
                    username,
                    password: reveal(SecretField::Password, c.has_password)?,
                    private_key: reveal(SecretField::PrivateKey, c.has_private_key)?,
                    passphrase: reveal(SecretField::Passphrase, c.has_passphrase)?,
                }));
            }
            let target = session::Target {
                kind: entry.kind,
                host: entry.host.clone(),
                port: entry.port,
                url: entry.url.clone(),
                username,
                domain: cred.map(|c| c.domain.clone()).unwrap_or_default(),
                password: if entry.kind == Kind::Rdp {
                    reveal(SecretField::Password, cred.is_some_and(|c| c.has_password))?
                } else {
                    None
                },
            };
            session::open(target).map(Launch::External).map_err(Error::Invalid)
        },
        move |ui, r| match r {
            Ok(Launch::External(msg)) => ui.set_status(msg.into()),
            Ok(Launch::Ssh(c)) => open_ssh(ui, &launched, c),
            Ok(Launch::Rdp(c)) => open_rdp(ui, &launched, c),
            Ok(Launch::Pve(c)) => open_pve(ui, &launched, c),
            Err(e) => ui.set_status(format!("Ouverture impossible : {e}").into()),
        },
    );
}

fn open_pve(ui: &AppWindow, entry: &Entry, ticket: relay_desktop_core::source::PveConsole) {
    let id = with(|s| {
        s.next_session += 1;
        s.next_session
    });
    let w = ui.as_weak();
    let console = pve::spawn(ticket, move |state, banner| session_event(w.clone(), id, state, banner));
    let index = with(|s| {
        s.sessions.push(Session {
            id,
            entry_id: entry.id.clone(),
            title: entry.name.clone(),
            icon: "pve",
            state: "connecting",
            banner: "Connexion console Proxmox…".into(),
            kind: "pve",
            term: None,
            input: None,
            rdp: None,
            pve: Some(console),
        });
        s.sessions.len() as i32 - 1
    });
    show_tabs(ui);
    ui.set_active_tab(index);
    redraw(ui);
}

fn show_tabs(ui: &AppWindow) {
    let tabs: Vec<SessionTab> = with(|s| {
        s.sessions
            .iter()
            .map(|x| SessionTab {
                title: x.title.clone().into(),
                kind: x.icon.into(),
                state: x.state.into(),
            })
            .collect()
    });
    ui.set_tabs(ModelRc::new(VecModel::from(tabs)));
}

fn active_session<R>(ui: &AppWindow, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    let i = ui.get_active_tab();
    with(|s| if i >= 0 { s.sessions.get_mut(i as usize).map(f) } else { None })
}

fn redraw(ui: &AppWindow) {
    let frame = active_session(ui, |x| {
        (
            x.pve
                .as_ref()
                .and_then(|c| c.frame.lock().unwrap().image())
                .or_else(|| x.term.as_ref().map(|t| t.lock().unwrap().render())),
            x.banner.clone(),
            x.kind,
        )
    });
    if let Some((buf, banner, kind)) = frame {
        if let Some(buf) = buf {
            ui.set_term_image(slint::Image::from_rgba8(buf));
        } else {
            ui.set_term_image(slint::Image::default());
        }
        ui.set_session_banner(banner.into());
        ui.set_active_kind(kind.into());
    }
}

/// Fits the active terminal to the visible area and tells the server.
fn fit_terminal(ui: &AppWindow) {
    let (w, h) = with(|s| s.term_area);
    let scale = ui.window().scale_factor();
    active_session(ui, |x| {
        let (Some(term), Some(input)) = (&x.term, &x.input) else { return };
        let mut t = term.lock().unwrap();
        let (rows, cols) = t.grid_for(w * scale - 8.0 * scale, h * scale - 8.0 * scale);
        if (rows, cols) != t.size() {
            t.resize(rows, cols);
            let _ = input.send(ssh::Input::Resize(cols, rows));
        }
    });
    redraw(ui);
}

fn session_event(weak: slint::Weak<AppWindow>, id: u64, state: &'static str, banner: String) {
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = weak.upgrade() else { return };
        with(|s| {
            if let Some(x) = s.sessions.iter_mut().find(|x| x.id == id) {
                x.state = state;
                x.banner = banner.clone();
            }
        });
        if !banner.is_empty() {
            ui.set_status(banner.into());
        } else if state == "connected" {
            ui.set_status("Session connectée".into());
        }
        show_tabs(&ui);
        redraw(&ui);
    });
}

fn open_ssh(ui: &AppWindow, entry: &Entry, c: SshCreds) {
    let Some(fonts) = FONTS.get_or_init(term::Fonts::load).clone() else {
        ui.set_status("Police à chasse fixe introuvable (Consolas).".into());
        return;
    };
    let scale = ui.window().scale_factor();
    ui.set_scale(scale);
    let term = Arc::new(Mutex::new(term::Term::new(fonts, (14.0 * scale).round(), 30, 100)));
    let redraw_flag = Arc::new(AtomicBool::new(false));
    let id = with(|s| {
        s.next_session += 1;
        s.next_session
    });
    let params = ssh::Params {
        host: entry.host.clone(),
        port: entry.port,
        username: c.username.clone(),
        password: c.password,
        private_key: c.private_key,
        passphrase: c.passphrase,
        cols: 100,
        rows: 30,
        known_hosts: settings::dir().join("known_hosts.json"),
    };
    let (weak, t2, flag) = (ui.as_weak(), term.clone(), redraw_flag.clone());
    let input = ssh::spawn(rt().handle(), params, move |ev| match ev {
        ssh::Event::Data(d) => {
            t2.lock().unwrap().feed(&d);
            if !flag.swap(true, Ordering::AcqRel) {
                let (weak, flag) = (weak.clone(), flag.clone());
                let _ = slint::invoke_from_event_loop(move || {
                    flag.store(false, Ordering::Release);
                    if let Some(ui) = weak.upgrade() {
                        if active_session(&ui, |x| x.id) == Some(id) {
                            redraw(&ui);
                        }
                    }
                });
            }
        }
        ssh::Event::Connected { fingerprint, new_key } => {
            let note = if new_key {
                format!("Première connexion : clé d'hôte enregistrée ({fingerprint})")
            } else {
                String::new()
            };
            session_event(weak.clone(), id, "connected", note);
        }
        ssh::Event::Closed(err) => {
            let msg = match err {
                Some(e) => format!("Déconnecté : {e}"),
                None => "Session terminée".to_string(),
            };
            session_event(weak.clone(), id, "closed", msg);
        }
    });
    let title = if c.username.is_empty() {
        entry.name.clone()
    } else {
        format!("{} ({})", entry.name, c.username)
    };
    let index = with(|s| {
        s.sessions.push(Session {
            id,
            title,
            icon: "ssh",
            entry_id: entry.id.clone(),
            state: "connecting",
            banner: format!("Connexion à {}:{}…", entry.host, entry.port),
            kind: "ssh",
            term: Some(term),
            input: Some(input),
            rdp: None,
            pve: None,
        });
        s.sessions.len() as i32 - 1
    });
    show_tabs(ui);
    ui.set_active_tab(index);
    fit_terminal(ui);
}

/// Window handle of the Slint window, parent of the RDP controls.
fn parent_hwnd(ui: &AppWindow) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match ui.window().window_handle().window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

fn open_rdp(ui: &AppWindow, entry: &Entry, c: RdpCreds) {
    let Some(parent) = parent_hwnd(ui) else {
        ui.set_status("Fenêtre principale introuvable pour la session RDP.".into());
        return;
    };
    let scale = ui.window().scale_factor();
    let (_, _, w, h) = with(|s| s.session_area);
    let (w, h) = if w > 100.0 && h > 100.0 {
        ((w * scale) as u32, (h * scale) as u32)
    } else {
        (1280, 800)
    };
    let user = if c.domain.is_empty() || c.username.contains('\\') || c.username.contains('@') {
        c.username.clone()
    } else {
        format!("{}\\{}", c.domain, c.username)
    };
    let params = rdp::Params {
        host: entry.host.clone(),
        port: entry.port,
        username: c.username.clone(),
        domain: c.domain.clone(),
        password: c.password,
        width: w,
        height: h,
    };
    let rect = with(|s| s.rdp_rect);
    match rdp::RdpSession::connect(parent, rect, &params) {
        Ok(session) => {
            let id = with(|s| {
                s.next_session += 1;
                s.next_session
            });
            let title = if user.is_empty() {
                entry.name.clone()
            } else {
                format!("{} ({user})", entry.name)
            };
            let index = with(|s| {
                s.sessions.push(Session {
                    id,
                    title,
                    icon: "rdp",
                    entry_id: entry.id.clone(),
                    state: "connecting",
                    banner: String::new(),
                    kind: "rdp",
                    term: None,
                    input: None,
                    rdp: Some(session),
                    pve: None,
                });
                s.sessions.len() as i32 - 1
            });
            show_tabs(ui);
            ui.set_active_tab(index);
            redraw(ui);
            ui.set_status(format!("Connexion RDP à {}:{}…", entry.host, entry.port).into());
        }
        Err(e) => ui.set_status(format!("Session RDP impossible : {e}").into()),
    }
}

/// Keeps the RDP child windows over the active tab ("airspace": they are
/// hidden when another tab, a dialog or a minimized window would be covered)
/// and follows their connection state.
fn sync_rdp(ui: &AppWindow) {
    let active = ui.get_active_tab();
    let covered = !ui.get_dialog().is_empty() || ui.window().is_minimized();
    let scale = ui.window().scale_factor();
    let (ax, ay, aw, ah) = with(|s| s.session_area);
    let rect = (
        (ax * scale).round() as i32,
        (ay * scale).round() as i32,
        (aw * scale).round() as i32,
        (ah * scale).round() as i32,
    );
    let mut changed = Vec::new();
    with(|s| {
        let stable = s.rdp_rect == rect;
        s.rdp_rect = rect;
        for (i, x) in s.sessions.iter_mut().enumerate() {
            let Some(r) = x.rdp.as_mut() else { continue };
            let state = match r.state() {
                rdp::State::Connected => "connected",
                rdp::State::Connecting => "connecting",
                rdp::State::Disconnected if x.state == "connecting" || x.state == "connected" => "closed",
                rdp::State::Disconnected => x.state,
            };
            if state != x.state {
                x.state = state;
                if state == "closed" {
                    x.banner = "Session Bureau à distance terminée".into();
                }
                if i as i32 == active {
                    ui.set_status(
                        if state == "closed" {
                            x.banner.clone()
                        } else if state == "connected" {
                            format!("RDP connecté : {}", x.title)
                        } else {
                            format!("Connexion RDP : {}…", x.title)
                        }
                        .into(),
                    );
                }
                changed.push(x.title.clone());
            }
            let visible = i as i32 == active && !covered && x.state != "closed" && rect.2 > 0 && rect.3 > 0;
            r.place(rect, visible);
            if visible && stable && x.state == "connected" {
                r.resize_desktop(rect.2 as u32, rect.3 as u32);
            }
        }
    });
    if !changed.is_empty() {
        show_tabs(ui);
        redraw(ui);
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    ui.window().set_maximized(true);
    with(|s| s.settings = Settings::load());
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    ui.set_dark_mode(with(|s| s.settings.theme.as_deref() != Some("light")));
    ui.set_status("Prêt".into());

    let w = ui.as_weak();
    let h = move |f: fn(&AppWindow)| {
        let w = w.clone();
        move || {
            if let Some(ui) = w.upgrade() {
                f(&ui)
            }
        }
    };
    ui.on_add_source(h(|ui| {
        ui.set_src_type(0);
        ui.set_src_name("".into());
        ui.set_src_url("".into());
        ui.set_src_create(true);
        ui.set_src_path(settings::dir().join("Relay.rdb").to_string_lossy().to_string().into());
        ui.set_dialog_error("".into());
        ui.set_dialog("source".into());
    }));
    ui.on_remove_source(h(|ui| {
        let idx = ui.get_source_index();
        let Some(cfg) = with(|s| s.settings.sources.get(idx as usize).cloned()) else {
            return;
        };
        with(|s| s.pending = Pending::RemoveSource(idx as usize));
        ui.set_confirm_text(
            format!(
                "Retirer la source « {} » de cette liste ? Les données ne sont pas supprimées.",
                cfg.name
            )
            .into(),
        );
        ui.set_dialog_error("".into());
        ui.set_dialog("confirm".into());
    }));
    ui.on_refresh(h(load));
    ui.on_open_session(h(|ui| open_session(ui, false)));
    ui.on_open_external(h(|ui| open_session(ui, true)));
    ui.on_new_entry(h(|ui| open_entry_dialog(ui, None)));
    ui.on_new_folder(h(|ui| open_folder_dialog(ui, None)));
    ui.on_edit_selected(h(|ui| match with(|s| s.current.clone()) {
        Some(Sel::Entry(id)) => {
            if let Some(e) = with(|s| s.tree.entries.iter().find(|e| e.id == id).cloned()) {
                open_entry_dialog(ui, Some(e));
            }
        }
        Some(Sel::Folder(id)) => {
            if let Some(f) = with(|s| s.tree.folders.iter().find(|f| f.id == id).cloned()) {
                open_folder_dialog(ui, Some(f));
            }
        }
        None => {}
    }));
    ui.on_delete_selected(h(|ui| {
        let Some((sel, label)) = with(|s| {
            let sel = s.current.clone()?;
            let label = match &sel {
                Sel::Entry(id) => format!("l'entrée « {} »", s.tree.entries.iter().find(|e| &e.id == id)?.name),
                Sel::Folder(id) => format!(
                    "le dossier « {} » (il doit être vide)",
                    s.tree.folders.iter().find(|f| &f.id == id)?.name
                ),
            };
            Some((sel, label))
        }) else {
            return;
        };
        with(|s| s.pending = Pending::Delete(sel));
        ui.set_confirm_text(format!("Supprimer définitivement {label} ?").into());
        ui.set_dialog_error("".into());
        ui.set_dialog("confirm".into());
    }));
    ui.on_new_credential(h(|ui| {
        with(|s| {
            s.cred_edit = None;
            s.back_to_creds = false;
        });
        ui.set_c_title("Nouvel identifiant".into());
        ui.set_c_totp("".into());
        for set in [
            AppWindow::set_c_name,
            AppWindow::set_c_username,
            AppWindow::set_c_domain,
            AppWindow::set_c_password,
            AppWindow::set_c_notes,
        ] {
            set(ui, "".into());
        }
        ui.set_dialog_error("".into());
        ui.set_dialog("credential".into());
    }));
    let w = ui.as_weak();
    ui.on_open_link(move |url| {
        if let Some(ui) = w.upgrade() {
            notes::open_link(&ui, &url);
        }
    });
    let w = ui.as_weak();
    ui.on_act(move |id| {
        if let Some(ui) = w.upgrade() {
            features::act(&ui, &id);
        }
    });
    let w = ui.as_weak();
    ui.on_cred_selected_changed(move |_| {
        if let Some(ui) = w.upgrade() {
            ui.set_cred_info("".into());
        }
    });
    let w = ui.as_weak();
    let probe_timer = slint::Timer::default();
    probe_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(60), move || {
        if let Some(ui) = w.upgrade() {
            if ui.get_connected() {
                features::probe_entries(&ui, None);
            }
        }
    });
    ui.on_submit_dialog(h(submit));
    ui.on_check_updates(h(update::check));
    ui.on_update_auto_toggled(|on| {
        with(|s| {
            s.settings.auto_update = on;
            let _ = s.settings.save();
        })
    });
    ui.on_cancel_dialog(h(close_dialog));
    ui.on_browse_path(h(|ui| {
        #[cfg(windows)]
        {
            let dlg = rfd::FileDialog::new()
                .add_filter("Base Relay", &["rdb"])
                .set_directory(settings::dir());
            let picked = if ui.get_src_create() {
                dlg.set_file_name("Relay.rdb").save_file()
            } else {
                dlg.pick_file()
            };
            if let Some(p) = picked {
                ui.set_src_path(p.to_string_lossy().to_string().into());
            }
        }
        #[cfg(not(windows))]
        let _ = ui;
    }));

    let w = ui.as_weak();
    ui.on_source_selected(move |i| {
        if let Some(ui) = w.upgrade() {
            if i >= 0 {
                activate(&ui, i as usize);
            }
        }
    });
    let w = ui.as_weak();
    ui.on_search_edited(move |q| {
        with(|s| s.search = q.to_string());
        if let Some(ui) = w.upgrade() {
            rebuild(&ui);
        }
    });
    let w = ui.as_weak();
    ui.on_row_selected(move |i| {
        with(|s| s.current = s.rows.get(i as usize).cloned());
        if let Some(ui) = w.upgrade() {
            update_detail(&ui);
        }
    });
    let w = ui.as_weak();
    ui.on_row_toggled(move |i| {
        with(|s| {
            if let Some(Sel::Folder(id)) = s.rows.get(i as usize).cloned() {
                if !s.expanded.remove(&id) {
                    s.expanded.insert(id);
                }
            }
        });
        if let Some(ui) = w.upgrade() {
            rebuild(&ui);
        }
    });
    let w = ui.as_weak();
    ui.on_row_activated(move |i| {
        let Some(ui) = w.upgrade() else { return };
        match with(|s| s.rows.get(i as usize).cloned()) {
            Some(Sel::Folder(id)) => {
                with(|s| {
                    if !s.expanded.remove(&id) {
                        s.expanded.insert(id);
                    }
                });
                rebuild(&ui);
            }
            Some(Sel::Entry(id)) => {
                with(|s| s.current = Some(Sel::Entry(id)));
                open_session(&ui, false);
            }
            None => {}
        }
    });

    let w = ui.as_weak();
    ui.on_tree_key(move |key| {
        let Some(ui) = w.upgrade() else { return };
        let i = ui.get_selected();
        let n = with(|s| s.rows.len()) as i32;
        let row = with(|s| s.rows.get(i.max(0) as usize).cloned());
        let select = |j: i32| {
            if (0..n).contains(&j) {
                ui.set_selected(j);
                ui.invoke_row_selected(j);
            }
        };
        match key.as_str() {
            "up" => select(if i < 0 { 0 } else { i - 1 }),
            "down" => select(i + 1),
            "enter" if i >= 0 => ui.invoke_row_activated(i),
            "delete" if i >= 0 => ui.invoke_delete_selected(),
            "edit" if i >= 0 => ui.invoke_edit_selected(),
            "right" | "left" if i >= 0 => {
                let Some(Sel::Folder(id)) = row else { return };
                let open = with(|s| s.expanded.contains(&id));
                if (key == "right") != open {
                    ui.invoke_row_toggled(i);
                } else if key == "right" {
                    select(i + 1);
                }
            }
            _ => {}
        }
    });

    let w = ui.as_weak();
    ui.on_tab_activated(move |i| {
        if let Some(ui) = w.upgrade() {
            if i >= 0 {
                fit_terminal(&ui);
            }
        }
    });
    let w = ui.as_weak();
    ui.on_tab_closed(move |i| {
        let Some(ui) = w.upgrade() else { return };
        let active = ui.get_active_tab();
        let left = with(|s| {
            if (i as usize) < s.sessions.len() {
                let x = s.sessions.remove(i as usize);
                if let Some(input) = &x.input {
                    let _ = input.send(ssh::Input::Close);
                }
            }
            s.sessions.len() as i32
        });
        let next = if active > i {
            active - 1
        } else if active == i {
            (i.min(left - 1)).max(-1)
        } else {
            active
        };
        show_tabs(&ui);
        ui.set_active_tab(next);
        if next >= 0 {
            fit_terminal(&ui);
        }
    });
    let w = ui.as_weak();
    ui.on_term_size(move |width, height| {
        with(|s| s.term_area = (width, height));
        if let Some(ui) = w.upgrade() {
            fit_terminal(&ui);
        }
    });
    let w = ui.as_weak();
    ui.on_term_key(move |text, ctrl, alt, shift| {
        let Some(ui) = w.upgrade() else { return };
        use slint::platform::Key;
        if shift && (text == slint::SharedString::from(Key::PageUp) || text == slint::SharedString::from(Key::PageDown)) {
            let up = text == slint::SharedString::from(Key::PageUp);
            active_session(&ui, |x| {
                x.term.as_ref().map(|t| t.lock().unwrap().scroll(if up { 10 } else { -10 }))
            });
            redraw(&ui);
            return;
        }
        let sent = active_session(&ui, |x| {
            let (Some(term), Some(input)) = (&x.term, &x.input) else {
                return false;
            };
            let mut t = term.lock().unwrap();
            let Some(bytes) = term::key_bytes(&text, ctrl, alt, t.application_cursor()) else {
                return false;
            };
            t.scroll(-1_000_000);
            input.send(ssh::Input::Data(bytes)).is_ok()
        });
        if sent == Some(true) {
            redraw(&ui);
        }
    });
    let w = ui.as_weak();
    ui.on_term_paste(move || {
        let Some(ui) = w.upgrade() else { return };
        let Some(text) = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()) else {
            return;
        };
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        active_session(&ui, |x| {
            let (Some(term), Some(input)) = (&x.term, &x.input) else { return };
            let bracketed = term.lock().unwrap().bracketed_paste();
            let data = if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text };
            let _ = input.send(ssh::Input::Data(data.into_bytes()));
        });
    });
    let w = ui.as_weak();
    ui.on_term_scroll(move |n| {
        if let Some(ui) = w.upgrade() {
            active_session(&ui, |x| x.term.as_ref().map(|t| t.lock().unwrap().scroll(n as isize)));
            redraw(&ui);
        }
    });

    let w = ui.as_weak();
    ui.on_session_area(move |x, y, width, height| {
        with(|s| s.session_area = (x, y, width, height));
        if let Some(ui) = w.upgrade() {
            sync_rdp(&ui);
        }
    });
    let w = ui.as_weak();
    let rdp_timer = slint::Timer::default();
    rdp_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(150), move || {
        if let Some(ui) = w.upgrade() {
            sync_rdp(&ui);
        }
    });
    let w = ui.as_weak();
    let pve_timer = slint::Timer::default();
    pve_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(40), move || {
        if let Some(ui) = w.upgrade() {
            if ui.get_active_kind() == "pve" {
                redraw(&ui);
            }
        }
    });
    let w = ui.as_weak();
    ui.on_console_pointer(move |x, y, width, height, buttons| {
        let Some(ui) = w.upgrade() else { return };
        active_session(&ui, |s| {
            if let Some(c) = &s.pve {
                let f = c.frame.lock().unwrap();
                if width > 0.0 && height > 0.0 && f.width > 0 && f.height > 0 {
                    let px = (x / width * f.width as f32).clamp(0.0, (f.width - 1) as f32) as u16;
                    let py = (y / height * f.height as f32).clamp(0.0, (f.height - 1) as f32) as u16;
                    let _ = c.input.send(vnc::X11Event::PointerEvent((px, py, buttons as u8).into()));
                }
            }
        });
    });
    let w = ui.as_weak();
    ui.on_console_key(move |text, down| {
        let Some(ui) = w.upgrade() else { return };
        if let Some(code) = pve::keysym(&text) {
            active_session(&ui, |s| {
                if let Some(c) = &s.pve {
                    let _ = c.input.send(vnc::X11Event::KeyEvent((code, down).into()));
                }
            });
        }
    });
    ui.set_upd_current(env!("CARGO_PKG_VERSION").into());
    ui.set_upd_auto(with(|s| s.settings.auto_update));
    update::check_on_startup(&ui);

    let last = with(|s| s.settings.last.clone());
    show_sources(&ui, last.as_deref());
    if let Some(i) = last.and_then(|id| with(|s| s.settings.sources.iter().position(|c| c.id == id))) {
        activate(&ui, i);
    } else if with(|s| s.settings.sources.is_empty()) {
        ui.invoke_add_source();
    }
    ui.run()
}
