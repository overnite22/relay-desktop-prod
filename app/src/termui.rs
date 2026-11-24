//! Keyboard and mouse of SSH tabs, like Windows Terminal / PuTTY: mouse
//! selection (drag, double-click word, triple-click line, Shift+click),
//! Ctrl+C / Ctrl+Shift+C / Ctrl+Insert copy, Ctrl+V / Ctrl+Shift+V /
//! Shift+Insert / right or middle click paste, mouse reporting to remote
//! applications (Shift forces the local selection).

use super::*;
use relay_desktop_core::termsel::{self, Mods, MouseEvent, MouseMode, Point, Unit};
use std::time::{Duration, Instant};

/// Pointer held down in the terminal.
#[derive(Clone, Copy)]
struct Drag {
    /// Reported to the remote application instead of selecting.
    app: bool,
    button: u8,
    /// Last position, physical pixels relative to the terminal image.
    x: f32,
    y: f32,
    /// Last reported cell (application drags).
    cell: (u16, u16),
}

#[derive(Default)]
struct Pointer {
    drag: Option<Drag>,
    /// Time, cell and count of the last left click (double / triple click).
    last_click: Option<(Instant, Point, u32)>,
}

thread_local! {
    static POINTER: RefCell<Pointer> = RefCell::default();
}

fn pointer<R>(f: impl FnOnce(&mut Pointer) -> R) -> R {
    POINTER.with(|p| f(&mut p.borrow_mut()))
}

fn clipboard_set(text: &str) -> bool {
    arboard::Clipboard::new().and_then(|mut c| c.set_text(text)).is_ok()
}

/// Copies the selection of the active terminal (and clears it if `clear`);
/// false when nothing is selected.
fn copy(ui: &AppWindow, clear: bool) -> bool {
    let text = active_session(ui, |x| {
        let mut t = x.term.as_ref()?.lock().unwrap();
        let text = t.selection_text();
        if clear {
            t.clear_selection();
        }
        text
    })
    .flatten();
    let Some(text) = text else { return false };
    let msg = if clipboard_set(&text) {
        format!("Copié dans le presse-papiers ({} caractères)", text.chars().count())
    } else {
        "Presse-papiers indisponible".to_string()
    };
    ui.set_status(msg.into());
    if clear {
        redraw(ui);
    }
    true
}

/// Sends the clipboard text to the active terminal.
fn paste(ui: &AppWindow) {
    let Some(text) = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()) else {
        ui.set_status("Presse-papiers vide ou sans texte".into());
        return;
    };
    if text.is_empty() {
        return;
    }
    let sent = active_session(ui, |x| {
        let (Some(term), Some(input)) = (&x.term, &x.input) else {
            return None;
        };
        let mut t = term.lock().unwrap();
        let bracketed = t.bracketed_paste();
        t.clear_selection();
        t.scroll(-1_000_000);
        input
            .send(ssh::Input::Data(t.encode_input(termsel::paste_bytes(&text, bracketed))))
            .ok()
            .map(|_| bracketed)
    })
    .flatten();
    if let Some(bracketed) = sent {
        let lines = text.trim_end_matches(['\r', '\n']).lines().count();
        if !bracketed && text.contains('\n') {
            // Non-blocking warning: the shell runs each line as it arrives.
            ui.set_status(
                format!("Collé : {lines} ligne(s) exécutée(s) au fur et à mesure (le serveur n'utilise pas le collage encadré)").into(),
            );
        }
        redraw(ui);
    }
}

fn key_is(text: &SharedString, k: slint::platform::Key) -> bool {
    *text == SharedString::from(k)
}

