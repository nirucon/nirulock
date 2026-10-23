//! Default Niru Noir palette, shared field for field with NIRUWM and NIRUBAR.

#[derive(Clone, Debug)]
pub struct Palette {
    pub background: u32,
    pub foreground: u32,
    pub muted: u32,
    pub border: u32,
    pub focus: u32,
}

impl Default for Palette {
    fn default() -> Self {
        // Niru Noir: 0f0f10, e5e5e5, a8a8a8, 2a2a2d, 5a5a60
        Self {
            // Pure black. The idle and lock screens cover the whole display,
            // and a hint of colour read as "something is behind this".
            background: 0x000000,
            foreground: 0xe5e5e5,
            muted: 0xa8a8a8,
            border: 0x2a2a2d,
            focus: 0x5a5a60,
        }
    }
}

/// Read a `six hex digits` colour, rejecting anything else.
pub fn parse_hex(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_niru_noir() {
        let p = Palette::default();
        assert_eq!(p.background, 0x000000, "the idle screen covers everything");
        assert_eq!(p.foreground, 0xe5e5e5);
        assert_eq!(p.muted, 0xa8a8a8);
    }

    #[test]
    fn rejects_short_and_long_values() {
        assert_eq!(parse_hex("0f0f10"), Some(0x0f0f10));
        assert_eq!(parse_hex("fff"), None);
        assert_eq!(parse_hex("gggggg"), None);
    }
}
