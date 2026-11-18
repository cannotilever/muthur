//! WY-60 keys → bytes for a program that believes TERM=vt100 (xterm codes for
//! the keys vt100 terminfo lacks).

use crate::input::Key;

/// Bytes to send for `key`, or `None` if the shell never sees it (F1–F3 switch
/// tabs; shifted F-keys and unknown sequences have no meaning to it).
/// `app_cursor` is DECCKM, which the running program may have switched on.
pub fn encode_key(key: &Key, app_cursor: bool) -> Option<Vec<u8>> {
    let arrow = |c: u8| if app_cursor { vec![0x1b, b'O', c] } else { vec![0x1b, b'[', c] };
    let tilde = |n: u8| format!("\x1b[{n}~").into_bytes();
    Some(match key {
        Key::Char(c) => c.to_string().into_bytes(),
        Key::Ctrl(c) => vec![(*c as u8) & 0x1f],
        Key::Enter => b"\r".to_vec(),
        Key::Backspace => vec![0x08],
        Key::Tab => b"\t".to_vec(),
        Key::Esc => vec![0x1b],
        Key::Up => arrow(b'A'),
        Key::Down => arrow(b'B'),
        Key::Right => arrow(b'C'),
        Key::Left => arrow(b'D'),
        Key::Home => b"\x1b[H".to_vec(),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::DeleteChar => tilde(3),
        Key::InsertKey => tilde(2),
        Key::Pf(n @ 1..=4) => vec![0x1b, b'O', b'P' + n - 1],
        Key::F(4) => b"\x1bOS".to_vec(),
        Key::F(n @ 5..=16) => tilde([15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29][*n as usize - 5]),
        Key::F(_) | Key::Pf(_) | Key::ShiftF(_) | Key::Unknown(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Key;

    #[test]
    fn encodes_every_key() {
        let cases: Vec<(Key, Option<&[u8]>)> = vec![
            (Key::Char('a'), Some(b"a")),
            (Key::Ctrl(']'), Some(b"\x1d")),
            (Key::Ctrl('c'), Some(b"\x03")),
            (Key::Enter, Some(b"\r")),
            (Key::Backspace, Some(b"\x08")),
            (Key::Tab, Some(b"\t")),
            (Key::Esc, Some(b"\x1b")),
            (Key::Up, Some(b"\x1b[A")),
            (Key::Down, Some(b"\x1b[B")),
            (Key::Right, Some(b"\x1b[C")),
            (Key::Left, Some(b"\x1b[D")),
            (Key::Home, Some(b"\x1b[H")),
            (Key::PageUp, Some(b"\x1b[5~")),
            (Key::PageDown, Some(b"\x1b[6~")),
            (Key::DeleteChar, Some(b"\x1b[3~")),
            (Key::InsertKey, Some(b"\x1b[2~")),
            (Key::Pf(1), Some(b"\x1bOP")),
            (Key::Pf(3), Some(b"\x1bOR")),
            (Key::F(4), Some(b"\x1bOS")),
            (Key::F(5), Some(b"\x1b[15~")),
            (Key::F(6), Some(b"\x1b[17~")),
            (Key::F(16), Some(b"\x1b[29~")),
            (Key::F(1), None),
            (Key::F(3), None),
            (Key::ShiftF(2), None),
            (Key::Unknown(b"\x1b[99z".to_vec()), None),
        ];
        for (key, want) in cases {
            assert_eq!(encode_key(&key, false).as_deref(), want, "{key:?}");
        }
    }

    #[test]
    fn application_cursor_mode() {
        assert_eq!(encode_key(&Key::Up, true).as_deref(), Some(&b"\x1bOA"[..]));
        assert_eq!(encode_key(&Key::Left, true).as_deref(), Some(&b"\x1bOD"[..]));
    }
}
