//! The attribute / charset probe screen.
//!
//! Each probe is a numbered, self-labelled row so the user can report
//! "items 3, 9 and 13 look wrong". Needs at least `MIN_ROWS` rows.

use std::fmt::Write as _;

/// Rows used by the header, all probes and the bottom row.
pub const MIN_ROWS: u16 = 23;
const SAMPLE_COL: u16 = 26;
const SAMPLE: &str = "THE QUICK BROWN FOX 0123";

struct Screen(String);

impl Screen {
    fn at(&mut self, row: u16, col: u16) -> &mut Self {
        let _ = write!(self.0, "\x1b[{row};{col}H");
        self
    }
    fn s(&mut self, text: &str) -> &mut Self {
        self.0.push_str(text);
        self
    }
    /// `NN LABEL` at column 1, then park at the sample column.
    fn label(&mut self, row: u16, n: u8, label: &str) -> &mut Self {
        let _ = write!(self.0, "\x1b[{row};1H{n:02} {label}");
        self.at(row, SAMPLE_COL)
    }
}

/// Build the probe screen for a `cols`×`rows` terminal (`rows >= MIN_ROWS`).
pub fn card(cols: u16, rows: u16) -> Vec<u8> {
    let mut sc = Screen(String::new());
    sc.s("\x1b[0m\x1b(B\x1b[r\x1b[2J");
    let title = format!(" MUTHUR TESTCARD  {cols}x{rows}  -  REPORT ITEM NUMBERS THAT LOOK WRONG");
    let title: String = format!("{title:<w$}", w = cols as usize).chars().take(cols as usize).collect();
    sc.at(1, 1).s("\x1b[7m").s(&title).s("\x1b[0m");

    let attrs: [(&str, &str); 9] = [
        ("NORMAL", ""),
        ("BOLD", "\x1b[1m"),
        ("UNDERLINE", "\x1b[4m"),
        ("BLINK", "\x1b[5m"),
        ("REVERSE", "\x1b[7m"),
        ("BOLD+REVERSE", "\x1b[1;7m"),
        ("UNDERLINE+REVERSE", "\x1b[4;7m"),
        ("BOLD+UL+BLINK+REV", "\x1b[1;4;5;7m"),
        ("DIM (SGR 2)", "\x1b[2m"),
    ];
    let mut row = 2;
    for (i, (label, sgr)) in attrs.iter().enumerate() {
        sc.label(row, i as u8 + 1, label).s(sgr).s(SAMPLE).s("\x1b[0m");
        row += 1;
    }

    // 10: the whole DEC Special Graphics range.
    let graphics: String = (0x5fu8..=0x7e).map(char::from).collect();
    sc.label(row, 10, "DEC GRAPHICS 5F-7E").s("\x1b(0").s(&graphics).s("\x1b(B");
    row += 1;

    // 11: a box drawn with line-drawing characters.
    sc.label(row, 11, "LINE-DRAWN BOX");
    sc.s("\x1b(0lqqqqqqqqqqqk\x1b(B");
    sc.at(row + 1, SAMPLE_COL).s("\x1b(0x\x1b(B  MU/TH/UR \x1b(0x\x1b(B");
    sc.at(row + 2, SAMPLE_COL).s("\x1b(0mqqqqqqqqqqqj\x1b(B");
    row += 3;

    // 12: double-width line (the line attribute applies to the whole row).
    sc.at(row, 1).s("\x1b#6").s("12 DOUBLE WIDTH LINE");
    row += 1;

    // 13: double-height pair: same text on top and bottom halves.
    sc.at(row, 1).s("\x1b#3").s("13 DOUBLE HEIGHT");
    sc.at(row + 1, 1).s("\x1b#4").s("13 DOUBLE HEIGHT");
    row += 2;

    // 14: scroll region of 3 rows; 5 lines printed, only 3..5 should remain.
    sc.label(row, 14, "SCROLL REGION").s("EXPECT SCROLL 3,4,5 BELOW");
    let top = row + 1;
    let _ = write!(sc.0, "\x1b[{top};{}r", top + 2);
    sc.at(top, SAMPLE_COL).s("SCROLL 1");
    for n in 2..=5 {
        let _ = write!(sc.0, "\r\n\x1b[{}CSCROLL {n}", SAMPLE_COL - 1);
    }
    sc.s("\x1b[r");
    row += 4;

    // 15: write to the last column, then one more char: does it wrap?
    sc.label(row, 15, "LAST COLUMN").s("X AT LAST COL; DOES Y WRAP BELOW?");
    sc.at(row, cols).s("XY");

    let bottom = format!("BOTTOM ROW {rows} - PRESS ANY KEY");
    sc.at(rows, 1).s(&bottom).s("\x1b[0m\x1b(B");
    sc.0.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `ESC [ row ; col H` target in `bytes`.
    fn cup_targets(bytes: &[u8]) -> Vec<(u16, u16)> {
        let s = String::from_utf8_lossy(bytes);
        s.split("\x1b[")
            .skip(1)
            .filter_map(|seg| {
                let end = seg.find(|c: char| !(c.is_ascii_digit() || c == ';'))?;
                if !seg[end..].starts_with('H') {
                    return None;
                }
                let (r, c) = seg[..end].split_once(';')?;
                Some((r.parse().ok()?, c.parse().ok()?))
            })
            .collect()
    }

    #[test]
    fn card_is_seven_bit() {
        assert!(card(80, 43).iter().all(|&b| b < 0x80));
    }

    #[test]
    fn card_probes_every_feature() {
        let s = String::from_utf8(card(80, 43)).unwrap();
        for seq in [
            "\x1b[1m", "\x1b[4m", "\x1b[5m", "\x1b[7m", "\x1b[2m", "\x1b(0", "\x1b(B", "\x1b#6", "\x1b#3",
            "\x1b#4", "\x1b[r",
        ] {
            assert!(s.contains(seq), "missing {seq:?}");
        }
        assert!(s.contains("BOTTOM ROW 43"));
        assert!(s.ends_with("\x1b[0m\x1b(B"));
    }

    #[test]
    fn card_respects_geometry() {
        for (c, r) in [(80, 43), (80, 24)] {
            let targets = cup_targets(&card(c, r));
            assert!(!targets.is_empty());
            for (row, col) in targets {
                assert!(row >= 1 && col >= 1 && row <= r && col <= c, "{row};{col} outside {c}x{r}");
            }
        }
    }
}
