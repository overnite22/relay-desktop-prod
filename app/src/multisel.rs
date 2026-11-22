//! Multiple selection in the tree (Ctrl / Maj + clic), drag and drop of rows
//! and the grouped actions (open, delete, move).

use super::*;
use relay_desktop_core::moves::{self, Item};

fn item(sel: &Sel) -> Item {
    match sel {
        Sel::Folder(id) => Item::Folder(id.clone()),
        Sel::Entry(id) => Item::Entry(id.clone()),
    }
}

/// Items an action applies to: the marked rows, else the current one.
pub fn targets(s: &State) -> Vec<Item> {
    if s.marked.len() > 1 {
        s.marked.iter().map(item).collect()
    } else {
        s.current.iter().map(item).collect()
    }
}

/// More than one row is marked.
pub fn active() -> bool {
    with(|s| s.marked.len() > 1)
}

/// Plain click or keyboard move: the marks are dropped.
pub fn clear(ui: &AppWindow) {
    let had = with(|s| {
        let had = !s.marked.is_empty();
        s.marked.clear();
        had
    });
    if had {
        rebuild(ui);
    }
}

/// Ctrl + clic toggles a row, Maj + clic marks the range from the current row.
pub fn clicked(ui: &AppWindow, i: i32, ctrl: bool, shift: bool) {
    let anchor = ui.get_selected();
    with(|s| {
        let Some(row) = s.rows.get(i as usize).cloned() else { return };
        if shift && anchor >= 0 {
            let (a, b) = (anchor.min(i) as usize, anchor.max(i) as usize);
            let range: Vec<Sel> = s.rows[a..=b.min(s.rows.len() - 1)].to_vec();
            if !ctrl {
                s.marked.clear();
            }
            for r in range {
                if !s.marked.contains(&r) {
                    s.marked.push(r);
                }
            }
        } else {
            if s.marked.is_empty() {
                if let Some(cur) = s.current.clone() {
                    s.marked.push(cur);
                }
            }
            if let Some(p) = s.marked.iter().position(|m| m == &row) {
                s.marked.remove(p);
            } else {
                s.marked.push(row.clone());
            }
        }
        // The anchor of the next Maj + clic stays on the first row of a range.
        if !shift {
            s.current = Some(row);
        }
    });
    rebuild(ui);
    let n = with(|s| s.marked.len());
    if n > 1 {
        ui.set_status(format!("{n} éléments sélectionnés").into());
    }
}

/// Opens every marked entry (folders are ignored).
pub fn open_all(ui: &AppWindow) {
    let ids: Vec<String> = with(|s| {
        s.marked
            .iter()
            .filter_map(|m| match m {
                Sel::Entry(id) => Some(id.clone()),
                Sel::Folder(_) => None,
            })
            .collect()
    });
    for id in ids {
        launch(ui, &id, false);
    }
}

fn describe(s: &State, items: &[Item]) -> String {
    if let [one] = items {
        let name = match one {
            Item::Entry(id) => s
                .tree
                .entries
                .iter()
                .find(|e| &e.id == id)
                .map(|e| format!("l'entrée « {} »", e.name)),
            Item::Folder(id) => s
                .tree
                .folders
                .iter()
                .find(|f| &f.id == id)
                .map(|f| format!("le dossier « {} »", f.name)),
        };
        return name.unwrap_or_else(|| "l'élément".into());
    }
    let folders = items.iter().filter(|i| matches!(i, Item::Folder(_))).count();
    let entries = items.len() - folders;
    match (entries, folders) {
        (e, 0) => format!("{e} entrées"),
        (0, f) => format!("{f} dossiers"),
        (e, f) => format!("{e} entrée(s) et {f} dossier(s)"),
    }
}

/// Deletes the marked rows after confirmation.
pub fn delete_all(ui: &AppWindow) {
    let (items, text) = with(|s| {
        let items = targets(s);
        let text = format!(
            "Supprimer définitivement {} ? Les dossiers doivent être vides ou sélectionnés avec leur contenu.",
            describe(s, &items)
        );
        (moves::delete_order(&s.tree, &items), text)
    });
    features::confirm(ui, text, move |ui| {
        let Some(src) = source() else { return };
        bg(
            ui,
            "Suppression…",
            move || {
                let mut failed = Vec::new();
                for i in &items {
                    let r = match i {
                        Item::Entry(id) => src.delete_entry(id),
                        Item::Folder(id) => src.delete_folder(id),
                    };
                    match r {
                        Ok(()) | Err(Error::NotFound) => {}
                        Err(e) => failed.push(e.to_string()),
                    }
                }
                Ok(failed)
            },
            |ui, r| {
                with(|s| {
                    s.marked.clear();
                    s.current = None;
                });
                close_dialog(ui);
                load(ui);
                if let Ok(failed) = r {
                    if let Some(first) = failed.first() {
                        ui.set_status(format!("{} suppression(s) refusée(s) : {first}", failed.len()).into());
                    }
                }
            },
        );
    });
}