fn on_key(ui: &AppWindow, text: SharedString, ctrl: bool, alt: bool, shift: bool) {
    use slint::platform::Key;
    if shift && (key_is(&text, Key::PageUp) || key_is(&text, Key::PageDown)) {
        let up = key_is(&text, Key::PageUp);
        active_session(ui, |x| x.term.as_ref().map(|t| t.lock().unwrap().scroll(if up { 10 } else { -10 })));
        redraw(ui);
        return;
    }
    let letter = |c: &str| text.as_str().eq_ignore_ascii_case(c);
    let selected = || active_session(ui, |x| x.term.as_ref().is_some_and(|t| t.lock().unwrap().has_selection())) == Some(true);
    // Copy: Ctrl+Shift+C and Ctrl+Insert always, Ctrl+C when text is selected
    // (otherwise ^C goes to the shell).
    if ctrl && !alt && ((letter("c") && (shift || selected())) || key_is(&text, Key::Insert)) {
        copy(ui, true);
        return;
    }
    if (ctrl && !alt && letter("v")) || (shift && !ctrl && !alt && key_is(&text, Key::Insert)) {
        paste(ui);
        return;
    }
    let sent = active_session(ui, |x| {
        let (Some(term), Some(input)) = (&x.term, &x.input) else {
            return false;
        };
        let mut t = term.lock().unwrap();
        let Some(bytes) = term::key_bytes(&text, ctrl, alt, t.application_cursor()) else {
            return false;
        };
        let bytes = t.encode_input(bytes);
        t.clear_selection();
        t.scroll(-1_000_000);
        input.send(ssh::Input::Data(bytes)).is_ok()
    });
    if sent == Some(true) {
        redraw(ui);
    }
}

/// What to do once the terminal lock is released.
enum After {
    Nothing,
    Redraw,
    Copy,
    CopyClear,
    Paste,
}

fn on_mouse(ui: &AppWindow, kind: i32, button: i32, x: f32, y: f32, mods: Mods) {
    let drag = pointer(|p| p.drag);
    // Hover without a button: nothing to do.
    if kind == 2 && drag.is_none() {
        return;
    }
    if kind == 3 {
        pointer(|p| p.drag = None);
        return;
    }
    let scale = ui.window().scale_factor();
    let (px, py) = (x * scale, y * scale);
    let button = button.clamp(0, 2) as u8;
    let copy_on_select = with(|s| s.settings.term_copy_on_select);
    let after = active_session(ui, |s| {
        let Some(term) = &s.term else { return After::Nothing };
        let mut t = term.lock().unwrap();
        let (cell, bound, _) = t.hit(px, py);
        let row = t.view_row(cell.line).unwrap_or(0);
        let report = |t: &term::Term, ev: MouseEvent| {
            if let (Some(input), Some(bytes)) = (
                &s.input,
                termsel::mouse_report(t.mouse_mode(), t.mouse_encoding(), ev, cell.col, row, mods),
            ) {
                let _ = input.send(ssh::Input::Data(bytes));
            }
        };
        match (kind, drag) {
            // Press.
            (0, _) => {
                if s.input.is_some() && t.mouse_mode() != MouseMode::None && !mods.shift {
                    t.clear_selection();
                    report(&t, MouseEvent::Press(button));
                    pointer(|p| {
                        p.drag = Some(Drag {
                            app: true,
                            button,
                            x: px,
                            y: py,
                            cell: (cell.col, row),
                        })
                    });
                    return After::Redraw;
                }
                if button != 0 {
                    return After::Nothing;
                }
                let clicks = pointer(|p| {
                    let n = match p.last_click {
                        Some((at, c, n))
                            if at.elapsed() < Duration::from_millis(500) && c.line == cell.line && c.col.abs_diff(cell.col) <= 1 =>
                        {
                            n % 3 + 1
                        }
                        _ => 1,
                    };
                    p.last_click = Some((Instant::now(), cell, n));
                    p.drag = Some(Drag {
                        app: false,
                        button,
                        x: px,
                        y: py,
                        cell: (cell.col, row),
                    });
                    n
                });
                if mods.shift && clicks == 1 && t.has_anchor() {
                    t.select_extend(cell, bound);
                } else {
                    t.select_start(Unit::from_clicks(clicks), cell, bound);
                }
                After::Redraw
            }
            // Move with a button held.
            (2, Some(mut d)) => {
                if d.app {
                    if matches!(t.mouse_mode(), MouseMode::ButtonMotion | MouseMode::AnyMotion) && d.cell != (cell.col, row) {
                        report(&t, MouseEvent::Drag(d.button));
                        d.cell = (cell.col, row);
                    }
                } else {
                    t.select_extend(cell, bound);
                }
                (d.x, d.y) = (px, py);
                pointer(|p| p.drag = Some(d));
                if d.app {
                    After::Nothing
                } else {
                    After::Redraw
                }
            }
            // Release of the button that started the drag.
            (1, Some(d)) if d.button == button => {
                pointer(|p| p.drag = None);
                if d.app {
                    report(&t, MouseEvent::Release(button));
                    After::Nothing
                } else if !t.has_selection() {
                    // A plain click: the empty selection stays as the anchor of Shift+click.
                    After::Redraw
                } else if copy_on_select {
                    After::Copy
                } else {
                    After::Nothing
                }
            }
            // Right click: copies the selection (and clears it) or pastes;
            // middle click pastes.
            (1, _) if button == 2 => {
                if t.has_selection() {
                    After::CopyClear
                } else {
                    After::Paste
                }
            }
            (1, _) if button == 1 => After::Paste,
            _ => After::Nothing,
        }
    });
    match after.unwrap_or(After::Nothing) {
        After::Nothing => {}
        After::Redraw => redraw(ui),
        After::Copy => {
            copy(ui, false);
            redraw(ui);
        }
        After::CopyClear => {
            copy(ui, true);
        }
        After::Paste => paste(ui),
    }
}

