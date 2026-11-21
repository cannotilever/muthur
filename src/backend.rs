//! `Wy60Backend`: the only path to the wire.
//!
//! Keeps two full grids: `want` (what ratatui asked for, built from its diffs)
//! and `shown` (what the terminal displays). Each frame emits exactly the cells
//! where they differ, so the output is correct whatever happened before. A
//! scroll hint lets a band of lines be moved with DECSTBM + LF/RI first; the
//! compare then repairs anything the full-width scroll disturbed, so a wrong
//! hint costs bytes, never correctness.
//!
//! Line attributes (DECDHL) are kept the same way, per row: a row whose
//! attribute changes is re-sent whole, and a double row only sends its
//! left half (it holds `cols / 2` double-width cells).
//!
//! Cursor, attributes and charset are tracked so each is only emitted on
//! change. After the last column the cursor is unknown (VT100 pending wrap).

use crate::dialect::{Attrs, Charset, Dialect, Glyph, LineAttr, map_glyph};
use crate::scroll::{HintSlot, ScrollHint};
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

/// Use cursor-forward instead of an absolute move for gaps up to this size.
const MAX_CUF: u16 = 4;

/// A cell as it appears on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct CellState {
    glyph: Glyph,
    attrs: Attrs,
}

const BLANK: CellState = CellState {
    glyph: Glyph::Ascii(b' '),
    attrs: Attrs {
        bold: false,
        dim: false,
        underline: false,
        blink: false,
        reverse: false,
    },
};

pub struct Wy60Backend<W: Write, D: Dialect> {
    writer: W,
    dialect: D,
    cols: u16,
    rows: u16,
    out: Vec<u8>,
    want: Vec<CellState>,
    /// `None` = unknown (always re-sent).
    shown: Vec<Option<CellState>>,
    hints: Option<HintSlot>,
    want_lines: Vec<LineAttr>,
    /// `None` = unknown (always re-sent).
    shown_lines: Vec<Option<LineAttr>>,
    /// (row, col); `None` = unknown.
    cursor: Option<(u16, u16)>,
    attrs: Option<Attrs>,
    charset: Option<Charset>,
}

impl<W: Write, D: Dialect> Wy60Backend<W, D> {
    /// Assumes the screen starts blank (callers clear it first).
    pub fn new(writer: W, dialect: D, cols: u16, rows: u16) -> Self {
        let n = cols as usize * rows as usize;
        Self {
            writer,
            dialect,
            cols,
            rows,
            out: Vec::new(),
            want: vec![BLANK; n],
            shown: vec![Some(BLANK); n],
            hints: None,
            want_lines: vec![LineAttr::Single; rows as usize],
            shown_lines: vec![Some(LineAttr::Single); rows as usize],
            cursor: None,
            attrs: None,
            charset: None,
        }
    }

    pub fn with_hints(mut self, slot: HintSlot) -> Self {
        self.hints = Some(slot);
        self
    }

    /// Line attributes for the next frame: the listed rows, every other row
    /// single. Cells past `cols / 2` on a double row are not shown.
    pub fn set_line_attrs(&mut self, rows: &[(u16, LineAttr)]) {
        self.want_lines.fill(LineAttr::Single);
        for &(row, a) in rows {
            if let Some(l) = self.want_lines.get_mut(row as usize) {
                *l = a;
            }
        }
    }

    /// Columns a row can show under its current line attribute.
    fn row_width(&self, row: u16) -> u16 {
        match self.want_lines[row as usize] {
            LineAttr::Single => self.cols,
            LineAttr::DoubleTop | LineAttr::DoubleBottom => self.cols / 2,
        }
    }

    pub fn writer(&self) -> &W {
        &self.writer
    }

    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    fn idx(&self, row: u16, col: u16) -> usize {
        row as usize * self.cols as usize + col as usize
    }

    fn move_to(&mut self, row: u16, col: u16) {
        match self.cursor {
            Some(c) if c == (row, col) => {}
            Some((r, c)) if r == row && col > c && col - c <= MAX_CUF => self.dialect.cuf(&mut self.out, col - c),
            _ => self.dialect.cup(&mut self.out, row, col),
        }
        self.cursor = Some((row, col));
    }

    fn set_attrs(&mut self, a: Attrs) {
        if self.attrs != Some(a) {
            self.dialect.sgr(&mut self.out, a);
            self.attrs = Some(a);
        }
    }

