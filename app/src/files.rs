//! "Fichiers (SFTP)" tab of an SSH session: browse, sort, upload (button or
//! files dropped from the Explorer), download, rename, delete, new folder,
//! with progress and cancellation of the transfers. The SFTP channel runs on
//! the connection of the terminal tab (`ssh::Input::OpenSftp`).

use super::*;
use relay_desktop_core::sftp::{self, Item, Progress, Remote, SortKey};
use slint::Model;
use std::rc::Rc;
use std::sync::atomic::Ordering as AtOrd;
use std::time::Instant;

pub struct FilesTab {
    remote: Option<Arc<Remote>>,
    /// Id of the SSH tab whose connection carries the SFTP channel.
    terminal: u64,
    path: String,
    items: Vec<Item>,
    sort: SortKey,
    desc: bool,
    error: String,
    loading: bool,
    /// Number of the last listing request: older replies are ignored.
    gen: u64,
    xfers: Vec<Xfer>,
}

struct Xfer {
    id: u64,
    name: String,
    up: bool,
    progress: Arc<Progress>,
    state: XState,
    started: Instant,
}

#[derive(PartialEq)]
enum XState {
    Queued,
    Running,
    Done,
    Failed(String),
}

static NEXT_XFER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl FilesTab {
    /// Id of the SSH tab carrying the SFTP channel.
    pub fn terminal(&self) -> u64 {
        self.terminal
    }
}

impl Drop for FilesTab {
    /// Closing the tab cancels its transfers and closes the SFTP channel.
    fn drop(&mut self) {
        for x in &self.xfers {
            x.progress.cancel();
        }
        if let Some(r) = self.remote.take() {
            rt().spawn(async move { r.close().await });
        }
    }
}

/// Sets the state and banner of the tab `id` (UI thread).
fn tab_state(ui: &AppWindow, id: u64, state: &'static str, banner: String) {
    with(|s| {
        if let Some(x) = s.sessions.iter_mut().find(|x| x.id == id) {
            x.state = state;
            x.banner = banner;
        }
    });
    show_tabs(ui);
    redraw(ui);
}

/// Runs `f` on the files tab `id`.
fn tab<R>(id: u64, f: impl FnOnce(&mut FilesTab) -> R) -> Option<R> {
    with(|s| s.sessions.iter_mut().find(|x| x.id == id).and_then(|x| x.files.as_mut()).map(f))
}

fn active(ui: &AppWindow) -> Option<u64> {
    active_session(ui, |x| x.files.is_some().then_some(x.id)).flatten()
}

/// Opens a files tab on the SSH session of the active tab.
pub fn open(ui: &AppWindow) {
    let found = active_session(ui, |x| match (&x.input, x.kind, x.state, &x.player) {
        (Some(input), "ssh", "connected", None) => Some((x.id, x.title.clone(), x.entry_id.clone(), input.clone())),
        _ => None,
    })
    .flatten();
    let Some((terminal, title, entry_id, input)) = found else {
        ui.set_status("Fichiers (SFTP) : ouvrez d'abord une session SSH connectée et activez son onglet.".into());
        return;
    };
    let allowed = with(|s| {
        s.tree
            .entries
            .iter()
            .find(|e| e.id == entry_id)
            .is_none_or(|e| relay_desktop_core::conn::sftp_enabled(&e.settings))
    });
    if !allowed {
        ui.set_status("Fichiers (SFTP) : désactivé pour cette entrée (Modifier > Terminal SSH).".into());
        return;
    }
    let id = with(|s| {
        s.next_session += 1;
        s.next_session
    });
    let files = FilesTab {
        remote: None,
        terminal,
        path: String::new(),
        items: Vec::new(),
        sort: SortKey::Name,
        desc: false,
        error: String::new(),
        loading: true,
        gen: 0,
        xfers: Vec::new(),
    };
    let index = with(|s| {
        s.sessions.push(Session {
            id,
            entry_id,
            title: format!("Fichiers – {}", title.trim_start_matches("● ")),
            icon: "folder-open",
            state: "connecting",
            banner: "Ouverture du canal SFTP…".into(),
            kind: "files",
            term: None,
            input: None,
            rdp: None,
            pve: None,
            pt: 0.0,
            rec: None,
            player: None,
            files: Some(files),
            replay: None,
        });
        s.sessions.len() as i32 - 1
    });
    show_tabs(ui);
    ui.set_active_tab(index);
    redraw(ui);
    connect(ui, id, input);
}

