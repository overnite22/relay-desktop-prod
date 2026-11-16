//! Entry notes rendered from Markdown, their editor, and entry attachments
//! (server API or encrypted table of the local database).

use super::*;
use relay_desktop_core::markdown::{self, Block};
use relay_desktop_core::{attachment_name, Attachment, MAX_ATTACHMENT_SIZE};
use slint::StyledText;

fn styled(md: &str) -> StyledText {
    StyledText::from_markdown(md).unwrap_or_else(|_| StyledText::from_plain_text(md))
}

fn block(b: Block) -> NoteBlock {
    let mut n = NoteBlock::default();
    match b {
        Block::Heading { level, text } => {
            n.kind = "h".into();
            n.depth = level as i32;
            n.text = styled(&format!("**{text}**"));
        }
        Block::Paragraph(t) => {
            n.kind = "p".into();
            n.text = styled(&t);
        }
        Block::Item { depth, marker, text } => {
            n.kind = "li".into();
            n.depth = depth as i32;
            n.marker = marker.into();
            n.text = styled(&text);
        }
        Block::Quote(t) => {
            n.kind = "quote".into();
            n.text = styled(&t);
        }
        Block::Code(t) => {
            n.kind = "code".into();
            n.plain = t.into();
        }
        Block::Table(t) => {
            n.kind = "table".into();
            n.plain = t.into();
        }
        Block::Rule => n.kind = "hr".into(),
    }
    n
}

/// Shows the notes of the selected entry and refreshes its attachments when
/// the selection changed.
pub fn show(ui: &AppWindow, notes: &str) {
    let blocks: Vec<NoteBlock> = markdown::parse(notes).into_iter().map(block).collect();
    ui.set_note_blocks(ModelRc::new(VecModel::from(blocks)));
    let entry = with(|s| match &s.current {
        Some(Sel::Entry(id)) => Some(id.clone()),
        _ => None,
    });
    if with(|s| s.att_entry != entry) {
        with(|s| {
            s.att_entry = entry.clone();
            s.attachments.clear();
        });
        fill(ui);
        if entry.is_some() {
            load(ui);
        }
    }
}

fn size(n: i64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} Mio", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} Kio", n as f64 / 1024.0),
        n => format!("{n} o"),
    }
}

fn fill(ui: &AppWindow) {
    let rows: Vec<ListRow> = with(|s| {
        s.attachments
            .iter()
            .map(|a| ListRow {
                name: a.name.clone().into(),
                detail: size(a.size).into(),
                extra: a.created_at.replace('T', " ").chars().take(16).collect::<String>().into(),
                status: 0,
            })
            .collect()
    });
    if rows.is_empty() && ui.get_att_info().is_empty() {
        ui.set_att_info("Aucune pièce jointe.".into());
    }
    ui.set_att_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_att_selected(-1);
}

/// Loads the attachments of the selected entry without blocking the UI.
fn load(ui: &AppWindow) {
    let (Some(src), Some(id)) = (source(), with(|s| s.att_entry.clone())) else {
        return;
    };
    ui.set_att_info("Chargement…".into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let r = src.attachments(&id);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            if with(|s| s.att_entry.as_deref() != Some(id.as_str())) {
                return;
            }
            match r {
                Ok(list) => {
                    ui.set_att_info("Aucune pièce jointe.".into());
                    with(|s| s.attachments = list);
                }
                Err(e) => ui.set_att_info(format!("Pièces jointes indisponibles : {e}").into()),
            }
            fill(&ui);
        });
    });
}

fn selected(ui: &AppWindow) -> Option<(String, Attachment)> {
    let i = usize::try_from(ui.get_att_selected()).ok()?;
    with(|s| Some((s.att_entry.clone()?, s.attachments.get(i)?.clone())))
}

fn current_entry(s: &State) -> Option<Entry> {
    match &s.current {
        Some(Sel::Entry(id)) => s.tree.entries.iter().find(|e| &e.id == id).cloned(),
        _ => None,
    }
}

