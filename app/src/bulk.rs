//! Bulk SSH commands, run natively from this PC on the SSH entries of the
//! selection (entry, folder subtree or everything), with the saved command
//! library of the source.

use super::*;
use relay_desktop_core::command::{self, Options, RunResult, SavedCommand};
use std::time::Duration;

#[derive(Default)]
pub struct Run {
    targets: Vec<Entry>,
    results: Vec<RunResult>,
    cancel: Option<tokio::sync::watch::Sender<bool>>,
    saved: Vec<SavedCommand>,
    generation: u64,
}

/// Stops the running command, if any.
pub fn stop() {
    with(|s| {
        if let Some(tx) = s.cmd.cancel.take() {
            let _ = tx.send(true);
        }
    });
}

fn label(r: &RunResult) -> (i32, String) {
    match r.status {
        command::STATUS_OK => (1, format!("OK · {} ms", r.duration_ms)),
        command::STATUS_FAILED => (2, format!("code {} · {} ms", r.exit_code.unwrap_or_default(), r.duration_ms)),
        command::STATUS_ERROR => (2, r.error.clone()),
        command::STATUS_CANCELED => (0, "arrêté".into()),
        command::STATUS_RUNNING => (0, "en cours…".into()),
        _ => (0, "en attente".into()),
    }
}

fn fill(ui: &AppWindow) {
    let (rows, summary, retryable) = with(|s| {
        let rows: Vec<ListRow> = s
            .cmd
            .results
            .iter()
            .map(|r| {
                let (status, extra) = label(r);
                ListRow {
                    name: r.name.clone().into(),
                    detail: r.address.clone().into(),
                    extra: extra.into(),
                    status,
                }
            })
            .collect();
        let count = |st: &str| s.cmd.results.iter().filter(|r| r.status == st).count();
        let summary = if s.cmd.results.is_empty() {
            String::new()
        } else {
            format!(
                "{} OK · {} en échec · {} en erreur · {} arrêté(s) · {} restant(s)",
                count(command::STATUS_OK),
                count(command::STATUS_FAILED),
                count(command::STATUS_ERROR),
                count(command::STATUS_CANCELED),
                count(command::STATUS_PENDING) + count(command::STATUS_RUNNING)
            )
        };
        let retryable = s
            .cmd
            .results
            .iter()
            .any(|r| [command::STATUS_FAILED, command::STATUS_ERROR, command::STATUS_CANCELED].contains(&r.status));
        (rows, summary, retryable)
    });
    ui.set_cmd_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_cmd_summary(summary.into());
    ui.set_cmd_retryable(retryable);
    show_output(ui);
}

fn show_output(ui: &AppWindow) {
    let text = with(|s| {
        usize::try_from(ui.get_cmd_selected())
            .ok()
            .and_then(|i| s.cmd.results.get(i))
            .map(|r| {
                let mut t = String::new();
                if r.new_key {
                    t.push_str("(nouvelle clé d'hôte mémorisée)\n");
                }
                if !r.error.is_empty() {
                    t.push_str(&format!("Erreur : {}\n", r.error));
                }
                t.push_str(&r.output);
                if r.truncated {
                    t.push_str("\n… sortie tronquée");
                }
                t
            })
            .unwrap_or_default()
    });
    ui.set_cmd_output(text.into());
}

fn fill_saved(ui: &AppWindow) {
    let names = with(|s| {
        std::iter::once("(commandes enregistrées)".to_string())
            .chain(
                s.cmd
                    .saved
                    .iter()
                    .map(|c| if c.sudo { format!("{} (sudo)", c.name) } else { c.name.clone() }),
            )
            .collect()
    });
    ui.set_cmd_saved_names(strings(names));
    ui.set_cmd_saved_index(0);
}

fn load_saved(ui: &AppWindow) {
    let Some(src) = source() else { return };
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let r = src.saved_commands();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            match r {
                Ok(list) => with(|s| s.cmd.saved = list),
                Err(e) => ui.set_status(format!("Commandes enregistrées indisponibles : {e}").into()),
            }
            fill_saved(&ui);
        });
    });
}

fn open(ui: &AppWindow) {
    let targets: Vec<Entry> = with(|s| {
        let ids = features::ssh_targets(s);
        s.tree.entries.iter().filter(|e| ids.contains(&e.id)).cloned().collect()
    });
    if targets.is_empty() {
        return ui.set_status("Aucune entrée SSH dans la sélection.".into());
    }
    let names: Vec<&str> = targets.iter().take(6).map(|e| e.name.as_str()).collect();
    let more = targets.len().saturating_sub(names.len());
    ui.set_cmd_targets_text(
        format!(
            "{} serveur(s) SSH, depuis ce poste : {}{}",
            targets.len(),
            names.join(", "),
            if more > 0 { format!(" et {more} autre(s)") } else { String::new() }
        )
        .into(),
    );
    with(|s| {
        s.cmd.targets = targets;
        s.cmd.results.clear();
    });
    ui.set_cmd_selected(-1);
    ui.set_cmd_running(false);
    fill(ui);
    fill_saved(ui);
    load_saved(ui);
    ui.set_dialog_error("".into());
    ui.set_dialog("cmd".into());
}

