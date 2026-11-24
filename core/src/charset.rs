//! Remote character set of an SSH terminal (like PuTTY's "Remote character
//! set"): the output is decoded to UTF-8 for the terminal emulator, typed and
//! pasted text is encoded back. Also the keyboard rule for AltGr.

/// Character sets offered per entry (`charset` setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Charset {
    #[default]
    Utf8,
    Latin1,
    Latin9,
    Cp1252,
    Cp437,
    Cp850,
}

pub const CHARSETS: [(Charset, &str, &str); 6] = [
    (Charset::Utf8, "", "UTF-8"),
    (Charset::Latin1, "iso-8859-1", "ISO-8859-1 (Latin-1)"),
    (Charset::Latin9, "iso-8859-15", "ISO-8859-15 (Latin-9, €)"),
    (Charset::Cp1252, "windows-1252", "Windows-1252"),
    (Charset::Cp437, "cp437", "CP437 (DOS, cadres)"),
    (Charset::Cp850, "cp850", "CP850 (DOS Europe de l'Ouest)"),
];

/// Upper halves (0x80 to 0xFF) of the DOS code pages.
const CP437: &str =
    "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
const CP850: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø×ƒáíóúñÑªº¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ðÐÊËÈıÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµþÞÚÛÙýÝ¯´\u{ad}±‗¾¶§÷¸°¨·¹³²■\u{a0}";
/// 0x80 to 0x9F of Windows-1252 (undefined positions kept as C1 codes).
const CP1252: &str = "€\u{81}‚ƒ„…†‡ˆ‰Š‹Œ\u{8d}Ž\u{8f}\u{90}‘’“”•–—˜™š›œ\u{9d}žŸ";
/// Differences of ISO-8859-15 with Latin-1.
const LATIN9: [(u8, char); 8] = [
    (0xa4, '€'),
    (0xa6, 'Š'),
    (0xa8, 'š'),
    (0xb4, 'Ž'),
    (0xb8, 'ž'),
    (0xbc, 'Œ'),
    (0xbd, 'œ'),
    (0xbe, 'Ÿ'),
];

impl Charset {
    pub fn from_key(key: &str) -> Charset {
        CHARSETS
            .iter()
            .find(|(_, k, _)| k.eq_ignore_ascii_case(key.trim()))
            .map_or(Charset::Utf8, |c| c.0)
    }

    pub fn key(self) -> &'static str {
        CHARSETS.iter().find(|c| c.0 == self).map_or("", |c| c.1)
    }

    pub fn index(self) -> usize {
        CHARSETS.iter().position(|c| c.0 == self).unwrap_or(0)
    }

    /// Codeset part of a POSIX locale (`fr_FR.UTF-8`); `None` when no common
    /// locale uses it.
    pub fn locale_codeset(self) -> Option<&'static str> {
        match self {
            Charset::Utf8 => Some("UTF-8"),
            Charset::Latin1 => Some("ISO-8859-1"),
            Charset::Latin9 => Some("ISO-8859-15"),
            Charset::Cp1252 => Some("CP1252"),
            Charset::Cp437 | Charset::Cp850 => None,
        }
    }

    fn char_of(self, b: u8) -> char {
        if b < 0x80 {
            return b as char;
        }
        match self {
            Charset::Utf8 | Charset::Latin1 => b as char,
            Charset::Latin9 => LATIN9.iter().find(|(k, _)| *k == b).map_or(b as char, |(_, c)| *c),
            Charset::Cp1252 if b < 0xa0 => CP1252.chars().nth(usize::from(b - 0x80)).unwrap_or('?'),
            Charset::Cp1252 => b as char,
            Charset::Cp437 => CP437.chars().nth(usize::from(b - 0x80)).unwrap_or('?'),
            Charset::Cp850 => CP850.chars().nth(usize::from(b - 0x80)).unwrap_or('?'),
        }
    }

    /// Remote output as UTF-8 (UTF-8 is passed through: the emulator copes
    /// with sequences split between two reads).
    pub fn decode(self, bytes: &[u8]) -> Vec<u8> {
        if self == Charset::Utf8 || bytes.is_ascii() {
            return bytes.to_vec();
        }
        let mut out = String::with_capacity(bytes.len() + 8);
        out.extend(bytes.iter().map(|&b| self.char_of(b)));
        out.into_bytes()
    }

    /// Text typed or pasted, in the remote encoding ('?' when the character
    /// does not exist there).
    pub fn encode(self, text: &str) -> Vec<u8> {
        if self == Charset::Utf8 || text.is_ascii() {
            return text.as_bytes().to_vec();
        }
        text.chars()
            .map(|c| {
                if (c as u32) < 0x80 {
                    return c as u8;
                }
                (0x80..=0xffu8).find(|&b| self.char_of(b) == c).unwrap_or(b'?')
            })
            .collect()
    }
}

