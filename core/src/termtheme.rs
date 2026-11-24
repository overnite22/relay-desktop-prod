//! Colour themes of the SSH terminal (Windows Terminal schemes).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub key: &'static str,
    pub name: &'static str,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub cursor: [u8; 3],
    pub selection: [u8; 3],
    /// ANSI colours 0 to 15.
    pub palette: [[u8; 3]; 16],
}

const fn rgb(v: u32) -> [u8; 3] {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

const fn pal(v: [u32; 16]) -> [[u8; 3]; 16] {
    let mut out = [[0; 3]; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = rgb(v[i]);
        i += 1;
    }
    out
}

const CAMPBELL: [u32; 16] = [
    0x0c0c0c, 0xc50f1f, 0x13a10e, 0xc19c00, 0x0037da, 0x881798, 0x3a96dd, 0xcccccc, 0x767676, 0xe74856, 0x16c60c, 0xf9f1a5, 0x3b78ff,
    0xb4009e, 0x61d6d6, 0xf2f2f2,
];
const SOLARIZED: [u32; 16] = [
    0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5, 0x002b36, 0xcb4b16, 0x586e75, 0x657b83, 0x839496,
    0x6c71c4, 0x93a1a1, 0xfdf6e3,
];
const TANGO: [u32; 16] = [
    0x000000, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf, 0x555753, 0xef2929, 0x8ae234, 0xfce94f, 0x729fcf,
    0xad7fa8, 0x34e2e2, 0xeeeeec,
];

const fn theme(key: &'static str, name: &'static str, fg: u32, bg: u32, cursor: u32, selection: u32, palette: [u32; 16]) -> Theme {
    Theme {
        key,
        name,
        fg: rgb(fg),
        bg: rgb(bg),
        cursor: rgb(cursor),
        selection: rgb(selection),
        palette: pal(palette),
    }
}

/// Themes offered; the first one (Campbell, Windows Terminal) is the default.
pub const THEMES: [Theme; 11] = [
    theme("campbell", "Campbell", 0xcccccc, 0x0c0c0c, 0xffffff, 0x264f78, CAMPBELL),
    theme(
        "one-half-dark",
        "One Half Dark",
        0xdcdfe4,
        0x282c34,
        0xffffff,
        0x3e4451,
        [
            0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4, 0x5a6374, 0xe06c75, 0x98c379, 0xe5c07b,
            0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
        ],
    ),
    theme(
        "one-half-light",
        "One Half Light",
        0x383a42,
        0xfafafa,
        0x4f525d,
        0xd7dae0,
        [
            0x383a42, 0xe45649, 0x50a14f, 0xc18301, 0x0184bc, 0xa626a4, 0x0997b3, 0xfafafa, 0x4f525d, 0xdf6c75, 0x98c379, 0xe4c07a,
            0x61afef, 0xc577dd, 0x56b5c1, 0xffffff,
        ],
    ),
    theme(
        "solarized-dark",
        "Solarized Dark",
        0x839496,
        0x002b36,
        0x93a1a1,
        0x073642,
        SOLARIZED,
    ),
    theme(
        "solarized-light",
        "Solarized Light",
        0x657b83,
        0xfdf6e3,
        0x002b36,
        0xeee8d5,
        SOLARIZED,
    ),
    theme("tango-dark", "Tango Dark", 0xd3d7cf, 0x000000, 0xffffff, 0x3a3a3a, TANGO),
    theme("tango-light", "Tango Light", 0x555753, 0xffffff, 0x000000, 0xd0d0d0, TANGO),
    theme(
        "dracula",
        "Dracula",
        0xf8f8f2,
        0x282a36,
        0xf8f8f2,
        0x44475a,
        [
            0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2, 0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5,
            0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
    ),
    theme(
        "gruvbox-dark",
        "Gruvbox Dark",
        0xebdbb2,
        0x282828,
        0xebdbb2,
        0x504945,
        [
            0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984, 0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f,
            0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ],
    ),
    theme(
        "monokai",
        "Monokai",
        0xf8f8f2,
        0x272822,
        0xf8f8f0,
        0x49483e,
        [
            0x272822, 0xf92672, 0xa6e22e, 0xf4bf75, 0x66d9ef, 0xae81ff, 0xa1efe4, 0xf8f8f2, 0x75715e, 0xf92672, 0xa6e22e, 0xf4bf75,
            0x66d9ef, 0xae81ff, 0xa1efe4, 0xf9f8f5,
        ],
    ),
    theme(
        "rdm-classic",
        "Classique (noir et blanc)",
        0xffffff,
        0x000000,
        0xffffff,
        0x404040,
        CAMPBELL,
    ),
];

/// Theme of `key` (Campbell when unknown or empty).
pub fn theme_of(key: &str) -> &'static Theme {
    THEMES.iter().find(|t| t.key == key).unwrap_or(&THEMES[0])
}

pub fn index_of(key: &str) -> usize {
    THEMES.iter().position(|t| t.key == key).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes() {
        assert_eq!(theme_of("").name, "Campbell");
        assert_eq!(theme_of("inconnu").name, "Campbell");
        assert_eq!(theme_of("dracula").bg, [0x28, 0x2a, 0x36]);
        assert_eq!(THEMES[0].palette[9], [0xe7, 0x48, 0x56]);
        assert_eq!(index_of("monokai"), 9);
        let mut keys: Vec<_> = THEMES.iter().map(|t| t.key).collect();
        keys.dedup();
        assert_eq!(keys.len(), THEMES.len());
    }
}
