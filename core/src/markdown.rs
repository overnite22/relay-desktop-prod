//! Splits entry notes (Markdown, GitHub flavour like Relay web) into blocks
//! the UI can render. Inline formatting (bold, italic, code, links) is left
//! in each block's text for the toolkit's inline Markdown renderer; raw HTML
//! is neutralised and images are replaced by their alternative text.

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph(String),
    /// List item; `marker` is "•", "1." or a check box.
    Item {
        depth: u8,
        marker: String,
        text: String,
    },
    Code(String),
    Quote(String),
    /// Table rows, cells separated by " │ ", shown in a fixed-width font.
    Table(String),
    Rule,
}

fn fence(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let t = line.trim_start();
    let level = t.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &t[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((level as u8, rest.trim().trim_end_matches('#').trim_end()))
}

fn rule(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3 && ["-", "*", "_"].iter().any(|c| t.chars().all(|x| x.to_string() == *c))
}

fn item(line: &str) -> Option<(u8, String, &str)> {
    let indent = line.len() - line.trim_start().len();
    let t = line.trim_start();
    let (marker, rest) = if let Some(r) = t.strip_prefix("- ").or(t.strip_prefix("* ")).or(t.strip_prefix("+ ")) {
        ("•".to_string(), r)
    } else {
        let digits = t.bytes().take_while(u8::is_ascii_digit).count();
        let after = &t[digits..];
        if digits == 0 || digits > 9 || !(after.starts_with(". ") || after.starts_with(") ")) {
            return None;
        }
        (format!("{}.", &t[..digits]), &after[2..])
    };
    let (marker, rest) = if let Some(r) = rest.strip_prefix("[ ] ") {
        ("☐".to_string(), r)
    } else if let Some(r) = rest.strip_prefix("[x] ").or(rest.strip_prefix("[X] ")) {
        ("☑".to_string(), r)
    } else {
        (marker, rest)
    };
    Some(((indent / 2).min(4) as u8, marker, rest))
}

fn table_row(line: &str) -> Option<String> {
    let t = line.trim();
    if !t.starts_with('|') {
        return None;
    }
    let cells: Vec<&str> = t.trim_matches('|').split('|').map(str::trim).collect();
    if cells.iter().all(|c| !c.is_empty() && c.chars().all(|x| matches!(x, '-' | ':'))) {
        return Some(String::new()); // separator row
    }
    Some(cells.join(" │ "))
}

/// Inline text safe for the inline renderer: images become their alt text and
/// HTML tags are escaped so they show literally (Relay web skips HTML).
pub fn inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("![") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find("](").and_then(|a| after[a..].find(')').map(|b| (a, a + b))) {
            Some((a, b)) => {
                out.push_str(&after[..a]);
                rest = &after[b + 1..];
            }
            None => {
                out.push_str("![");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace('<', "\\<")
}

pub fn parse(notes: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let flush = |para: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            blocks.push(Block::Paragraph(inline(&para.join("\n"))));
            para.clear();
        }
    };
    let lines: Vec<&str> = notes.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.trim().is_empty() {
            flush(&mut para, &mut blocks);
            continue;
        }
        if let Some(f) = fence(line) {
            flush(&mut para, &mut blocks);
            let mut code = Vec::new();
            while i < lines.len() && !lines[i].trim_start().starts_with(f) {
                code.push(lines[i]);
                i += 1;
            }
            i += 1;
            blocks.push(Block::Code(code.join("\n")));
            continue;
        }
        if let Some((level, text)) = heading(line) {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Heading { level, text: inline(text) });
            continue;
        }
        if rule(line) {
            // "text\n---" is a setext heading in Markdown.
            match para.pop() {
                Some(last) if line.trim_start().starts_with('-') => {
                    flush(&mut para, &mut blocks);
                    blocks.push(Block::Heading {
                        level: 2,
                        text: inline(last.trim()),
                    });
                }
                last => {
                    para.extend(last);
                    flush(&mut para, &mut blocks);
                    blocks.push(Block::Rule);
                }
            }
            continue;
        }
        if let Some(t) = line.trim_start().strip_prefix('>') {
            flush(&mut para, &mut blocks);
            let mut quote = vec![t.strip_prefix(' ').unwrap_or(t)];
            while i < lines.len() {
                match lines[i].trim_start().strip_prefix('>') {
                    Some(t) => quote.push(t.strip_prefix(' ').unwrap_or(t)),
                    None => break,
                }
                i += 1;
            }
            blocks.push(Block::Quote(inline(&quote.join("\n"))));
            continue;
        }
        if let Some((depth, marker, text)) = item(line) {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Item {
                depth,
                marker,
                text: inline(text),
            });
            continue;
        }
        if let Some(row) = table_row(line) {
            flush(&mut para, &mut blocks);
            let mut rows = vec![row];
            while i < lines.len() {
                match table_row(lines[i]) {
                    Some(r) => rows.push(r),
                    None => break,
                }
                i += 1;
            }
            rows.retain(|r| !r.is_empty());
            blocks.push(Block::Table(rows.join("\n")));
            continue;
        }
        // Continuation of a list item.
        if para.is_empty() && line.starts_with("  ") {
            if let Some(Block::Item { text, .. }) = blocks.last_mut() {
                text.push(' ');
                text.push_str(&inline(line.trim()));
                continue;
            }
        }
        para.push(line.trim_end());
    }
    flush(&mut para, &mut blocks);
    blocks
}

/// Links opened from notes: web only, never local files or scripts.
pub fn safe_link(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("https://") || u.starts_with("http://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks() {
        let md = "# Titre\nTexte **gras**\nsuite\n\n- un\n  - deux\n1. trois\n- [x] fait\n\n```\nls -l\n<b>\n```\n> cité\n---\n| a | b |\n|---|---|\n| 1 | 2 |\nSous-titre\n---\n";
        assert_eq!(
            parse(md),
            vec![
                Block::Heading {
                    level: 1,
                    text: "Titre".into()
                },
                Block::Paragraph("Texte **gras**\nsuite".into()),
                Block::Item {
                    depth: 0,
                    marker: "•".into(),
                    text: "un".into()
                },
                Block::Item {
                    depth: 1,
                    marker: "•".into(),
                    text: "deux".into()
                },
                Block::Item {
                    depth: 0,
                    marker: "1.".into(),
                    text: "trois".into()
                },
                Block::Item {
                    depth: 0,
                    marker: "☑".into(),
                    text: "fait".into()
                },
                Block::Code("ls -l\n<b>".into()),
                Block::Quote("cité".into()),
                Block::Rule,
                Block::Table("a │ b\n1 │ 2".into()),
                Block::Heading {
                    level: 2,
                    text: "Sous-titre".into()
                },
            ]
        );
    }

    #[test]
    fn inline_is_sanitised() {
        assert_eq!(inline("voir ![schéma](x.png) <script>"), "voir schéma \\<script>");
        assert_eq!(inline("[site](https://a.b) et `code`"), "[site](https://a.b) et `code`");
        assert_eq!(parse("#hashtag"), vec![Block::Paragraph("#hashtag".into())]);
        assert!(safe_link("https://x") && !safe_link("mailto:a@b"));
        assert!(!safe_link("file:///c:/x") && !safe_link("javascript:alert(1)"));
    }

    #[test]
    fn unterminated_fence_keeps_text() {
        assert_eq!(parse("```\na\nb"), vec![Block::Code("a\nb".into())]);
    }
}
