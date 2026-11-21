//! Streaming markdown cleaner and ASCII-safe transliteration for model output.

use crate::dialect::{Glyph, map_glyph};

const LINK_MAX: usize = 200;
const HASH_MAX: usize = 7;

/// Strips markdown markers from a streamed reply, one chunk at a time.
pub struct Cleaner {
    /// Characters buffered at the start of a line until the prefix is decided.
    line_start: Option<String>,
    link: Option<String>,
    pending_underscore: bool,
    in_fence: bool,
    /// Dropping a fence line up to its newline.
    dropping: bool,
}

impl Default for Cleaner {
    fn default() -> Self {
        Self::new()
    }
}

impl Cleaner {
    pub fn new() -> Self {
        Self {
            line_start: Some(String::new()),
            link: None,
            pending_underscore: false,
            in_fence: false,
            dropping: false,
        }
    }

    pub fn push(&mut self, chunk: &str) -> String {
        let mut out = String::new();
        for c in chunk.chars() {
            self.step(c, &mut out);
        }
        out
    }

    pub fn finish(&mut self) -> String {
        let mut out = String::new();
        self.flush_line(&mut out);
        self.dropping = false;
        out
    }

    fn step(&mut self, c: char, out: &mut String) {
        if self.dropping {
            if c == '\n' {
                self.dropping = false;
                self.line_start = Some(String::new());
            }
            return;
        }
        if c == '\n' {
            self.flush_line(out);
            out.push('\n');
            self.line_start = Some(String::new());
            return;
        }
        if let Some(mut buf) = self.line_start.take() {
            buf.push(c);
            self.decide(buf, out);
            return;
        }
        self.body(c, out);
    }

    /// Resolve everything buffered on the current line (no newline emitted).
    fn flush_line(&mut self, out: &mut String) {
        if let Some(buf) = self.line_start.take() {
            self.body_str(&buf, out);
        }
        if let Some(l) = self.link.take() {
            out.push_str(&l);
        }
        if std::mem::take(&mut self.pending_underscore) {
            out.push('_');
        }
    }

    fn body_str(&mut self, s: &str, out: &mut String) {
        for ch in s.chars() {
            self.body(ch, out);
        }
    }

    fn body(&mut self, c: char, out: &mut String) {
        if self.in_fence {
            out.push(c);
            return;
        }
        self.inline(c, out);
    }

    fn decide(&mut self, buf: String, out: &mut String) {
        if buf == "```" {
            self.in_fence = !self.in_fence;
            self.dropping = true;
            return;
        }
        if buf.len() < 3 && "```".starts_with(buf.as_str()) {
            self.line_start = Some(buf);
            return;
        }
        if self.in_fence {
            out.push_str(&buf);
            return;
        }
        let hashes = buf.chars().all(|c| c == '#');
        if hashes && buf.len() < HASH_MAX {
            self.line_start = Some(buf);
            return;
        }
        if let Some(h) = buf.strip_suffix(' ')
            && !h.is_empty()
            && h.chars().all(|c| c == '#')
        {
            return;
        }
        if matches!(buf.as_str(), "*" | "+" | "-") {
            self.line_start = Some(buf);
            return;
        }
        if matches!(buf.as_str(), "* " | "+ " | "- ") {
            out.push_str("- ");
            return;
        }
        self.body_str(&buf, out);
    }

    fn inline(&mut self, c: char, out: &mut String) {
        if let Some(mut l) = self.link.take() {
            l.push(c);
            if let Some(i) = l.find(']') {
                match l[i + 1..].chars().next() {
                    None => {}
                    Some('(') => {
                        if c == ')' {
                            let text = &l[1..i];
                            let url = &l[i + 2..l.len() - 1];
                            out.push_str(&format!("{text} ({url})"));
                            return;
                        }
                    }
                    Some(_) => {
                        out.push_str(&l);
                        return;
                    }
                }
            }
            if l.chars().count() >= LINK_MAX {
                out.push_str(&l);
            } else {
                self.link = Some(l);
            }
            return;
        }
        if c == '_' {
            if std::mem::take(&mut self.pending_underscore) {
                return; // "__" dropped
            }
            self.pending_underscore = true;
            return;
        }
        if std::mem::take(&mut self.pending_underscore) {
            out.push('_');
        }
        match c {
            '*' | '`' => {}
            '[' => self.link = Some("[".to_string()),
            _ => out.push(c),
        }
    }
}

/// Keep printable ASCII, newline and DEC graphics glyphs; transliterate the rest.
/// `\r` is dropped and `\t` becomes four spaces.
pub fn ascii_safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\r' => continue,
            '\t' => {
                out.push_str("    ");
                continue;
            }
            _ => {}
        }
        if c == '\n' || (' '..='~').contains(&c) {
            out.push(c);
            continue;
        }
        if matches!(map_glyph(&c.to_string()), Glyph::Graphics(_)) {
            out.push(c);
            continue;
        }
        let t: String = deunicode::deunicode_char(c)
            .unwrap_or("")
            .chars()
            .filter(|ch| (' '..='~').contains(ch))
            .collect();
        if t.is_empty() {
            out.push('?');
        } else {
            out.push_str(&t);
        }
    }
    out
}

/// Cleaner(all) -> uppercase (if `upper`) -> ascii_safe. Used for history redisplay.
pub fn display_text(raw: &str, upper: bool) -> String {
    let mut c = Cleaner::new();
    let mut s = c.push(raw);
    s += &c.finish();
    if upper {
        s = s.to_uppercase();
    }
    ascii_safe(&s)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_markdown() {
        assert_eq!(
            display_text("## Status\n**All** `systems` nominal.\n* one\n```\ncode\n```\nsee [docs](http://x)\n", false),
            "Status\nAll systems nominal.\n- one\ncode\nsee docs (http://x)\n"
        );
    }
    #[test]
    fn markers_split_across_chunks() {
        let src = "## Head\n**bo";
        let rest = "ld** [l](u) x_y __z__\n";
        for cut in 0..src.len() {
            let mut c = Cleaner::new();
            let mut out = c.push(&src[..cut]);
            out += &c.push(&src[cut..]);
            out += &c.push(rest);
            out += &c.finish();
            assert_eq!(out, "Head\nbold l (u) x_y z\n", "cut {cut}");
        }
    }
    #[test]
    fn unterminated_link_is_flushed() {
        assert_eq!(display_text("a [b c", false), "a [b c");
    }
    #[test]
    fn transliterates_punctuation_and_emoji() {
        assert_eq!(ascii_safe("A \u{2014} \u{201c}B\u{201d} \u{2026}"), "A -- \"B\" ...");
        let s = ascii_safe("\u{1f680} \u{65e5}");
        assert!(s.chars().all(|c| c == '\n' || (' '..='~').contains(&c)));
    }
    #[test]
    fn tabs_expand_and_cr_dropped() {
        assert_eq!(ascii_safe("a\tb\r\nc\r"), "a    b\nc");
    }
    #[test]
    fn uppercases() {
        assert_eq!(display_text("ok", true), "OK");
    }
}
