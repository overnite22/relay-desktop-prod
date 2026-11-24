//! Replay of the session recordings of the Relay server: raw Guacamole
//! protocol kept by `internal/guac/recording.go` (drawing instructions, image
//! streams and `sync` frames). Rendering covers what guacd emits for RDP and
//! SSH: layers and buffers (`size`, `move`, `shade`, `dispose`), images
//! (`img` + `blob` / `end`, legacy `png` / `jpeg`), rectangles filled with a
//! color (`rect` + `cfill`), `copy` / `transfer` and `cursor`. Curves, lines,
//! strokes, clipping and transforms are ignored.

use base64::Engine;
use std::collections::HashMap;

/// One instruction: opcode and arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instr {
    pub op: String,
    pub args: Vec<String>,
}

/// Parses `LEN.VALUE,LEN.VALUE;` instructions (lengths in Unicode
/// characters). A truncated last instruction is ignored.
pub fn parse(data: &str) -> Result<Vec<Instr>, String> {
    let mut out = Vec::new();
    let mut elems: Vec<String> = Vec::new();
    let mut it = data.char_indices().peekable();
    loop {
        // Skip whitespace between instructions.
        while let Some(&(_, c)) = it.peek() {
            if elems.is_empty() && c.is_whitespace() {
                it.next();
            } else {
                break;
            }
        }
        let Some(&(start, _)) = it.peek() else { break };
        let mut len = 0usize;
        let mut digits = 0;
        loop {
            match it.next() {
                Some((_, c @ '0'..='9')) => {
                    len = len
                        .checked_mul(10)
                        .and_then(|l| l.checked_add(c as usize - '0' as usize))
                        .ok_or("longueur invalide")?;
                    digits += 1;
                }
                Some((_, '.')) if digits > 0 => break,
                None => return Ok(out),
                Some((i, c)) => return Err(format!("enregistrement illisible à l'octet {i} (« {c} » après {start})")),
            }
        }
        let value_start = it.peek().map_or(data.len(), |&(i, _)| i);
        let mut end = value_start;
        for _ in 0..len {
            match it.next() {
                Some((i, c)) => end = i + c.len_utf8(),
                None => return Ok(out),
            }
        }
        elems.push(data[value_start..end].to_string());
        match it.next() {
            Some((_, ',')) => {}
            Some((_, ';')) => {
                let mut args = std::mem::take(&mut elems);
                let op = args.remove(0);
                out.push(Instr { op, args });
            }
            None => return Ok(out),
            Some((i, c)) => return Err(format!("séparateur inattendu « {c} » à l'octet {i}")),
        }
    }
    Ok(out)
}

/// RGBA surface (straight alpha).
#[derive(Debug, Clone, Default)]
struct Layer {
    w: u32,
    h: u32,
    px: Vec<u8>,
    x: i32,
    y: i32,
    z: i32,
    parent: i32,
    opacity: u8,
    /// Rectangles of the current path (filled by `cfill`).
    path: Vec<(i32, i32, i32, i32)>,
    /// The next path instruction starts a new path.
    path_done: bool,
}

impl Layer {
    fn new(w: u32, h: u32) -> Self {
        Layer {
            w,
            h,
            px: vec![0; (w * h * 4) as usize],
            opacity: 255,
            ..Default::default()
        }
    }

    fn resize(&mut self, w: u32, h: u32) {
        if (w, h) == (self.w, self.h) {
            return;
        }
        let mut px = vec![0; (w * h * 4) as usize];
        for y in 0..self.h.min(h) {
            let n = (self.w.min(w) * 4) as usize;
            let s = (y * self.w * 4) as usize;
            let d = (y * w * 4) as usize;
            px[d..d + n].copy_from_slice(&self.px[s..s + n]);
        }
        (self.w, self.h, self.px) = (w, h, px);
    }