/// Asks the terminal's connection for an SFTP channel, then lists the home folder.
fn connect(ui: &AppWindow, id: u64, input: tokio::sync::mpsc::UnboundedSender<ssh::Input>) {
    // Starting folder of the entry (`sftpRoot`), else the home folder.
    let root = with(|s| {
        let entry = &s.sessions.iter().find(|x| x.id == id)?.entry_id;
        let e = s.tree.entries.iter().find(|e| &e.id == entry)?;
        Some(relay_desktop_core::conn::SshOptions::from_settings(&e.settings).sftp_root)
    })
    .unwrap_or_default();
    let (reply, opened) = tokio::sync::oneshot::channel();
    if input.send(ssh::Input::OpenSftp(reply)).is_err() {
        tab_state(ui, id, "closed", "Session SSH fermée : reconnectez-la puis « Reconnecter ».".into());
        return;
    }
    let weak = ui.as_weak();
    rt().spawn(async move {
        let result = match opened.await {
            Err(_) => Err("la session SSH s'est fermée".to_string()),
            Ok(Err(e)) => Err(e),
            Ok(Ok(remote)) => {
                let start = if root.is_empty() {
                    Err(String::new())
                } else {
                    remote.canonical(&root).await
                };
                let home = match start {
                    Ok(p) => p,
                    Err(_) => remote.canonical(".").await.unwrap_or_else(|_| "/".into()),
                };
                Ok((Arc::new(remote), home))
            }
        };
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            match result {
                Ok((remote, home)) => {
                    tab(id, |f| f.remote = Some(remote));
                    tab_state(&ui, id, "connected", String::new());
                    list(&ui, id, home);
                }
                Err(e) => {
                    tab(id, |f| f.loading = false);
                    tab_state(&ui, id, "closed", format!("SFTP : {e}"));
                }
            }
        });
    });
}

/// "Reconnecter" of a files tab: new SFTP channel on its SSH tab (or on the
/// connected SSH tab of the same entry when it was reconnected).
pub fn reconnect(ui: &AppWindow) {
    let Some(id) = active(ui) else { return };
    let input = with(|s| {
        let x = s.sessions.iter().find(|x| x.id == id)?;
        let terminal = x.files.as_ref()?.terminal;
        let entry = x.entry_id.clone();
        let ssh = s.sessions.iter().find(|t| t.id == terminal && t.state == "connected").or_else(|| {
            s.sessions
                .iter()
                .find(|t| t.kind == "ssh" && t.state == "connected" && !entry.is_empty() && t.entry_id == entry && t.player.is_none())
        })?;
        Some((ssh.id, ssh.input.clone()?))
    });
    let Some((terminal, input)) = input else {
        ui.set_status("Reconnexion SFTP : la session SSH est fermée, reconnectez d'abord son onglet.".into());
        return;
    };
    tab(id, |f| {
        f.terminal = terminal;
        f.remote = None;
        f.loading = true;
    });
    tab_state(ui, id, "connecting", "Ouverture du canal SFTP…".into());
    connect(ui, id, input);
}

fn remote(id: u64) -> Option<Arc<Remote>> {
    tab(id, |f| f.remote.clone()).flatten()
}

/// Lists `path` in the tab `id` (selection cleared).
fn list(ui: &AppWindow, id: u64, path: String) {
    let Some(r) = remote(id) else { return };
    let Some(gen) = tab(id, |f| {
        f.gen += 1;
        f.loading = true;
        f.gen
    }) else {
        return;
    };
    show(ui);
    let weak = ui.as_weak();
    rt().spawn(async move {
        let result = r.list(&path).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            let fresh = tab(id, |f| {
                if f.gen != gen {
                    return false;
                }
                f.loading = false;
                match result {
                    Ok(mut items) => {
                        sftp::sort(&mut items, f.sort, f.desc);
                        f.items = items;
                        f.path = path;
                        f.error.clear();
                    }
                    Err(e) => f.error = format!("{path} : {e}"),
                }
                true
            });
            if fresh == Some(true) {
                if active(&ui) == Some(id) {
                    ui.set_fb_selected(-1);
                }
                show(&ui);
            }
        });
    });
}

fn current(ui: &AppWindow) -> Option<(u64, String)> {
    let id = active(ui)?;
    tab(id, |f| (id, f.path.clone()))
}

