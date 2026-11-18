//! Scroll hints: a tab tells the backend "this band of rows moved", so the
//! backend can scroll the WY-60 with DECSTBM instead of repainting.

use std::cell::Cell;
use std::rc::Rc;

/// Rows `top..bottom` (0-based, exclusive) moved up by `lines` (down if negative).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollHint {
    pub top: u16,
    pub bottom: u16,
    pub lines: i16,
}

/// Shared between a tab (sets it during render) and the backend (takes it in draw).
pub type HintSlot = Rc<Cell<Option<ScrollHint>>>;

fn matches(prev: &[u64], cur: &[u64], k: isize) -> usize {
    (0..cur.len() as isize)
        .filter(|&i| {
            let j = i + k;
            j >= 0 && (j as usize) < prev.len() && cur[i as usize] == prev[j as usize]
        })
        .count()
}

/// The vertical shift between two frames of row hashes, if one explains most rows
/// better than "nothing moved".
pub fn detect_shift(prev: &[u64], cur: &[u64]) -> Option<i16> {
    let n = cur.len().min(prev.len());
    if n < 2 {
        return None;
    }
    let base = matches(prev, cur, 0);
    let mut best: Option<(usize, isize)> = None;
    for k in 1..=(n / 2) as isize {
        for k in [k, -k] {
            let m = matches(prev, cur, k);
            if best.is_none_or(|(bm, _)| m > bm) {
                best = Some((m, k));
            }
        }
    }
    let (m, k) = best?;
    (m >= n / 2 && m > base).then_some(k as i16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_up_shift() {
        assert_eq!(detect_shift(&[1, 2, 3, 4, 5, 6], &[2, 3, 4, 5, 6, 9]), Some(1));
    }

    #[test]
    fn detects_down_shift() {
        assert_eq!(detect_shift(&[1, 2, 3, 4, 5, 6], &[0, 1, 2, 3, 4, 5]), Some(-1));
    }

    #[test]
    fn detects_multi_line_shift() {
        assert_eq!(detect_shift(&[1, 2, 3, 4, 5, 6, 7, 8], &[4, 5, 6, 7, 8, 0, 0, 0]), Some(3));
    }

    #[test]
    fn no_shift_when_unrelated_or_unchanged() {
        assert_eq!(detect_shift(&[1, 2, 3, 4], &[5, 6, 7, 8]), None);
        assert_eq!(detect_shift(&[1, 2, 3, 4], &[1, 2, 3, 4]), None);
    }

    #[test]
    fn appending_below_blank_rows_is_not_a_scroll() {
        // A partly filled screen gains a line: no scroll, even though blanks match at k=1.
        assert_eq!(detect_shift(&[1, 2, 0, 0, 0, 0], &[1, 2, 3, 0, 0, 0]), None);
    }
}