/// Runs the command on `only` (indexes of targets), all targets when `None`.
fn start(ui: &AppWindow, only: Option<Vec<usize>>) {
    let commandline = ui.get_cmd_text().trim().to_string();
    if commandline.is_empty() {
        return ui.set_dialog_error("Saisissez une commande.".into());
    }
    let Ok(secs) = ui.get_cmd_timeout().trim().parse::<u64>() else {
        return ui.set_dialog_error("Délai invalide (en secondes).".into());
    };
    let Some(src) = source() else { return };
    ui.set_dialog_error("".into());
    let (tx, rx) = tokio::sync::watch::channel(false);
    let (generation, indexes, entries, tree, creds) = with(|s| {
        s.cmd.generation += 1;
        s.cmd.cancel = Some(tx);
        let indexes: Vec<usize> = only.unwrap_or_else(|| (0..s.cmd.targets.len()).collect());
        if s.cmd.results.len() != s.cmd.targets.len() {
            s.cmd.results = s
                .cmd
                .targets
                .iter()
                .map(|e| RunResult {
                    entry_id: e.id.clone(),
                    name: e.name.clone(),
                    address: command::address(e),
                    status: command::STATUS_PENDING,
                    ..Default::default()
                })
                .collect();
        }
        for &i in &indexes {
            s.cmd.results[i].status = command::STATUS_PENDING;
        }
        let entries: Vec<Entry> = indexes.iter().map(|&i| s.cmd.targets[i].clone()).collect();
        (s.cmd.generation, indexes, entries, s.tree.clone(), s.creds.clone())
    });
    ui.set_cmd_running(true);
    fill(ui);
    let opt = Options {
        command: commandline,
        sudo: ui.get_cmd_sudo(),
        timeout: Duration::from_secs(secs.clamp(1, 3600)),
        concurrency: 8,
    };
    let weak = ui.as_weak();
    let done = ui.as_weak();
    std::thread::spawn(move || {
        command::run(
            rt().handle(),
            src.as_ref(),
            &tree,
            &creds,
            &entries,
            &opt,
            &settings::dir().join("known_hosts.json"),
            rx,
            move |k, r| {
                let (weak, i) = (weak.clone(), indexes[k]);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let current = with(|s| {
                        if s.cmd.generation != generation || i >= s.cmd.results.len() {
                            return false;
                        }
                        s.cmd.results[i] = r;
                        true
                    });
                    if current {
                        fill(&ui);
                    }
                });
            },
        );
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = done.upgrade() else { return };
            if with(|s| s.cmd.generation == generation) {
                with(|s| s.cmd.cancel = None);
                ui.set_cmd_running(false);
                ui.set_status("Commande terminée".into());
            }
        });
    });
}

/// Handles the command dialog actions; returns false for other ids.
pub fn act(ui: &AppWindow, id: &str) -> bool {
    match id {
        "cmd" => open(ui),
        "cmd-run" => {
            if !ui.get_cmd_running() {
                start(ui, None);
            }
        }
        "cmd-retry" => {
            let failed: Vec<usize> = with(|s| {
                s.cmd
                    .results
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.status != command::STATUS_OK)
                    .map(|(i, _)| i)
                    .collect()
            });
            if !failed.is_empty() {
                start(ui, Some(failed));
            }
        }
        "cmd-stop" => stop(),
        "cmd-show" => show_output(ui),
        "cmd-pick" => {
            let i = ui.get_cmd_saved_index();
            if let Some(c) = with(|s| usize::try_from(i - 1).ok().and_then(|i| s.cmd.saved.get(i).cloned())) {
                ui.set_cmd_text(c.command.into());
                ui.set_cmd_sudo(c.sudo);
                ui.set_cmd_name(c.name.into());
            }
        }
        "cmd-save" => {
            let Some(src) = source() else { return true };
            let name = ui.get_cmd_name().trim().to_string();
            // Same name as a saved command: update it.
            let id = with(|s| s.cmd.saved.iter().find(|c| c.name == name).map(|c| c.id.clone())).unwrap_or_default();
            let c = SavedCommand {
                id,
                name,
                command: ui.get_cmd_text().trim().to_string(),
                sudo: ui.get_cmd_sudo(),
                ..Default::default()
            };
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let r = src.save_command(&c);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match r {
                        Ok(c) => {
                            ui.set_status(format!("Commande « {} » enregistrée", c.name).into());
                            load_saved(&ui);
                        }
                        Err(e) => ui.set_dialog_error(e.to_string().into()),
                    }
                });
            });
        }
        "cmd-forget" => {
            let i = ui.get_cmd_saved_index();
            let (Some(src), Some(c)) = (
                source(),
                with(|s| usize::try_from(i - 1).ok().and_then(|i| s.cmd.saved.get(i).cloned())),
            ) else {
                return true;
            };
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let r = src.delete_command(&c.id);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match r {
                        Ok(()) => load_saved(&ui),
                        Err(e) => ui.set_dialog_error(e.to_string().into()),
                    }
                });
            });
        }
        _ => return false,
    }
    true
}
