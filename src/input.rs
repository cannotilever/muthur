//! Keyboard input: WY-60 byte sequences → `Key`s.
//!
//! Defaults come from `docs/hardware.md`. A sequence that diverges from every
//! known key is consumed to its terminator and reported as `Key::Unknown`, so
//! a stray ESC never leaks into the prefix state machine.

use anyhow::bail;
use std::collections::BTreeMap;

const ESC: u8 = 0x1b;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    /// Control character other than the named ones, e.g. 0x1d → `Ctrl(']')`.
    Ctrl(char),
    Enter,
    Backspace,
    Tab,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    PageUp,
    PageDown,
    DeleteChar,
    InsertKey,
    F(u8),
    ShiftF(u8),
    Pf(u8),
    Unknown(Vec<u8>),
}

/// Named key → byte sequence table.
#[derive(Clone, Debug)]
pub struct Keymap {
    entries: Vec<(String, Vec<u8>, Key)>,
}

impl Keymap {
    /// Measured on the WY-60 in VT100 personality (docs/hardware.md).
    pub fn wy60_default() -> Self {
        let mut e: Vec<(String, Vec<u8>, Key)> = Vec::new();
        let mut add = |name: &str, seq: &str, key: Key| e.push((name.to_string(), seq.as_bytes().to_vec(), key));
        let f = ["\x1b[?5i", "\x1b[?3i", "\x1b[2i", "\x1b[@", "\x1b[M"];
        let shift_f = ["\x1b[5i", "\x1b[?1i", "\x1b[0i", "\x1b[L", "\x1b[K"];
        let f6_16 = [17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29];
        for (i, s) in f.iter().enumerate() {
            add(&format!("F{}", i + 1), s, Key::F(i as u8 + 1));
        }
        for (i, n) in f6_16.iter().enumerate() {
            add(&format!("F{}", i + 6), &format!("\x1b[{n}~"), Key::F(i as u8 + 6));
        }
        for (i, s) in shift_f.iter().enumerate() {
            add(&format!("SHIFT-F{}", i + 1), s, Key::ShiftF(i as u8 + 1));
        }
        for i in 0..5u8 {
            add(&format!("SHIFT-F{}", i + 6), &format!("\x1b[{}~", 31 + i), Key::ShiftF(i + 6));
        }
        for i in 0..4u8 {
            add(&format!("SHIFT-F{}", i + 11), &format!("\x1b[{}~", 1 + i), Key::ShiftF(i + 11));
        }
        add("PAGEUP", "\x1b[5~", Key::PageUp); // Shift-F15
        add("PAGEDOWN", "\x1b[6~", Key::PageDown); // Shift-F16
        add("UP", "\x1b[A", Key::Up);
        add("DOWN", "\x1b[B", Key::Down);
        add("RIGHT", "\x1b[C", Key::Right);
        add("LEFT", "\x1b[D", Key::Left);
        add("HOME", "\x1b[H", Key::Home);
        // Sent once a program enables application cursor / keypad mode (smkx),
        // which reaches the WY-60 directly in passthrough.
        add("APP-UP", "\x1bOA", Key::Up);
        add("APP-DOWN", "\x1bOB", Key::Down);
        add("APP-RIGHT", "\x1bOC", Key::Right);
        add("APP-LEFT", "\x1bOD", Key::Left);
        add("APP-ENTER", "\x1bOM", Key::Enter);
        for (i, d) in ('0'..='9').enumerate() {
            add(&format!("APP-KP{d}"), &format!("\x1bO{}", (b'p' + i as u8) as char), Key::Char(d));
        }
        add("APP-KPMINUS", "\x1bOm", Key::Char('-'));
        add("APP-KPCOMMA", "\x1bOl", Key::Char(','));
        add("APP-KPDOT", "\x1bOn", Key::Char('.'));
        add("PF1", "\x1bOP", Key::Pf(1));
        add("DELETECHAR", "\x1bOQ", Key::DeleteChar); // Line Del/Char
        add("PF3", "\x1bOR", Key::Pf(3));
        add("INSERT", "\x1bOS", Key::InsertKey);
        Keymap { entries: e }
    }

