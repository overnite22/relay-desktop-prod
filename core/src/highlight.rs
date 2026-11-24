//! Syntax colouring of the SSH terminal (like RDM's "Syntax coloring"):
//! keyword or regular expression rules give a text colour, a background and
//! bold to the matching text. Applied when rendering, the data is unchanged.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rule {
    /// Keyword (whole word) or regular expression.
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    /// `#rrggbb`, empty = unchanged.
    pub fg: String,
    pub bg: String,
    pub bold: bool,
    pub enabled: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Rule {
            pattern: String::new(),
            regex: false,
            case_sensitive: false,
            fg: String::new(),
            bg: String::new(),
            bold: false,
            enabled: true,
        }
    }
}

/// Colours offered by the rule editor (name, `#rrggbb`).
pub const COLORS: [(&str, &str); 11] = [
    ("Inchangée", ""),
    ("Rouge", "#f14c4c"),
    ("Orange", "#f0883e"),
    ("Jaune", "#e5e510"),
    ("Vert", "#23d18b"),
    ("Cyan", "#29b8db"),
    ("Bleu", "#3b8eea"),
    ("Magenta", "#d670d6"),
    ("Blanc", "#ffffff"),
    ("Gris", "#808080"),
    ("Noir", "#000000"),
];

fn rule(pattern: &str, regex: bool, fg: &str, bold: bool, enabled: bool) -> Rule {
    Rule {
        pattern: pattern.into(),
        regex,
        fg: fg.into(),
        bold,
        enabled,
        ..Default::default()
    }
}

/// Default set: errors in red, warnings in yellow, success in green,
/// addresses and URLs in cyan, absolute paths in blue (off).
pub fn default_rules() -> Vec<Rule> {
    vec![
        rule(
            r"\b(error|errors|err|failed|failure|fail|fatal|critical|denied|refused|panic|unreachable)\b",
            true,
            "#f14c4c",
            true,
            true,
        ),
        rule(r"\b(warning|warnings|warn|deprecated)\b", true, "#e5e510", false, true),
        rule(
            r"\b(ok|success|successful|succeeded|done|active|running|enabled|up)\b",
            true,
            "#23d18b",
            false,
            true,
        ),
        rule(r"\bhttps?://[^\s'\x22<>]+", true, "#29b8db", false, true),
        rule(
            r"\b(?:\d{1,3}\.){3}\d{1,3}(?:/\d{1,2})?(?::\d{1,5})?\b",
            true,
            "#29b8db",
            false,
            true,
        ),
        rule(
            r"\b(?:[0-9a-f]{1,4}:){7}[0-9a-f]{1,4}\b|\b(?:[0-9a-f]{1,4}:){1,6}:(?:[0-9a-f]{1,4}(?::[0-9a-f]{1,4}){0,5})?\b|::1\b",
            true,
            "#29b8db",
            false,
            true,
        ),
        rule(r"(?:^|\s)(?:/[\w.@+-]+)+/?", true, "#3b8eea", false, false),
    ]
}

/// Colours and weight given to a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mark {
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
    pub bold: bool,
}

