//! Terminal emulation (vt100) rendered to an RGBA image with a monospace
//! system font (Consolas), for embedded SSH tabs.

use relay_desktop_core::termsel::{Line, MouseEncoding, MouseMode, Point, Selection, Unit};
use slint::{Rgba8Pixel, SharedPixelBuffer};
use std::collections::HashMap;
use std::num::NonZeroU16;
use std::sync::Arc;

use relay_desktop_core::charset::Charset;
use relay_desktop_core::highlight::Highlighter;
use relay_desktop_core::termtheme::{Theme, THEMES};

/// Fonts tried, in this order, for characters missing from the terminal font
/// (file, index in a collection): box drawing and Powerline, symbols, emoji
/// (monochrome), Japanese, Korean, Chinese.
const FALLBACK: [(&str, u32); 7] = [
    ("CascadiaMono.ttf", 0),
    ("seguisym.ttf", 0),
    ("seguiemj.ttf", 0),
    ("msgothic.ttc", 0),
    ("YuGothR.ttc", 0),
    ("malgun.ttf", 0),
    ("simsun.ttc", 0),
];

fn fonts_dir() -> std::path::PathBuf {
    std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| r"C:\Windows".into())
        .join("Fonts")
}

fn read_font(name: &str, index: u32) -> Option<fontdue::Font> {
    let bytes = std::fs::read(fonts_dir().join(name)).ok()?;
    let settings = fontdue::FontSettings {
        collection_index: index,
        ..Default::default()
    };
    fontdue::Font::from_bytes(bytes, settings).ok()
}

pub struct Fonts {
    regular: fontdue::Font,
    bold: Option<fontdue::Font>,
    /// Fallback fonts, loaded on first use.
    fallback: Vec<(&'static str, u32, std::sync::OnceLock<Option<fontdue::Font>>)>,
}

impl Fonts {
    /// Loads the Windows font file `file` (falls back on Consolas or another
    /// monospace font shipped with Windows).
    pub fn load(file: &str) -> Option<Arc<Fonts>> {
        let (name, regular) = [file, "consola.ttf", "CascadiaMono.ttf", "lucon.ttf", "cour.ttf"]
            .iter()
            .find_map(|n| read_font(n, 0).map(|f| (*n, f)))?;
        let bold = match name {
            "consola.ttf" => read_font("consolab.ttf", 0),
            "cour.ttf" => read_font("courbd.ttf", 0),
            _ => None,
        };
        let fallback = FALLBACK
            .iter()
            .filter(|(f, _)| !f.eq_ignore_ascii_case(name))
            .map(|(f, i)| (*f, *i, std::sync::OnceLock::new()))
            .collect();
        Some(Arc::new(Fonts { regular, bold, fallback }))
    }

    /// Font drawing `ch`: the terminal font when it has the glyph, else the
    /// first fallback font that has it.
    fn pick(&self, ch: char, bold: bool) -> &fontdue::Font {
        let main = if bold {
            self.bold.as_ref().unwrap_or(&self.regular)
        } else {
            &self.regular
        };
        // Private use characters differ between fonts (Powerline is drawn).
        if main.lookup_glyph_index(ch) != 0 || ('\u{e000}'..='\u{f8ff}').contains(&ch) {
            return main;
        }
        self.fallback
            .iter()
            .filter_map(|(file, index, cell)| cell.get_or_init(|| read_font(file, *index)).as_ref())
            .find(|f| f.lookup_glyph_index(ch) != 0)
            .unwrap_or(main)
    }
}

/// Colours and history of a terminal (theme of the Options or of the entry).
#[derive(Clone, Copy)]
pub struct Style {
    pub theme: &'static Theme,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub scrollback: usize,
    /// Bold text in the 8 base colours uses their bright variant.
    pub bold_bright: bool,
}

impl Default for Style {
    fn default() -> Self {
        let theme = &THEMES[0];
        Style {
            theme,
            fg: theme.fg,
            bg: theme.bg,
            scrollback: 5000,
            bold_bright: true,
        }
    }
}

/// Counts rows dropped off the top of the history (main screen) or of the
/// alternate screen, so absolute line numbers stay stable.
#[derive(Default)]
struct Dropped([i64; 2]);

impl vt100::Callbacks for Dropped {
    fn on_scroll(&mut self, _: vt100::capture::RowContents<'_>, alternate: bool) {
        self.0[usize::from(alternate)] += 1;
    }
}

pub struct Term {
    parser: vt100::Parser<Dropped>,
    fonts: Arc<Fonts>,
    style: Style,
    px: f32,
    cell_w: usize,
    cell_h: usize,
    ascent: f32,
    glyphs: HashMap<(char, bool), (fontdue::Metrics, Vec<u8>)>,
    /// Remote character set (typed and pasted text is encoded with it).
    pub charset: Charset,
    /// Syntax colouring rules, when enabled for this terminal.
    highlight: Option<Arc<Highlighter>>,
    /// Text selection and the screen (main / alternate) it belongs to.
    sel: Option<(Selection, bool)>,
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v.max(1)).unwrap()
}