    /// Replace sequences by key name (`[keys]` in config).
    pub fn with_overrides(mut self, overrides: &BTreeMap<String, String>) -> anyhow::Result<Self> {
        for (name, seq) in overrides {
            let upper = name.to_ascii_uppercase();
            let Some(entry) = self.entries.iter_mut().find(|(n, _, _)| *n == upper) else {
                bail!("unknown key name {name:?} in [keys]");
            };
            entry.1 = seq.as_bytes().to_vec();
        }
        Ok(self)
    }

    fn exact(&self, seq: &[u8]) -> Option<&Key> {
        self.entries.iter().find(|(_, s, _)| s == seq).map(|(_, _, k)| k)
    }

    fn is_strict_prefix(&self, seq: &[u8]) -> bool {
        self.entries.iter().any(|(_, s, _)| s.len() > seq.len() && s.starts_with(seq))
    }
}

fn decode_byte(b: u8) -> Key {
    match b {
        b'\r' => Key::Enter,
        0x08 | 0x7f => Key::Backspace,
        b'\t' => Key::Tab,
        ESC => Key::Esc,
        0x00..=0x1f => Key::Ctrl((b + 0x40).to_ascii_lowercase() as char),
        0x20..=0x7e => Key::Char(b as char),
        _ => Key::Unknown(vec![b]),
    }
}

/// Index of the last byte of the escape sequence at the start of `p`, if complete.
fn sequence_end(p: &[u8]) -> Option<usize> {
    match p.get(1)? {
        b'[' => p.iter().skip(2).position(|b| (0x40..=0x7e).contains(b)).map(|i| i + 2),
        b'O' => (p.len() > 2).then_some(2),
        _ => Some(1),
    }
}

/// Incremental parser. Feed bytes as they arrive; call `timeout` after
/// `KEY_GAP` of silence to resolve a pending lone ESC.
pub struct KeyParser {
    keymap: Keymap,
    pending: Vec<u8>,
}

impl KeyParser {
    pub fn new(keymap: Keymap) -> Self {
        Self { keymap, pending: Vec::new() }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(bytes);
        let mut keys = Vec::new();
        while let Some(&first) = self.pending.first() {
            if first != ESC {
                keys.push(decode_byte(self.pending.remove(0)));
                continue;
            }
            // Longest known sequence that is a prefix of what we have.
            let known = (1..=self.pending.len())
                .rev()
                .find_map(|n| self.keymap.exact(&self.pending[..n]).map(|k| (n, k.clone())));
            if let Some((n, key)) = known {
                self.pending.drain(..n);
                keys.push(key);
                continue;
            }
            if self.keymap.is_strict_prefix(&self.pending) {
                break; // wait for more bytes
            }
            // ESC followed by anything but `[`/`O` is an Esc keypress, then a new key.
            if self.pending.len() >= 2 && !matches!(self.pending[1], b'[' | b'O') {
                self.pending.remove(0);
                keys.push(Key::Esc);
                continue;
            }
            // A control byte can't be inside a sequence: what came before it was
            // a broken sequence, and the control is a key of its own.
            if let Some(i) = self.pending.iter().skip(1).position(|&b| b < 0x20) {
                keys.push(Key::Unknown(self.pending.drain(..=i).collect()));
                continue;
            }
            match sequence_end(&self.pending) {
                Some(end) => keys.push(Key::Unknown(self.pending.drain(..=end).collect())),
                None => break, // incomplete unknown sequence; timeout resolves it
            }
        }
        keys
    }

