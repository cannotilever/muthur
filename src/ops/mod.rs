//! Shared operations plumbing: command runner, audit log, output truncation.
pub mod audit;
pub mod runner;

use serde::{Deserialize, Serialize};

pub const CAP: usize = 4096;

/// Truncate to at most CAP bytes on a char boundary, marking the cut.
pub fn cap(text: String) -> String {
    if text.len() <= CAP {
        return text;
    }
    let mut end = CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[TRUNCATED]", &text[..end])
}

/// Keep the first and last `limit / 2` bytes (on char boundaries), noting how many were dropped.
pub fn head_tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let half = limit / 2;
    let mut h = half;
    while !text.is_char_boundary(h) {
        h -= 1;
    }
    let mut t = text.len() - half;
    while !text.is_char_boundary(t) {
        t += 1;
    }
    format!("{}{}{}", &text[..h], omitted(t - h), &text[t..])
}

/// The cut marker `head_tail` puts between head and tail.
pub fn omitted(n: usize) -> String {
    format!("\n[… {n} BYTES OMITTED …]\n")
}

/// Which agent issued an operation.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentId {
    #[default]
    Muthur,
    Tech,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_tail_short_is_unchanged() {
        assert_eq!(head_tail("abc", 8), "abc");
    }

    #[test]
    fn head_tail_keeps_ends_and_counts() {
        let s = format!("{}{}{}", "a".repeat(10), "m".repeat(100), "z".repeat(10));
        let t = head_tail(&s, 20);
        assert!(t.starts_with("aaaaaaaaaa") && t.ends_with("zzzzzzzzzz"));
        assert!(t.contains("[… 100 BYTES OMITTED …]"), "{t}");
    }

    #[test]
    fn head_tail_cuts_on_char_boundaries() {
        let t = head_tail(&"€".repeat(100), 21);
        assert!(std::str::from_utf8(t.as_bytes()).is_ok());
    }
    #[test]
    fn cap_truncates_on_char_boundary() {
        let c = cap("é".repeat(3000));
        assert!(c.ends_with("\n[TRUNCATED]"));
        assert!(c.len() <= CAP + 12);
        assert_eq!(cap("short".into()), "short");
    }

    #[test]
    fn cap_cuts_mid_character() {
        let c = cap(format!("aa{}", "€".repeat(2000)));
        assert!(c.ends_with("\n[TRUNCATED]"));
        assert!(c.len() < 4096 + 12 && c.len() > 4000);
        assert!(c.starts_with("aa€"));
    }
}
