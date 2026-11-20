//! The output language spoken to the terminal, and the allowlist that
//! `--strict` enforces.

use ratatui::style::Modifier;
use std::fmt::Write as _;

/// Display attributes the terminal supports (all verified on the WY-60).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Attrs {
    pub bold: bool,
    pub dim: bool,
    pub underline: bool,
    pub blink: bool,
    pub reverse: bool,
}

impl From<Modifier> for Attrs {
    fn from(m: Modifier) -> Self {
        Attrs {
            bold: m.contains(Modifier::BOLD),
            dim: m.contains(Modifier::DIM),
            underline: m.contains(Modifier::UNDERLINED),
            blink: m.intersects(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK),
            reverse: m.contains(Modifier::REVERSED),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Charset {
    Ascii,
    Graphics,
}

/// A row's DEC line attribute. Double rows are double width too: they hold
/// half as many columns.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum LineAttr {
    #[default]
    Single,
    /// Top half of a double-height pair (DECDHL).
    DoubleTop,
    /// Bottom half of a double-height pair (DECDHL).
    DoubleBottom,
}

/// What a cell's symbol becomes on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    Ascii(u8),
    /// A byte in the DEC Special Graphics set.
    Graphics(u8),
    /// A space with reverse video toggled (full blocks).
    ReverseSpace,
}

/// Map a ratatui cell symbol to something the VT100 can show.
pub fn map_glyph(symbol: &str) -> Glyph {
    let mut chars = symbol.chars();
    let Some(c) = chars.next() else { return Glyph::Ascii(b' ') };
    if chars.next().is_some() {
        return Glyph::Ascii(b'?');
    }
    if (' '..='~').contains(&c) {
        return Glyph::Ascii(c as u8);
    }
    let g = match c {
        '─' | '━' | '═' => b'q',
        '│' | '┃' | '║' => b'x',
        '┌' | '┏' | '╔' | '╭' => b'l',
        '┐' | '┓' | '╗' | '╮' => b'k',
        '└' | '┗' | '╚' | '╰' => b'm',
        '┘' | '┛' | '╝' | '╯' => b'j',
        '├' | '┣' | '╠' => b't',
        '┤' | '┫' | '╣' => b'u',
        '┬' | '┳' | '╦' => b'w',
        '┴' | '┻' | '╩' => b'v',
        '┼' | '╋' | '╬' => b'n',
        '◆' => b'`',
        '▒' | '░' | '▓' => b'a',
        '°' => b'f',
        '±' => b'g',
        '≤' => b'y',
        '≥' => b'z',
        'π' => b'{',
        '≠' => b'|',
        '£' => b'}',
        '·' => b'~',
        '⎺' => b'o',
        '⎻' => b'p',
        '⎼' => b'r',
        '⎽' => b's',
        '█' | '▉' | '▊' | '▋' | '▇' | '▆' | '▅' | '▌' | '▐' | '▀' | '▄' => return Glyph::ReverseSpace,
        '▁' | '▂' | '▃' | '▏' | '▎' | '▍' => return Glyph::Ascii(b' '),
        _ => return transliterate(c),
    };
    Glyph::Graphics(g)
}

/// ASCII stand-in for a character the terminal cannot show.
fn transliterate(c: char) -> Glyph {
    // Prompt glyphs that deunicode renders poorly.
    let explicit = match c {
        '❯' | '›' | '➜' | '→' | '»' | '▶' => Some(b'>'),
        '❮' | '‹' | '←' | '«' | '◀' => Some(b'<'),
        '✓' | '✔' => Some(b'v'),
        '✗' | '✘' => Some(b'x'),
        _ => None,
    };
    if let Some(b) = explicit {
        return Glyph::Ascii(b);
    }
    match deunicode::deunicode_char(c).and_then(|s| s.bytes().next()) {
        Some(b) if (0x21..=0x7e).contains(&b) => Glyph::Ascii(b),
        _ => Glyph::Ascii(b'?'),
    }
}

/// An output language. Rows/cols are 0-based in, 1-based on the wire.
pub trait Dialect {
    fn cup(&self, out: &mut Vec<u8>, row: u16, col: u16);
    fn cuf(&self, out: &mut Vec<u8>, n: u16);
    /// Reset, then set exactly `a`.
    fn sgr(&self, out: &mut Vec<u8>, a: Attrs);
    fn charset(&self, out: &mut Vec<u8>, c: Charset);
    /// Reset attributes, charset and scroll region; clear; home.
    fn clear(&self, out: &mut Vec<u8>);
    fn set_scroll_region(&self, out: &mut Vec<u8>, top: u16, bottom: u16);
    fn reset_scroll_region(&self, out: &mut Vec<u8>);
    /// Set the cursor row's line attribute.
    fn line_attr(&self, out: &mut Vec<u8>, a: LineAttr);
    /// Line feed: at the bottom margin, scrolls the region up one line.
    fn index(&self, out: &mut Vec<u8>);
    /// Reverse index: at the top margin, scrolls the region down one line.
    fn reverse_index(&self, out: &mut Vec<u8>);
    /// Is this complete escape sequence safe to send?
    fn is_allowed(&self, seq: &[u8]) -> bool;
}

/// The WY-60's VT100 personality (enhanced mode).
#[derive(Clone, Copy, Debug, Default)]
pub struct Vt100Dialect;

fn push(out: &mut Vec<u8>, args: std::fmt::Arguments) {
    let mut s = String::new();
    let _ = s.write_fmt(args);
    out.extend_from_slice(s.as_bytes());
}

impl Dialect for Vt100Dialect {
    fn cup(&self, out: &mut Vec<u8>, row: u16, col: u16) {
        push(out, format_args!("\x1b[{};{}H", row + 1, col + 1));
    }
    fn cuf(&self, out: &mut Vec<u8>, n: u16) {
        push(out, format_args!("\x1b[{n}C"));
    }
    fn sgr(&self, out: &mut Vec<u8>, a: Attrs) {
        out.extend_from_slice(b"\x1b[0");
        for (on, p) in [(a.bold, b'1'), (a.dim, b'2'), (a.underline, b'4'), (a.blink, b'5'), (a.reverse, b'7')] {
            if on {
                out.extend_from_slice(&[b';', p]);
            }
        }
        out.push(b'm');
    }
    fn charset(&self, out: &mut Vec<u8>, c: Charset) {
        out.extend_from_slice(match c {
            Charset::Ascii => b"\x1b(B",
            Charset::Graphics => b"\x1b(0",
        });
    }
    fn clear(&self, out: &mut Vec<u8>) {
        // SI, DECCKM off and DECKPNM undo modes a program may have left set
        // through passthrough.
        out.extend_from_slice(b"\x1b[0m\x0f\x1b(B\x1b[?1l\x1b>\x1b[r\x1b[2J\x1b[H");
    }
    fn set_scroll_region(&self, out: &mut Vec<u8>, top: u16, bottom: u16) {
        push(out, format_args!("\x1b[{};{}r", top + 1, bottom + 1));
    }
    fn reset_scroll_region(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[r");
    }
    fn line_attr(&self, out: &mut Vec<u8>, a: LineAttr) {
        out.extend_from_slice(match a {
            LineAttr::Single => b"\x1b#5",
            LineAttr::DoubleTop => b"\x1b#3",
            LineAttr::DoubleBottom => b"\x1b#4",
        });
    }
    fn index(&self, out: &mut Vec<u8>) {
        out.push(b'\n');
    }
    fn reverse_index(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1bM");
    }
    fn is_allowed(&self, seq: &[u8]) -> bool {
        match seq {
            [0x1b, b'M'] | [0x1b, b'>'] => true,
            b"\x1b[?1l" => true,
            [0x1b, b'(', b'0' | b'B'] => true,
            [0x1b, b'#', b'3' | b'4' | b'5' | b'6'] => true,
            [0x1b, b'[', body @ .., fin] => {
                if !body.iter().all(|b| b.is_ascii_digit() || *b == b';') {
                    return false;
                }
                let params_ok = |allowed: &[&str]| {
                    body.split(|b| *b == b';')
                        .all(|p| p.is_empty() || allowed.contains(&std::str::from_utf8(p).unwrap_or("x")))
                };
                match fin {
                    b'H' | b'r' | b'C' => true,
                    b'm' => params_ok(&["0", "1", "2", "4", "5", "7"]),
                    b'J' | b'K' => params_ok(&["0", "1", "2"]),
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_glyphs_map_to_dec() {
        assert_eq!(map_glyph("┌"), Glyph::Graphics(b'l'));
        assert_eq!(map_glyph("╯"), Glyph::Graphics(b'j'));
        assert_eq!(map_glyph("━"), Glyph::Graphics(b'q'));
    }

    #[test]
    fn blocks_and_unknowns() {
        assert_eq!(map_glyph("█"), Glyph::ReverseSpace);
        assert_eq!(map_glyph("\u{E000}"), Glyph::Ascii(b'?'));
        assert_eq!(map_glyph(""), Glyph::Ascii(b' '));
        assert_eq!(map_glyph("A"), Glyph::Ascii(b'A'));
    }

    #[test]
    fn transliterates_common_unicode() {
        assert_eq!(map_glyph("é"), Glyph::Ascii(b'e'));
        assert_eq!(map_glyph("…"), Glyph::Ascii(b'.'));
        assert_eq!(map_glyph("—"), Glyph::Ascii(b'-'));
        assert_eq!(map_glyph("❯"), Glyph::Ascii(b'>'));
        assert_eq!(map_glyph("中"), Glyph::Ascii(b'Z'));
        assert_eq!(map_glyph("▉"), Glyph::ReverseSpace);
    }

    #[test]
    fn reverse_index_allowed() {
        let mut o = vec![];
        Vt100Dialect.reverse_index(&mut o);
        assert_eq!(o, b"\x1bM");
        assert!(Vt100Dialect.is_allowed(b"\x1bM"));
    }

    #[test]
    fn clear_resets_modes_left_by_passthrough() {
        let mut o = vec![];
        Vt100Dialect.clear(&mut o);
        let s = String::from_utf8(o).unwrap();
        for seq in ["\x0f", "\x1b[?1l", "\x1b>"] {
            assert!(s.contains(seq), "{seq:?} missing from {s:?}");
        }
        assert!(Vt100Dialect.is_allowed(b"\x1b[?1l") && Vt100Dialect.is_allowed(b"\x1b>"));
        assert!(!Vt100Dialect.is_allowed(b"\x1b[?25l"));
    }

    #[test]
    fn emitters_are_one_based() {
        let mut o = vec![];
        Vt100Dialect.cup(&mut o, 0, 0);
        Vt100Dialect.sgr(&mut o, Attrs { bold: true, reverse: true, ..Default::default() });
        assert_eq!(o, b"\x1b[1;1H\x1b[0;1;7m");
    }

    #[test]
    fn modifier_conversion() {
        use ratatui::style::Modifier;
        let a = Attrs::from(Modifier::BOLD | Modifier::RAPID_BLINK | Modifier::ITALIC);
        assert_eq!(a, Attrs { bold: true, blink: true, ..Default::default() });
    }

    #[test]
    fn allowlist() {
        let d = Vt100Dialect;
        for ok in [
            &b"\x1b[12;80H"[..], b"\x1b[0;1;4;5;7m", b"\x1b[m", b"\x1b[2J", b"\x1b[r", b"\x1b[3;20r", b"\x1b(0",
            b"\x1b#6", b"\x1b[5C",
        ] {
            assert!(d.is_allowed(ok), "{ok:?}");
        }
        for bad in [&b"\x1b[?25l"[..], b"\x1b[38;5;2m", b"\x1b[31m", b"\x1b]0;x\x07", b"\x1b[3~", b"\x1b(A"] {
            assert!(!d.is_allowed(bad), "{bad:?}");
        }
    }
}