/// Bytes (UTF-8) of a typed character. AltGr reaches the application as
/// Ctrl+Alt: a character it produces (`@ # { [ | \ ~ € ^ ]` on an AZERTY
/// keyboard) is sent as is; Ctrl+letter gives the control code, Alt alone
/// prefixes ESC.
pub fn char_input(c: char, ctrl: bool, alt: bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    if ctrl && alt && !c.is_ascii_alphabetic() && !c.is_control() {
        out.extend(c.to_string().as_bytes());
        return Some(out);
    }
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
    use super::*;

    #[test]
    fn tables() {
        assert_eq!(CP437.chars().count(), 128);
        assert_eq!(CP850.chars().count(), 128);
        assert_eq!(CP1252.chars().count(), 32);
        for (cs, key, _) in CHARSETS {
            assert_eq!(Charset::from_key(key), cs);
            assert_eq!(cs.key(), key);
            // Every byte decodes to a character that encodes back to it.
            for b in (0x20..=0xffu8).filter(|_| cs != Charset::Utf8) {
                let s = String::from_utf8(cs.decode(&[b])).unwrap();
                {
                    assert_eq!(cs.encode(&s), vec![b], "{cs:?} {b:#x}");
                }
            }
        }
        assert_eq!(Charset::from_key("nope"), Charset::Utf8);
    }

    #[test]
    fn decode_encode() {
        assert_eq!(Charset::Utf8.decode("café".as_bytes()), "café".as_bytes());
        assert_eq!(Charset::Latin1.decode(b"caf\xe9 \x1b[1m"), "café \x1b[1m".as_bytes());
        assert_eq!(Charset::Latin9.decode(b"\xa4"), "€".as_bytes());
        assert_eq!(Charset::Cp1252.decode(b"\x80\x93"), "€“".as_bytes());
        assert_eq!(Charset::Cp437.decode(b"\xc9\xcd\xbb\xba"), "╔═╗║".as_bytes());
        assert_eq!(Charset::Cp850.decode(b"\x82\xd2"), "éÊ".as_bytes());
        assert_eq!(Charset::Latin1.encode("é€"), vec![0xe9, b'?']);
        assert_eq!(Charset::Latin9.encode("é€"), vec![0xe9, 0xa4]);
        assert_eq!(Charset::Cp1252.encode("€"), vec![0x80]);
        assert_eq!(Charset::Utf8.encode("€"), "€".as_bytes());
        assert_eq!(Charset::Latin9.locale_codeset(), Some("ISO-8859-15"));
    }

    #[test]
    fn altgr_and_control() {
        // AltGr (Ctrl+Alt) characters of an AZERTY keyboard.
        for c in ['@', '#', '{', '[', '|', '\\', '~', '€', '`', '^', ']', '}'] {
            assert_eq!(char_input(c, true, true), Some(c.to_string().into_bytes()), "{c}");
        }
        assert_eq!(char_input('c', true, false), Some(vec![3]));
        assert_eq!(char_input('[', true, false), Some(vec![0x1b]));
        assert_eq!(char_input('c', true, true), Some(vec![0x1b, 3]));
        assert_eq!(char_input('x', false, true), Some(b"\x1bx".to_vec()));
        assert_eq!(char_input('é', false, false), Some("é".as_bytes().to_vec()));
        assert_eq!(char_input('ê', false, false), Some("ê".as_bytes().to_vec()));
        assert_eq!(char_input('é', true, false), Some("é".as_bytes().to_vec()));
        assert_eq!(char_input('!', true, false), None);
    }
}
