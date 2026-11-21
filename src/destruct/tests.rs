use std::time::{Duration, Instant};

use super::*;
use crate::input::Key;
use crate::ops::runner::RunOutput;

fn s(x: f64) -> Duration {
    Duration::from_secs_f64(x)
}

fn mk(total: u64) -> (Destruct, Instant) {
    let t0 = Instant::now();
    let mut pw = Secret::new();
    for c in "pw".chars() {
        pw.push(c);
    }
    let req = DestructRequest { kind: Kind::Shutdown, total: Duration::from_secs(total), password: pw };
    (Destruct::new(req, t0, 7), t0)
}

fn answer(d: &mut Destruct, now: Instant) {
    d.key(&Key::Char('x'), now);
    d.key(&Key::Enter, now);
}

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
