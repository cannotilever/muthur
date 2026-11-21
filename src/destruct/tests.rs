use std::time::{Duration, Instant};

use super::*;
use crate::app::ARMING;
use crate::input::Key;
use crate::ops::runner::RunOutput;

fn s(x: f64) -> Duration {
    Duration::from_secs_f64(x)
}

fn ok() -> RunOutput {
    RunOutput::Exited {
        code: 0,
        stdout: String::new(),
        stderr: String::new(),
    }
}

fn denied() -> RunOutput {
    RunOutput::Exited {
        code: 1,
        stdout: String::new(),
        stderr: "sudo: 1 incorrect password attempt\n".into(),
    }
}

/// A shutdown at the clearance stage, drawn twice at `t`; armed from `t + ARMING`.
fn clearance(total: u64, t: Instant) -> Destruct {
    let req = DestructRequest {
        kind: Kind::Shutdown,
        total: Duration::from_secs(total),
    };
    let mut d = Destruct::new(req, 7);
    d.rendered(t);
    d.rendered(t);
    d
}

fn type_str(d: &mut Destruct, text: &str, now: Instant) {
    for c in text.chars() {
        assert!(d.key(&Key::Char(c), now).is_none());
    }
}

/// Type `pw` and Enter once armed; returns the instant to verify at.
fn submit(d: &mut Destruct, t: Instant) -> Instant {
    let armed = t + ARMING;
    type_str(d, "pw", armed);
    match d.key(&Key::Enter, armed) {
        Some(Effect::Verify(p)) => assert_eq!(p.expose(), "pw"),
        _ => panic!("expected Verify"),
    }
    armed + s(0.5)
}

/// A running countdown, verified (and so started) at the returned instant.
fn mk(total: u64) -> (Destruct, Instant) {
    let t = Instant::now();
    let mut d = clearance(total, t);
    let t0 = submit(&mut d, t);
    d.on_verified(&ok(), t0);
    (d, t0)
}

fn answer(d: &mut Destruct, now: Instant) {
    d.key(&Key::Char('x'), now);
    d.key(&Key::Enter, now);
}

// ---- Clearance ----

#[test]
fn clearance_ignores_keys_until_armed() {
    let t = Instant::now();
    let req = DestructRequest {
        kind: Kind::Shutdown,
        total: Duration::from_secs(60),
    };
    let mut d = Destruct::new(req, 7);
    type_str(&mut d, "ab", t);
    d.rendered(t);
    type_str(&mut d, "ab", t + s(5.0));
    d.rendered(t + s(5.0));
    type_str(&mut d, "ab", t + s(5.5));
    assert!(d.key(&Key::Enter, t + s(5.9)).is_none());
    assert_eq!(d.clearance_chars(), Some(0));
    type_str(&mut d, "pw", t + s(6.0));
    assert_eq!(d.clearance_chars(), Some(2));
}

#[test]
fn esc_cancels_clearance_even_unarmed() {
    let req = DestructRequest {
        kind: Kind::Reboot,
        total: Duration::from_secs(60),
    };
    let mut d = Destruct::new(req, 7);
    assert!(matches!(d.key(&Key::Esc, Instant::now()), Some(Effect::Done(Outcome::Cancelled))));
    assert_eq!(d.outcome_so_far(), Outcome::Cancelled);
}

#[test]
fn backspace_and_empty_enter() {
    let t = Instant::now();
    let mut d = clearance(60, t);
    let armed = t + ARMING;
    assert!(d.key(&Key::Enter, armed).is_none(), "empty Enter does nothing");
    type_str(&mut d, "pxy", armed);
    d.key(&Key::Backspace, armed);
    d.key(&Key::Backspace, armed);
    assert_eq!(d.clearance_chars(), Some(1));
    type_str(&mut d, "w", armed);
    match d.key(&Key::Enter, armed) {
        Some(Effect::Verify(p)) => assert_eq!(p.expose(), "pw"),
        _ => panic!("expected Verify"),
    }
    assert_eq!(d.clearance_chars(), None, "verifying");
}

