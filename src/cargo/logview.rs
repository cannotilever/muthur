//! The log view: a container's last lines, cleaned for the wire, wrapped to
//! the pane and scrolled from the bottom.

use crate::input::Key;
use crate::utility::widgets::panel;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

const HINT: &str = "L REFRESH  ESC BACK  SH-F15/16 SCROLL";
const TAB: usize = 8;

/// Log text made safe for the wire: escape sequences (CSI, OSC, and any
/// other ESC sequence) removed, `\r\n` → `\n`, lone `\r` dropped, tabs
/// expanded to the next multiple of 8, other C0/C1 controls dropped.
/// Non-ASCII is left for the backend, which maps what it cannot draw.
pub(super) fn clean(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut col = 0;
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => match it.next() {
                // CSI: parameters and intermediates up to a final byte @..~.
                Some('[') => {
                    for c in it.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST (ESC \).
                Some(']') => {
                    while let Some(c) = it.next() {
                        if c == '\x07' || (c == '\x1b' && it.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                // Anything else: intermediates 0x20..0x2F, then one final byte.
                Some(c) if (' '..='/').contains(&c) => {
                    while it.next_if(|c| (' '..='/').contains(c)).is_some() {}
                    it.next();
                }
                _ => {}
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
}

impl LogView {
    pub(super) fn new(id: String, name: String, raw: &str) -> Self {
        let text = clean(raw);
        let lines = text.lines().count();
        Self { id, name, text, lines, scroll_from_bottom: 0, rows: 1, wrapped: 0 }
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

    pub(super) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let block = panel(&format!("LOGS - {} ({} LINES)", self.name, self.lines));
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.height < 2 {
            return;
        }
        let w = inner.width as usize;
        let bottom = inner.y + inner.height - 1;
        buf.set_stringn(inner.x, bottom, HINT, w, Style::new());
        let rows = wrap(&self.text, w);
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
    fn wrap_never_exceeds_width() {
        let w = wrap(&"x".repeat(130), 59);
        assert_eq!(w.iter().map(|l| l.chars().count()).collect::<Vec<_>>(), vec![59, 59, 12]);
        assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
        assert_eq!(wrap("a\n", 10), vec!["a"]);
        assert!(wrap("", 10).is_empty());
    }
}
