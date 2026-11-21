//! Time-injected teletype reveal buffer.

use std::time::Instant;

pub struct Reveal {
    text: String,
    /// Characters shown.
    shown: usize,
    /// Characters in `text`.
    total: usize,
    /// Byte offset of `shown` characters.
    byte: usize,
    cps: u32,
    last: Option<Instant>,
    credit: f64,
    skipping: bool,
}

impl Reveal {
    pub fn new(cps: u32) -> Self {
        Self {
            text: String::new(),
            shown: 0,
            total: 0,
            byte: 0,
            cps,
            last: None,
            credit: 0.0,
            skipping: false,
        }
    }

    pub fn push(&mut self, s: &str, now: Instant) {
        if self.caught_up() {
            self.last = Some(now);
            self.credit = 0.0;
        }
        self.text.push_str(s);
        self.total += s.chars().count();
        if self.skipping || self.cps == 0 {
            self.show_all();
        }
    }

    pub fn advance(&mut self, now: Instant) -> bool {
        if self.skipping || self.cps == 0 {
            let changed = self.shown != self.total;
            self.show_all();
            return changed;
        }
        let last = self.last.unwrap_or(now);
        self.last = Some(now);
        let avail = self.total - self.shown;
        if avail == 0 {
            self.credit = 0.0;
            return false;
        }
        let elapsed = now.saturating_duration_since(last).as_secs_f64();
        let budget = elapsed * f64::from(self.cps) + self.credit;
        let n = (budget.floor() as usize).min(avail);
        self.credit = if n == avail { 0.0 } else { budget - n as f64 };
        if n == 0 {
            return false;
        }
        let rest = &self.text[self.byte..];
        let adv = rest.char_indices().nth(n).map_or(rest.len(), |(i, _)| i);
        self.byte += adv;
        self.shown += n;
        true
    }

    pub fn skip(&mut self) {
        self.skipping = true;
        self.show_all();
    }

    pub fn reset(&mut self) {
        self.text.clear();
        self.shown = 0;
        self.total = 0;
        self.byte = 0;
        self.last = None;
        self.credit = 0.0;
        self.skipping = false;
    }

    pub fn visible(&self) -> &str {
        &self.text[..self.byte]
    }

    pub fn caught_up(&self) -> bool {
        self.shown == self.total
    }

    fn show_all(&mut self) {
        self.shown = self.total;
        self.byte = self.text.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn reveal_paces_at_cps() {
        let t0 = Instant::now();
        let mut r = Reveal::new(50);
        r.push("0123456789", t0);
        r.advance(t0 + Duration::from_millis(100));
        assert_eq!(r.visible(), "01234");
        r.advance(t0 + Duration::from_millis(130));
        assert_eq!(r.visible(), "012345");
    }
    #[test]
    fn reveal_does_not_burst_after_stall() {
        let t0 = Instant::now();
        let mut r = Reveal::new(50);
        r.push("AB", t0);
        r.advance(t0 + Duration::from_secs(1));
        r.push("CDEFG", t0 + Duration::from_secs(5));
        r.advance(t0 + Duration::from_millis(5020));
        assert_eq!(r.visible(), "ABC");
    }
    #[test]
    fn skip_shows_all_and_later_pushes() {
        let t0 = Instant::now();
        let mut r = Reveal::new(50);
        r.push("ABC", t0);
        r.skip();
        assert_eq!(r.visible(), "ABC");
        r.push("D", t0);
        assert_eq!(r.visible(), "ABCD");
        r.reset();
        r.push("E", t0);
        assert_eq!(r.visible(), "");
    }
}