    fn set_charset(&mut self, c: Charset) {
        if self.charset != Some(c) {
            self.dialect.charset(&mut self.out, c);
            self.charset = Some(c);
        }
    }

    /// Record a ratatui cell in `want`. Wide graphemes blank the cells they cover.
    fn apply(&mut self, col: u16, row: u16, cell: &Cell) {
        let i = self.idx(row, col);
        self.want[i] = CellState {
            glyph: map_glyph(cell.symbol()),
            attrs: Attrs::from(cell.modifier),
        };
        let width = (cell.symbol().width() as u16).max(1);
        for extra in 1..width {
            if col + extra < self.cols {
                let j = self.idx(row, col + extra);
                self.want[j] = BLANK;
            }
        }
    }

    fn put(&mut self, row: u16, col: u16, width: u16, st: CellState) {
        let mut attrs = st.attrs;
        let (charset, byte) = match st.glyph {
            Glyph::Ascii(b) => (Charset::Ascii, b),
            Glyph::Graphics(b) => (Charset::Graphics, b),
            Glyph::ReverseSpace => {
                attrs.reverse = !attrs.reverse;
                (Charset::Ascii, b' ')
            }
        };
        self.move_to(row, col);
        self.set_attrs(attrs);
        self.set_charset(charset);
        self.out.push(byte);
        self.cursor = if col + 1 >= width { None } else { Some((row, col + 1)) };
    }

    /// Scroll a band of full-width lines on the terminal and in `shown`.
    fn scroll(&mut self, h: ScrollHint) {
        let bottom = h.bottom.min(self.rows);
        if h.top + 1 >= bottom {
            return;
        }
        let height = (bottom - h.top) as i16;
        let k = h.lines.clamp(-height, height);
        if k == 0 {
            return;
        }
        // New lines must come in blank, not in the current attributes.
        self.set_attrs(Attrs::default());
        self.dialect.set_scroll_region(&mut self.out, h.top, bottom - 1);
        if k > 0 {
            self.dialect.cup(&mut self.out, bottom - 1, 0);
            for _ in 0..k {
                self.dialect.index(&mut self.out);
            }
        } else {
            self.dialect.cup(&mut self.out, h.top, 0);
            for _ in 0..-k {
                self.dialect.reverse_index(&mut self.out);
            }
        }
        self.dialect.reset_scroll_region(&mut self.out);
        self.cursor = Some((0, 0)); // DECSTBM homes the cursor.

        let cols = self.cols as usize;
        let band: Vec<Option<CellState>> = self.shown[h.top as usize * cols..bottom as usize * cols].to_vec();
        let rows = (bottom - h.top) as isize;
        for r in 0..rows {
            let src = r + k as isize;
            for c in 0..cols {
                let v = if (0..rows).contains(&src) {
                    band[src as usize * cols + c]
                } else {
                    Some(BLANK)
                };
                self.shown[(h.top as usize + r as usize) * cols + c] = v;
            }
        }
        // Line attributes travel with their lines; new lines come in single.
        let lines: Vec<Option<LineAttr>> = self.shown_lines[h.top as usize..bottom as usize].to_vec();
        for r in 0..rows {
            let src = r + k as isize;
            self.shown_lines[h.top as usize + r as usize] = if (0..rows).contains(&src) {
                lines[src as usize]
            } else {
                Some(LineAttr::Single)
            };
        }
    }
}

