//! Local recordings of SSH tabs (asciicast v2, `%APPDATA%\Relay\recordings`)
//! and their replay in a read-only terminal tab. RDP sessions are not
//! recorded: the ActiveX control gives no access to the image stream.

use super::*;
use relay_desktop_core::cast::{self, Event, Recording};
use std::io::BufWriter;
use std::time::Instant;

pub fn dir() -> PathBuf {
    settings::dir().join("recordings")
}

/// Shared writer of a recorded tab (written from the SSH thread).
#[derive(Clone)]
pub struct Recorder(Arc<Mutex<Option<cast::Writer<BufWriter<std::fs::File>>>>>);

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn create(title: &str, cols: u16, rows: u16) -> Result<Recorder, String> {
    let d = dir();
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let now = unix_now();
    let path = d.join(cast::file_name(title, now));
    let file = std::fs::File::create(&path).map_err(|e| format!("{} : {e}", path.display()))?;
    let w = cast::Writer::new(BufWriter::new(file), cols, rows, title, now.max(0) as u64).map_err(|e| e.to_string())?;
    Ok(Recorder(Arc::new(Mutex::new(Some(w)))))
}

impl Recorder {
    fn with(&self, f: impl FnOnce(&mut cast::Writer<BufWriter<std::fs::File>>) -> std::io::Result<()>) {
        let mut g = self.0.lock().unwrap();
        // A write error (disk full…) stops the recording, not the session.
        if g.as_mut().is_some_and(|w| f(w).is_err()) {
            *g = None;
        }
    }

    pub fn output(&self, bytes: &[u8]) {
        self.with(|w| w.output(bytes));
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        self.with(|w| w.resize(cols, rows));
    }

    pub fn finish(&self) {
        self.with(|w| w.flush());
        *self.0.lock().unwrap() = None;
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if Arc::strong_count(&self.0) == 1 {
            self.with(|w| w.flush());
        }
    }
}

/// Replay state of a tab.
pub struct Player {
    rec: Recording,
    start: Instant,
    next: usize,
}

/// "Ouvrir et enregistrer": SSH only.
pub fn record(ui: &AppWindow) {
    let kind = with(|s| match &s.current {
        Some(Sel::Entry(id)) => s.tree.entries.iter().find(|e| &e.id == id).map(|e| e.kind),
        _ => None,
    });
    match kind {
        Some(Kind::Ssh) => {
            with(|s| s.record_next = true);
            open_session(ui, false);
        }
        _ => ui.set_status("Enregistrement local disponible pour les sessions SSH uniquement (non applicable au RDP intégré).".into()),
    }
}

/// "Lire un enregistrement": picks a .cast file and replays it in a tab.
pub fn play(ui: &AppWindow) {
    #[cfg(windows)]
    {
        let _ = std::fs::create_dir_all(dir());
        let Some(path) = rfd::FileDialog::new()
            .set_title("Lire un enregistrement")
            .set_directory(dir())
            .add_filter("Enregistrement asciicast", &["cast"])
            .pick_file()
        else {
            return;
        };
        bg(
            ui,
            "Lecture de l'enregistrement…",
            move || {
                let f = std::fs::File::open(&path).map_err(|e| Error::Invalid(e.to_string()))?;
                let rec = cast::parse(std::io::BufReader::new(f), 2.0).map_err(Error::Invalid)?;
                let name = path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                Ok((name, rec))
            },
            |ui, r| match r {
                Ok((name, rec)) => open_player(ui, name, rec),
                Err(e) => ui.set_status(format!("Enregistrement illisible : {e}").into()),
            },
        );
    }
    #[cfg(not(windows))]
    let _ = ui;
}

fn open_player(ui: &AppWindow, name: String, rec: Recording) {
    let (font, pt) = with(|s| {
        let size = s.settings.term_font_size;
        (
            s.settings.term_font.clone().unwrap_or_default(),
            if (6..=48).contains(&size) { size as f32 } else { 14.0 },
        )
    });
    let Some(fonts) = fonts(SshOptions::font_file(&font)) else {
        ui.set_status("Police à chasse fixe introuvable (Consolas).".into());
        return;
    };
    let scale = ui.window().scale_factor();
    ui.set_scale(scale);
    let (rows, cols) = (rec.header.height.max(2), rec.header.width.max(10));
    let term = term::Term::new(fonts, (pt * scale).round(), rows, cols, term::Style::default());
    let index = with(|s| {
        s.next_session += 1;
        let id = s.next_session;
        add_session(
            s,
            Session {
                id,
                entry_id: String::new(),
                title: format!("▶ {name}"),
                icon: "ssh",
                state: "connected",
                banner: format!("Lecture ({:.0} s)…", rec.duration()),
                kind: "ssh",
                term: Some(Arc::new(Mutex::new(term))),
                input: None,
                rdp: None,
                pve: None,
                pt,
                rec: None,
                player: Some(Player {
                    rec,
                    start: Instant::now(),
                    next: 0,
                }),
            },
        )
    });
    show_tabs(ui);
    ui.set_active_tab(index);
    redraw(ui);
}

/// Feeds the due events of every replay; redraws the active one.
pub fn tick(ui: &AppWindow) {
    let active = ui.get_active_tab();
    let mut dirty = false;
    with(|s| {
        for (i, x) in s.sessions.iter_mut().enumerate() {
            let (Some(p), Some(term)) = (x.player.as_mut(), &x.term) else {
                continue;
            };
            if p.next >= p.rec.events.len() {
                continue;
            }
            let now = p.start.elapsed().as_secs_f64();
            let mut t = term.lock().unwrap();
            while let Some((_, ev)) = p.rec.events.get(p.next).filter(|e| e.0 <= now) {
                match ev {
                    Event::Output(d) => t.feed(d.as_bytes()),
                    Event::Resize(c, r) => t.resize(*r, *c),
                }
                p.next += 1;
            }
            if p.next >= p.rec.events.len() {
                x.banner = format!("Lecture terminée ({:.0} s)", p.rec.duration());
            }
            dirty |= i as i32 == active;
        }
    });
    if dirty {
        redraw(ui);
    }
}
