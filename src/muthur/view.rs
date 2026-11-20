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
    /// Direct mode: the operator's line, drawn `$ <text>`.
    Command(String),
    /// Direct mode: a harness-authored RUN / REVIEW / EXIT line. Consecutive
    /// ones sit together, and long ones wrap with a 5-column indent.
    Harness(String),
}

/// Continuation indent of a wrapped harness line.
const HANGING: usize = 5;

/// Word wrap to `width` columns, breaking at spaces (spacing inside a line is
/// kept). Words longer than `width` are hard-broken, '\n' forces a break and
/// empty lines are preserved.
pub fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    text.split('\n').flat_map(|para| wrap_para(para, width, width)).collect()
}

/// `wrap`, with every line after the first indented by `indent` columns.
pub fn wrap_hanging(text: &str, width: u16, indent: usize) -> Vec<String> {
    let width = usize::from(width.max(1));
    let rest = width.saturating_sub(indent).max(1);
    let pad = " ".repeat(width - rest);
    let mut out = Vec::new();
    for (i, para) in text.split('\n').enumerate() {
        let first = if i == 0 { width } else { rest };
        for (j, line) in wrap_para(para, first, rest).into_iter().enumerate() {
            out.push(if i == 0 && j == 0 { line } else { format!("{pad}{line}") });
        }
    }
    out
}

/// One paragraph: the first line at most `first` columns, the others `rest`.
fn wrap_para(para: &str, first: usize, rest_width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut width = first;
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
        width = rest_width;
    }
    out.push(rest.into_iter().collect());
    out
}

/// All transcript lines at `width`: banner, then each entry after a blank line
/// (a harness line right after another has none), then the reply being
/// revealed (with the block cursor) if any.
pub fn transcript_lines(entries: &[Entry], revealing: Option<&str>, width: u16) -> Vec<String> {
    let mut lines = vec![BANNER.to_string()];
    let mut prev_harness = false;
    for e in entries {
        let harness = matches!(e, Entry::Harness(_));
        if !(harness && prev_harness) {
            lines.push(String::new());
        }
        prev_harness = harness;
        let text = match e {
            Entry::User(s) => format!("> {s}"),
            Entry::Command(s) => format!("$ {s}"),
            Entry::Reply(s) | Entry::Note(s) => s.clone(),
            Entry::Harness(s) => {
                lines.extend(wrap_hanging(&ascii_safe(s), width, HANGING));
                continue;
            }
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
