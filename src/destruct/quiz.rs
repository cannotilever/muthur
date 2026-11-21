//! The self-destruct abort questionnaire: a pure state machine. Every
//! submitted answer counts as an abort to the caller; the answers only decide
//! what the strip says next.

use std::time::{Duration, Instant};

use crate::input::Key;

const RETORT: Duration = Duration::from_millis(1500);
const MAX_INPUT: usize = 30;
const FALLBACK_SEED: u64 = 0x9E37_79B9_7F4A_7C15;
const SPECIAL_ORDER: [&str; 3] = ["CREW EXPENDABLE.", "PRIORITY ONE.", "ALL OTHER CONSIDERATIONS SECONDARY."];

pub struct Quiz {
    rng: u64,
    step: usize,
    problem: (i64, i64, i64),
    order_count: usize,
    input: String,
    retort: Option<(String, Instant)>,
}

impl Quiz {
    pub fn new(seed: u64) -> Quiz {
        let mut q = Quiz {
            rng: if seed == 0 { FALLBACK_SEED } else { seed },
            step: 1,
            problem: (0, 0, 0),
            order_count: 0,
            input: String::new(),
            retort: None,
        };
        q.problem = q.new_problem();
        q
    }

    fn next(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    fn new_problem(&mut self) -> (i64, i64, i64) {
        loop {
            let a = 11 + (self.next() % 19) as i64;
            let b = 11 + (self.next() % 19) as i64;
            let c = 1 + (self.next() % 9) as i64;
            if (a, b, c) != self.problem {
                return (a, b, c);
            }
        }
    }

    pub fn step(&self) -> usize {
        self.step
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn question(&self, now: Instant) -> String {
        if let Some((text, until)) = &self.retort
            && now < *until
        {
            return text.clone();
        }
        match self.step {
            1 => "TO ABORT, TYPE ABORT".into(),
            2 => "CONFIRM: TYPE troba".into(),
            3 => {
                let (a, b, c) = self.problem;
                format!("SOLVE: {a} x {b} - {c} =")
            }
            4 => "WHAT IS THE AIRSPEED VELOCITY OF AN UNLADEN SWALLOW?".into(),
            5 => "ENTER SPECIAL ORDER 937 AUTHORIZATION:".into(),
            6 => "ACTIVATE OMEGA 13? Y/N".into(),
            _ => "PLEASE HOLD. YOUR ABORT IS IMPORTANT TO US.".into(),
        }
    }

    /// True when this key submitted an answer (non-empty Enter).
    pub fn key(&mut self, key: &Key, now: Instant) -> bool {
        match key {
            Key::Char(c) => {
                if self.input.chars().count() < MAX_INPUT {
                    self.input.push(*c);
                }
                false
            }
            Key::Backspace => {
                self.input.pop();
                false
            }
            Key::Ctrl('c') => {
                self.input.clear();
                false
            }
            Key::Enter => {
                let answer = std::mem::take(&mut self.input);
                let answer = answer.trim();
                if answer.is_empty() {
                    return false;
                }
                self.submit(answer, now);
                true
            }
            _ => false,
        }
    }

    fn show(&mut self, text: &str, now: Instant) {
        self.retort = Some((text.to_string(), now + RETORT));
    }

    fn submit(&mut self, answer: &str, now: Instant) {
        match self.step {
            1 => {
                if answer.eq_ignore_ascii_case("abort") {
                    self.step = 2;
                } else {
                    self.show("INPUT NOT RECOGNIZED", now);
                }
            }
            2 => {
                if answer == "troba" {
                    self.step = 3;
                } else if answer.eq_ignore_ascii_case("troba") {
                    self.show("LOWERCASE.", now);
                } else {
                    self.show("INCORRECT.", now);
                }
            }
            3 => {
                let (a, b, c) = self.problem;
                if answer.parse::<i64>() == Ok(a * b - c) {
                    self.step = 4;
                } else {
                    self.problem = self.new_problem();
                    self.show("INCORRECT. RECALCULATING.", now);
                }
            }
            4 => {
                if swallow_right(answer) {
                    self.step = 5;
                    self.show("HUH? I... I DON'T KNOW THAT.", now);
                } else {
                    self.show("INCORRECT. AFRICAN OR EUROPEAN?", now);
                }
            }
            5 => {
                self.show(SPECIAL_ORDER[self.order_count], now);
                self.order_count += 1;
                if self.order_count == SPECIAL_ORDER.len() {
                    self.step = 6;
                }
            }
            6 => {
                self.step = 7;
                self.show("OMEGA 13 OFFLINE.", now);
            }
            _ => self.show("PLEASE CONTINUE TO HOLD.", now),
        }
    }
}

fn swallow_right(answer: &str) -> bool {
    let lower = answer.to_lowercase();
    if lower.contains("african") || lower.contains("european") {
        return true;
    }
    lower.split(|c: char| !c.is_ascii_digit()).any(|t| t == "24" || t == "11")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_answer(q: &mut Quiz, s: &str, now: Instant) -> bool {
        for c in s.chars() {
            q.key(&Key::Char(c), now);
        }
        q.key(&Key::Enter, now)
    }

    fn operands(q: &Quiz, now: Instant) -> (i64, i64, i64) {
        let text = q.question(now);
        let t: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(t[0], "SOLVE:", "{text}");
        (t[1].parse().unwrap(), t[3].parse().unwrap(), t[5].parse().unwrap())
    }

    /// A fresh quiz driven to step `n` with right answers (time far apart so
    /// no retort lingers at `t0`).
    fn at_step(n: usize) -> Quiz {
        let mut q = Quiz::new(7);
        let t = Instant::now();
        while q.step() < n {
            let ans = match q.step() {
                1 => "abort".to_string(),
                2 => "troba".to_string(),
                3 => {
                    let (a, b, c) = operands(&q, t);
                    (a * b - c).to_string()
                }
                4 => "african".to_string(),
                _ => "x".to_string(),
            };
            let reps = if q.step() == 5 { 3 } else { 1 };
            for _ in 0..reps {
                type_answer(&mut q, &ans, t);
            }
        }
        q
    }

    #[test]
    fn abort_any_case_advances() {
        let t0 = Instant::now();
        let mut q = Quiz::new(1);
        assert!(type_answer(&mut q, "aBoRt", t0));
        assert_eq!(q.step(), 2);
        assert_eq!(q.question(t0 + RETORT), "CONFIRM: TYPE troba");
    }

    #[test]
    fn abort_wrong_is_not_recognized() {
        let t0 = Instant::now();
        let mut q = Quiz::new(1);
        assert!(type_answer(&mut q, "no", t0));
        assert_eq!(q.question(t0), "INPUT NOT RECOGNIZED");
        assert_eq!(q.step(), 1);
    }

    #[test]
    fn troba_uppercase_gets_lowercase_retort() {
        let t0 = Instant::now();
        let mut q = at_step(2);
        type_answer(&mut q, "TROBA", t0);
        assert_eq!(q.question(t0), "LOWERCASE.");
        assert_eq!(q.question(t0 + Duration::from_millis(1500)), "CONFIRM: TYPE troba");
        assert_eq!(q.step(), 2);
        type_answer(&mut q, "xyz", t0);
        assert_eq!(q.question(t0), "INCORRECT.");
    }

    #[test]
    fn math_right_and_wrong() {
        let t0 = Instant::now();
        let mut q = at_step(3);
        let (a, b, c) = operands(&q, t0);
        type_answer(&mut q, "nope", t0);
        assert_eq!(q.question(t0), "INCORRECT. RECALCULATING.");
        let after = operands(&q, t0 + Duration::from_millis(1500));
        assert_ne!((a, b, c), after);
        assert!((11..=29).contains(&after.0) && (11..=29).contains(&after.1));
        assert!((1..=9).contains(&after.2));
        let (a, b, c) = after;
        type_answer(&mut q, &format!(" {} ", a * b - c), t0);
        assert_eq!(q.step(), 4);
    }

    #[test]
    fn swallow_accepts_african_and_numbers() {
        let t0 = Instant::now();
        for ans in ["African or European?", "24 mph", "11 m/s"] {
            let mut q = at_step(4);
            assert!(type_answer(&mut q, ans, t0));
            assert_eq!(q.step(), 5, "{ans}");
            assert_eq!(q.question(t0), "HUH? I... I DON'T KNOW THAT.");
            assert_eq!(q.question(t0 + RETORT), "ENTER SPECIAL ORDER 937 AUTHORIZATION:");
        }
        for ans in ["240", "fast"] {
            let mut q = at_step(4);
            type_answer(&mut q, ans, t0);
            assert_eq!(q.step(), 4);
            assert_eq!(q.question(t0), "INCORRECT. AFRICAN OR EUROPEAN?");
        }
    }

    #[test]
    fn special_order_cycles_then_moves_on() {
        let t0 = Instant::now();
        let mut q = at_step(5);
        for (i, r) in SPECIAL_ORDER.iter().enumerate() {
            type_answer(&mut q, "please", t0);
            assert_eq!(q.question(t0), *r);
            assert_eq!(q.step(), if i == 2 { 6 } else { 5 });
        }
    }

    #[test]
    fn omega_then_hold_forever() {
        let t0 = Instant::now();
        let mut q = at_step(6);
        type_answer(&mut q, "y", t0);
        assert_eq!(q.question(t0), "OMEGA 13 OFFLINE.");
        assert_eq!(q.step(), 7);
        for _ in 0..10 {
            type_answer(&mut q, "pls", t0);
            assert_eq!(q.question(t0), "PLEASE CONTINUE TO HOLD.");
            assert_eq!(q.step(), 7);
        }
    }

    #[test]
    fn empty_enter_is_not_an_answer() {
        let t0 = Instant::now();
        let mut q = Quiz::new(1);
        assert!(!q.key(&Key::Enter, t0));
        q.key(&Key::Char(' '), t0);
        assert!(!q.key(&Key::Enter, t0));
        assert_eq!(q.question(t0), "TO ABORT, TYPE ABORT");
    }

    #[test]
    fn input_capped_at_30() {
        let t0 = Instant::now();
        let mut q = Quiz::new(1);
        for _ in 0..40 {
            q.key(&Key::Char('a'), t0);
        }
        assert_eq!(q.input().len(), 30);
        q.key(&Key::Backspace, t0);
        assert_eq!(q.input().len(), 29);
    }

    #[test]
    fn ctrl_c_clears_input() {
        let t0 = Instant::now();
        let mut q = Quiz::new(1);
        q.key(&Key::Char('a'), t0);
        q.key(&Key::Ctrl('c'), t0);
        assert_eq!(q.input(), "");
    }

    #[test]
    fn typing_during_retort_keeps_input() {
        let t0 = Instant::now();
        let mut q = Quiz::new(1);
        type_answer(&mut q, "no", t0);
        q.key(&Key::Char('a'), t0);
        q.key(&Key::Char('b'), t0);
        assert_eq!(q.question(t0), "INPUT NOT RECOGNIZED");
        assert_eq!(q.input(), "ab");
    }

    #[test]
    fn zero_seed_works() {
        let q = Quiz::new(0);
        assert_ne!(q.problem, (0, 0, 0));
    }
}
