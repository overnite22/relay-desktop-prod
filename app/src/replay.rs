//! Native replay of the server recordings (Guacamole protocol, see
//! `relay_desktop_core::guac`) in a tab: play / pause, position and speed.

use super::*;
use relay_desktop_core::guac::{Player, Recording};
use std::time::Instant;

pub const SPEEDS: [f32; 5] = [0.5, 1.0, 2.0, 4.0, 8.0];

pub struct Replay {
    player: Player,
    playing: bool,
    /// Position when the play clock was (re)started, and that instant.
    base_ms: u64,
    started: Instant,
    speed: f32,
    pos_ms: u64,
}

impl Replay {
    fn now_ms(&self) -> u64 {
        if self.playing {
            self.base_ms + (self.started.elapsed().as_secs_f64() * 1000.0 * self.speed as f64) as u64
        } else {
            self.base_ms
        }
    }

    fn restart_clock(&mut self, at: u64) {
        self.base_ms = at;
        self.started = Instant::now();
    }
}

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Opens a replay tab (`kind` = protocol of the recording, for the icon).
pub fn open(ui: &AppWindow, title: &str, kind: &str, rec: Recording) {
    let id = with(|s| {
        s.next_session += 1;
        s.next_session
    });
    let mut player = Player::new(rec);
    player.seek(0);
    let replay = Replay {
        player,
        playing: true,
        base_ms: 0,
        started: Instant::now(),
        speed: 1.0,
        pos_ms: 0,
    };
    let icon = match kind {
        "rdp" => "rdp",
        "vnc" => "vnc",
        _ => "ssh",
    };
    let index = with(|s| {
        s.sessions.push(Session {
            id,
            entry_id: String::new(),
            title: format!("▶ {title}"),
            icon,
            state: "connected",
            banner: String::new(),
            kind: "replay",
            term: None,
            input: None,
            rdp: None,
            pve: None,
            pt: 0.0,
            rec: None,
            player: None,
            files: None,
            replay: Some(Box::new(replay)),
        });
        s.sessions.len() as i32 - 1
    });
    ui.set_rp_speed(1);
    show_tabs(ui);
    ui.set_active_tab(index);
    redraw(ui);
}

/// Pushes the image and the controls of the active replay.
pub fn show(ui: &AppWindow) {
    let view = active_session(ui, |x| {
        let r = x.replay.as_ref()?;
        let (w, h, px) = r.player.render();
        Some((w, h, px, r.playing, r.pos_ms, r.player.duration_ms(), r.speed))
    })
    .flatten();
    let Some((w, h, px, playing, pos, duration, speed)) = view else {
        return;
    };
    if w > 0 && h > 0 {
        let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&px, w, h);
        ui.set_term_image(slint::Image::from_rgba8(buf));
    } else {
        ui.set_term_image(slint::Image::default());
    }
    ui.set_rp_playing(playing);
    ui.set_rp_pos(if duration > 0 {
        pos as f32 / duration as f32 * 1000.0
    } else {
        1000.0
    });
    ui.set_rp_time(format!("{} / {}", clock(pos), clock(duration)).into());
    if let Some(i) = SPEEDS.iter().position(|s| *s == speed) {
        ui.set_rp_speed(i as i32);
    }
}

/// Advances the active replay (40 ms timer).
pub fn tick(ui: &AppWindow) {
    let changed = active_session(ui, |x| {
        let r = x.replay.as_mut()?;
        if !r.playing {
            return None;
        }
        let duration = r.player.duration_ms();
        let t = r.now_ms().min(duration);
        let drawn = r.player.seek(t);
        let moved = t / 1000 != r.pos_ms / 1000;
        r.pos_ms = t;
        let ended = t >= duration;
        if ended {
            r.playing = false;
            r.restart_clock(t);
            x.banner = "Lecture terminée".into();
        }
        Some(drawn || moved || ended)
    })
    .flatten();
    if changed == Some(true) {
        redraw(ui);
    }
}

pub fn toggle(ui: &AppWindow) {
    active_session(ui, |x| {
        let Some(r) = x.replay.as_mut() else { return };
        let duration = r.player.duration_ms();
        if r.playing {
            let t = r.now_ms().min(duration);
            r.playing = false;
            r.restart_clock(t);
            r.pos_ms = t;
        } else {
            // Play again from the start once the end is reached.
            let t = if r.pos_ms >= duration { 0 } else { r.pos_ms };
            r.player.seek(t);
            r.pos_ms = t;
            r.playing = true;
            r.restart_clock(t);
            x.banner.clear();
        }
    });
    redraw(ui);
}

/// Slider moved: `value` between 0 and 1000.
pub fn seek(ui: &AppWindow, value: f32) {
    active_session(ui, |x| {
        let Some(r) = x.replay.as_mut() else { return };
        let t = (value.clamp(0.0, 1000.0) as f64 / 1000.0 * r.player.duration_ms() as f64) as u64;
        r.player.seek(t);
        r.pos_ms = t;
        r.restart_clock(t);
        x.banner.clear();
    });
    redraw(ui);
}

pub fn set_speed(ui: &AppWindow, index: i32) {
    active_session(ui, |x| {
        let Some(r) = x.replay.as_mut() else { return };
        let t = r.now_ms().min(r.player.duration_ms());
        r.speed = SPEEDS.get(index as usize).copied().unwrap_or(1.0);
        r.restart_clock(t);
    });
}
