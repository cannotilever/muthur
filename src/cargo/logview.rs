//! The log view: a container's last lines, cleaned for the wire, wrapped to
//! the pane and scrolled from the bottom.

use crate::input::Key;
use crate::scroll::{HintSlot, ScrollHint};
use crate::utility::widgets::panel;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

const HINT: &str = "L REFRESH  ESC BACK  SH-F15/16 SCROLL";
const TAB: usize = 8;
/// Most cleaned log text kept (the newest), so a container that writes huge
/// lines cannot make wrapping and redraws expensive.
pub(super) const MAX_TEXT: usize = 64 * 1024;

/// Log text made safe for the wire: escape sequences (CSI — 7- or 8-bit —
/// OSC up to its terminator or the end of the line, and any other ESC
/// sequence) removed, `\r\n` → `\n`, lone `\r` dropped, tabs expanded to
/// the next multiple of 8, other C0/C1 controls dropped. An ESC directly
/// before a control character is dropped alone.
/// Non-ASCII is left for the backend, which maps what it cannot draw.
pub(super) fn clean(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut col = 0;
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            // CSI, 8-bit form.
            '\u{9b}' => skip_csi(&mut it),
            '\x1b' => match it.peek().copied() {
                Some('[') => {
                    it.next();
                    skip_csi(&mut it);
                }
                // OSC: up to BEL or ST (ESC \), and never past the line end.
                Some(']') => {
                    it.next();
                    while let Some(c) = it.next_if(|&c| c != '\n') {
                        if c == '\x07' || (c == '\x1b' && it.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                // Intermediates 0x20..0x2F, then one final byte.
                Some(c) if (' '..='/').contains(&c) => {
                    while it.next_if(|c| (' '..='/').contains(c)).is_some() {}
                    it.next_if(|c| !c.is_control());
                }
                // A stray ESC before a newline or tab: keep that character.
                Some(c) if c.is_control() => {}
                Some(_) => {
                    it.next();
                }
                None => {}
            },
            '\n' => {
                out.push('\n');
                col = 0;
            }
            '\t' => {
                let n = TAB - col % TAB;
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            c if c.is_control() => {} // includes \r, BEL, DEL and C1
            c => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

/// CSI parameters and intermediates (0x20..0x3F), then its final byte
/// (@..~). Stops at anything else, which is then handled as ordinary text.
fn skip_csi(it: &mut std::iter::Peekable<std::str::Chars>) {
    while it.next_if(|c| (' '..='?').contains(c)).is_some() {}
    it.next_if(|c| ('@'..='~').contains(c));
}

/// `text` as rows of at most `width` characters; a trailing newline does not
/// add an empty row.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            rows.push(String::new());
        }
        rows.extend(chars.chunks(width).map(|c| c.iter().collect::<String>()));
    }
    rows
}

pub(super) struct LogView {
    pub(super) id: String,
    name: String,
    /// Cleaned, unwrapped.
    text: String,
    lines: usize,
    /// 0 = at the bottom (newest).
    scroll_from_bottom: usize,
    /// From the last render: text rows shown, and rows after wrapping.
    rows: usize,
    wrapped: usize,
    /// `text` wrapped at a width, kept until the width changes.
    wrap_cache: Option<(usize, Vec<String>)>,
    /// The last frame's text area and its first row: what a scroll hint
    /// compares against.
    last: Option<(Rect, usize)>,
}

impl LogView {
    pub(super) fn new(id: String, name: String, raw: &str) -> Self {
        let mut text = clean(raw);
        if text.len() > MAX_TEXT {
            // Keep the newest, starting on a line boundary.
            let mut cut = text.len() - MAX_TEXT;
            while !text.is_char_boundary(cut) {
                cut += 1;
            }
            let start = text[cut..].find('\n').map_or(text.len(), |i| cut + i + 1);
            text.drain(..start);
        }
        let lines = text.lines().count();
        Self { id, name, text, lines, scroll_from_bottom: 0, rows: 1, wrapped: 0, wrap_cache: None, last: None }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn key(&mut self, key: &Key) {
        let top = self.wrapped.saturating_sub(self.rows);
        self.scroll_from_bottom = match key {
            Key::Up => self.scroll_from_bottom + 1,
            Key::Down => self.scroll_from_bottom.saturating_sub(1),
            Key::PageUp => self.scroll_from_bottom + self.rows,
            Key::PageDown => self.scroll_from_bottom.saturating_sub(self.rows),
            Key::Home => top,
            _ => return,
        }
        .min(top);
    }

    /// Draws the view; when only the scroll position changed by less than a
    /// page, leaves a hint so the backend scrolls the band instead of
    /// repainting it.
    pub(super) fn render(&mut self, area: Rect, buf: &mut Buffer, hints: &HintSlot) {
        let block = panel(&format!("LOGS - {} ({} LINES)", self.name, self.lines));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height < 2 {
            return;
        }
        let w = inner.width as usize;
        let bottom = inner.y + inner.height - 1;
        buf.set_stringn(inner.x, bottom, HINT, w, Style::new());
        if self.wrap_cache.as_ref().is_none_or(|(cw, _)| *cw != w) {
            self.wrap_cache = Some((w, wrap(&self.text, w)));
            self.last = None;
        }
        let Some((_, rows)) = &self.wrap_cache else { return };
        self.rows = (inner.height - 1) as usize;
        self.wrapped = rows.len();
        if rows.is_empty() {
            buf.set_stringn(inner.x, inner.y, "NO LOG OUTPUT", w, Style::new());
            return;
        }
        self.scroll_from_bottom = self.scroll_from_bottom.min(rows.len().saturating_sub(self.rows));
        let end = rows.len() - self.scroll_from_bottom;
        let start = end.saturating_sub(self.rows);
        for (i, row) in rows[start..end].iter().enumerate() {
            buf.set_stringn(inner.x, inner.y + i as u16, row, w, Style::new());
        }
        let text_area = Rect { height: self.rows as u16, ..inner };
        if let Some((prev_area, prev_start)) = self.last
            && prev_area == text_area
        {
            let lines = start as isize - prev_start as isize;
            if lines != 0 && lines.unsigned_abs() < self.rows {
                hints.set(Some(ScrollHint { top: text_area.y, bottom: text_area.y + text_area.height, lines: lines as i16 }));
            }
        }
        self.last = Some((text_area, start));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_ansi_cr_tabs_controls() {
        assert_eq!(clean("\x1b[31mred\x1b[0m\r\nA\tB\nbar\r50%\x07\x1b]0;title\x07end\n"), "red\nA       B\nbar50%end\n");
        assert_eq!(clean("\x1b]8;;http://x\x1b\\link\x1b(Bok\u{9b}\u{7f}é"), "linkoké");
    }

    #[test]
    fn clean_edge_cases() {
        assert_eq!(clean("a\x1b]0;unterminated\nb\n"), "a\nb\n", "OSC stops at the line end");
        assert_eq!(clean("\u{9b}31mred\u{9b}0m"), "red", "8-bit CSI");
        assert_eq!(clean("a\x1b\nb"), "a\nb", "ESC before a newline keeps it");
        assert_eq!(clean("a\x1b\tb"), "a       b", "ESC before a tab keeps it");
    }

    #[test]
    fn huge_logs_keep_the_newest_lines() {
        let raw: String = (0..2000).map(|i| format!("{i:04} {}\n", "z".repeat(95))).collect();
        let v = LogView::new("1".into(), "WEB".into(), &raw);
        assert!(v.text.len() <= MAX_TEXT, "{}", v.text.len());
        assert!(v.text.starts_with(|c: char| c.is_ascii_digit()), "starts on a line boundary");
        assert!(v.text.ends_with(&format!("1999 {}\n", "z".repeat(95))));
        assert_eq!(v.lines, v.text.lines().count());
    }

    #[test]
    fn wrap_never_exceeds_width() {
        let w = wrap(&"x".repeat(130), 59);
        assert_eq!(w.iter().map(|l| l.chars().count()).collect::<Vec<_>>(), vec![59, 59, 12]);
        assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
        assert_eq!(wrap("a\n", 10), vec!["a"]);
        assert!(wrap("", 10).is_empty());
    }
}
