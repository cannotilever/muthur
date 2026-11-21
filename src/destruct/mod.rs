//! The self-destruct countdown: a pure state machine. The caller feeds it
//! ticks and keys and carries out the returned `Effect`s.

pub mod quiz;
pub mod record;
pub mod screen;

use std::time::{Duration, Instant};

use crate::input::Key;
use crate::ops::runner::RunOutput;
use crate::secret::Secret;

use quiz::Quiz;

pub const TICK: Duration = Duration::from_millis(100);
const FREEZE_HOLD: Duration = Duration::from_millis(5500);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Shutdown,
    Reboot,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Shutdown => "SHUTDOWN",
            Kind::Reboot => "REBOOT",
        }
    }

    pub fn verb(self) -> &'static str {
        match self {
            Kind::Shutdown => "poweroff",
            Kind::Reboot => "reboot",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Aborted,
    Frozen,
    Executed,
    Failed,
}

pub enum Effect {
    Execute(Secret),
    Done(Outcome),
}

/// Why the execute command failed (uppercase), or `None` if it succeeded.
pub fn failure_reason(out: &RunOutput) -> Option<String> {
    let reason = match out {
        RunOutput::Exited { code: 0, .. } => return None,
        RunOutput::Exited { code, stderr, .. } => stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("EXIT {code}")),
        RunOutput::TimedOut => "TIMED OUT".into(),
        RunOutput::NotFound => "NOT FOUND".into(),
        RunOutput::Cancelled => "CANCELLED".into(),
    };
    Some(reason.to_uppercase())
}

/// Runs `argv` under sudo: (seq, argv, timeout, password for stdin). The
/// closure owns the password and drops (zeroizes) it once the command has
/// exited (it is borrowed as stdin until then).
pub type SudoRun = Box<dyn FnMut(u64, Vec<String>, Duration, Secret)>;

/// What `App` needs to run a real countdown.
pub struct DestructDeps {
    pub run: SudoRun,
    pub audit: crate::ops::audit::SharedAudit,
    pub timestamp: Box<dyn Fn() -> String>,
}

/// `sudo -S -k -p "" <cmd>`: the password comes on stdin, never cached.
pub fn sudo_argv(cmd: &[&str]) -> Vec<String> {
    ["sudo", "-S", "-k", "-p", ""].iter().chain(cmd).map(|s| s.to_string()).collect()
}

pub struct DestructRequest {
    pub kind: Kind,
    pub total: Duration,
    pub password: Secret,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Phase {
    Running,
    Frozen {
        since: Instant,
    },
    Executing,
    Failed(String),
    /// Preview only: the clock reached zero and nothing was executed.
    Zero,
}

pub struct Destruct {
    pub(crate) kind: Kind,
    pub(crate) started: Instant,
    pub(crate) total: Duration,
    pub(crate) phase: Phase,
    pub(crate) quiz: Quiz,
    password: Option<Secret>,
    preview: bool,
    answered: bool,
}

impl Destruct {
    pub fn new(req: DestructRequest, started: Instant, seed: u64) -> Destruct {
        Destruct {
            kind: req.kind,
            started,
            total: req.total,
            phase: Phase::Running,
            quiz: Quiz::new(seed),
            password: Some(req.password),
            preview: false,
            answered: false,
        }
    }

    pub fn preview(kind: Kind, total: Duration, started: Instant, seed: u64) -> Destruct {
        Destruct {
            kind,
            started,
            total,
            phase: Phase::Running,
            quiz: Quiz::new(seed),
            password: None,
            preview: true,
            answered: false,
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn quiz(&self) -> &Quiz {
        &self.quiz
    }

    pub fn has_password(&self) -> bool {
        self.password.is_some()
    }

    /// The instant the clock reaches zero.
    pub(crate) fn zero_at(&self) -> Instant {
        self.started + self.total
    }

    fn remaining(&self, now: Instant) -> Duration {
        self.zero_at().saturating_duration_since(now)
    }

    pub fn outcome_so_far(&self) -> Outcome {
        match self.phase {
            Phase::Running | Phase::Zero => Outcome::Aborted,
            Phase::Frozen { .. } => Outcome::Frozen,
            Phase::Executing => Outcome::Executed,
            Phase::Failed(_) => Outcome::Failed,
        }
    }

    pub fn tick(&mut self, now: Instant) -> Option<Effect> {
        match self.phase {
            Phase::Running => {
                let rem = self.remaining(now);
                if rem <= TICK && self.answered {
                    self.phase = Phase::Frozen { since: now };
                } else if rem.is_zero() && !self.answered {
                    if self.preview {
                        self.phase = Phase::Zero;
                    } else {
                        self.phase = Phase::Executing;
                        return self.password.take().map(Effect::Execute);
                    }
                }
                None
            }
            Phase::Frozen { since } if now >= since + FREEZE_HOLD => Some(Effect::Done(Outcome::Frozen)),
            _ => None,
        }
    }

    pub fn key(&mut self, key: &Key, now: Instant) -> Option<Effect> {
        match &self.phase {
            Phase::Running => {
                if *key == Key::Esc {
                    return Some(Effect::Done(Outcome::Aborted));
                }
                if self.quiz.key(key, now) {
                    self.answered = true;
                    self.password = None;
                }
                None
            }
            Phase::Frozen { .. } => (*key == Key::Esc).then_some(Effect::Done(Outcome::Frozen)),
            Phase::Failed(_) => Some(Effect::Done(Outcome::Failed)),
            Phase::Zero => Some(Effect::Done(Outcome::Aborted)),
            Phase::Executing => None,
        }
    }

    pub fn on_executed(&mut self, out: &RunOutput) {
        if let Some(reason) = failure_reason(out) {
            self.phase = Phase::Failed(reason);
        }
    }

    /// Digits M M S S t.
    pub fn counter(&self, now: Instant) -> [u8; 5] {
        let tenths = match self.phase {
            Phase::Running => self.remaining(now).as_millis().div_ceil(100) as u64,
            Phase::Frozen { .. } => 1,
            _ => 0,
        };
        let (m, sec, t) = (tenths / 600, (tenths / 10) % 60, tenths % 10);
        [(m / 10 % 10) as u8, (m % 10) as u8, (sec / 10) as u8, (sec % 10) as u8, t as u8]
    }
}

#[cfg(test)]
mod tests;
