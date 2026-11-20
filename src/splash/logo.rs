//! The boot-splash logo: block art parsed from `assets/wy-logo.txt`.
//!
//! A line starting with `^` is drawn double height (and so double width),
//! centred on the screen; it takes two screen rows.

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
    double: Vec<bool>,
}

impl Logo {
    /// Parse art text. A trailing newline is ignored and trailing spaces on a
    /// line are not significant. A leading `^` marks a double-height line; its
    /// own leading spaces are dropped (it is centred on the screen).
    pub fn parse(src: &str) -> Logo {
        let (mut rows, mut double) = (Vec::new(), Vec::new());
        for l in src.lines() {
            let (text, dbl) = match l.strip_prefix('^') {
                Some(rest) => (rest.trim_start_matches(' '), true),
                None => (l, false),
            };
            rows.push(
                text.trim_end_matches(' ')
                    .chars()
                    .map(|c| if c == '█' { Cell::Block } else { Cell::Char(c) })
                    .collect(),
            );
            double.push(dbl);
        }
        Logo { rows, double }
    }

    pub fn shipped() -> &'static Logo {
        static L: OnceLock<Logo> = OnceLock::new();
        L.get_or_init(|| Logo::parse(include_str!("../../assets/wy-logo.txt")))
    }

    pub fn rows(&self) -> &[Vec<Cell>] {
        &self.rows
    }

    /// Whether row `r` is drawn double height.
    pub fn is_double(&self, r: usize) -> bool {
        self.double[r]
    }

    /// Screen rows: a double-height line counts twice.
    pub fn height(&self) -> usize {
        self.double.iter().map(|&d| if d { 2 } else { 1 }).sum()
    }

    /// Screen columns: a double-height line's are double width.
    pub fn width(&self) -> usize {
        self.rows.iter().zip(&self.double).map(|(r, &d)| r.len() * if d { 2 } else { 1 }).max().unwrap_or(0)
    }

    /// Width of the single-height lines, which are centred as one block.
    pub fn single_width(&self) -> usize {
        self.rows.iter().zip(&self.double).filter(|(_, d)| !**d).map(|(r, _)| r.len()).max().unwrap_or(0)
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
    fn caret_marks_a_double_line() {
        let l = Logo::parse("AB\n^   W Y  \nC\n");
        assert_eq!(l.height(), 4);
        assert!(!l.is_double(0) && l.is_double(1) && !l.is_double(2));
        assert_eq!(l.rows()[1], vec![Cell::Char('W'), Cell::Char(' '), Cell::Char('Y')]);
        assert_eq!(l.width(), 6);
        assert_eq!(l.single_width(), 2);
    }

    #[test]
    fn shipped_wordmark_is_double() {
        let l = Logo::shipped();
        let r = (0..l.rows().len()).find(|&r| l.is_double(r)).expect("a double line");
        let s: String = l.rows()[r].iter().map(|c| if let Cell::Char(ch) = c { *ch } else { '#' }).collect();
        assert_eq!(s, "W E Y L A N D - Y U T A N I");
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