impl<W: Write, D: Dialect> Backend for Wy60Backend<W, D> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        for (x, y, cell) in content {
            // ratatui's diff already omits skipped cells.
            if x < self.cols && y < self.rows {
                self.apply(x, y, cell);
            }
        }
        if let Some(h) = self.hints.as_ref().and_then(|s| s.take()) {
            self.scroll(h);
        }
        for row in 0..self.rows {
            let line = self.want_lines[row as usize];
            if self.shown_lines[row as usize] != Some(line) {
                // The row's characters change size: repaint all of it.
                self.dialect.cup(&mut self.out, row, 0);
                self.dialect.line_attr(&mut self.out, line);
                self.cursor = Some((row, 0));
                self.shown_lines[row as usize] = Some(line);
                let start = self.idx(row, 0);
                self.shown[start..start + self.cols as usize].fill(None);
            }
            let width = self.row_width(row);
            for col in 0..self.cols {
                let i = self.idx(row, col);
                let want = self.want[i];
                if self.shown[i] != Some(want) {
                    // Past the right margin of a double row there is nothing to
                    // write to; a single row repaints it after the attribute change.
                    if col < width {
                        self.put(row, col, width, want);
                    }
                    self.shown[i] = Some(want);
                }
            }
        }
        // Leave a known state for anything that writes outside the backend.
        self.set_attrs(Attrs::default());
        self.set_charset(Charset::Ascii);
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        Ok(()) // The VT100 subset has no DECTCEM.
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        let (row, col) = self.cursor.unwrap_or((0, 0));
        Ok(Position { x: col, y: row })
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let p = position.into();
        if self.cursor != Some((p.y, p.x)) {
            self.dialect.cup(&mut self.out, p.y, p.x);
            self.cursor = Some((p.y, p.x));
        }
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        self.dialect.clear(&mut self.out);
        self.want.fill(BLANK);
        self.shown.fill(Some(BLANK));
        // ED 2 should make every line single; re-assert any that weren't.
        for l in &mut self.shown_lines {
            if *l != Some(LineAttr::Single) {
                *l = None;
            }
        }
        self.cursor = Some((0, 0));
        self.attrs = Some(Attrs::default());
        self.charset = Some(Charset::Ascii);
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        let seq: &[u8] = match clear_type {
            ClearType::All => return self.clear(),
            ClearType::AfterCursor => b"\x1b[J",
            ClearType::BeforeCursor => b"\x1b[1J",
            ClearType::CurrentLine => b"\x1b[2K",
            ClearType::UntilNewLine => b"\x1b[K",
        };
        self.out.extend_from_slice(seq);
        // Don't model partial clears: forget what is shown so it is all re-sent.
        self.shown.fill(None);
        self.shown_lines.fill(None);
        Ok(())
    }

    fn size(&self) -> io::Result<Size> {
        Ok(Size::new(self.cols, self.rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: Size::new(self.cols, self.rows),
            pixels: Size::new(0, 0),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.out.is_empty() {
            self.writer.write_all(&self.out)?;
            self.out.clear();
        }
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Vt100Dialect;
    use crate::scroll::{HintSlot, ScrollHint};
    use crate::strict::StrictWriter;
    use ratatui::Terminal;
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;
    use ratatui::widgets::{Block, Paragraph};
    use std::num::NonZeroU16;

    type T = Terminal<Wy60Backend<Vec<u8>, Vt100Dialect>>;

    fn term() -> T {
        Terminal::new(Wy60Backend::new(Vec::new(), Vt100Dialect, 80, 43)).unwrap()
    }

    fn take(t: &mut T) -> Vec<u8> {
        std::mem::take(t.backend_mut().writer_mut())
    }

    fn screen(bytes: &[u8]) -> vt100::Parser {
        let mut p = vt100::Parser::new(NonZeroU16::new(43).unwrap(), NonZeroU16::new(80).unwrap(), 0);
        p.process(bytes);
        p
    }

    fn text_at(p: &vt100::Parser, row: u16, col: u16, len: u16) -> String {
        (col..col + len)
            .map(|c| match p.screen().cell(row, c).unwrap().contents() {
                "" => " ".to_string(), // never written = blank on a real terminal
                s => s.to_string(),
            })
            .collect()
    }

    fn put(t: &mut T, x: u16, y: u16, text: &str, style: Style) {
        t.draw(|f| f.render_widget(Paragraph::new(Span::styled(text.to_string(), style)), Rect::new(x, y, text.len() as u16, 1)))
            .unwrap();
    }

    #[test]
    fn round_trip_text_and_attrs() {
        let mut t = term();
        t.draw(|f| {
            f.render_widget(
                Paragraph::new(Span::styled("HELLO", Style::new().add_modifier(Modifier::BOLD))),
                Rect::new(5, 2, 5, 1),
            );
            f.render_widget(
                Paragraph::new(Span::styled("X", Style::new().add_modifier(Modifier::REVERSED))),
                Rect::new(79, 0, 1, 1),
            );
        })
        .unwrap();
        let p = screen(&take(&mut t));
        assert_eq!(text_at(&p, 2, 5, 5), "HELLO");
        assert!(p.screen().cell(2, 5).unwrap().bold());
        assert!(!p.screen().cell(2, 4).unwrap().bold());
        assert_eq!(text_at(&p, 0, 79, 1), "X");
        assert!(p.screen().cell(0, 79).unwrap().inverse());
    }

    #[test]
    fn borders_become_dec_graphics() {
        let mut t = term();
        t.draw(|f| f.render_widget(Block::bordered(), Rect::new(0, 0, 4, 3))).unwrap();
        let bytes = take(&mut t);
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(s.contains("\x1b(0") && s.contains("lqqk"), "{s:?}");
        assert_eq!(text_at(&screen(&bytes), 0, 0, 4), "lqqk");
    }

    #[test]
    fn second_identical_frame_emits_nothing() {
        let mut t = term();
        put(&mut t, 3, 3, "SAME", Style::new());
        take(&mut t);
        put(&mut t, 3, 3, "SAME", Style::new());
        assert!(take(&mut t).is_empty());
    }

    #[test]
    fn adjacent_cells_need_no_cursor_moves() {
        let mut t = term();
        put(&mut t, 10, 7, "ABCDEF", Style::new());
        let s = String::from_utf8(take(&mut t)).unwrap();
        assert_eq!(s.matches('H').count(), 1, "{s:?}");
    }

    #[test]
    fn small_gap_uses_cursor_forward() {
        let mut t = term();
        t.draw(|f| {
            f.render_widget(Paragraph::new("AB"), Rect::new(0, 5, 2, 1));
            f.render_widget(Paragraph::new("CD"), Rect::new(4, 5, 2, 1));
        })
        .unwrap();
        let bytes = take(&mut t);
        assert!(String::from_utf8_lossy(&bytes).contains("AB\x1b[2CCD"));
        assert_eq!(text_at(&screen(&bytes), 5, 4, 2), "CD");
    }

    #[test]
    fn bottom_right_cell_does_not_scroll() {
        let mut t = term();
        t.draw(|f| {
            f.render_widget(Paragraph::new("Z"), Rect::new(79, 42, 1, 1));
            f.render_widget(Paragraph::new("T"), Rect::new(0, 0, 1, 1));
        })
        .unwrap();
        // A second frame writes after Z, which must not have scrolled the screen.
        t.draw(|f| {
            f.render_widget(Paragraph::new("Z"), Rect::new(79, 42, 1, 1));
            f.render_widget(Paragraph::new("U"), Rect::new(0, 0, 1, 1));
        })
        .unwrap();
        let p = screen(&take(&mut t));
        assert_eq!(text_at(&p, 0, 0, 1), "U");
        assert_eq!(text_at(&p, 42, 79, 1), "Z");
    }

    #[test]
    fn non_ascii_becomes_question_mark() {
        let mut t = term();
        put(&mut t, 0, 0, "\u{E000}", Style::new());
        let bytes = take(&mut t);
        assert!(bytes.contains(&b'?'));
        assert!(bytes.iter().all(|&b| b < 0x80));
    }

    #[test]
    fn block_glyph_is_reverse_space() {
        let mut t = term();
        put(&mut t, 1, 1, "█", Style::new());
        let p = screen(&take(&mut t));
        let c = p.screen().cell(1, 1).unwrap();
        assert!(c.inverse());
        assert_eq!(c.contents(), " ");
    }

    #[test]
    fn frame_ends_with_attrs_and_charset_reset() {
        let mut t = term();
        t.draw(|f| f.render_widget(Block::bordered().style(Style::new().add_modifier(Modifier::BOLD)), Rect::new(0, 0, 3, 3)))
            .unwrap();
        let s = String::from_utf8(take(&mut t)).unwrap();
        assert!(s.ends_with("\x1b[0m\x1b(B") || s.ends_with("\x1b(B\x1b[0m"), "{s:?}");
    }

    #[test]
    fn wide_char_clears_its_second_cell() {
        let mut t = term();
        put(&mut t, 0, 0, "ABCD", Style::new());
        t.draw(|f| f.render_widget(Paragraph::new("中CD"), Rect::new(0, 0, 4, 1))).unwrap();
        let p = screen(&take(&mut t));
        assert_eq!(text_at(&p, 0, 0, 4), "Z CD"); // 中 transliterates to "Zhong"
    }

    fn rows_frame(t: &mut T, rows: &[String], hint: Option<(&HintSlot, ScrollHint)>) -> Vec<u8> {
        if let Some((slot, h)) = hint {
            slot.set(Some(h));
        }
        t.draw(|f| {
            for (i, r) in rows.iter().enumerate() {
                f.render_widget(Paragraph::new(r.as_str()), Rect::new(0, i as u16, 80, 1));
            }
        })
        .unwrap();
        take(t)
    }

    fn hinted_term() -> (T, HintSlot) {
        let slot = HintSlot::default();
        (
            Terminal::new(Wy60Backend::new(Vec::new(), Vt100Dialect, 80, 43).with_hints(slot.clone())).unwrap(),
            slot,
        )
    }

    fn line(n: usize) -> String {
        format!("LINE {n:03} THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG {n:03}")
    }

    fn screen_rows(p: &vt100::Parser, rows: u16) -> Vec<String> {
        (0..rows).map(|r| text_at(p, r, 0, 80).trim_end().to_string()).collect()
    }

    #[test]
    fn scroll_hint_beats_repaint() {
        let (mut t, slot) = hinted_term();
        let a: Vec<String> = (0..40).map(line).collect();
        let b: Vec<String> = (1..41).map(line).collect();
        let mut all = rows_frame(&mut t, &a, None);
        let second = rows_frame(&mut t, &b, Some((&slot, ScrollHint { top: 0, bottom: 40, lines: 1 })));
        assert!(second.len() < 200, "{} bytes", second.len());
        all.extend(&second);
        assert_eq!(screen_rows(&screen(&all), 40), b);
    }

    #[test]
    fn scroll_hint_repairs_static_columns() {
        let (mut t, slot) = hinted_term();
        let frame = |start: usize| -> Vec<String> {
            (0..20)
                .map(|i| {
                    if i < 2 || i % 5 == 0 {
                        format!("SIDE{i:02}| {}", line(start + i))
                    } else {
                        format!("      | {}", line(start + i))
                    }
                })
                .collect()
        };
        let a = frame(0);
        let b: Vec<String> = frame(0)
            .iter()
            .zip(frame(1))
            .enumerate()
            .map(|(i, (old, new))| if i < 2 { old.clone() } else { format!("{}{}", &new[..8], &new[8..]) })
            .collect();
        let mut all = rows_frame(&mut t, &a, None);
        all.extend(rows_frame(&mut t, &b, Some((&slot, ScrollHint { top: 2, bottom: 20, lines: 1 }))));
        assert_eq!(screen_rows(&screen(&all), 20), b);
    }

    #[test]
    fn wrong_hint_is_still_correct() {
        let (mut t, slot) = hinted_term();
        let a: Vec<String> = (0..30).map(line).collect();
        let mut all = rows_frame(&mut t, &a, None);
        all.extend(rows_frame(&mut t, &a, Some((&slot, ScrollHint { top: 0, bottom: 30, lines: 3 }))));
        assert_eq!(screen_rows(&screen(&all), 30), a);
    }

    #[test]
    fn reverse_scroll() {
        let (mut t, slot) = hinted_term();
        let a: Vec<String> = (1..31).map(line).collect();
        let b: Vec<String> = (0..30).map(line).collect();
        let mut all = rows_frame(&mut t, &a, None);
        let second = rows_frame(&mut t, &b, Some((&slot, ScrollHint { top: 0, bottom: 30, lines: -1 })));
        assert!(String::from_utf8_lossy(&second).contains("\x1bM"));
        assert!(second.len() < 200, "{} bytes", second.len());
        all.extend(&second);
        assert_eq!(screen_rows(&screen(&all), 30), b);
    }

    #[test]
    fn output_after_scroll_passes_strict() {
        let slot = HintSlot::default();
        let w = StrictWriter::new(Vec::new(), Vt100Dialect);
        let mut t = Terminal::new(Wy60Backend::new(w, Vt100Dialect, 80, 43).with_hints(slot.clone())).unwrap();
        for start in 0..3 {
            if start > 0 {
                slot.set(Some(ScrollHint { top: 0, bottom: 10, lines: 1 }));
            }
            t.draw(|f| {
                for i in 0..10u16 {
                    f.render_widget(Paragraph::new(line(start + i as usize)), Rect::new(0, i, 80, 1));
                }
            })
            .unwrap();
        }
        assert!(t.backend().writer().violations.is_empty(), "{:?}", t.backend().writer().violations);
    }

    #[test]
    fn cursor_position_is_not_reemitted() {
        let mut t = term();
        t.backend_mut().set_cursor_position(Position { x: 5, y: 5 }).unwrap();
        t.backend_mut().set_cursor_position(Position { x: 5, y: 5 }).unwrap();
        t.backend_mut().flush().unwrap();
        assert_eq!(String::from_utf8(take(&mut t)).unwrap(), "\x1b[6;6H");
    }

    #[test]
    fn output_passes_strict_mode() {
        let w = StrictWriter::new(Vec::new(), Vt100Dialect);
        let mut t = Terminal::new(Wy60Backend::new(w, Vt100Dialect, 80, 43)).unwrap();
        t.clear().unwrap();
        t.draw(|f| {
            f.render_widget(Block::bordered().title("TÏTLE █"), Rect::new(0, 0, 20, 5));
            f.render_widget(
                Paragraph::new(Span::styled("ATTRS", Style::new().add_modifier(Modifier::all()))),
                Rect::new(2, 2, 5, 1),
            );
        })
        .unwrap();
        assert!(t.backend().writer().violations.is_empty(), "{:?}", t.backend().writer().violations);
    }

    #[test]
    fn double_height_rows_send_decdhl_and_only_the_left_half() {
        let mut t = term();
        take(&mut t);
        t.backend_mut().set_line_attrs(&[(5, LineAttr::DoubleTop), (6, LineAttr::DoubleBottom)]);
        t.draw(|f| {
            for y in [5, 6] {
                f.render_widget(Paragraph::new("HI"), Rect::new(10, y, 2, 1));
                f.render_widget(Paragraph::new("Z"), Rect::new(60, y, 1, 1));
            }
        })
        .unwrap();
        let out = String::from_utf8(take(&mut t)).unwrap();
        assert!(out.contains("\x1b[6;1H\x1b#3"), "{out:?}");
        assert!(out.contains("\x1b[7;1H\x1b#4"), "{out:?}");
        assert_eq!(out.matches("HI").count(), 2);
        assert!(!out.contains('Z'), "cells past the half are not sent: {out:?}");

        // Unchanged: nothing more to send.
        t.backend_mut().set_line_attrs(&[(5, LineAttr::DoubleTop), (6, LineAttr::DoubleBottom)]);
        t.draw(|f| {
            for y in [5, 6] {
                f.render_widget(Paragraph::new("HI"), Rect::new(10, y, 2, 1));
                f.render_widget(Paragraph::new("Z"), Rect::new(60, y, 1, 1));
            }
        })
        .unwrap();
        let out = String::from_utf8(take(&mut t)).unwrap();
        assert!(!out.contains("HI") && !out.contains('#'), "{out:?}");

        // Back to single: ESC#5 and the whole row again, right half included.
        t.backend_mut().set_line_attrs(&[]);
        t.draw(|f| {
            for y in [5, 6] {
                f.render_widget(Paragraph::new("HI"), Rect::new(10, y, 2, 1));
                f.render_widget(Paragraph::new("Z"), Rect::new(60, y, 1, 1));
            }
        })
        .unwrap();
        let out = String::from_utf8(take(&mut t)).unwrap();
        assert_eq!(out.matches("\x1b#5").count(), 2, "{out:?}");
        assert_eq!(out.matches("HI").count(), 2);
        assert_eq!(out.matches('Z').count(), 2);
    }

    #[test]
    fn clear_reasserts_single_on_double_rows() {
        let mut t = term();
        t.backend_mut().set_line_attrs(&[(3, LineAttr::DoubleTop), (4, LineAttr::DoubleBottom)]);
        t.draw(|_| {}).unwrap();
        t.clear().unwrap();
        take(&mut t);
        t.backend_mut().set_line_attrs(&[]);
        t.draw(|_| {}).unwrap();
        let out = String::from_utf8(take(&mut t)).unwrap();
        assert!(out.contains("\x1b[4;1H\x1b#5") && out.contains("\x1b[5;1H\x1b#5"), "{out:?}");
        assert_eq!(out.matches("\x1b#5").count(), 2, "{out:?}");
    }
}