    fn get(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h {
            return [0; 4];
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }

    /// Draws `src` at (x, y): replace when `copy`, else "source over".
    fn put(&mut self, x: i32, y: i32, src: [u8; 4], copy: bool) {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        let d = &mut self.px[i..i + 4];
        if copy || src[3] == 255 {
            d.copy_from_slice(&src);
        } else if src[3] > 0 {
            let a = src[3] as u32;
            let da = d[3] as u32 * (255 - a) / 255;
            let oa = a + da;
            for c in 0..3 {
                d[c] = ((src[c] as u32 * a + d[c] as u32 * da) / oa.max(1)) as u8;
            }
            d[3] = oa as u8;
        }
    }

    fn path_rect(&mut self, r: (i32, i32, i32, i32)) {
        if self.path_done {
            self.path.clear();
            self.path_done = false;
        }
        self.path.push(r);
    }
}

/// Composite operation of a channel mask: 0xC = source (replace), others
/// drawn as "over".
fn replaces(mask: i64) -> bool {
    mask == 0x0C
}

struct Stream {
    layer: i32,
    mask: i64,
    mime: String,
    x: i32,
    y: i32,
    data: Vec<u8>,
}

/// Display rebuilt by the instructions of a recording.
#[derive(Default)]
pub struct Display {
    layers: HashMap<i32, Layer>,
    streams: HashMap<String, Stream>,
}

fn int(args: &[String], i: usize) -> i64 {
    args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn decode(mime: &str, bytes: &[u8]) -> Option<image::RgbaImage> {
    let format = match mime {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        _ => image::guess_format(bytes).ok()?,
    };
    image::load_from_memory_with_format(bytes, format).ok().map(|i| i.to_rgba8())
}

impl Display {
    fn layer(&mut self, id: i32) -> &mut Layer {
        self.layers.entry(id).or_insert_with(|| Layer::new(0, 0))
    }

    fn draw_image(&mut self, layer: i32, mask: i64, x: i32, y: i32, img: &image::RgbaImage) {
        let l = self.layer(layer);
        let copy = replaces(mask);
        for (ix, iy, p) in img.enumerate_pixels() {
            l.put(x + ix as i32, y + iy as i32, p.0, copy);
        }
    }

    fn copy(&mut self, src: i32, (sx, sy, w, h): (i32, i32, i32, i32), mask: i64, dst: i32, dx: i32, dy: i32) {
        let Some(s) = self.layers.get(&src) else { return };
        let mut pixels = Vec::with_capacity((w.max(0) * h.max(0)) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.push(s.get(sx + x, sy + y));
            }
        }
        let copy = replaces(mask);
        let d = self.layer(dst);
        let mut i = 0;
        for y in 0..h {
            for x in 0..w {
                d.put(dx + x, dy + y, pixels[i], copy);
                i += 1;
            }
        }
    }

    /// Applies one instruction.
    pub fn apply(&mut self, ins: &Instr) {
        let a = &ins.args;
        match ins.op.as_str() {
            "size" => {
                let (w, h) = (int(a, 1).clamp(0, 8192) as u32, int(a, 2).clamp(0, 8192) as u32);
                self.layer(int(a, 0) as i32).resize(w, h);
            }
            "img" => {
                self.streams.insert(
                    a.first().cloned().unwrap_or_default(),
                    Stream {
                        mask: int(a, 1),
                        layer: int(a, 2) as i32,
                        mime: a.get(3).cloned().unwrap_or_default(),
                        x: int(a, 4) as i32,
                        y: int(a, 5) as i32,
                        data: Vec::new(),
                    },
                );
            }
            "blob" => {
                if let (Some(s), Some(d)) = (a.first().and_then(|id| self.streams.get_mut(id)), a.get(1)) {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(d.trim()) {
                        s.data.extend_from_slice(&bytes);
                    }
                }
            }
            "end" => {
                if let Some(s) = a.first().and_then(|id| self.streams.remove(id)) {
                    if let Some(img) = decode(&s.mime, &s.data) {
                        self.draw_image(s.layer, s.mask, s.x, s.y, &img);
                    }
                }
            }
            "png" | "jpeg" => {
                let mime = if ins.op == "png" { "image/png" } else { "image/jpeg" };
                if let Some(img) = a
                    .get(4)
                    .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d.trim()).ok())
                    .and_then(|b| decode(mime, &b))
                {
                    self.draw_image(int(a, 1) as i32, int(a, 0), int(a, 2) as i32, int(a, 3) as i32, &img);
                }
            }
            "rect" => {
                let r = (int(a, 1) as i32, int(a, 2) as i32, int(a, 3) as i32, int(a, 4) as i32);
                self.layer(int(a, 0) as i32).path_rect(r);
            }
            "cfill" => {
                let color = [int(a, 2) as u8, int(a, 3) as u8, int(a, 4) as u8, int(a, 5) as u8];
                let copy = replaces(int(a, 0));
                let l = self.layer(int(a, 1) as i32);
                for (x, y, w, h) in std::mem::take(&mut l.path) {
                    for py in y.max(0)..(y + h).min(l.h as i32) {
                        for px in x.max(0)..(x + w).min(l.w as i32) {
                            l.put(px, py, color, copy);
                        }
                    }
                }
                l.path_done = true;
            }
            "copy" => {
                let r = (int(a, 1) as i32, int(a, 2) as i32, int(a, 3) as i32, int(a, 4) as i32);
                self.copy(int(a, 0) as i32, r, int(a, 5), int(a, 6) as i32, int(a, 7) as i32, int(a, 8) as i32);
            }
            // Raster operations other than "source" are drawn as a copy.
            "transfer" => {
                let r = (int(a, 1) as i32, int(a, 2) as i32, int(a, 3) as i32, int(a, 4) as i32);
                self.copy(int(a, 0) as i32, r, 0x0C, int(a, 6) as i32, int(a, 7) as i32, int(a, 8) as i32);
            }
            "move" => {
                let l = self.layer(int(a, 0) as i32);
                (l.parent, l.x, l.y, l.z) = (int(a, 1) as i32, int(a, 2) as i32, int(a, 3) as i32, int(a, 4) as i32);
            }
            "shade" => self.layer(int(a, 0) as i32).opacity = int(a, 1).clamp(0, 255) as u8,
            "dispose" => {
                let id = int(a, 0) as i32;
                if id != 0 {
                    self.layers.remove(&id);
                }
            }
            _ => {}
        }
    }

    /// Size of the default layer.
    pub fn size(&self) -> (u32, u32) {
        self.layers.get(&0).map_or((0, 0), |l| (l.w, l.h))
    }

    /// Visible image: default layer over black, then the visible layers
    /// (positive ids) at their position and opacity.
    pub fn render(&self) -> (u32, u32, Vec<u8>) {
        let Some(base) = self.layers.get(&0) else {
            return (0, 0, Vec::new());
        };
        let mut out = Layer::new(base.w, base.h);
        out.px.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
        let mut visible: Vec<(&i32, &Layer)> = self.layers.iter().filter(|(id, _)| **id >= 0).collect();
        visible.sort_by_key(|(id, l)| (**id != 0, l.z, **id));
        for (id, l) in visible {
            let (mut ox, mut oy, mut p, mut depth) = (0, 0, *id, 0);
            while p != 0 && depth < 16 {
                let Some(pl) = self.layers.get(&p) else { break };
                ox += pl.x;
                oy += pl.y;
                p = pl.parent;
                depth += 1;
            }
            for y in 0..l.h as i32 {
                for x in 0..l.w as i32 {
                    let mut c = l.get(x, y);
                    c[3] = (c[3] as u32 * l.opacity as u32 / 255) as u8;
                    out.put(ox + x, oy + y, c, false);
                }
            }
        }
        (out.w, out.h, out.px)
    }
}

