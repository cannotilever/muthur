//! Key echo: records the raw bytes each WY-60 key sends.

use crate::link::ReadTimeout;
use std::io::{self, Write};
use std::time::Duration;

/// Bytes arriving within this gap of each other belong to one key.
/// ~190 byte-times at 38400 baud: generous for one escape sequence.
pub const KEY_GAP: Duration = Duration::from_millis(50);
const CTRL_RBRACKET: u8 = 0x1d;

/// Human-readable form of a key's bytes, e.g. `ESC O P  (1b 4f 50)`.
pub fn describe(bytes: &[u8]) -> String {
    let tokens: Vec<String> = bytes
        .iter()
        .map(|&b| match b {
            0x1b => "ESC".into(),
            0x7f => "DEL".into(),
            0x20 => "SP".into(),
            0x00..=0x1f => format!("^{}", (b + 0x40) as char),
            0x21..=0x7e => (b as char).to_string(),
            _ => format!("\\x{b:02x}"),
        })
        .collect();
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}  ({})", tokens.join(" "), hex.join(" "))
}

/// Block until a key arrives, then gather bytes until `gap` passes with no more.
pub fn read_key(r: &mut impl ReadTimeout, gap: Duration) -> io::Result<Vec<u8>> {
    let mut buf = [0u8; 64];
    let mut key = Vec::new();
    while key.is_empty() {
        let n = r.read_timeout(&mut buf, Duration::from_secs(3600))?;
        key.extend_from_slice(&buf[..n]);
    }
    loop {
        let n = r.read_timeout(&mut buf, gap)?;
        if n == 0 {
            return Ok(key);
        }
        key.extend_from_slice(&buf[..n]);
    }
}

/// Echo each key's description to the terminal and to `log` until `^] ^]`.
pub fn echo_loop(link: &mut (impl ReadTimeout + Write), log: &mut impl Write) -> io::Result<()> {
    link.write_all(b"\x1b[0m\x1b[2J\x1b[HMUTHUR KEY ECHO - PRESS EACH KEY ONCE.  ^] ^] TO EXIT.\r\n\r\n")?;
    link.flush()?;
    let mut prev_was_exit = false;
    loop {
        let key = read_key(link, KEY_GAP)?;
        let d = describe(&key);
        link.write_all(format!("{d}\r\n").as_bytes())?;
        link.flush()?;
        writeln!(log, "{d}")?;
        let is_exit = key == [CTRL_RBRACKET];
        if is_exit && prev_was_exit {
            return log.flush();
        }
        prev_was_exit = is_exit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::ReadTimeout;
    use std::collections::VecDeque;
    use std::io;
    use std::time::Duration;

    /// Scripted reader: `Some(bytes)` is returned by one read, `None` is a timeout.
    struct Scripted(VecDeque<Option<Vec<u8>>>);
    impl Scripted {
        fn new(v: Vec<Option<Vec<u8>>>) -> Self {
            Self(v.into())
        }
    }
    impl ReadTimeout for Scripted {
        fn read_timeout(&mut self, buf: &mut [u8], _t: Duration) -> io::Result<usize> {
            match self.0.pop_front() {
                Some(Some(b)) => {
                    buf[..b.len()].copy_from_slice(&b);
                    Ok(b.len())
                }
                Some(None) => Ok(0),
                None => Err(io::ErrorKind::UnexpectedEof.into()),
            }
        }
    }

    struct ScriptedIo(Scripted, Vec<u8>);
    impl ScriptedIo {
        fn new(v: Vec<Option<Vec<u8>>>) -> Self {
            Self(Scripted::new(v), Vec::new())
        }
    }
    impl ReadTimeout for ScriptedIo {
        fn read_timeout(&mut self, buf: &mut [u8], t: Duration) -> io::Result<usize> {
            self.0.read_timeout(buf, t)
        }
    }
    impl io::Write for ScriptedIo {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.1.write(b)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn describes_pf1() {
        assert_eq!(describe(&[0x1b, b'O', b'P']), "ESC O P  (1b 4f 50)");
    }

    #[test]
    fn describes_controls_and_high() {
        assert_eq!(describe(&[0x1d, 0x7f, 0x9b]), "^] DEL \\x9b  (1d 7f 9b)");
        assert_eq!(describe(b" "), "SP  (20)");
    }

    #[test]
    fn groups_burst_into_one_key() {
        let mut r = Scripted::new(vec![Some(b"\x1b[A".to_vec()), None]);
        assert_eq!(read_key(&mut r, Duration::from_millis(50)).unwrap(), b"\x1b[A");
    }

    #[test]
    fn groups_split_sequence_into_one_key() {
        let mut r = Scripted::new(vec![Some(vec![0x1b]), Some(b"[A".to_vec()), None]);
        assert_eq!(read_key(&mut r, Duration::from_millis(50)).unwrap(), b"\x1b[A");
    }

    #[test]
    fn lone_escape_is_its_own_key() {
        let mut r = Scripted::new(vec![Some(vec![0x1b]), None, Some(b"a".to_vec()), None]);
        assert_eq!(read_key(&mut r, Duration::from_millis(50)).unwrap(), vec![0x1b]);
        assert_eq!(read_key(&mut r, Duration::from_millis(50)).unwrap(), b"a");
    }

    #[test]
    fn double_ctrl_bracket_exits_and_logs() {
        let mut io = ScriptedIo::new(vec![
            Some(b"x".to_vec()),
            None,
            Some(vec![0x1d]),
            None,
            Some(vec![0x1d]),
            None,
        ]);
        let mut log = Vec::new();
        echo_loop(&mut io, &mut log).unwrap();
        assert_eq!(String::from_utf8(log).unwrap(), "x  (78)\n^]  (1d)\n^]  (1d)\n");
        assert!(String::from_utf8_lossy(&io.1).contains("^]  (1d)\r\n"));
    }
}
