//! Terminal emulation (vt100) rendered to an RGBA image with a monospace
//! system font (Consolas), for embedded SSH tabs.

use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::collections::HashMap;
use std::num::NonZeroU16;
use std::sync::Arc;

const BG: [u8; 3] = [0x0c, 0x0c, 0x0c];
const FG: [u8; 3] = [0xcc, 0xcc, 0xcc];
/// Campbell palette (Windows Terminal) for the 16 base colors.
const BASE: [[u8; 3]; 16] = [
    [0x0c, 0x0c, 0x0c],
    [0xc5, 0x0f, 0x1f],
    [0x13, 0xa1, 0x0e],
    [0xc1, 0x9c, 0x00],
    [0x00, 0x37, 0xda],
    [0x88, 0x17, 0x98],
    [0x3a, 0x96, 0xdd],
    [0xcc, 0xcc, 0xcc],
    [0x76, 0x76, 0x76],
    [0xe7, 0x48, 0x56],
    [0x16, 0xc6, 0x0c],
    [0xf9, 0xf1, 0xa5],
    [0x3b, 0x78, 0xff],
    [0xb4, 0x00, 0x9e],
    [0x61, 0xd6, 0xd6],
    [0xf2, 0xf2, 0xf2],
];

pub struct Fonts {
    regular: fontdue::Font,
    bold: Option<fontdue::Font>,
}

impl Fonts {
    /// Loads the Windows font file `file` (falls back on Consolas or another
    /// monospace font shipped with Windows).
    pub fn load(file: &str) -> Option<Arc<Fonts>> {
        let dir = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| r"C:\Windows".into())
            .join("Fonts");
        let read = |name: &str| {
            std::fs::read(dir.join(name))
                .ok()
                .and_then(|b| fontdue::Font::from_bytes(b, fontdue::FontSettings::default()).ok())
        };
        let (name, regular) = [file, "consola.ttf", "CascadiaMono.ttf", "lucon.ttf", "cour.ttf"]
            .iter()
            .find_map(|n| read(n).map(|f| (*n, f)))?;
        let bold = match name {
            "consola.ttf" => read("consolab.ttf"),
            "cour.ttf" => read("courbd.ttf"),
            _ => None,
        };
        Some(Arc::new(Fonts { regular, bold }))
    }
}

/// Colours and history of a terminal (entry settings).
#[derive(Clone, Copy)]
pub struct Style {
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub scrollback: usize,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            fg: FG,
            bg: BG,
            scrollback: 5000,
        }
    }
}

pub struct Term {
    parser: vt100::Parser,
    fonts: Arc<Fonts>,
    style: Style,
    px: f32,
    cell_w: usize,
    cell_h: usize,
    ascent: f32,
    glyphs: HashMap<(char, bool), (fontdue::Metrics, Vec<u8>)>,
    scroll: usize,
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v.max(1)).unwrap()
}

fn color(c: vt100::Color, default: [u8; 3]) -> [u8; 3] {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => [r, g, b],
        vt100::Color::Idx(i) if i < 16 => BASE[i as usize],
        vt100::Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            [level(i / 36), level((i / 6) % 6), level(i % 6)]
        }
        vt100::Color::Idx(i) => {
            let v = 8 + (i - 232) * 10;
            [v, v, v]
        }
    }
}

impl Term {
    pub fn new(fonts: Arc<Fonts>, px: f32, rows: u16, cols: u16, style: Style) -> Term {
        let mut t = Term {
            parser: vt100::Parser::new(nz(rows), nz(cols), style.scrollback),
            fonts,
            style,
            px: 0.0,
            cell_w: 1,
            cell_h: 1,
            ascent: 0.0,
            glyphs: HashMap::new(),
            scroll: 0,
        };
        t.set_px(px);
        t
    }

