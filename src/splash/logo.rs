//! The boot-splash logo: block art parsed from `assets/wy-logo.txt`.
//!
//! A line starting with `^` is drawn double height (and so double width),
//! centred on the screen; it takes two screen rows.

use crate::dialect::LineAttr;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
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
        self.rows
            .iter()
            .zip(&self.double)
            .map(|(r, &d)| r.len() * if d { 2 } else { 1 })
            .max()
            .unwrap_or(0)
    }

    /// Width of the single-height lines, which are centred as one block.
    pub fn single_width(&self) -> usize {
        self.rows
            .iter()
            .zip(&self.double)
            .filter(|(_, d)| !**d)
            .map(|(r, _)| r.len())
            .max()
            .unwrap_or(0)
    }
}

/// Each logo row with its screen row (relative to `area`), starting at `top`.
/// A double row that would not fit whole is left out.
pub fn layout(logo: &Logo, area: Rect, top: i32) -> Vec<(usize, i32)> {
    let mut y = top;
    let mut out = Vec::new();
    for r in 0..logo.rows().len() {
        let h = if logo.is_double(r) { 2 } else { 1 };
        if h == 1 || y + 1 < i32::from(area.height) {
            out.push((r, y));
        }
        y += h;
    }
    out
}

/// The DECDHL rows the logo needs (absolute screen rows).
pub fn line_attrs(logo: &Logo, area: Rect, top: i32) -> Vec<(u16, LineAttr)> {
    layout(logo, area, top)
        .into_iter()
        .filter(|&(r, y)| logo.is_double(r) && y >= 0)
        .flat_map(|(_, y)| {
            let y = area.y + y as u16;
            [(y, LineAttr::DoubleTop), (y + 1, LineAttr::DoubleBottom)]
        })
        .collect()
}

/// Draw the logo from row `top`, centred. A double row's cells are double
/// width, so it is laid out on half the columns and drawn into both of its
/// screen rows; single rows are centred as one block.
pub fn draw(logo: &Logo, area: Rect, top: i32, buf: &mut Buffer) {
    let put = |buf: &mut Buffer, x: i32, y: i32, s: &str| {
        if y < 0 || y >= i32::from(area.height) || x < 0 || x >= i32::from(area.width) {
            return;
        }
        buf[(area.x + x as u16, area.y + y as u16)].set_symbol(s);
    };
    let lx = (i32::from(area.width) - logo.single_width() as i32) / 2;
    for (r, y) in layout(logo, area, top) {
        let row = &logo.rows()[r];
        let double = logo.is_double(r);
        let x0 = if double { (i32::from(area.width / 2) - row.len() as i32) / 2 } else { lx };
        for (c, cell) in row.iter().enumerate() {
            let s = match cell {
                Cell::Block => "█".to_string(),
                Cell::Char(' ') => continue,
                Cell::Char(ch) => ch.to_string(),
            };
            put(buf, x0 + c as i32, y, &s);
            if double {
                put(buf, x0 + c as i32, y + 1, &s);
            }
        }
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
    fn layout_skips_double_row_that_does_not_fit() {
        use crate::dialect::LineAttr;
        let l = Logo::parse("^AB\nC\n");
        let area = Rect::new(0, 0, 80, 3);
        assert_eq!(layout(&l, area, 2), vec![(1, 4)]);
        assert_eq!(layout(&l, area, 0), vec![(0, 0), (1, 2)]);
        assert_eq!(line_attrs(&l, area, 0), vec![(0, LineAttr::DoubleTop), (1, LineAttr::DoubleBottom)]);
    }

    #[test]
    fn draw_centres_single_and_double_rows() {
        let l = Logo::parse("^AB\nC\n");
        let area = Rect::new(0, 0, 10, 4);
        let mut buf = Buffer::empty(area);
        draw(&l, area, 0, &mut buf);
        // Double row: half width 5, len 2 → x = 1, in both screen rows.
        assert_eq!((buf[(1, 0)].symbol(), buf[(2, 0)].symbol()), ("A", "B"));
        assert_eq!((buf[(1, 1)].symbol(), buf[(2, 1)].symbol()), ("A", "B"));
        // Single rows centre as one block on single_width (1) → x = 4.
        assert_eq!(buf[(4, 2)].symbol(), "C");
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