/// Handles the notes and attachment actions; returns false for other ids.
pub fn act(ui: &AppWindow, id: &str) -> bool {
    match id {
        "notes-edit" => {
            let Some(e) = with(|s| current_entry(s)) else { return true };
            ui.set_notes_text(e.notes.into());
            ui.set_dialog_error("".into());
            ui.set_dialog("notes".into());
        }
        "att-add" => add(ui),
        "att-get" => download(ui),
        "att-del" => {
            let (Some(src), Some((entry, a))) = (source(), selected(ui)) else {
                return true;
            };
            features::confirm(ui, format!("Supprimer la pièce jointe « {} » ?", a.name), move |ui| {
                bg(
                    ui,
                    "Suppression…",
                    move || src.delete_attachment(&entry, &a.id),
                    |ui, r| match r {
                        Ok(()) => {
                            close_dialog(ui);
                            ui.set_status("Pièce jointe supprimée".into());
                            load(ui);
                        }
                        Err(e) => fail(ui, e),
                    },
                );
            });
        }
        _ => return false,
    }
    true
}

/// Saves the notes dialog; returns false when another dialog is open.
pub fn submit(ui: &AppWindow) -> bool {
    if ui.get_dialog() != "notes" {
        return false;
    }
    let (Some(src), Some(mut e)) = (source(), with(|s| current_entry(s))) else {
        return true;
    };
    e.notes = ui.get_notes_text().to_string();
    bg(
        ui,
        "Enregistrement des notes…",
        move || src.save_entry(&e),
        |ui, r| match r {
            Ok(saved) => {
                close_dialog(ui);
                with(|s| s.current = Some(Sel::Entry(saved.id)));
                load_tree(ui);
            }
            Err(e) => fail(ui, e),
        },
    );
    true
}

fn load_tree(ui: &AppWindow) {
    super::load(ui);
}

fn add(ui: &AppWindow) {
    #[cfg(windows)]
    {
        let (Some(src), Some(entry)) = (source(), with(|s| s.att_entry.clone())) else {
            return;
        };
        let Some(path) = rfd::FileDialog::new().set_title("Ajouter une pièce jointe").pick_file() else {
            return;
        };
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let Some(name) = attachment_name(&name) else {
            return ui.set_status("Nom de fichier invalide".into());
        };
        bg(
            ui,
            &format!("Envoi de « {name} »…"),
            move || {
                let len = std::fs::metadata(&path).map_err(|e| Error::Invalid(e.to_string()))?.len();
                if len as usize > MAX_ATTACHMENT_SIZE {
                    return Err(Error::Invalid("le fichier dépasse 10 Mio".into()));
                }
                let data = std::fs::read(&path).map_err(|e| Error::Invalid(e.to_string()))?;
                src.add_attachment(&entry, &name, &data)
            },
            |ui, r| match r {
                Ok(a) => {
                    ui.set_status(format!("Pièce jointe « {} » ajoutée", a.name).into());
                    load(ui);
                }
                Err(e) => ui.set_status(e.to_string().into()),
            },
        );
    }
    #[cfg(not(windows))]
    let _ = (ui, attachment_name, MAX_ATTACHMENT_SIZE);
}

fn download(ui: &AppWindow) {
    #[cfg(windows)]
    {
        let (Some(src), Some((entry, a))) = (source(), selected(ui)) else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("Enregistrer la pièce jointe")
            .set_file_name(&a.name)
            .save_file()
        else {
            return;
        };
        bg(
            ui,
            &format!("Téléchargement de « {} »…", a.name),
            move || {
                let data = src.attachment_data(&entry, &a.id)?;
                std::fs::write(&path, data).map_err(|e| Error::Invalid(e.to_string()))?;
                Ok(path)
            },
            |ui, r| match r {
                Ok(p) => ui.set_status(format!("Enregistré : {}", p.display()).into()),
                Err(e) => ui.set_status(e.to_string().into()),
            },
        );
    }
    #[cfg(not(windows))]
    let _ = (ui, selected);
}

/// Opens a link of the notes in the default browser (http and https only).
pub fn open_link(ui: &AppWindow, url: &str) {
    if !markdown::safe_link(url) {
        return ui.set_status("Lien ignoré : seuls les liens http et https sont ouverts.".into());
    }
    let msg = session::open(session::Target {
        kind: Kind::Web,
        host: String::new(),
        port: 0,
        url: url.trim().to_string(),
        username: String::new(),
        domain: String::new(),
        password: None,
    });
    if let Err(e) = msg {
        ui.set_status(e.into());
    }
}