fn on_wheel(ui: &AppWindow, n: i32, x: f32, y: f32, shift: bool) {
    let scale = ui.window().scale_factor();
    active_session(ui, |s| {
        let Some(term) = &s.term else { return };
        let mut t = term.lock().unwrap();
        if let (false, Some(input)) = (shift, &s.input) {
            let up = n > 0;
            if t.mouse_mode() != MouseMode::None {
                let (cell, _, _) = t.hit(x * scale, y * scale);
                let row = t.view_row(cell.line).unwrap_or(0);
                let ev = MouseEvent::Wheel { up };
                if let Some(bytes) = termsel::mouse_report(t.mouse_mode(), t.mouse_encoding(), ev, cell.col, row, Mods::default()) {
                    let _ = input.send(ssh::Input::Data(bytes));
                }
                return;
            }
            if t.alternate_screen() {
                // less, man, vim without mouse: arrow keys (Windows Terminal).
                let c = if up { 'A' } else { 'B' };
                let seq = if t.application_cursor() {
                    format!("\x1bO{c}")
                } else {
                    format!("\x1b[{c}")
                };
                let _ = input.send(ssh::Input::Data(seq.repeat(n.unsigned_abs() as usize).into_bytes()));
                return;
            }
        }
        t.scroll(n as isize);
    });
    redraw(ui);
}

/// Scrolls the history while a selection is dragged above or below the
/// terminal.
fn autoscroll(ui: &AppWindow) {
    let Some(d) = pointer(|p| p.drag).filter(|d| !d.app) else { return };
    let moved = active_session(ui, |s| {
        let mut t = s.term.as_ref()?.lock().unwrap();
        let (_, _, outside) = t.hit(d.x, d.y);
        if outside == 0 {
            return None;
        }
        t.scroll(-(outside.clamp(-5, 5) as isize));
        let (cell, bound, _) = t.hit(d.x, d.y);
        t.select_extend(cell, bound);
        Some(())
    })
    .flatten();
    if moved.is_some() {
        redraw(ui);
    }
}

pub fn install(ui: &AppWindow) -> slint::Timer {
    let w = ui.as_weak();
    ui.on_term_key(move |text, ctrl, alt, shift| {
        if let Some(ui) = w.upgrade() {
            on_key(&ui, text, ctrl, alt, shift);
        }
    });
    let w = ui.as_weak();
    ui.on_term_mouse(move |kind, button, x, y, shift, ctrl, alt| {
        if let Some(ui) = w.upgrade() {
            on_mouse(&ui, kind, button, x, y, Mods { shift, alt, ctrl });
        }
    });
    let w = ui.as_weak();
    ui.on_term_wheel(move |n, x, y, shift| {
        if let Some(ui) = w.upgrade() {
            on_wheel(&ui, n, x, y, shift);
        }
    });
    let w = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(60), move || {
        if let Some(ui) = w.upgrade() {
            autoscroll(&ui);
        }
    });
    timer
}