fn selected(ui: &AppWindow) -> Option<(u64, String, Item)> {
    let id = active(ui)?;
    let i = ui.get_fb_selected();
    tab(id, |f| {
        let item = f.items.get(usize::try_from(i).ok()?)?.clone();
        Some((id, f.path.clone(), item))
    })
    .flatten()
}

/// Pushes the state of the active files tab to the view.
pub fn show(ui: &AppWindow) {
    let Some(id) = active(ui) else { return };
    let ready = with(|s| s.sessions.iter().any(|x| x.id == id && x.state == "connected"));
    let view = tab(id, |f| {
        let rows: Vec<FileRow> = f
            .items
            .iter()
            .map(|i| FileRow {
                name: i.name.clone().into(),
                dir: i.dir,
                link: i.link,
                size: if i.dir { String::new() } else { sftp::fmt_size(i.size) }.into(),
                date: sftp::fmt_time(i.mtime).into(),
                perms: if i.mode == 0 { String::new() } else { i.perms() }.into(),
            })
            .collect();
        (rows, f.path.clone(), f.sort, f.desc, f.error.clone(), f.loading, f.remote.is_some())
    });
    let Some((rows, path, sort, desc, error, loading, has_remote)) = view else {
        return;
    };
    if ui.get_fb_selected() >= rows.len() as i32 {
        ui.set_fb_selected(-1);
    }
    ui.set_fb_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_fb_path(path.into());
    ui.set_fb_sort(sort as i32);
    ui.set_fb_desc(desc);
    ui.set_fb_error(error.into());
    ui.set_fb_loading(loading);
    ui.set_fb_ready(ready && has_remote);
    show_xfers(ui);
}

fn show_xfers(ui: &AppWindow) {
    let Some(id) = active(ui) else { return };
    let rows = tab(id, |f| {
        f.xfers
            .iter()
            .map(|x| {
                let done = x.progress.done.load(AtOrd::Acquire);
                let total = x.progress.total.load(AtOrd::Acquire);
                let (detail, progress) = match &x.state {
                    XState::Queued => ("En attente".to_string(), 0.0),
                    XState::Running => {
                        let secs = x.started.elapsed().as_secs_f64().max(0.1);
                        let rate = sftp::fmt_size((done as f64 / secs) as u64);
                        (
                            format!("{} / {} · {rate}/s", sftp::fmt_size(done), sftp::fmt_size(total)),
                            x.progress.ratio(),
                        )
                    }
                    XState::Done => (format!("Terminé ({})", sftp::fmt_size(total.max(done))), 1.0),
                    XState::Failed(e) => (e.clone(), x.progress.ratio()),
                };
                XferRow {
                    name: x.name.clone().into(),
                    up: x.up,
                    progress,
                    detail: detail.into(),
                    running: matches!(x.state, XState::Queued | XState::Running),
                    failed: matches!(x.state, XState::Failed(_)),
                }
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    // Updated in place: a new model would recreate the rows (and lose a
    // click on "Annuler" in progress) at every refresh.
    let model = ui.get_fb_xfers();
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<XferRow>>() {
        if vm.row_count() == rows.len() {
            for (i, row) in rows.into_iter().enumerate() {
                if vm.row_data(i).as_ref() != Some(&row) {
                    vm.set_row_data(i, row);
                }
            }
            return;
        }
    }
    ui.set_fb_xfers(ModelRc::new(VecModel::from(rows)));
}

/// Refreshes the progress bars of the active files tab.
pub fn install(ui: &AppWindow) -> slint::Timer {
    let timer = slint::Timer::default();
    let w = ui.as_weak();
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(250), move || {
        let Some(ui) = w.upgrade() else { return };
        let Some(id) = active(&ui) else { return };
        if tab(id, |f| f.xfers.iter().any(|x| x.state == XState::Running)) == Some(true) {
            show_xfers(&ui);
        }
    });
    timer
}

fn set_state(id: u64, xfer: u64, state: XState) {
    tab(id, |f| {
        if let Some(x) = f.xfers.iter_mut().find(|x| x.id == xfer) {
            if state == XState::Running {
                x.started = Instant::now();
            }
            x.state = state;
        }
    });
}

/// Queues transfers on the tab `id` and runs them one after the other.
/// `jobs` = (name, local path, remote path).
fn transfer(ui: &AppWindow, id: u64, up: bool, jobs: Vec<(String, PathBuf, String)>, refresh: Option<String>) {
    let Some(r) = remote(id) else { return };
    let queued: Vec<(u64, Arc<Progress>, PathBuf, String)> = tab(id, |f| {
        jobs.into_iter()
            .map(|(name, local, remote)| {
                let xid = NEXT_XFER.fetch_add(1, AtOrd::AcqRel) + 1;
                let progress = Arc::new(Progress::default());
                f.xfers.push(Xfer {
                    id: xid,
                    name,
                    up,
                    progress: progress.clone(),
                    state: XState::Queued,
                    started: Instant::now(),
                });
                (xid, progress, local, remote)
            })
            .collect()
    })
    .unwrap_or_default();
    show_xfers(ui);
    let weak = ui.as_weak();
    rt().spawn(async move {
        let post = |f: Box<dyn FnOnce(&AppWindow) + Send>| {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    f(&ui);
                }
            });
        };
        let (mut ok, mut failed) = (0, 0);
        for (xid, progress, local, path) in queued {
            if progress.cancelled() {
                post(Box::new(move |_| set_state(id, xid, XState::Failed(sftp::CANCELLED.into()))));
                failed += 1;
                continue;
            }
            post(Box::new(move |_| set_state(id, xid, XState::Running)));
            let result = if up {
                r.upload(&local, &path, &progress).await
            } else {
                r.download(&path, &local, &progress).await
            };
            let state = match result {
                Ok(()) => {
                    ok += 1;
                    XState::Done
                }
                Err(e) => {
                    failed += 1;
                    XState::Failed(e)
                }
            };
            post(Box::new(move |ui| {
                set_state(id, xid, state);
                show_xfers(ui);
            }));
        }
        post(Box::new(move |ui| {
            let what = if up { "envoyé(s)" } else { "téléchargé(s)" };
            let msg = if failed == 0 {
                format!("{ok} fichier(s) {what}")
            } else {
                format!("{ok} fichier(s) {what}, {failed} en échec ou annulé(s)")
            };
            ui.set_status(msg.into());
            if let Some(dir) = refresh {
                if tab(id, |f| f.path == dir) == Some(true) {
                    list(ui, id, dir);
                }
            }
        }));
    });
}