/// "Déplacer vers…": dialog with the destination folder.
pub fn move_dialog(ui: &AppWindow) {
    let (items, title) = with(|s| {
        let items = targets(s);
        let title = format!("Déplacer {}", describe(s, &items));
        (items, title)
    });
    if items.is_empty() {
        return;
    }
    let current = with(|s| match items.first() {
        Some(Item::Entry(id)) => s.tree.entries.iter().find(|e| &e.id == id).and_then(|e| e.folder_id.clone()),
        Some(Item::Folder(id)) => s.tree.folders.iter().find(|f| &f.id == id).and_then(|f| f.parent_id.clone()),
        None => None,
    });
    ui.set_move_target(fill_choices(ui, current.as_deref(), None));
    with(|s| s.move_items = items);
    ui.set_move_title(title.into());
    ui.set_dialog_error("".into());
    ui.set_dialog("move".into());
}

pub fn submit_move(ui: &AppWindow) {
    let (items, target) = with(|s| {
        (
            s.move_items.clone(),
            s.folder_choices.get(ui.get_move_target() as usize).cloned().flatten(),
        )
    });
    apply_move(ui, items, target);
}

fn apply_move(ui: &AppWindow, items: Vec<Item>, target: Option<String>) {
    let plan = match with(|s| moves::plan_move(&s.tree, &items, target.as_deref())) {
        Ok(p) => p,
        Err(e) => {
            if ui.get_dialog().is_empty() {
                ui.set_status(format!("Déplacement impossible : {e}").into());
            } else {
                fail(ui, e);
            }
            return;
        }
    };
    if plan.is_empty() {
        close_dialog(ui);
        ui.set_status("Rien à déplacer : déjà dans ce dossier.".into());
        return;
    }
    let Some(src) = source() else { return };
    let n = plan.len();
    bg(
        ui,
        "Déplacement…",
        move || {
            for f in &plan.folders {
                src.save_folder(f)?;
            }
            for e in &plan.entries {
                src.save_entry(e)?;
            }
            Ok(())
        },
        move |ui, r| match r {
            Ok(()) => {
                with(|s| {
                    if let Some(t) = &target {
                        s.expanded.insert(t.clone());
                    }
                });
                close_dialog(ui);
                load(ui);
                ui.set_status(format!("{n} élément(s) déplacé(s)").into());
            }
            Err(e) => {
                if ui.get_dialog().is_empty() {
                    ui.set_status(format!("Déplacement : {e}").into());
                } else {
                    fail(ui, e);
                }
                load(ui);
            }
        },
    );
}

/// End of a drag: row `from` (with the other marked rows if it is one of
/// them) dropped on row `to` (a folder, or the folder of an entry; -1: root).
pub fn dropped(ui: &AppWindow, from: i32, to: i32) {
    if to < -1 || from == to {
        return;
    }
    let found = with(|s| {
        let dragged = s.rows.get(from as usize)?.clone();
        let items: Vec<Item> = if s.marked.len() > 1 && s.marked.contains(&dragged) {
            s.marked.iter().map(item).collect()
        } else {
            vec![item(&dragged)]
        };
        let target = match s.rows.get(to as usize) {
            None => None,
            Some(Sel::Folder(id)) => Some(id.clone()),
            Some(Sel::Entry(id)) => s.tree.entries.iter().find(|e| &e.id == id)?.folder_id.clone(),
        };
        let name = target
            .as_ref()
            .map(|t| format!("« {} »", folder_path(&s.tree, Some(t))))
            .unwrap_or_else(|| "la racine".into());
        Some((items, target, name))
    });
    let Some((items, target, name)) = found else { return };
    // Nothing to do (already there): no confirmation.
    match with(|s| moves::plan_move(&s.tree, &items, target.as_deref())) {
        Ok(p) if p.is_empty() => return,
        Err(e) => {
            ui.set_status(format!("Déplacement impossible : {e}").into());
            return;
        }
        Ok(_) => {}
    }
    if ui.get_server_mode() {
        let what = with(|s| describe(s, &items));
        features::confirm_as(ui, format!("Déplacer {what} vers {name} ?"), "Déplacer", move |ui| {
            apply_move(ui, items, target)
        });
    } else {
        apply_move(ui, items, target);
    }
}
