//! `Wy60Backend`: the only path to the wire. Turns ratatui's cell diff into
//! bytes of a `Dialect`.
//!
//! Tracks cursor, attributes and charset so each is only emitted on change.
//! After writing the last column the cursor becomes unknown (VT100 pending
//! wrap), so the next cell always gets an absolute move.

use crate::dialect::{Attrs, Charset, Dialect, Glyph, map_glyph};
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

/// Use cursor-forward instead of an absolute move for gaps up to this size.
const MAX_CUF: u16 = 4;

pub struct Wy60Backend<W: Write, D: Dialect> {
    writer: W,
    dialect: D,
    cols: u16,
    rows: u16,
    out: Vec<u8>,
    /// (row, col); `None` = unknown.
    cursor: Option<(u16, u16)>,
    attrs: Option<Attrs>,
    charset: Option<Charset>,
}

impl<W: Write, D: Dialect> Wy60Backend<W, D> {
    pub fn new(writer: W, dialect: D, cols: u16, rows: u16) -> Self {
        Self { writer, dialect, cols, rows, out: Vec::new(), cursor: None, attrs: None, charset: None }
    }

    pub fn writer(&self) -> &W {
        &self.writer
    }

    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
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

    fn put(&mut self, row: u16, col: u16, cell: &Cell) {
        let mut attrs = Attrs::from(cell.modifier);
        let (charset, byte) = match map_glyph(cell.symbol()) {
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
        // A wide grapheme covers extra cells that ratatui's diff never sends:
        // blank them so nothing stale shows through.
        let width = (cell.symbol().width() as u16).max(1);
        for extra in 1..width {
            if col + extra < self.cols {
                self.out.push(b' ');
            }
        }
        self.cursor = if col + width >= self.cols { None } else { Some((row, col + width)) };
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
                self.put(y, x, cell);
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
        self.dialect.cup(&mut self.out, p.y, p.x);
        self.cursor = Some((p.y, p.x));
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        self.dialect.clear(&mut self.out);
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
        Ok(())
    }

    fn size(&self) -> io::Result<Size> {
        Ok(Size::new(self.cols, self.rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize { columns_rows: Size::new(self.cols, self.rows), pixels: Size::new(0, 0) })
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
        (col..col + len).map(|c| p.screen().cell(row, c).unwrap().contents().to_string()).collect()
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
        put(&mut t, 0, 0, "é", Style::new());
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
        assert_eq!(text_at(&p, 0, 0, 4), "? CD");
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
}