#[test]
fn verifying_ignores_keys_but_esc() {
    let t = Instant::now();
    let mut d = clearance(60, t);
    let at = submit(&mut d, t);
    assert!(d.key(&Key::Char('x'), at).is_none());
    assert!(d.key(&Key::Enter, at).is_none());
    assert!(d.tick(at + s(30.0)).is_none());
    assert!(matches!(d.key(&Key::Esc, at), Some(Effect::Done(Outcome::Cancelled))));
}

#[test]
fn counter_shows_total_during_clearance() {
    let t = Instant::now();
    let mut d = clearance(60, t);
    assert_eq!(d.counter(t + s(30.0)), [0, 1, 0, 0, 0]);
    let at = submit(&mut d, t);
    assert_eq!(d.counter(at + s(30.0)), [0, 1, 0, 0, 0]);
    d.on_verified(&denied(), at);
    assert_eq!(d.counter(at + s(1.0)), [0, 1, 0, 0, 0]);
}

#[test]
fn verified_starts_counter_and_executes_with_same_password() {
    let t = Instant::now();
    let mut d = clearance(10, t);
    let at = submit(&mut d, t);
    assert!(!d.has_password());
    d.on_verified(&ok(), at);
    assert!(d.has_password());
    assert_eq!(d.outcome_so_far(), Outcome::Aborted);
    assert_eq!(d.counter(at), [0, 0, 1, 0, 0]);
    assert_eq!(d.counter(at + s(5.0)), [0, 0, 0, 5, 0]);
    assert!(d.tick(at + s(9.95)).is_none());
    match d.tick(at + s(10.0)) {
        Some(Effect::Execute(p)) => assert_eq!(p.expose(), "pw"),
        _ => panic!("expected Execute"),
    }
}

#[test]
fn denied_cancels_after_two_seconds() {
    let t = Instant::now();
    let mut d = clearance(60, t);
    let at = submit(&mut d, t);
    d.on_verified(&denied(), at);
    assert!(!d.has_password());
    assert_eq!(d.outcome_so_far(), Outcome::Cancelled);
    assert!(d.key(&Key::Char('x'), at + s(1.0)).is_none());
    assert!(d.tick(at + s(1.95)).is_none());
    assert!(matches!(d.tick(at + s(2.0)), Some(Effect::Done(Outcome::Cancelled))));
}

#[test]
fn esc_in_denied_is_immediate() {
    let t = Instant::now();
    let mut d = clearance(60, t);
    let at = submit(&mut d, t);
    d.on_verified(&RunOutput::TimedOut, at);
    assert!(matches!(d.key(&Key::Esc, at + s(0.1)), Some(Effect::Done(Outcome::Cancelled))));
}

#[test]
fn verified_outside_verifying_is_ignored() {
    let t = Instant::now();
    let mut d = clearance(60, t);
    d.on_verified(&ok(), t);
    assert_eq!(d.clearance_chars(), Some(0), "still at clearance");
    let (mut d, t0) = mk(60);
    d.on_verified(&denied(), t0 + s(1.0));
    assert_eq!(d.outcome_so_far(), Outcome::Aborted, "still running");
}

#[test]
fn cancelled_serializes_lowercase() {
    assert_eq!(serde_json::to_value(Outcome::Cancelled).unwrap(), "cancelled");
}

// ---- Countdown ----

#[test]
fn counter_ceils_to_tenths() {
    let (d, t0) = mk(70);
    assert_eq!(d.counter(t0), [0, 1, 1, 0, 0]);
    assert_eq!(d.counter(t0 + s(0.05)), [0, 1, 1, 0, 0]);
    assert_eq!(d.counter(t0 + s(69.95)), [0, 0, 0, 0, 1]);
    let (d, t0) = mk(600);
    assert_eq!(d.counter(t0), [1, 0, 0, 0, 0]);
}

