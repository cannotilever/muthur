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
/// At zero the command waits for the final frame to be drawn, but no longer
/// than this (a stalled or switched-off terminal never stops it).
const FINAL_GRACE: Duration = Duration::from_secs(2);
/// How long `CLEARANCE DENIED` shows before the screen gives up.
const DENIED_HOLD: Duration = Duration::from_secs(2);

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
    /// Left at the clearance stage (Esc, Break, or a wrong password): never audited.
    Cancelled,
    Aborted,
    Frozen,
    Executed,
    Failed,
}

pub enum Effect {
    /// Check the typed password (`sudo -S -k true`); answer with `on_verified`.
    Verify(Secret),
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

/// A countdown chosen on the Utility tab; the password is asked for on the
/// destruct screen itself.
pub struct DestructRequest {
    pub kind: Kind,
    pub total: Duration,
}

#[derive(Debug)]
pub(crate) enum Phase {
    /// The password field. Keys count only from `armed_at`, which is set on the
    /// second render (the first one may not have reached the screen yet).
    Clearance {
        input: Secret,
        drawn: bool,
        armed_at: Option<Instant>,
    },
    /// The typed password is being checked; it becomes the execute password.
    Verifying {
        input: Secret,
    },
    /// The password was refused; the screen ends after `DENIED_HOLD`.
    Denied {
        since: Instant,
    },
    Running,
    Frozen {
        since: Instant,
    },
    /// The last tenth with no answer: the screen shows `00000` and stops
    /// changing; the command runs at zero once this frame has been drawn.
    Final {
        drawn: bool,
    },
    Executing,
    Failed(String),
    /// Preview only: the clock reached zero and nothing was executed.
    Zero,
}

pub struct Destruct {
    pub(crate) kind: Kind,
    /// When the countdown started; `None` until clearance is passed.
    pub(crate) started: Option<Instant>,
    /// When the screen stopped changing (freeze or final frame): the X holds
    /// its phase from this instant.
    pub(crate) held_at: Option<Instant>,
    pub(crate) total: Duration,
    pub(crate) phase: Phase,
    pub(crate) quiz: Quiz,
    password: Option<Secret>,
    preview: bool,
    answered: bool,
}

impl Destruct {
    /// A real countdown, starting at the clearance stage.
    pub fn new(req: DestructRequest, seed: u64) -> Destruct {
        Destruct {
            kind: req.kind,
            started: None,
            held_at: None,
            total: req.total,
            phase: Phase::Clearance {
                input: Secret::new(),
                drawn: false,
                armed_at: None,
            },
            quiz: Quiz::new(seed),
            password: None,
            preview: false,
            answered: false,
        }
    }