fn color(c: vt100::Color, default: [u8; 3], palette: &[[u8; 3]; 16]) -> [u8; 3] {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => [r, g, b],
        vt100::Color::Idx(i) if i < 16 => palette[i as usize],
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
            parser: vt100::Parser::new_with_callbacks(nz(rows), nz(cols), style.scrollback, Dropped::default()),
            fonts,
            style,
            px: 0.0,
            cell_w: 1,
            cell_h: 1,
            ascent: 0.0,
            glyphs: HashMap::new(),
            sel: None,
            charset: Charset::Utf8,
            highlight: None,
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
        self.sel = None;
        self.parser.set_size(nz(rows), nz(cols));
    }

    pub fn application_cursor(&self) -> bool {
        self.parser.screen().application_cursor()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    pub fn alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    pub fn mouse_mode(&self) -> MouseMode {
        use vt100::MouseProtocolMode as M;
        match self.parser.screen().mouse_protocol_mode() {
            M::None => MouseMode::None,
            M::Press => MouseMode::Press,
            M::PressRelease => MouseMode::PressRelease,
            M::ButtonMotion => MouseMode::ButtonMotion,
            M::AnyMotion => MouseMode::AnyMotion,
        }
    }

    pub fn mouse_encoding(&self) -> MouseEncoding {
        use vt100::MouseProtocolEncoding as E;
        match self.parser.screen().mouse_protocol_encoding() {
            E::Default => MouseEncoding::Default,
            E::Utf8 => MouseEncoding::Utf8,
            E::Sgr => MouseEncoding::Sgr,
        }
    }

    /// Scrolls the view back `delta` rows (negative: towards the live screen).
    pub fn scroll(&mut self, delta: isize) {
        let screen = self.parser.screen_mut();
        let max = screen.scrollback_len() as isize;
        let to = (screen.scrollback() as isize + delta).clamp(0, max) as usize;
        screen.set_scrollback(to);
    }

    /// Absolute line shown on the first visible row.
    fn first_line(&self) -> i64 {
        let screen = self.parser.screen();
        let alt = screen.alternate_screen();
        let dropped = self.parser.callbacks().0[usize::from(alt)];
        if alt {
            dropped
        } else {
            dropped + (screen.scrollback_len() - screen.scrollback()) as i64
        }
    }

    /// Cell under the physical point (`x`, `y`) of the image (row clamped to
    /// the screen), the nearest cell boundary, and how many rows above
    /// (negative) or below (positive) the screen the point is.
    pub fn hit(&self, x: f32, y: f32) -> (Point, Point, i32) {
        let (rows, cols) = self.size();
        let row = (y / self.cell_h as f32).floor() as i32;
        let outside = if row < 0 { row } else { (row - rows as i32 + 1).max(0) };
        let line = self.first_line() + i64::from(row.clamp(0, rows as i32 - 1));
        let cell = (x / self.cell_w as f32).floor().clamp(0.0, cols as f32 - 1.0) as u16;
        let bound = (x / self.cell_w as f32).round().clamp(0.0, cols as f32) as u16;
        (Point::new(line, cell), Point::new(line, bound), outside)
    }

    /// Visible row of an absolute line, if shown.
    pub fn view_row(&self, line: i64) -> Option<u16> {
        let r = line - self.first_line();
        (0..i64::from(self.size().0)).contains(&r).then_some(r as u16)
    }

    /// Reads an absolute line, from the history if needed.
    fn line(&mut self, abs: i64) -> Option<Line> {
        let (rows, cols) = self.size();
        let alt = self.alternate_screen();
        let idx = abs - self.parser.callbacks().0[usize::from(alt)];
        let screen = self.parser.screen_mut();
        let len = if alt { 0 } else { screen.scrollback_len() as i64 };
        if idx < 0 || idx >= len + i64::from(rows) {
            return None;
        }
        let saved = screen.scrollback();
        let (offset, r) = if idx >= len {
            (0, (idx - len) as u16)
        } else {
            ((len - idx) as usize, 0)
        };
        if offset != saved {
            screen.set_scrollback(offset);
        }
        let cells = (0..cols)
            .map(|c| match screen.cell(r, c) {
                Some(cell) if cell.is_wide_continuation() => String::new(),
                Some(cell) if cell.has_contents() => cell.contents().to_string(),
                _ => " ".to_string(),
            })
            .collect();
        let wrapped = screen.row_wrapped(r);
        if offset != saved {
            screen.set_scrollback(saved);
        }
        Some(Line { cells, wrapped })
    }

    /// The selection, dropped when the other screen (main / alternate) is shown.
    fn valid_selection(&mut self) -> Option<&Selection> {
        let alt = self.alternate_screen();
        if self.sel.as_ref().is_some_and(|(_, a)| *a != alt) {
            self.sel = None;
        }
        self.sel.as_ref().map(|(s, _)| s)
    }

    /// A selection exists, even empty (start of Shift+click).
    pub fn has_anchor(&mut self) -> bool {
        self.valid_selection().is_some()
    }

    pub fn has_selection(&mut self) -> bool {
        self.valid_selection().is_some_and(|s| !s.is_empty())
    }

    pub fn select_start(&mut self, unit: Unit, cell: Point, bound: Point) {
        let alt = self.alternate_screen();
        let sel = Selection::new(&mut |l| self.line(l), unit, cell, bound);
        self.sel = Some((sel, alt));
    }

    pub fn select_extend(&mut self, cell: Point, bound: Point) {
        let Some(mut sel) = self.valid_selection().cloned() else { return };
        sel.extend(&mut |l| self.line(l), cell, bound);
        self.sel = Some((sel, self.alternate_screen()));
    }

    /// Clears the selection; true if there was one.
    pub fn clear_selection(&mut self) -> bool {
        self.sel.take().is_some_and(|(s, _)| !s.is_empty())
    }

    /// Selected text (CRLF line breaks on Windows), if any.
    pub fn selection_text(&mut self) -> Option<String> {
        let sel = self.valid_selection().filter(|s| !s.is_empty()).cloned()?;
        let text = sel.text(&mut |l| self.line(l));
        Some(if cfg!(windows) { text.replace('\n', "\r\n") } else { text })
    }

    pub fn render(&mut self) -> SharedPixelBuffer<Rgba8Pixel> {
        let (rows, cols) = self.size();
        let (w, h) = (cols as usize * self.cell_w, rows as usize * self.cell_h);
        let style = self.style;
        let (fg0, bg0, palette) = (style.fg, style.bg, &style.theme.palette);
        let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w as u32, h as u32);
        let px = buf.make_mut_bytes();
        for chunk in px.as_chunks_mut::<4>().0 {
            chunk.copy_from_slice(&[bg0[0], bg0[1], bg0[2], 255]);
        }
        let first = self.first_line();
        let alt = self.alternate_screen();
        let sel = self
            .sel
            .as_ref()
            .filter(|(s, a)| *a == alt && !s.is_empty())
            .map(|(s, _)| s.clone());
        let screen = self.parser.screen();
        let glyphs = &mut self.glyphs;
        let fonts = &self.fonts;
        let (size, ascent, cell_w, cell_h) = (self.px, self.ascent, self.cell_w, self.cell_h);
        let (cur_r, cur_c) = screen.cursor_position();
        let show_cursor = !screen.hide_cursor() && screen.scrollback() == 0;
        // Syntax colouring of each visible row (cell contents, None for the
        // second half of a wide character).
        let marks: Vec<Vec<Option<relay_desktop_core::highlight::Mark>>> = match &self.highlight {
            Some(hl) => (0..rows)
                .map(|r| {
                    let cells: Vec<Option<String>> = (0..cols)
                        .map(|c| match screen.cell(r, c) {
                            Some(cell) if cell.is_wide_continuation() => None,
                            Some(cell) if cell.has_contents() => Some(cell.contents().to_string()),
                            _ => Some(" ".to_string()),
                        })
                        .collect();
                    hl.row_marks(&cells)
                })
                .collect(),
            None => Vec::new(),
        };
        for (pass, r) in (0..2).flat_map(|p| (0..rows).map(move |r| (p, r))) {
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut fg = color(cell.fgcolor(), fg0, palette);
                let mut bg = color(cell.bgcolor(), bg0, palette);
                let mut bold = cell.bold();
                if bold && style.bold_bright {
                    if let vt100::Color::Idx(i) = cell.fgcolor() {
                        if i < 8 {
                            fg = palette[i as usize + 8];
                        }
                    }
                }
                if let Some(Some(m)) = marks.get(r as usize).and_then(|row| row.get(c as usize)) {
                    fg = m.fg.unwrap_or(fg);
                    bg = m.bg.unwrap_or(bg);
                    bold |= m.bold;
                }
                let span: usize = if cell.is_wide() { 2 } else { 1 };
                let selected = sel
                    .as_ref()
                    .is_some_and(|s| (c..c + span as u16).any(|k| s.contains(first + i64::from(r), k)));
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if show_cursor && r == cur_r && c == cur_c {
                    (fg, bg) = (bg0, style.theme.cursor);
                } else if selected {
                    bg = style.theme.selection;
                    if fg == bg {
                        fg = fg0;
                    }
                }
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
                    let cell = Cell {
                        origin: (x0, y0),
                        room: cell_w * span,
                        height: cell_h,
                        ch,
                        bold,
                        fg,
                    };
                    draw_glyph(glyphs, fonts, size, ascent, px, (w, h), cell);
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

    /// Syntax colouring of this terminal (None = off).
    pub fn set_highlight(&mut self, highlight: Option<Arc<Highlighter>>) {
        self.highlight = highlight.filter(|h| !h.is_empty());
    }

    /// Typed or pasted bytes (UTF-8) in the remote character set.
    pub fn encode_input(&self, bytes: Vec<u8>) -> Vec<u8> {
        if self.charset == Charset::Utf8 {
            return bytes;
        }
        self.charset.encode(&String::from_utf8_lossy(&bytes))
    }
}

/// Character to draw in a cell (or two for a wide character).
struct Cell {
    origin: (usize, usize),
    /// Width available in pixels.
    room: usize,
    height: usize,
    ch: char,
    bold: bool,
    fg: [u8; 3],
}

type GlyphCache = HashMap<(char, bool), (fontdue::Metrics, Vec<u8>)>;

fn draw_glyph(glyphs: &mut GlyphCache, fonts: &Fonts, size: f32, ascent: f32, px: &mut [u8], (w, h): (usize, usize), cell: Cell) {
    let Cell {
        origin: (x0, y0),
        room,
        height,
        ch,
        bold,
        fg,
    } = cell;
    // Powerline separators (no Windows font has them): drawn in the cell.
    if ('\u{e0b0}'..='\u{e0b3}').contains(&ch) {
        let solid = ch == '\u{e0b0}' || ch == '\u{e0b2}';
        let right = ch == '\u{e0b0}' || ch == '\u{e0b1}';
        let half = height as f32 / 2.0;
        for dy in 0..height {
            let y = y0 + dy;
            // Width of the triangle on this row (point in the middle).
            let reach = ((1.0 - ((dy as f32 + 0.5) - half).abs() / half) * room as f32).round() as usize;
            for dx in 0..room {
                let inside = if solid { dx < reach } else { dx + 1 >= reach && dx <= reach };
                let x = if right { x0 + dx } else { x0 + room - 1 - dx };
                if inside && x < w && y < h {
                    let i = (y * w + x) * 4;
                    px[i..i + 3].copy_from_slice(&fg);
                }
            }
        }
        return;
    }
    let (m, bitmap) = glyphs.entry((ch, bold)).or_insert_with(|| {
        let font = fonts.pick(ch, bold);
        let (m, b) = font.rasterize(ch, size);
        // A fallback glyph wider than its cells is scaled down to fit.
        if m.advance_width > room as f32 * 1.05 && m.advance_width > 0.0 {
            font.rasterize(ch, size * room as f32 / m.advance_width)
        } else {
            (m, b)
        }
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
    relay_desktop_core::charset::char_input(c, ctrl, alt)
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
