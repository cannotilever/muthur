//! MU/TH/UR tab drawing: word wrap, transcript lines and frame helpers.

use super::text::ascii_safe;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

pub const BANNER: &str = "INTERFACE 2037 READY.";
pub const CURSOR: char = '█';

/// One transcript item. Text is already display-ready (cleaned, cased, ASCII).
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    User(String),
    Reply(String),
    Note(String),
}

/// Word wrap to `width` columns, breaking at spaces (spacing inside a line is
/// kept). Words longer than `width` are hard-broken, '\n' forces a break and
/// empty lines are preserved.
pub fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut rest: Vec<char> = para.chars().collect();
        while rest.len() > width {
            let space = rest[..=width].iter().rposition(|c| *c == ' ');
            let line: String = match space {
                Some(i) => rest[..i].iter().collect::<String>().trim_end().to_string(),
                None => String::new(),
            };
            let cut = match space {
                Some(i) if !line.is_empty() => i,
                _ => width,
            };
            out.push(if line.is_empty() { rest[..cut].iter().collect() } else { line });
            let skip = rest[cut..].iter().take_while(|c| **c == ' ').count();
            rest.drain(..cut + skip);
        }
        out.push(rest.into_iter().collect());
    }
    out
}

/// All transcript lines at `width`: banner, then each entry after a blank line,
/// then the reply being revealed (with the block cursor) if any.
pub fn transcript_lines(entries: &[Entry], revealing: Option<&str>, width: u16) -> Vec<String> {
    let mut lines = vec![BANNER.to_string()];
    for e in entries {
        lines.push(String::new());
        let text = match e {
            Entry::User(s) => format!("> {s}"),
            Entry::Reply(s) | Entry::Note(s) => s.clone(),
        };
        lines.extend(wrap(&ascii_safe(&text), width));
    }
    if let Some(v) = revealing {
        lines.push(String::new());
        lines.extend(wrap(&format!("{v}{CURSOR}"), width));
    }
    lines
}

/// `├───┤` over the frame's row `y`.
pub fn separator(buf: &mut Buffer, area: Rect, y: u16) {
    if area.width < 2 {
        return;
    }
    let right = area.x + area.width - 1;
    for x in area.x..=right {
        let sym = match x {
            _ if x == area.x => "├",
            _ if x == right => "┤",
            _ => "─",
        };
        buf[(x, y)].set_symbol(sym);
    }
}

/// Write `text` at (x, y), clipped to `width` columns.
pub fn put(buf: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    if width > 0 {
        buf.set_stringn(x, y, text, usize::from(width), style);
    }
}