/// Parsed recording split into `sync` frames.
pub struct Recording {
    instrs: Vec<Instr>,
    /// (time since the first frame in ms, index after the frame's `sync`).
    frames: Vec<(u64, usize)>,
}

impl Recording {
    pub fn parse(data: &str) -> Result<Self, String> {
        let instrs = parse(data)?;
        let mut frames = Vec::new();
        let mut first = None;
        for (i, ins) in instrs.iter().enumerate() {
            if ins.op == "sync" {
                let ts = int(&ins.args, 0).max(0) as u64;
                let base = *first.get_or_insert(ts);
                let last = frames.last().map_or(0, |f: &(u64, usize)| f.0);
                frames.push((ts.saturating_sub(base).max(last), i + 1));
            }
        }
        if frames.is_empty() {
            return Err("aucune image dans cet enregistrement (instruction « sync » absente)".into());
        }
        Ok(Recording { instrs, frames })
    }

    pub fn duration_ms(&self) -> u64 {
        self.frames.last().map_or(0, |f| f.0)
    }

    pub fn frames(&self) -> usize {
        self.frames.len()
    }
}

/// Recording being replayed: seeking forward applies the next frames,
/// seeking backward replays from the start.
pub struct Player {
    rec: Recording,
    display: Display,
    next: usize,
}

impl Player {
    pub fn new(rec: Recording) -> Self {
        Player {
            rec,
            display: Display::default(),
            next: 0,
        }
    }

    pub fn duration_ms(&self) -> u64 {
        self.rec.duration_ms()
    }