pub fn parse_color(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Compiled rules; the first matching rule wins for each character.
#[derive(Debug, Default)]
pub struct Highlighter {
    rules: Vec<(regex::Regex, Mark)>,
}

impl Highlighter {
    /// Compiles the enabled rules; invalid ones are skipped and reported.
    pub fn new(rules: &[Rule]) -> (Highlighter, Vec<String>) {
        let mut out = Highlighter::default();
        let mut errors = Vec::new();
        for r in rules.iter().filter(|r| r.enabled && !r.pattern.trim().is_empty()) {
            match compile(r) {
                Ok(re) => {
                    let mark = Mark {
                        fg: parse_color(&r.fg),
                        bg: parse_color(&r.bg),
                        bold: r.bold,
                    };
                    if mark != Mark::default() {
                        out.rules.push((re, mark));
                    }
                }
                Err(e) => errors.push(format!("« {} » : {e}", r.pattern)),
            }
        }
        (out, errors)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Mark of each character of `line` (indexed in characters).
    pub fn marks(&self, line: &str) -> Vec<Option<Mark>> {
        let starts: Vec<usize> = line.char_indices().map(|(i, _)| i).collect();
        let mut out = vec![None; starts.len()];
        let char_at = |byte: usize| starts.partition_point(|&s| s < byte);
        for (re, mark) in &self.rules {
            for m in re.find_iter(line) {
                for slot in &mut out[char_at(m.start())..char_at(m.end())] {
                    if slot.is_none() {
                        *slot = Some(*mark);
                    }
                }
            }
        }
        out
    }

    /// Marks per column of a terminal row given as cell contents: a wide
    /// character fills two columns, its continuation cell is `None`.
    pub fn row_marks(&self, cells: &[Option<String>]) -> Vec<Option<Mark>> {
        let mut line = String::new();
        let mut cols = Vec::new();
        for (col, cell) in cells.iter().enumerate() {
            let Some(text) = cell else { continue };
            let mut chars = text.chars();
            let first = chars.next().unwrap_or(' ');
            line.push(first);
            cols.push(col);
            // Combining characters follow their base in the same cell.
            for c in chars {
                line.push(c);
                cols.push(col);
            }
        }
        let mut out = vec![None; cells.len()];
        for (i, m) in self.marks(&line).into_iter().enumerate() {
            if let Some(m) = m {
                let col = cols[i];
                out[col] = Some(m);
                if col + 1 < cells.len() && cells[col + 1].is_none() {
                    out[col + 1] = Some(m);
                }
            }
        }
        out
    }
}

fn compile(r: &Rule) -> Result<regex::Regex, String> {
    let pattern = if r.regex {
        r.pattern.clone()
    } else {
        let p = regex::escape(r.pattern.trim());
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let t = r.pattern.trim();
        format!(
            "{}{p}{}",
            if word(t.chars().next()) { r"\b" } else { "" },
            if word(t.chars().last()) { r"\b" } else { "" }
        )
    };
    regex::RegexBuilder::new(&pattern)
        .case_insensitive(!r.case_sensitive)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| e.to_string().lines().last().unwrap_or("expression invalide").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colored(h: &Highlighter, line: &str) -> String {
        h.marks(line)
            .iter()
            .zip(line.chars())
            .map(|(m, c)| match m.and_then(|m| m.fg) {
                Some([0xf1, ..]) => 'R',
                Some([0xe5, ..]) => 'Y',
                Some([0x23, ..]) => 'G',
                Some([0x29, ..]) => 'C',
                Some(_) => 'X',
                None if c == ' ' => ' ',
                None => '.',
            })
            .collect()
    }

    #[test]
    fn default_set() {
        let (h, errors) = Highlighter::new(&default_rules());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(colored(&h, "ERROR: Connection refused"), "RRRRR. .......... RRRRRRR");
        assert_eq!(colored(&h, "warn okay ok"), "YYYY .... GG");
        assert_eq!(colored(&h, "ping 203.0.113.7 fe80::1 fini"), ".... CCCCCCCCCCC CCCCCCC ....");
        assert_eq!(colored(&h, "voir https://example.com/a?b=1 x"), ".... CCCCCCCCCCCCCCCCCCCCCCCCC .");
        assert_eq!(colored(&h, "terror"), "......");
    }

    #[test]
    fn keywords_order_and_errors() {
        let rules = vec![
            Rule {
                pattern: "C++".into(),
                fg: "#ffffff".into(),
                ..Default::default()
            },
            Rule {
                pattern: "srv-lab".into(),
                case_sensitive: true,
                bg: "#000000".into(),
                bold: true,
                ..Default::default()
            },
            Rule {
                pattern: "srv".into(),
                fg: "#ffffff".into(),
                ..Default::default()
            },
            Rule {
                pattern: "(".into(),
                regex: true,
                fg: "#ffffff".into(),
                ..Default::default()
            },
            Rule {
                pattern: "ignored".into(),
                enabled: false,
                fg: "#ffffff".into(),
                ..Default::default()
            },
        ];
        let (h, errors) = Highlighter::new(&rules);
        assert_eq!(errors.len(), 1);
        let m = h.marks("C++ SRV-LAB srv-lab");
        assert_eq!(m[0].unwrap().fg, Some([255, 255, 255]));
        // Case-sensitive "srv-lab" does not match "SRV-LAB": the "srv" rule does.
        assert_eq!(m[4].unwrap().fg, Some([255, 255, 255]));
        assert!(m[7].is_none());
        let first = m[12].unwrap();
        assert_eq!((first.bg, first.bold, first.fg), (Some([0, 0, 0]), true, None));
        assert!(h.marks("ignored")[0].is_none());
        assert_eq!(parse_color("#0a0B0c"), Some([10, 11, 12]));
        assert_eq!(parse_color("rouge"), None);
    }

    #[test]
    fn wide_cells() {
        let (h, _) = Highlighter::new(&default_rules());
        // "日本 error": two wide characters (4 columns), then the word.
        let cells: Vec<Option<String>> = ["日", "", "本", "", " ", "e", "r", "r", "o", "r"]
            .iter()
            .map(|s| if s.is_empty() { None } else { Some(s.to_string()) })
            .collect();
        let m = h.row_marks(&cells);
        assert!(m[..5].iter().all(Option::is_none));
        assert!(m[5..].iter().all(Option::is_some));
        let cells: Vec<Option<String>> = ["é", "c", "h", "e", "c", " ", "f", "a", "i", "l", "e", "d"]
            .iter()
            .map(|s| Some(s.to_string()))
            .collect();
        let m = h.row_marks(&cells);
        assert!(m[..6].iter().all(Option::is_none) && m[6..].iter().all(Option::is_some));
    }
}
