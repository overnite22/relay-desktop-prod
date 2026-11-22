//! Terminal text selection and mouse reporting, without rendering: positions
//! use absolute line numbers (stable while the view or the history scrolls)
//! so a selection stays on its text.

/// A cell (or the boundary before it) of the terminal content: `line` is
/// absolute, `col` is 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub line: i64,
    pub col: u16,
}

impl Point {
    pub fn new(line: i64, col: u16) -> Point {
        Point { line, col }
    }
}

/// One line of content: the text of each cell (`" "` for an empty cell, `""`
/// for the second half of a wide character) and whether it continues on the
/// next line (soft wrap).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub cells: Vec<String>,
    pub wrapped: bool,
}

impl Line {
    /// Line of `cols` cells holding `text` (one character per cell).
    pub fn text(text: &str, cols: usize, wrapped: bool) -> Line {
        let mut cells: Vec<String> = text.chars().take(cols).map(String::from).collect();
        cells.resize(cols, " ".into());
        Line { cells, wrapped }
    }
}

/// Reads an absolute line (`None` outside the content).
pub type Lines<'a> = dyn FnMut(i64) -> Option<Line> + 'a;

/// Granularity of a selection: drag, double-click, triple-click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Char,
    Word,
    Line,
}

impl Unit {
    /// 1 click = characters, 2 = words, 3 = lines.
    pub fn from_clicks(n: u32) -> Unit {
        match n {
            0 | 1 => Unit::Char,
            2 => Unit::Word,
            _ => Unit::Line,
        }
    }
}

/// Characters that end a word on double-click (paths, URLs and addresses stay
/// whole: `/ . : @ - _ ~ = ? & % +` are word characters).
const DELIMITERS: &str = "\"'`()[]{}<>|,;!$*^\\";

fn class(cell: &str) -> u8 {
    match cell.chars().next() {
        None => 1,
        Some(c) if c.is_whitespace() => 0,
        Some(c) if DELIMITERS.contains(c) || ('\u{2500}'..='\u{259f}').contains(&c) => 2,
        Some(_) => 1,
    }
}

/// Word under the cell `p` as `[start, end)`, following soft-wrapped lines.
pub fn word_at(get: &mut Lines, p: Point) -> (Point, Point) {
    let Some(line) = get(p.line).filter(|l| !l.cells.is_empty()) else {
        return (p, p);
    };
    let col = usize::from(p.col).min(line.cells.len() - 1);
    let cls = class(&line.cells[col]);
    let start_here = Point::new(p.line, col as u16);
    if cls == 2 {
        return (start_here, Point::new(p.line, col as u16 + 1));
    }
    // Left.
    let (mut l, mut c, mut cur) = (p.line, col, line.clone());
    loop {
        if c > 0 && class(&cur.cells[c - 1]) == cls {
            c -= 1;
            continue;
        }
        if c == 0 {
            if let Some(prev) = get(l - 1).filter(|x| x.wrapped && x.cells.last().is_some_and(|s| class(s) == cls)) {
                l -= 1;
                c = prev.cells.len() - 1;
                cur = prev;
                continue;
            }
        }
        break;
    }
    let start = Point::new(l, c as u16);
    // Right.
    let (mut l, mut c, mut cur) = (p.line, col, line);
    loop {
        if c + 1 < cur.cells.len() && class(&cur.cells[c + 1]) == cls {
            c += 1;
            continue;
        }
        if c + 1 == cur.cells.len() && cur.wrapped {
            if let Some(next) = get(l + 1).filter(|x| x.cells.first().is_some_and(|s| class(s) == cls)) {
                l += 1;
                c = 0;
                cur = next;
                continue;
            }
        }
        break;
    }
    (start, Point::new(l, c as u16 + 1))
}

