//! The boot-splash logo: block art parsed from `assets/wy-logo.txt`.

use std::sync::OnceLock;

pub const MAX_ROWS: usize = 22;
pub const MAX_COLS: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Block,
    Char(char),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Logo {
    rows: Vec<Vec<Cell>>,
}

impl Logo {
    /// Parse art text. A trailing newline is ignored and trailing spaces on a
    /// line are not significant.
    pub fn parse(src: &str) -> Logo {
        let rows = src
            .lines()
            .map(|l| {
                l.trim_end_matches(' ')
                    .chars()
                    .map(|c| if c == '█' { Cell::Block } else { Cell::Char(c) })
                    .collect()
            })
            .collect();
        Logo { rows }
    }

    pub fn shipped() -> &'static Logo {
        static L: OnceLock<Logo> = OnceLock::new();
        L.get_or_init(|| Logo::parse(include_str!("../../assets/wy-logo.txt")))
    }

    pub fn rows(&self) -> &[Vec<Cell>] {
        &self.rows
    }

    pub fn height(&self) -> usize {
        self.rows.len()
    }

    pub fn width(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_maps_blocks_and_keeps_text() {
        let l = Logo::parse("█ A\n  ██\n");
        assert_eq!(l.height(), 2);
        assert_eq!(l.width(), 4);
        assert_eq!(l.rows()[0], vec![Cell::Block, Cell::Char(' '), Cell::Char('A')]);
        assert_eq!(l.rows()[1][2], Cell::Block);
    }

    #[test]
    fn shipped_logo_fits_the_budget() {
        let l = Logo::shipped();
        assert!(l.height() >= 1 && l.height() <= MAX_ROWS, "{} rows", l.height());
        assert!(l.width() <= MAX_COLS, "{} cols", l.width());
        assert!(l.rows().iter().flatten().any(|c| *c != Cell::Char(' ')), "logo is blank");
    }

    #[test]
    fn shipped_logo_is_plain_ascii() {
        // ASCII art: one byte per cell on the wire, no attribute switching.
        for c in Logo::shipped().rows().iter().flatten() {
            assert!(matches!(c, Cell::Char(ch) if ch.is_ascii_graphic() || *ch == ' '), "{c:?}");
        }
    }
}