/// Sends local files to the displayed folder (replacing existing names after
/// confirmation); folders are skipped.
pub fn upload(ui: &AppWindow, paths: Vec<PathBuf>) {
    let Some((id, dir)) = current(ui) else { return };
    if remote(id).is_none() || dir.is_empty() {
        ui.set_status("Fichiers (SFTP) : canal non connecté.".into());
        return;
    }
    let (files, dirs): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|p| !p.is_dir());
    if !dirs.is_empty() {
        ui.set_status(format!("{} dossier(s) ignoré(s) : seuls les fichiers peuvent être envoyés.", dirs.len()).into());
    }
    let jobs: Vec<(String, PathBuf, String)> = files
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            let remote = sftp::join(&dir, &name);
            Some((name, p, remote))
        })
        .collect();
    if jobs.is_empty() {
        return;
    }
    let existing: Vec<String> = tab(id, |f| {
        jobs.iter()
            .filter(|(n, ..)| f.items.iter().any(|i| &i.name == n))
            .map(|(n, ..)| n.clone())
            .collect()
    })
    .unwrap_or_default();
    if existing.is_empty() {
        return transfer(ui, id, true, jobs, Some(dir));
    }
    let names = existing.iter().take(5).map(|n| format!("« {n} »")).collect::<Vec<_>>().join(", ");
    let more = if existing.len() > 5 {
        format!(" et {} autre(s)", existing.len() - 5)
    } else {
        String::new()
    };
    if existing
        .iter()
        .any(|n| tab(id, |f| f.items.iter().any(|i| &i.name == n && i.dir)) == Some(true))
    {
        ui.set_status(format!("Envoi refusé : un dossier porte déjà le nom {names}.").into());
        return;
    }
    features::confirm_as(
        ui,
        format!("{names}{more} existe(nt) déjà dans {dir}. Remplacer ?"),
        "Remplacer",
        move |ui| {
            close_dialog(ui);
            transfer(ui, id, true, jobs, Some(dir));
        },
    );
}

fn download(ui: &AppWindow, id: u64, item: Item) {
    if item.dir {
        ui.set_status("Le téléchargement d'un dossier n'est pas pris en charge : ouvrez-le et téléchargez ses fichiers.".into());
        return;
    }
    later(ui, move |ui| {
        let Some(local) = file_dialog(ui, "Enregistrer le fichier").set_file_name(&item.name).save_file() else {
            return;
        };
        transfer(ui, id, false, vec![(item.name.clone(), local, item.path.clone())], None);
    });
}

