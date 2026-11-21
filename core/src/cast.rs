//! asciicast v2 recordings of SSH sessions (the format of asciinema): a JSON
//! header line, then one `[time, "o" | "r", data]` line per event.

use serde::Deserialize;
use std::io::{BufRead, Write};
use std::time::Instant;

/// Writes a recording while the session runs.
pub struct Writer<W: Write> {
    out: W,
    start: Instant,
}

impl<W: Write> Writer<W> {
    pub fn new(mut out: W, cols: u16, rows: u16, title: &str, unix_time: u64) -> std::io::Result<Self> {
        let header = serde_json::json!({
            "version": 2,
            "width": cols,
            "height": rows,
            "timestamp": unix_time,
            "title": title,
            "env": {"TERM": "xterm-256color"},
        });
        writeln!(out, "{header}")?;
        Ok(Writer {
            out,
            start: Instant::now(),
        })
    }

    fn event(&mut self, kind: &str, data: &str) -> std::io::Result<()> {
        let t = self.start.elapsed().as_secs_f64();
        writeln!(self.out, "{}", serde_json::json!([(t * 1e6).round() / 1e6, kind, data]))
    }

    /// Terminal output (invalid UTF-8 is replaced).
    pub fn output(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.event("o", &String::from_utf8_lossy(bytes))
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> std::io::Result<()> {
        self.event("r", &format!("{cols}x{rows}"))
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Header {
    pub width: u16,
    pub height: u16,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Output(String),
    Resize(u16, u16),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub header: Header,
    /// Events with their time in seconds; pauses longer than `max_idle`
    /// are shortened (like `asciinema play -i`).
    pub events: Vec<(f64, Event)>,
}

impl Recording {
    pub fn duration(&self) -> f64 {
        self.events.last().map_or(0.0, |e| e.0)
    }
}

/// Reads an asciicast v2 file; unknown event kinds (input, markers) are skipped.
pub fn parse(r: impl BufRead, max_idle: f64) -> Result<Recording, String> {
    let mut lines = r.lines();
    let first = lines.next().ok_or("fichier vide")?.map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&first).map_err(|_| "en-tête asciicast illisible")?;
    if v.get("version").and_then(|v| v.as_u64()) != Some(2) {
        return Err("seul le format asciicast v2 est pris en charge".into());
    }
    let header: Header = serde_json::from_value(v).map_err(|e| format!("en-tête asciicast : {e}"))?;
    let (mut events, mut last, mut shift) = (Vec::new(), 0.0f64, 0.0f64);
    for (n, line) in lines.enumerate() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let (t, kind, data): (f64, String, String) = serde_json::from_str(&line).map_err(|_| format!("ligne {} illisible", n + 2))?;
        if t - last > max_idle {
            shift += t - last - max_idle;
        }
        last = t;
        let ev = match kind.as_str() {
            "o" => Event::Output(data),
            "r" => {
                let Some((c, r)) = data.split_once('x') else { continue };
                match (c.parse(), r.parse()) {
                    (Ok(c), Ok(r)) => Event::Resize(c, r),
                    _ => continue,
                }
            }
            _ => continue,
        };
        events.push(((t - shift).max(0.0), ev));
    }
    Ok(Recording { header, events })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_parse() {
        let mut buf = Vec::new();
        {
            let mut w = Writer::new(&mut buf, 100, 30, "web-01", 1_700_000_000).unwrap();
            w.output(b"hello \xff\r\n").unwrap();
            w.resize(120, 40).unwrap();
            w.output("é\"\x1b[0m".as_bytes()).unwrap();
            w.flush().unwrap();
        }
        let rec = parse(buf.as_slice(), 2.0).unwrap();
        assert_eq!(rec.header.width, 100);
        assert_eq!(rec.header.title, "web-01");
        assert_eq!(rec.events.len(), 3);
        assert_eq!(rec.events[0].1, Event::Output("hello \u{fffd}\r\n".into()));
        assert_eq!(rec.events[1].1, Event::Resize(120, 40));
        assert_eq!(rec.events[2].1, Event::Output("é\"\x1b[0m".into()));
    }

    #[test]
    fn idle_is_capped_and_bad_files_refused() {
        let file =
            "{\"version\":2,\"width\":80,\"height\":24}\n[0.5,\"o\",\"a\"]\n[10.5,\"o\",\"b\"]\n[10.7,\"i\",\"x\"]\n[11.0,\"o\",\"c\"]\n";
        let rec = parse(file.as_bytes(), 2.0).unwrap();
        let times: Vec<f64> = rec.events.iter().map(|e| e.0).collect();
        assert_eq!(times, vec![0.5, 2.5, 3.0]);
        assert_eq!(rec.duration(), 3.0);
        assert!(parse("{\"version\":1}".as_bytes(), 2.0).is_err());
        assert!(parse("".as_bytes(), 2.0).is_err());
    }
}

/// File name of a new recording: `<title>-<UTC date>.cast`, without
/// characters Windows refuses.
pub fn file_name(title: &str, unix_time: i64) -> String {
    let safe: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' })
        .take(60)
        .collect();
    let date = time::OffsetDateTime::from_unix_timestamp(unix_time)
        .ok()
        .and_then(|d| {
            d.format(time::macros::format_description!("[year][month][day]-[hour][minute][second]"))
                .ok()
        })
        .unwrap_or_else(|| unix_time.to_string());
    format!("{}-{date}.cast", safe.trim_matches('.'))
}

#[cfg(test)]
mod name_tests {
    #[test]
    fn file_names() {
        assert_eq!(super::file_name("web 01/prod:ssh", 0), "web_01_prod_ssh-19700101-000000.cast");
    }
}