    pub fn timeout(&mut self) -> Vec<Key> {
        let p = std::mem::take(&mut self.pending);
        match p.as_slice() {
            [] => vec![],
            [ESC] => vec![Key::Esc],
            [ESC, ..] => vec![Key::Unknown(p)],
            _ => p.into_iter().map(decode_byte).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn parser() -> KeyParser {
        KeyParser::new(Keymap::wy60_default())
    }

    #[test]
    fn f1_f2_f3() {
        assert_eq!(parser().feed(b"\x1b[?5i\x1b[?3i\x1b[2i"), vec![Key::F(1), Key::F(2), Key::F(3)]);
    }

    #[test]
    fn page_keys_and_delete() {
        assert_eq!(parser().feed(b"\x1b[5~\x1b[6~\x1bOQ"), vec![Key::PageUp, Key::PageDown, Key::DeleteChar]);
    }

    #[test]
    fn function_key_families() {
        assert_eq!(
            parser().feed(b"\x1b[17~\x1b[29~\x1b[5i\x1b[31~\x1b[1~\x1bOP\x1bOS\x1b[A\x1b[H"),
            vec![Key::F(6), Key::F(16), Key::ShiftF(1), Key::ShiftF(6), Key::ShiftF(11), Key::Pf(1), Key::InsertKey, Key::Up, Key::Home]
        );
    }

    #[test]
    fn split_sequence_within_gap_is_one_key() {
        let mut p = parser();
        assert!(p.feed(b"\x1b[").is_empty());
        assert_eq!(p.feed(b"?5i"), vec![Key::F(1)]);
    }

    #[test]
    fn lone_esc_after_timeout() {
        let mut p = parser();
        assert!(p.feed(b"\x1b").is_empty());
        assert_eq!(p.timeout(), vec![Key::Esc]);
        assert!(p.timeout().is_empty());
    }

    #[test]
    fn unknown_sequence_is_unknown_key() {
        assert_eq!(parser().feed(b"\x1b[99zA"), vec![Key::Unknown(b"\x1b[99z".to_vec()), Key::Char('A')]);
    }

    #[test]
    fn esc_then_char_is_two_keys() {
        assert_eq!(parser().feed(b"\x1bx"), vec![Key::Esc, Key::Char('x')]);
    }

    #[test]
    fn esc_esc_is_two_escapes() {
        let mut p = parser();
        assert_eq!(p.feed(b"\x1b\x1b"), vec![Key::Esc]);
        assert_eq!(p.timeout(), vec![Key::Esc]);
    }

    #[test]
    fn esc_then_prefix_key_split_across_reads() {
        let mut p = parser();
        assert!(p.feed(b"\x1b").is_empty());
        assert_eq!(p.feed(b"\x1d"), vec![Key::Esc, Key::Ctrl(']')]);
    }

    #[test]
    fn control_inside_csi_breaks_it() {
        assert_eq!(parser().feed(b"\x1b[1\x1d"), vec![Key::Unknown(b"\x1b[1".to_vec()), Key::Ctrl(']')]);
    }

    #[test]
    fn application_mode_keys() {
        assert_eq!(
            parser().feed(b"\x1bOA\x1bOD\x1bOM\x1bOp\x1bOy\x1bOm\x1bOl\x1bOn"),
            vec![Key::Up, Key::Left, Key::Enter, Key::Char('0'), Key::Char('9'), Key::Char('-'), Key::Char(','), Key::Char('.')]
        );
    }

    #[test]
    fn controls() {
        assert_eq!(
            parser().feed(b"\x1d\x08\r\x7fq\x03"),
            vec![Key::Ctrl(']'), Key::Backspace, Key::Enter, Key::Backspace, Key::Char('q'), Key::Ctrl('c')]
        );
    }

    #[test]
    fn override_replaces_sequence() {
        let km = Keymap::wy60_default().with_overrides(&BTreeMap::from([("F1".into(), "\x1b[11~".into())])).unwrap();
        let mut p = KeyParser::new(km);
        assert_eq!(p.feed(b"\x1b[11~"), vec![Key::F(1)]);
        assert_eq!(p.feed(b"\x1b[?5i"), vec![Key::Unknown(b"\x1b[?5i".to_vec())]);
    }

    #[test]
    fn override_unknown_name_errors() {
        let err = Keymap::wy60_default().with_overrides(&BTreeMap::from([("F99".into(), "x".into())])).unwrap_err();
        assert!(err.to_string().contains("F99"));
    }
}