/// File dialog owned by the main window (modal, kept in front of it).
fn file_dialog(ui: &AppWindow, title: &str) -> rfd::FileDialog {
    rfd::FileDialog::new().set_title(title).set_parent(&ui.window().window_handle())
}

/// Runs `f` once the current callback has returned: a modal dialog opened
/// from an accessibility action (UI Automation) would otherwise block it.
fn later(ui: &AppWindow, f: impl FnOnce(&AppWindow) + 'static) {
    let weak = ui.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(ui) = weak.upgrade() {
            f(&ui);
        }
    });
}

/// Runs `work` on the remote of the tab, then reloads the listing.
fn remote_op(
    ui: &AppWindow,
    id: u64,
    status: &str,
    work: impl FnOnce(Arc<Remote>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> + Send + 'static,
    in_dialog: bool,
) {
    let Some(r) = remote(id) else { return };
    let dir = tab(id, |f| f.path.clone()).unwrap_or_default();
    if in_dialog {
        ui.set_busy(true);
    }
    ui.set_status(status.into());
    let weak = ui.as_weak();
    rt().spawn(async move {
        let result = work(r).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_busy(false);
            match result {
                Ok(()) => {
                    if in_dialog {
                        close_dialog(&ui);
                    }
                    ui.set_status("".into());
                    list(&ui, id, dir);
                }
                Err(e) if in_dialog => fail(&ui, e),
                Err(e) => {
                    ui.set_status(format!("Échec : {e}").into());
                    list(&ui, id, dir);
                }
            }
        });
    });
}

type PromptFn = Rc<dyn Fn(&AppWindow, String)>;

thread_local! {
    /// Handler of the open text prompt; it stays armed until the dialog closes,
    /// so a refused name can be corrected.
    static PROMPT: RefCell<Option<PromptFn>> = RefCell::new(None);
}

fn prompt(ui: &AppWindow, title: &str, label: &str, ok: &str, text: &str, then: impl Fn(&AppWindow, String) + 'static) {
    PROMPT.with(|p| *p.borrow_mut() = Some(Rc::new(then)));
    ui.set_prompt_title(title.into());
    ui.set_prompt_label(label.into());
    ui.set_prompt_ok(ok.into());
    ui.set_prompt_text(text.into());
    ui.set_dialog_error("".into());
    ui.set_dialog("prompt".into());
}

/// Validation of the text prompt.
pub fn submit(ui: &AppWindow) -> bool {
    if ui.get_dialog() != "prompt" {
        return false;
    }
    if !ui.get_busy() {
        if let Some(f) = PROMPT.with(|p| p.borrow().clone()) {
            f(ui, ui.get_prompt_text().trim().to_string());
        }
    }
    true
}