/// Whole logical line (soft-wrapped rows included) around `line`.
pub fn line_at(get: &mut Lines, line: i64) -> (Point, Point) {
    let Some(mut last) = get(line) else {
        return (Point::new(line, 0), Point::new(line, 0));
    };
    let mut first = line;
    while get(first - 1).is_some_and(|l| l.wrapped) {
        first -= 1;
    }
    let mut end = line;
    while last.wrapped {
        match get(end + 1) {
            Some(next) => {
                end += 1;
                last = next;
            }
            None => break,
        }
    }
    (Point::new(first, 0), Point::new(end, last.cells.len() as u16))
}

/// Text between `start` and `end` (exclusive): soft-wrapped lines are joined
/// without a line break, trailing spaces of every line are dropped.
pub fn text(get: &mut Lines, start: Point, end: Point) -> String {
    let mut out = String::new();
    for l in start.line..=end.line {
        let Some(line) = get(l) else {
            if l < end.line {
                out.push('\n');
            }
            continue;
        };
        let len = line.cells.len();
        let c0 = if l == start.line { usize::from(start.col).min(len) } else { 0 };
        let c1 = if l == end.line { usize::from(end.col).min(len) } else { len };
        let seg = line.cells[c0..c1.max(c0)].concat();
        if line.wrapped && c1 == len && l < end.line {
            out.push_str(&seg);
        } else {
            out.push_str(seg.trim_end());
            if l < end.line {
                out.push('\n');
            }
        }
    }
    out
}

/// A selection: the unit range where it started (`anchor`) and the unit range
/// under the pointer (`head`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub unit: Unit,
    anchor: (Point, Point),
    head: (Point, Point),
}

fn unit_range(get: &mut Lines, unit: Unit, cell: Point, boundary: Point) -> (Point, Point) {
    match unit {
        Unit::Char => (boundary, boundary),
        Unit::Word => word_at(get, cell),
        Unit::Line => line_at(get, cell.line),
    }
}

impl Selection {
    /// Starts at the cell under the pointer (`cell`) or the nearest cell
    /// boundary (`boundary`, character selection).
    pub fn new(get: &mut Lines, unit: Unit, cell: Point, boundary: Point) -> Selection {
        let r = unit_range(get, unit, cell, boundary);
        Selection { unit, anchor: r, head: r }
    }

    /// Moves the free end (drag, Shift+click).
    pub fn extend(&mut self, get: &mut Lines, cell: Point, boundary: Point) {
        self.head = unit_range(get, self.unit, cell, boundary);
    }

    /// `[start, end)` in reading order.
    pub fn range(&self) -> (Point, Point) {
        (self.anchor.0.min(self.head.0), self.anchor.1.max(self.head.1))
    }

    pub fn is_empty(&self) -> bool {
        let (s, e) = self.range();
        s >= e
    }

    pub fn contains(&self, line: i64, col: u16) -> bool {
        let (s, e) = self.range();
        let p = Point::new(line, col);
        s <= p && p < e
    }

    pub fn text(&self, get: &mut Lines) -> String {
        let (s, e) = self.range();
        text(get, s, e)
    }
}