    /// Changes the font size: new cell metrics, glyph cache dropped.
    pub fn set_px(&mut self, px: f32) {
        let px = px.max(4.0);
        if px == self.px {
            return;
        }
        let lm = self.fonts.regular.horizontal_line_metrics(px);
        // Some fonts (Lucida Console) declare a line smaller than their glyphs:
        // grow the cell to the real extent of tall and descending characters so
        // a row background never covers the row above or below.
        let (mut top, mut bottom) = (0f32, 0f32);
        for ch in ['M', 'É', 'g', 'j', 'p', 'y', '|', '_', '(', '['] {
            let m = self.fonts.regular.metrics(ch, px);
            top = top.max((m.height as i32 + m.ymin) as f32);
            bottom = bottom.max(-m.ymin as f32);
        }
        self.ascent = lm.map(|m| m.ascent).unwrap_or(px * 0.8).max(top).ceil();
        let line = lm.map(|m| m.new_line_size).unwrap_or(px * 1.2);
        self.cell_h = line.max(self.ascent + bottom).ceil().max(1.0) as usize;
        self.cell_w = self.fonts.regular.metrics('M', px).advance_width.ceil().max(1.0) as usize;
        self.px = px;
        self.glyphs.clear();
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// Grid that fits `w`×`h` physical pixels.
    pub fn grid_for(&self, w: f32, h: f32) -> (u16, u16) {
        let rows = (h as usize / self.cell_h).clamp(2, 500) as u16;
        let cols = (w as usize / self.cell_w).clamp(10, 1000) as u16;
        (rows, cols)
    }

    pub fn size(&self) -> (u16, u16) {
        let (r, c) = self.parser.screen().size();
        (r.get(), c.get())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.set_size(nz(rows), nz(cols));
    }

    pub fn application_cursor(&self) -> bool {
        self.parser.screen().application_cursor()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    /// Scrolls the view back `lines` rows (0 = live screen).
    pub fn scroll(&mut self, delta: isize) {
        let max = self.parser.screen().scrollback_len() as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max) as usize;
        self.parser.screen_mut().set_scrollback(self.scroll);
    }

    pub fn render(&mut self) -> SharedPixelBuffer<Rgba8Pixel> {
        let (rows, cols) = self.size();
        let (w, h) = (cols as usize * self.cell_w, rows as usize * self.cell_h);
        let (fg0, bg0) = (self.style.fg, self.style.bg);
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w as u32, h as u32);
        let px = buf.make_mut_bytes();
        for chunk in px.as_chunks_mut::<4>().0 {
            chunk.copy_from_slice(&[bg0[0], bg0[1], bg0[2], 255]);
        }
        let screen = self.parser.screen();
        let glyphs = &mut self.glyphs;
        let fonts = &self.fonts;
        let (size, ascent, cell_w, cell_h) = (self.px, self.ascent, self.cell_w, self.cell_h);
        let (cur_r, cur_c) = screen.cursor_position();
        let show_cursor = !screen.hide_cursor() && self.scroll == 0;
        for (pass, r) in (0..2).flat_map(|p| (0..rows).map(move |r| (p, r))) {
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut fg = color(cell.fgcolor(), fg0);
                let mut bg = color(cell.bgcolor(), bg0);
                if cell.bold() {
                    if let vt100::Color::Idx(i) = cell.fgcolor() {
                        if i < 8 {
                            fg = BASE[i as usize + 8];
                        }
                    }
                }
                if cell.inverse() ^ (show_cursor && r == cur_r && c == cur_c) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let span = if cell.is_wide() { 2 } else { 1 };
                let (x0, y0) = (c as usize * cell_w, r as usize * cell_h);
                if pass == 0 {
                    if bg != bg0 {
                        for y in y0..y0 + cell_h {
                            for x in x0..(x0 + cell_w * span).min(w) {
                                let i = (y * w + x) * 4;
                                px[i..i + 3].copy_from_slice(&bg);
                            }
                        }
                    }
                    continue;
                }
                // Glyphs come after every background so a glyph running past
                // its cell is never covered by the next row or column.
                let text = cell.contents();
                if let Some(ch) = text.chars().next().filter(|c| !c.is_whitespace()) {
                    draw_glyph(glyphs, fonts, size, ascent, px, (w, h), (x0, y0), ch, cell.bold(), fg);
                }
                if cell.underline() {
                    let y = y0 + cell_h - 2;
                    for x in x0..(x0 + cell_w * span).min(w) {
                        let i = (y * w + x) * 4;
                        px[i..i + 3].copy_from_slice(&fg);
                    }
                }
            }
        }
        buf
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_glyph(
    glyphs: &mut HashMap<(char, bool), (fontdue::Metrics, Vec<u8>)>,
    fonts: &Fonts,
    size: f32,
    ascent: f32,
    px: &mut [u8],
    (w, h): (usize, usize),
    (x0, y0): (usize, usize),
    ch: char,
    bold: bool,
    fg: [u8; 3],
) {
    let (m, bitmap) = glyphs.entry((ch, bold)).or_insert_with(|| {
        let font = if bold {
            fonts.bold.as_ref().unwrap_or(&fonts.regular)
        } else {
            &fonts.regular
        };
        font.rasterize(ch, size)
    });
    let top = y0 as i32 + ascent.round() as i32 - m.height as i32 - m.ymin;
    let left = x0 as i32 + m.xmin;
    for gy in 0..m.height {
        let y = top + gy as i32;
        if y < 0 || y as usize >= h {
            continue;
        }
        for gx in 0..m.width {
            let x = left + gx as i32;
            if x < 0 || x as usize >= w {
                continue;
            }
            let a = bitmap[gy * m.width + gx] as u32;
            if a == 0 {
                continue;
            }
            let i = (y as usize * w + x as usize) * 4;
            for k in 0..3 {
                px[i + k] = ((fg[k] as u32 * a + px[i + k] as u32 * (255 - a)) / 255) as u8;
            }
        }
    }
}

/// Translates a Slint key event into bytes for the remote terminal.
pub fn key_bytes(text: &str, ctrl: bool, alt: bool, app_cursor: bool) -> Option<Vec<u8>> {
    use slint::platform::Key;
    let is = |k: Key| text == slint::SharedString::from(k).as_str();
    let arrow = |c: char| Some(if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") }.into_bytes());
    let fkeys = [
        (Key::F1, "\x1bOP"),
        (Key::F2, "\x1bOQ"),
        (Key::F3, "\x1bOR"),
        (Key::F4, "\x1bOS"),
        (Key::F5, "\x1b[15~"),
        (Key::F6, "\x1b[17~"),
        (Key::F7, "\x1b[18~"),
        (Key::F8, "\x1b[19~"),
        (Key::F9, "\x1b[20~"),
        (Key::F10, "\x1b[21~"),
        (Key::F11, "\x1b[23~"),
        (Key::F12, "\x1b[24~"),
    ];
    if is(Key::Return) {
        return Some(b"\r".to_vec());
    }
    if is(Key::Backspace) {
        return Some(if ctrl { b"\x08".to_vec() } else { b"\x7f".to_vec() });
    }
    if is(Key::Tab) {
        return Some(b"\t".to_vec());
    }
    if is(Key::Escape) {
        return Some(b"\x1b".to_vec());
    }
    if is(Key::UpArrow) {
        return arrow('A');
    }
    if is(Key::DownArrow) {
        return arrow('B');
    }
    if is(Key::RightArrow) {
        return arrow('C');
    }
    if is(Key::LeftArrow) {
        return arrow('D');
    }
    for (k, seq) in [
        (Key::Home, "\x1b[H"),
        (Key::End, "\x1b[F"),
        (Key::PageUp, "\x1b[5~"),
        (Key::PageDown, "\x1b[6~"),
        (Key::Delete, "\x1b[3~"),
        (Key::Insert, "\x1b[2~"),
    ]
    .into_iter()
    .chain(fkeys)
    {
        if is(k) {
            return Some(seq.as_bytes().to_vec());
        }
    }
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.clone().next()) else {
        return (!text.is_empty()).then(|| text.as_bytes().to_vec());
    };
    // Slint encodes non-text keys in the Unicode private use area.
    // Modifier keys alone (Shift, Control, Alt, AltGr, CapsLock, Meta…) are
    // encoded as U+0010..U+0018 and must not reach the remote shell.
    if ('\u{E000}'..='\u{F8FF}').contains(&c) || ('\u{10}'..='\u{18}').contains(&c) {
        return None;
    }
    let mut out = Vec::new();
    if alt {
        out.push(0x1b);
    }
    if ctrl && c.is_ascii() {
        let b = c.to_ascii_uppercase() as u8;
        match b {
            b'@'..=b'_' => out.push(b & 0x1f),
            b' ' | b'2' => out.push(0),
            b'/' => out.push(0x1f),
            _ => return None,
        }
    } else {
        out.extend(c.to_string().as_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::key_bytes;
    use slint::platform::Key;
    fn k(key: Key) -> String {
        slint::SharedString::from(key).to_string()
    }
    #[test]
    fn keys() {
        assert_eq!(key_bytes(&k(Key::Return), false, false, false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(&k(Key::UpArrow), false, false, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes("c", true, false, false), Some(vec![3]));
        assert_eq!(key_bytes("x", false, true, false), Some(b"\x1bx".to_vec()));
        assert_eq!(key_bytes("é", false, false, false), Some("é".as_bytes().to_vec()));
        assert_eq!(key_bytes(&k(Key::Shift), false, false, false), None);
    }
}