#[test]
fn no_answer_executes_at_zero() {
    let (mut d, t0) = mk(60);
    assert!(d.tick(t0 + s(59.95)).is_none());
    match d.tick(t0 + s(60.0)) {
        Some(Effect::Execute(p)) => assert_eq!(p.expose(), "pw"),
        _ => panic!("expected Execute"),
    }
    assert_eq!(d.counter(t0 + s(60.0)), [0; 5]);
}

#[test]
fn execute_emitted_once() {
    let (mut d, t0) = mk(60);
    assert!(matches!(d.tick(t0 + s(60.0)), Some(Effect::Execute(_))));
    assert!(d.tick(t0 + s(61.0)).is_none());
    assert!(d.tick(t0 + s(120.0)).is_none());
}

#[test]
fn answer_freezes_at_one_tenth() {
    let (mut d, t0) = mk(60);
    answer(&mut d, t0 + s(5.0));
    assert!(!d.has_password());
    assert!(d.tick(t0 + s(59.9)).is_none());
    assert_eq!(d.counter(t0 + s(59.9)), [0, 0, 0, 0, 1]);
    for t in [61.0, 62.0, 65.3] {
        assert!(d.tick(t0 + s(t)).is_none());
    }
    assert!(matches!(d.tick(t0 + s(65.4)), Some(Effect::Done(Outcome::Frozen))));
}

#[test]
fn late_answer_still_freezes() {
    let (mut d, t0) = mk(60);
    answer(&mut d, t0 + s(59.95));
    assert!(d.tick(t0 + s(59.97)).is_none());
    assert_eq!(d.outcome_so_far(), Outcome::Frozen);
}

#[test]
fn late_tick_past_zero_freezes_if_answered() {
    let (mut d, t0) = mk(60);
    answer(&mut d, t0 + s(1.0));
    assert!(d.tick(t0 + s(90.0)).is_none());
    assert_eq!(d.outcome_so_far(), Outcome::Frozen);
}

#[test]
fn esc_aborts_silently() {
    let (mut d, t0) = mk(60);
    assert!(matches!(d.key(&Key::Esc, t0 + s(3.0)), Some(Effect::Done(Outcome::Aborted))));
    let (mut d, t0) = mk(60);
    answer(&mut d, t0 + s(1.0));
    d.tick(t0 + s(59.95));
    assert!(matches!(d.key(&Key::Esc, t0 + s(60.0)), Some(Effect::Done(Outcome::Frozen))));
}

#[test]
fn failure_then_any_key() {
    let (mut d, t0) = mk(60);
    assert!(matches!(d.tick(t0 + s(60.0)), Some(Effect::Execute(_))));
    d.on_executed(&RunOutput::Exited {
        code: 1,
        stdout: String::new(),
        stderr: "sudo: 1 incorrect password attempt\n".into(),
    });
    assert_eq!(d.outcome_so_far(), Outcome::Failed);
    assert!(matches!(d.key(&Key::Char('a'), t0 + s(61.0)), Some(Effect::Done(Outcome::Failed))));
}

#[test]
fn executing_ignores_keys() {
    let (mut d, t0) = mk(60);
    d.tick(t0 + s(60.0));
    assert!(d.key(&Key::Esc, t0 + s(61.0)).is_none());
}

#[test]
fn preview_never_executes() {
    let t0 = Instant::now();
    let mut d = Destruct::preview(Kind::Reboot, Duration::from_secs(10), t0, 3);
    assert!(d.tick(t0 + s(10.0)).is_none());
    assert_eq!(d.counter(t0 + s(10.0)), [0; 5]);
    assert!(matches!(d.key(&Key::Char('a'), t0 + s(11.0)), Some(Effect::Done(Outcome::Aborted))));
}