    pub fn preview(kind: Kind, total: Duration, started: Instant, seed: u64) -> Destruct {
        Destruct {
            kind,
            started: Some(started),
            held_at: None,
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

    /// The instant the clock reaches zero (`None` before clearance).
    pub(crate) fn zero_at(&self) -> Option<Instant> {
        self.started.map(|s| s + self.total)
    }

    /// Time left; the whole total until the countdown starts.
    fn remaining(&self, now: Instant) -> Duration {
        self.zero_at().map_or(self.total, |z| z.saturating_duration_since(now))
    }

    /// True during the clearance stage (password field, verify, denial).
    pub(crate) fn in_clearance(&self) -> bool {
        matches!(self.phase, Phase::Clearance { .. } | Phase::Verifying { .. } | Phase::Denied { .. })
    }

    /// Characters typed into the password field, while it takes them.
    pub(crate) fn clearance_chars(&self) -> Option<usize> {
        match &self.phase {
            Phase::Clearance { input, .. } => Some(input.chars()),
            _ => None,
        }
    }

    /// Called after each render: arms the password field once it has been
    /// drawn twice (the first frame may not have reached the screen).
    pub fn rendered(&mut self, now: Instant) {
        if let Phase::Final { drawn } = &mut self.phase {
            *drawn = true;
        }
        if let Phase::Clearance { drawn, armed_at, .. } = &mut self.phase {
            if !*drawn {
                *drawn = true;
            } else if armed_at.is_none() {
                *armed_at = Some(now + crate::app::ARMING);
            }
        }
    }

    /// The verify command's result: success starts the countdown at `now`
    /// with the typed password; anything else is a denial.
    pub fn on_verified(&mut self, out: &RunOutput, now: Instant) {
        if !matches!(self.phase, Phase::Verifying { .. }) {
            return;
        }
        let Phase::Verifying { input } = std::mem::replace(&mut self.phase, Phase::Denied { since: now }) else {
            return;
        };
        if matches!(out, RunOutput::Exited { code: 0, .. }) {
            self.password = Some(input);
            self.started = Some(now);
            self.phase = Phase::Running;
        }
        // Otherwise `input` drops here, zeroized.
    }

    /// Past the point of no return on screen: the final frame or the command.
    pub fn committed(&self) -> bool {
        matches!(self.phase, Phase::Final { .. } | Phase::Executing)
    }

    pub fn outcome_so_far(&self) -> Outcome {
        match self.phase {
            Phase::Clearance { .. } | Phase::Verifying { .. } | Phase::Denied { .. } => Outcome::Cancelled,
            Phase::Running | Phase::Final { .. } | Phase::Zero => Outcome::Aborted,
            Phase::Frozen { .. } => Outcome::Frozen,
            Phase::Executing => Outcome::Executed,
            Phase::Failed(_) => Outcome::Failed,
        }
    }

    pub fn tick(&mut self, now: Instant) -> Option<Effect> {
        match self.phase {
            Phase::Running => {
                if self.remaining(now) > TICK {
                    return None;
                }
                self.held_at = Some(now);
                self.phase = if self.answered {
                    Phase::Frozen { since: now }
                } else if self.preview {
                    Phase::Zero
                } else {
                    Phase::Final { drawn: false }
                };
                // A late tick may already be at zero.
                self.tick(now)
            }
            Phase::Final { drawn } => {
                let zero = self.zero_at().unwrap_or(now);
                if now < zero || (!drawn && now < zero + FINAL_GRACE) {
                    return None;
                }
                self.phase = Phase::Executing;
                self.password.take().map(Effect::Execute)
            }
            Phase::Frozen { since } if now >= since + FREEZE_HOLD => Some(Effect::Done(Outcome::Frozen)),
            Phase::Denied { since } if now >= since + DENIED_HOLD => Some(Effect::Done(Outcome::Cancelled)),
            _ => None,
        }
    }

    pub fn key(&mut self, key: &Key, now: Instant) -> Option<Effect> {
        match &mut self.phase {
            Phase::Clearance { .. } | Phase::Verifying { .. } | Phase::Denied { .. } if *key == Key::Esc => Some(Effect::Done(Outcome::Cancelled)),
            Phase::Clearance { input, armed_at, .. } => {
                if !armed_at.is_some_and(|t| now >= t) {
                    return None;
                }
                match key {
                    Key::Char(c) if !c.is_control() => input.push(*c),
                    Key::Backspace => input.pop(),
                    Key::Enter if !input.is_empty() => {
                        let Phase::Clearance { input, .. } = std::mem::replace(&mut self.phase, Phase::Running) else {
                            return None;
                        };
                        let check = input.duplicate();
                        self.phase = Phase::Verifying { input };
                        return Some(Effect::Verify(check));
                    }
                    _ => {}
                }
                None
            }
            Phase::Verifying { .. } | Phase::Denied { .. } => None,
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
            Phase::Final { .. } => (*key == Key::Esc).then_some(Effect::Done(Outcome::Aborted)),
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
            Phase::Clearance { .. } | Phase::Verifying { .. } | Phase::Denied { .. } => self.total.as_millis().div_ceil(100) as u64,
            _ => 0,
        };
        let (m, sec, t) = (tenths / 600, (tenths / 10) % 60, tenths % 10);
        [(m / 10 % 10) as u8, (m % 10) as u8, (sec / 10) as u8, (sec % 10) as u8, t as u8]
    }
}

#[cfg(test)]
mod tests;
