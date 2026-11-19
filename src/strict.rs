//! `--strict`: check every outgoing byte against the dialect's allowlist.
//!
//! Bytes always pass through unchanged — strict mode reports, it never filters.
//! Set `MUTHUR_STRICT_PANIC=1` to panic on the first violation instead.

use crate::dialect::Dialect;
use std::io::{self, Write};

pub struct StrictWriter<W, D> {
    pub inner: W,
    dialect: D,
    /// An escape sequence still being received.
    pending: Vec<u8>,
    pub violations: Vec<Vec<u8>>,
    panic: bool,
}

impl<W: Write, D: Dialect> StrictWriter<W, D> {
    pub fn new(inner: W, dialect: D) -> Self {
        let panic = std::env::var("MUTHUR_STRICT_PANIC").is_ok_and(|v| v == "1");
        Self { inner, dialect, pending: Vec::new(), violations: Vec::new(), panic }
    }

    fn violation(&mut self, seq: Vec<u8>) {
        tracing::error!(seq = ?String::from_utf8_lossy(&seq), "strict: disallowed output");
        if self.panic {
            panic!("strict: disallowed output {seq:?}");
        }
        self.violations.push(seq);
    }

    fn check(&mut self, b: u8) {
        if !self.pending.is_empty() {
            self.pending.push(b);
            let complete = match (self.pending.len(), self.pending[1]) {
                (2, b'[' | b'(' | b'#') => false,
                (2, _) => true,
                (3, b'(' | b'#') => true,
                (_, b'[') => (0x40..=0x7e).contains(&b),
                _ => true,
            };
            if complete {
                let seq = std::mem::take(&mut self.pending);
                if !self.dialect.is_allowed(&seq) {
                    self.violation(seq);
                }
            }
            return;
        }
        match b {
            0x1b => self.pending.push(b),
            // BEL: the splash rings at READY.
            b'\r' | b'\n' | 0x07 | 0x08 | 0x0f | 0x20..=0x7e => {}
            _ => self.violation(vec![b]),
        }
    }
}

impl<W: Write, D: Dialect> Write for StrictWriter<W, D> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        for &b in &buf[..n] {
            self.check(b);
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Vt100Dialect;
    use std::io::Write;

    #[test]
    fn passes_allowed_and_records_violations() {
        let mut w = StrictWriter::new(Vec::new(), Vt100Dialect);
        w.write_all(b"\x1b[1;1HOK\r\n\x1b[?25l\xc3\xa9").unwrap();
        assert_eq!(w.inner, b"\x1b[1;1HOK\r\n\x1b[?25l\xc3\xa9");
        assert_eq!(w.violations, vec![b"\x1b[?25l".to_vec(), vec![0xc3], vec![0xa9]]);
    }

    #[test]
    fn sequence_split_across_writes() {
        let mut w = StrictWriter::new(Vec::new(), Vt100Dialect);
        w.write_all(b"\x1b[1").unwrap();
        w.write_all(b";1H").unwrap();
        assert!(w.violations.is_empty());
    }

    #[test]
    fn clear_sequence_is_allowed() {
        let mut w = StrictWriter::new(Vec::new(), Vt100Dialect);
        let mut o = vec![];
        crate::dialect::Dialect::clear(&Vt100Dialect, &mut o);
        w.write_all(&o).unwrap();
        assert!(w.violations.is_empty(), "{:?}", w.violations);
    }

    #[test]
    fn stray_controls_are_violations() {
        let mut w = StrictWriter::new(Vec::new(), Vt100Dialect);
        w.write_all(b"\x05\x1b]0;x").unwrap();
        assert_eq!(w.violations, vec![vec![0x05], b"\x1b]".to_vec()]);
    }

    #[test]
    fn bell_is_allowed() {
        let mut w = StrictWriter::new(Vec::new(), Vt100Dialect);
        w.write_all(b"\x07").unwrap();
        assert!(w.violations.is_empty(), "{:?}", w.violations);
    }
}