    /// Shows the last frame at or before `t` ms; true when the image changed.
    pub fn seek(&mut self, t: u64) -> bool {
        let k = self.rec.frames.partition_point(|f| f.0 <= t);
        let end = if k == 0 { self.rec.frames[0].1 } else { self.rec.frames[k - 1].1 };
        if end < self.next {
            self.display = Display::default();
            self.next = 0;
        }
        if end == self.next {
            return false;
        }
        for ins in &self.rec.instrs[self.next..end] {
            self.display.apply(ins);
        }
        self.next = end;
        true
    }

    pub fn render(&self) -> (u32, u32, Vec<u8>) {
        self.display.render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(parts: &[&str]) -> String {
        let mut s = parts
            .iter()
            .map(|p| format!("{}.{}", p.chars().count(), p))
            .collect::<Vec<_>>()
            .join(",");
        s.push(';');
        s
    }

    fn png_b64(w: u32, h: u32, rgba: [u8; 4]) -> String {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        base64::engine::general_purpose::STANDARD.encode(out.into_inner())
    }

    fn px(p: &Player, x: u32, y: u32) -> [u8; 4] {
        let (w, _, buf) = p.render();
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn parse_lengths_in_characters() {
        let all = parse("4.size,1.0,3.640;\n5.clipb,2.é€;3.end").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].op, "size");
        assert_eq!(all[0].args, ["0", "640"]);
        assert_eq!(all[1].args, ["é€"]);
        assert!(parse("4.size,x;").is_err());
    }

    #[test]
    fn replay_frames() {
        let png = png_b64(2, 1, [0, 0, 255, 255]);
        let (half, rest) = png.split_at(8);
        let data = [
            ins(&["size", "0", "6", "4"]),
            ins(&["size", "-1", "2", "2"]),
            ins(&["rect", "0", "0", "0", "6", "4"]),
            ins(&["cfill", "14", "0", "255", "0", "0", "255"]),
            ins(&["sync", "1000000"]),
            // Image stream in two blobs, at (1, 1) of the default layer.
            ins(&["img", "1", "12", "0", "image/png", "1", "1"]),
            ins(&["blob", "1", half]),
            ins(&["blob", "1", rest]),
            ins(&["end", "1"]),
            // Off-screen buffer filled in green, copied to (4, 3).
            ins(&["rect", "-1", "0", "0", "2", "2"]),
            ins(&["cfill", "14", "-1", "0", "255", "0", "255"]),
            ins(&["copy", "-1", "0", "0", "1", "1", "12", "0", "4", "3"]),
            ins(&["sync", "1000500"]),
            // Visible layer 1 at (5, 0), half transparent white.
            ins(&["size", "1", "1", "1"]),
            ins(&["move", "1", "0", "5", "0", "1"]),
            ins(&["rect", "1", "0", "0", "1", "1"]),
            ins(&["cfill", "12", "1", "255", "255", "255", "255"]),
            ins(&["shade", "1", "128"]),
            ins(&["sync", "1002000"]),
        ]
        .concat();
        let rec = Recording::parse(&data).unwrap();
        assert_eq!(rec.frames(), 3);
        assert_eq!(rec.duration_ms(), 2000);
        let mut p = Player::new(rec);
        assert!(p.seek(0));
        assert_eq!(p.render().0, 6);
        assert_eq!(px(&p, 1, 1), [255, 0, 0, 255]);
        assert!(p.seek(600));
        assert!(!p.seek(700));
        assert_eq!(px(&p, 1, 1), [0, 0, 255, 255]);
        assert_eq!(px(&p, 2, 1), [0, 0, 255, 255]);
        assert_eq!(px(&p, 3, 1), [255, 0, 0, 255]);
        assert_eq!(px(&p, 4, 3), [0, 255, 0, 255]);
        p.seek(5000);
        let [r, g, b, _] = px(&p, 5, 0);
        assert!(
            (r as i32 - 255).abs() <= 1 && (127..=129).contains(&g) && (127..=129).contains(&b),
            "{r} {g} {b}"
        );
        // Backward seek replays from the start.
        assert!(p.seek(0));
        assert_eq!(px(&p, 1, 1), [255, 0, 0, 255]);
        assert_eq!(px(&p, 5, 0), [255, 0, 0, 255]);
        assert!(Recording::parse(&ins(&["size", "0", "1", "1"])).is_err());
    }
}