pub fn act(ui: &AppWindow, action: &str) {
    let Some((id, dir)) = current(ui) else { return };
    match action {
        "refresh" => list(ui, id, dir),
        "up" if !dir.is_empty() && dir != "/" => list(ui, id, sftp::parent(&dir)),
        "go" => {
            let typed = ui.get_fb_path().trim().to_string();
            let Some(r) = remote(id) else { return };
            if typed.is_empty() {
                return;
            }
            // Relative paths and "~" are resolved by the server.
            let typed = if typed == "~" || typed.starts_with("~/") {
                typed.replacen('~', ".", 1)
            } else {
                typed
            };
            let weak = ui.as_weak();
            rt().spawn(async move {
                let resolved = r.canonical(&typed).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match resolved {
                        Ok(p) => list(&ui, id, p),
                        Err(e) => {
                            tab(id, |f| f.error = format!("{typed} : {e}"));
                            show(&ui);
                        }
                    }
                });
            });
        }
        "upload" => {
            later(ui, move |ui| {
                if let Some(paths) = file_dialog(ui, &format!("Envoyer vers {dir}")).pick_files() {
                    upload(ui, paths);
                }
            });
        }
        "download" => {
            if let Some((id, _, item)) = selected(ui) {
                download(ui, id, item);
            }
        }
        "mkdir" => prompt(
            ui,
            "Nouveau dossier",
            &format!("Nom du dossier à créer dans {dir}"),
            "Créer",
            "",
            move |ui, name| {
                if let Err(e) = sftp::check_name(&name) {
                    return fail(ui, e);
                }
                let path = sftp::join(&dir, &name);
                remote_op(
                    ui,
                    id,
                    "Création du dossier…",
                    move |r| Box::pin(async move { r.mkdir(&path).await }),
                    true,
                );
            },
        ),
        "rename" => {
            let Some((id, dir, item)) = selected(ui) else { return };
            let old = item.name.clone();
            let (label, text) = (format!("Nouveau nom de « {old} »"), old.clone());
            prompt(ui, "Renommer", &label, "Renommer", &text, move |ui, name| {
                if name == old {
                    return close_dialog(ui);
                }
                if let Err(e) = sftp::check_name(&name) {
                    return fail(ui, e);
                }
                let to = sftp::join(&dir, &name);
                let from = item.path.clone();
                remote_op(
                    ui,
                    id,
                    "Renommage…",
                    move |r| Box::pin(async move { r.rename(&from, &to).await }),
                    true,
                );
            });
        }
        "delete" => {
            let Some((id, _, item)) = selected(ui) else { return };
            let what = if item.dir && !item.link {
                format!("Supprimer le dossier « {} » et tout son contenu ?", item.name)
            } else {
                format!("Supprimer « {} » ?", item.name)
            };
            features::confirm(ui, what, move |ui| {
                close_dialog(ui);
                let path = item.path.clone();
                remote_op(
                    ui,
                    id,
                    "Suppression…",
                    move |r| Box::pin(async move { r.remove(&path).await }),
                    false,
                );
            });
        }
        "clear" => {
            tab(id, |f| f.xfers.retain(|x| matches!(x.state, XState::Queued | XState::Running)));
            show_xfers(ui);
        }
        _ => {}
    }
}

/// Double click / Enter: folder = open, file = download.
pub fn open_row(ui: &AppWindow, i: i32) {
    let Some(id) = active(ui) else { return };
    ui.set_fb_selected(i);
    let Some((_, _, item)) = selected(ui) else { return };
    if item.dir {
        list(ui, id, item.path);
    } else {
        download(ui, id, item);
    }
}

pub fn sort_by(ui: &AppWindow, key: i32) {
    let Some(id) = active(ui) else { return };
    let key = SortKey::from_index(key);
    let keep = selected(ui).map(|(_, _, i)| i.name);
    tab(id, |f| {
        f.desc = f.sort == key && !f.desc;
        f.sort = key;
        sftp::sort(&mut f.items, f.sort, f.desc);
    });
    show(ui);
    let index = keep.and_then(|n| tab(id, |f| f.items.iter().position(|i| i.name == n)).flatten());
    ui.set_fb_selected(index.map_or(-1, |i| i as i32));
}

pub fn cancel(ui: &AppWindow, i: i32) {
    let Some(id) = active(ui) else { return };
    tab(id, |f| {
        if let Some(x) = usize::try_from(i).ok().and_then(|i| f.xfers.get(i)) {
            x.progress.cancel();
        }
    });
    ui.set_status("Annulation du transfert…".into());
}

/// Explorer drag and drop (winit events): highlight while hovering a files
/// tab, upload the dropped files to its folder.
pub fn watch_drops(ui: &AppWindow) {
    use slint::winit_030::{winit::event::WindowEvent, EventResult, WinitWindowAccessor};
    let weak = ui.as_weak();
    let dropped: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    ui.window().on_winit_window_event(move |_, event| {
        let Some(ui) = weak.upgrade() else { return EventResult::Propagate };
        match event {
            WindowEvent::HoveredFile(_) => ui.set_fb_drop_hover(active(&ui).is_some()),
            WindowEvent::HoveredFileCancelled => ui.set_fb_drop_hover(false),
            WindowEvent::DroppedFile(path) => {
                ui.set_fb_drop_hover(false);
                // One event per file: the batch is sent once the drop is complete.
                let first = dropped.borrow().is_empty();
                dropped.borrow_mut().push(path.clone());
                if first {
                    let (weak, dropped) = (ui.as_weak(), dropped.clone());
                    slint::Timer::single_shot(std::time::Duration::from_millis(50), move || {
                        let paths = std::mem::take(&mut *dropped.borrow_mut());
                        let Some(ui) = weak.upgrade() else { return };
                        if active(&ui).is_some() {
                            upload(&ui, paths);
                        } else {
                            ui.set_status("Déposez les fichiers sur un onglet « Fichiers (SFTP) » pour les envoyer.".into());
                        }
                    });
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });
}
