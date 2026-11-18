//! DEC Special Graphics in the PTY stream → Unicode box drawing.
//!
//! `vt100` 0.18 ignores `ESC ( 0`, so programs drawing lines with TERM=vt100
//! (htop, dialog, ncurses) would show `lqqk`. This filter tracks G0/G1 and
//! SO/SI and rewrites graphics bytes to Unicode before the parser sees them.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Ground,
    Esc,
    /// After `ESC (` or `ESC )`: the next byte names the set for G0 / G1.
    Designate(u8),
    /// `ESC` + intermediate bytes, until a final byte.
    EscInter,
    Csi,
    Osc,
    OscEsc,
}

pub struct AcsFilter {
    state: State,
    g0_graphics: bool,
    g1_graphics: bool,
    /// SO (shift out) selects G1.
    shifted: bool,
}

impl Default for AcsFilter {
    fn default() -> Self {
        Self::new()
    }
}

fn graphic(b: u8) -> Option<char> {
    Some(match b {
        b'_' => ' ',
        b'`' => '◆',
        b'a' => '▒',
        b'f' => '°',
        b'g' => '±',
        b'j' => '┘',
        b'k' => '┐',
        b'l' => '┌',
        b'm' => '└',
        b'n' => '┼',
        b'o' => '⎺',
        b'p' => '⎻',
        b'q' => '─',
        b'r' => '⎼',
        b's' => '⎽',
        b't' => '├',
        b'u' => '┤',
        b'v' => '┴',
        b'w' => '┬',
        b'x' => '│',
        b'y' => '≤',
        b'z' => '≥',
        b'{' => 'π',
        b'|' => '≠',
        b'}' => '£',
        b'~' => '·',
        _ => return None,
    })
}

impl AcsFilter {
    pub fn new() -> Self {
        Self { state: State::Ground, g0_graphics: false, g1_graphics: false, shifted: false }
    }

    fn graphics_active(&self) -> bool {
        if self.shifted { self.g1_graphics } else { self.g0_graphics }
    }

    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            self.state = match self.state {
                State::Ground => match b {
                    0x1b => State::Esc,
                    0x0e => {
                        self.shifted = true;
                        State::Ground
                    }
                    0x0f => {
                        self.shifted = false;
                        State::Ground
                    }
                    _ => {
                        match graphic(b).filter(|_| self.graphics_active()) {
                            Some(c) => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
                            None => out.push(b),
                        }
                        State::Ground
                    }
                },
                State::Esc => match b {
                    b'(' | b')' => State::Designate(b),
                    _ => {
                        out.extend_from_slice(&[0x1b, b]);
                        match b {
                            b'[' => State::Csi,
                            b']' => State::Osc,
                            0x20..=0x2f => State::EscInter,
                            _ => State::Ground,
                        }
                    }
                },
                State::Designate(which) => {
                    let graphics = b == b'0';
                    if which == b'(' {
                        self.g0_graphics = graphics;
                    } else {
                        self.g1_graphics = graphics;
                    }
                    State::Ground
                }
                State::EscInter => {
                    out.push(b);
                    if (0x30..=0x7e).contains(&b) { State::Ground } else { State::EscInter }
                }
                State::Csi => {
                    out.push(b);
                    if (0x40..=0x7e).contains(&b) { State::Ground } else { State::Csi }
                }
                State::Osc => {
                    out.push(b);
                    match b {
                        0x07 => State::Ground,
                        0x1b => State::OscEsc,
                        _ => State::Osc,
                    }
                }
                State::OscEsc => {
                    out.push(b);
                    State::Ground
                }
            };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8]) -> String {
        String::from_utf8(AcsFilter::new().filter(input)).unwrap()
    }

    #[test]
    fn acs_switch_becomes_box_drawing() {
        assert_eq!(run(b"\x1b(0lqqk\x1b(Bok"), "┌──┐ok");
    }

    #[test]
    fn shift_out_uses_g1() {
        assert_eq!(run(b"\x1b)0a\x0eq\x0fq"), "a─q");
    }

    #[test]
    fn csi_params_untouched_in_graphics_mode() {
        assert_eq!(run(b"\x1b(0\x1b[12;40Hq"), "\x1b[12;40H─");
    }

    #[test]
    fn osc_untouched_in_graphics_mode() {
        assert_eq!(run(b"\x1b(0\x1b]0;title q\x07q"), "\x1b]0;title q\x07─");
    }

    #[test]
    fn split_across_calls() {
        let mut f = AcsFilter::new();
        let mut v = f.filter(b"\x1b(");
        v.extend(f.filter(b"0x"));
        assert_eq!(String::from_utf8(v).unwrap(), "│");
    }

    #[test]
    fn utf8_passes_through() {
        assert_eq!(run("héllo".as_bytes()), "héllo");
    }

    #[test]
    fn other_escapes_pass_through() {
        assert_eq!(run(b"\x1b7\x1b#8\x1bM"), "\x1b7\x1b#8\x1bM");
    }
}
