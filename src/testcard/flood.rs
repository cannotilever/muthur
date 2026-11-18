//! Flow-control flood pattern: a dropped byte visibly shifts a line's alphabet.

/// `lines` numbered lines of `cols - 1` chars, then a completion marker.
pub fn flood(cols: u16, lines: u32) -> Vec<u8> {
    let width = cols.saturating_sub(1) as usize;
    let mut out = String::with_capacity((width + 2) * lines as usize + 32);
    for n in 1..=lines {
        let prefix = format!("{n:05} ");
        out.push_str(&prefix);
        let start = n as usize % 26;
        out.extend((0..width.saturating_sub(prefix.len())).map(|i| (b'A' + ((start + i) % 26) as u8) as char));
        out.push_str("\r\n");
    }
    out.push_str(&format!("FLOOD COMPLETE {lines} LINES\r\n"));
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flood_lines_are_distinct_and_fit() {
        let s = String::from_utf8(flood(80, 100)).unwrap();
        let lines: Vec<_> = s.split("\r\n").collect();
        assert!(lines[..100].iter().all(|l| l.len() == 79));
        assert_ne!(lines[0][6..], lines[1][6..]);
        assert!(s.contains("FLOOD COMPLETE 100 LINES"));
    }
}