/// Clipboard text prepared for the remote shell: CRLF and LF become CR (the
/// Enter key); in bracketed paste mode the text is framed and may not close
/// the frame itself.
pub fn paste_bytes(clip: &str, bracketed: bool) -> Vec<u8> {
    let text = clip.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let inner = text.replace("\x1b[201~", "").replace("\x1b[200~", "");
        format!("\x1b[200~{inner}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

/// Mouse tracking requested by the remote application (DECSET 9/1000/1002/1003).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseMode {
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

/// Coordinates encoding of mouse reports (default X10, UTF-8 1005, SGR 1006).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEncoding {
    Default,
    Utf8,
    Sgr,
}

/// Mouse event for the remote application; buttons 0 left, 1 middle, 2 right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEvent {
    Press(u8),
    Release(u8),
    Drag(u8),
    Wheel { up: bool },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// xterm mouse report for the cell (`col`, `row`, 0-based), if the mode
/// asks for this event.
pub fn mouse_report(mode: MouseMode, enc: MouseEncoding, ev: MouseEvent, col: u16, row: u16, m: Mods) -> Option<Vec<u8>> {
    let wanted = match mode {
        MouseMode::None => false,
        MouseMode::Press => matches!(ev, MouseEvent::Press(_)),
        MouseMode::PressRelease => !matches!(ev, MouseEvent::Drag(_)),
        MouseMode::ButtonMotion | MouseMode::AnyMotion => true,
    };
    if !wanted {
        return None;
    }
    let (mut cb, release) = match ev {
        MouseEvent::Press(b) => (u32::from(b.min(2)), false),
        MouseEvent::Release(b) => (u32::from(b.min(2)), true),
        MouseEvent::Drag(b) => (u32::from(b.min(2)) + 32, false),
        MouseEvent::Wheel { up } => (if up { 64 } else { 65 }, false),
    };
    if mode != MouseMode::Press {
        cb += u32::from(m.shift) * 4 + u32::from(m.alt) * 8 + u32::from(m.ctrl) * 16;
    }
    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    if enc == MouseEncoding::Sgr {
        let end = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{cb};{x};{y}{end}").into_bytes());
    }
    if release {
        cb = (cb & !3) | 3;
    }
    let mut out = b"\x1b[M".to_vec();
    for v in [cb, x, y] {
        let v = v + 32;
        if enc == MouseEncoding::Utf8 {
            let c = char::from_u32(v).filter(|_| v < 2048)?;
            out.extend(c.to_string().as_bytes());
        } else {
            out.push(u8::try_from(v).ok()?);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines of an 10-column screen starting at absolute line 100.
    fn screen<'a>(rows: &'a [(&'a str, bool)]) -> impl FnMut(i64) -> Option<Line> + 'a {
        move |n| {
            let i = usize::try_from(n - 100).ok()?;
            rows.get(i).map(|(t, w)| Line::text(t, 10, *w))
        }
    }

    #[test]
    fn char_selection_and_text() {
        let rows = [("hello all ", false), ("second    ", false), ("third", false)];
        let mut g = screen(&rows);
        let mut s = Selection::new(&mut g, Unit::Char, Point::new(100, 6), Point::new(100, 6));
        assert!(s.is_empty());
        s.extend(&mut g, Point::new(101, 2), Point::new(101, 3));
        assert_eq!(s.text(&mut g), "all\nsec");
        assert!(s.contains(100, 6) && s.contains(100, 9) && s.contains(101, 0) && !s.contains(101, 3));
        // Backwards drag selects the same cells.
        let mut b = Selection::new(&mut g, Unit::Char, Point::new(101, 2), Point::new(101, 3));
        b.extend(&mut g, Point::new(100, 6), Point::new(100, 6));
        assert_eq!(b.range(), s.range());
        // Past the end of a line: line break, trailing spaces dropped.
        assert_eq!(text(&mut g, Point::new(101, 0), Point::new(102, 0)), "second\n");
    }

    #[test]
    fn wrapped_lines_are_joined() {
        let rows = [("0123456789", true), ("abc  ", false), ("x", false)];
        let mut g = screen(&rows);
        assert_eq!(text(&mut g, Point::new(100, 5), Point::new(102, 1)), "56789abc\nx");
        let (s, e) = line_at(&mut g, 101);
        assert_eq!((s, e), (Point::new(100, 0), Point::new(101, 10)));
        assert_eq!(text(&mut g, s, e), "0123456789abc");
    }

    #[test]
    fn words() {
        let rows = [("ls /etc/ss", true), ("h (x) a,b ", false)];
        let mut g = screen(&rows);
        // A path across the soft wrap is one word.
        let (s, e) = word_at(&mut g, Point::new(101, 0));
        assert_eq!(text(&mut g, s, e), "/etc/ssh");
        let (s, e) = word_at(&mut g, Point::new(100, 1));
        assert_eq!(text(&mut g, s, e), "ls");
        // Delimiters stop words and are selected alone.
        let (s, e) = word_at(&mut g, Point::new(101, 3));
        assert_eq!(text(&mut g, s, e), "x");
        let (s, e) = word_at(&mut g, Point::new(101, 2));
        assert_eq!(text(&mut g, s, e), "(");
        let (s, e) = word_at(&mut g, Point::new(101, 6));
        assert_eq!(text(&mut g, s, e), "a");
        // Word selection extends by whole words.
        let mut sel = Selection::new(&mut g, Unit::Word, Point::new(100, 0), Point::new(100, 0));
        sel.extend(&mut g, Point::new(101, 8), Point::new(101, 8));
        assert_eq!(sel.text(&mut g), "ls /etc/ssh (x) a,b");
        let ip = [("ping 203.0.113.7 -c1", false)];
        let mut g = move |n: i64| (n == 0).then(|| Line::text(ip[0].0, 30, false));
        let (s, e) = word_at(&mut g, Point::new(0, 8));
        assert_eq!(text(&mut g, s, e), "203.0.113.7");
    }

    #[test]
    fn wide_characters() {
        let mut cells: Vec<String> = ["日", "", "本", "", "x"].iter().map(|s| s.to_string()).collect();
        cells.resize(8, " ".into());
        let line = Line { cells, wrapped: false };
        let mut g = |n: i64| (n == 0).then(|| line.clone());
        assert_eq!(text(&mut g, Point::new(0, 0), Point::new(0, 8)), "日本x");
        assert_eq!(Unit::from_clicks(3), Unit::Line);
    }

    #[test]
    fn paste() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~");
    }

    #[test]
    fn mouse_reports() {
        let m = Mods::default();
        let r = |mode, enc, ev, c, r| mouse_report(mode, enc, ev, c, r, m);
        assert_eq!(r(MouseMode::None, MouseEncoding::Sgr, MouseEvent::Press(0), 0, 0), None);
        assert_eq!(
            r(MouseMode::PressRelease, MouseEncoding::Default, MouseEvent::Press(0), 4, 2),
            Some(b"\x1b[M %#".to_vec())
        );
        assert_eq!(
            r(MouseMode::PressRelease, MouseEncoding::Default, MouseEvent::Release(0), 4, 2),
            Some(b"\x1b[M#%#".to_vec())
        );
        assert_eq!(
            r(MouseMode::PressRelease, MouseEncoding::Sgr, MouseEvent::Release(2), 9, 0),
            Some(b"\x1b[<2;10;1m".to_vec())
        );
        assert_eq!(r(MouseMode::PressRelease, MouseEncoding::Sgr, MouseEvent::Drag(0), 1, 1), None);
        assert_eq!(
            r(MouseMode::ButtonMotion, MouseEncoding::Sgr, MouseEvent::Drag(0), 1, 1),
            Some(b"\x1b[<32;2;2M".to_vec())
        );
        assert_eq!(
            r(MouseMode::ButtonMotion, MouseEncoding::Sgr, MouseEvent::Wheel { up: false }, 0, 0),
            Some(b"\x1b[<65;1;1M".to_vec())
        );
        assert_eq!(r(MouseMode::Press, MouseEncoding::Default, MouseEvent::Release(0), 0, 0), None);
        // Too far for the X10 encoding, fine in UTF-8.
        assert_eq!(
            r(MouseMode::PressRelease, MouseEncoding::Default, MouseEvent::Press(0), 300, 0),
            None
        );
        assert_eq!(
            r(MouseMode::PressRelease, MouseEncoding::Utf8, MouseEvent::Press(0), 300, 0),
            Some([b"\x1b[M ".as_slice(), "ō".as_bytes(), b"!"].concat())
        );
        let ctrl = Mods { ctrl: true, ..m };
        assert_eq!(
            mouse_report(MouseMode::PressRelease, MouseEncoding::Sgr, MouseEvent::Press(1), 0, 0, ctrl),
            Some(b"\x1b[<17;1;1M".to_vec())
        );
    }
}
