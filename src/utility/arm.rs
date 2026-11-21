//! Arming the self-destruct: duration, password (armed like CARGO's Y/N), sudo verify.

use crate::destruct::{DestructRequest, Kind as DKind, SudoRun, sudo_argv};
use crate::input::Key;
use crate::ops::audit::SharedAudit;
use crate::ops::runner::RunOutput;
use crate::secret::Secret;
use std::time::{Duration, Instant};

const VERIFY_TIMEOUT: Duration = Duration::from_secs(10);
const STEPS: [u32; 6] = [10, 30, 60, 120, 300, 600];

pub struct UtilityDeps {
    pub run: SudoRun,
    pub now: Box<dyn Fn() -> Instant>,
    pub audit: SharedAudit,
    pub destruct_seconds: u32,
}

pub(super) enum State {
    Idle,
    Duration {
        kind: DKind,
        idx: usize,
    },
    Password {
        kind: DKind,
        secs: u32,
        secret: Secret,
        drawn: bool,
        armed_at: Option<Instant>,
    },
    Verifying {
        kind: DKind,
        secs: u32,
        seq: u64,
        secret: Secret,
    },
    Denied(&'static str),
}

pub(super) struct Arm {
    deps: Option<UtilityDeps>,
    pub(super) state: State,
    steps: Vec<u32>,
    seq: u64,
    obscured: bool,
    ready: Option<DestructRequest>,
}

fn mmss(secs: u32) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

impl Arm {
    pub(super) fn new(deps: Option<UtilityDeps>) -> Self {
        let mut steps = STEPS.to_vec();
        if let Some(d) = &deps
            && !steps.contains(&d.destruct_seconds)
        {
            steps.push(d.destruct_seconds);
            steps.sort_unstable();
        }
        Self {
            deps,
            state: State::Idle,
            steps,
            seq: 0,
            obscured: false,
            ready: None,
        }
    }

    pub(super) fn take(&mut self) -> Option<DestructRequest> {
        self.ready.take()
    }

    pub(super) fn tick_interval(&self) -> Option<Duration> {
        match &self.state {
            State::Password { armed_at, .. } if !armed_at.is_some_and(|t| self.now().is_some_and(|n| n >= t)) => Some(Duration::from_millis(100)),
            _ => None,
        }
    }

    fn now(&self) -> Option<Instant> {
        self.deps.as_ref().map(|d| (d.now)())
    }

    pub(super) fn set_obscured(&mut self, obscured: bool) {
        self.obscured = obscured;
        if obscured {
            // A verified request not taken while shown must never start later.
            self.ready = None;
        }
        if obscured && matches!(self.state, State::Duration { .. } | State::Password { .. } | State::Verifying { .. }) {
            self.state = State::Idle;
        }
    }

    /// Called from render; arms the password prompt after its first unobscured draw.
    pub(super) fn drawn(&mut self) {
        let now = self.now();
        if let State::Password { drawn, armed_at, .. } = &mut self.state
            && !self.obscured
        {
            if !*drawn {
                *drawn = true;
            } else if armed_at.is_none() {
                *armed_at = now.map(|n| n + crate::app::ARMING);
            }
        }
    }

    /// The strip text, if a prompt (or denial) is up.
    pub(super) fn strip(&self) -> Option<String> {
        Some(match &self.state {
            State::Idle => return None,
            State::Duration { kind, idx } => format!("{} IN {}  <-/-> ADJUST  ENTER CONFIRM  ESC CANCEL", kind.label(), mmss(self.steps[*idx])),
            State::Password { kind, secs, .. } => format!("CLEARANCE REQUIRED -- {} IN {} -- PASSWORD:", kind.label(), mmss(*secs)),
            State::Verifying { .. } => "VERIFYING".into(),
            State::Denied(t) => (*t).into(),
        })
    }

    /// Returns true when the key was consumed.
    pub(super) fn key(&mut self, key: &Key) -> bool {
        // The hand-over is over: a request not taken by now is stale (drop zeroizes).
        self.ready = None;
        let state = std::mem::replace(&mut self.state, State::Idle);
        match state {
            State::Idle => {
                if let Key::Char(c) = key
                    && let Some(kind) = match c.to_ascii_lowercase() {
                        's' => Some(DKind::Shutdown),
                        'b' => Some(DKind::Reboot),
                        _ => None,
                    }
                    && let Some(d) = &self.deps
                {
                    self.state = if d.audit.borrow().is_online() {
                        let idx = self.steps.iter().position(|&s| s == d.destruct_seconds).unwrap_or(0);
                        State::Duration { kind, idx }
                    } else {
                        State::Denied("AUDIT LOG OFFLINE")
                    };
                    return true;
                }
                false
            }
            State::Denied(_) => true,
            State::Duration { kind, mut idx } => {
                match key {
                    Key::Left => idx = idx.saturating_sub(1),
                    Key::Right => idx = (idx + 1).min(self.steps.len() - 1),
                    Key::Esc => return true,
                    Key::Enter => {
                        self.state = State::Password {
                            kind,
                            secs: self.steps[idx],
                            secret: Secret::new(),
                            drawn: false,
                            armed_at: None,
                        };
                        return true;
                    }
                    _ => {}
                }
                self.state = State::Duration { kind, idx };
                true
            }
            State::Password {
                kind,
                secs,
                mut secret,
                drawn,
                armed_at,
            } => {
                let armed = armed_at.is_some_and(|t| self.now().is_some_and(|n| n >= t));
                if armed {
                    match key {
                        Key::Esc => return true,
                        Key::Char(c) if !c.is_control() => secret.push(*c),
                        Key::Backspace => {
                            secret.pop();
                        }
                        Key::Enter if !secret.is_empty() => {
                            self.seq += 1;
                            let seq = self.seq;
                            if let Some(d) = &mut self.deps {
                                (d.run)(seq, sudo_argv(&["true"]), VERIFY_TIMEOUT, secret.duplicate());
                            }
                            self.state = State::Verifying { kind, secs, seq, secret };
                            return true;
                        }
                        _ => {}
                    }
                } else if matches!(key, Key::Esc) {
                    return true;
                }
                self.state = State::Password {
                    kind,
                    secs,
                    secret,
                    drawn,
                    armed_at,
                };
                true
            }
            State::Verifying { kind, secs, seq, secret } => {
                if matches!(key, Key::Esc) {
                    return true;
                }
                self.state = State::Verifying { kind, secs, seq, secret };
                true
            }
        }
    }

    pub(super) fn sudo_done(&mut self, seq: u64, out: RunOutput) {
        let State::Verifying { seq: mine, .. } = &self.state else { return };
        if *mine != seq {
            return;
        }
        let State::Verifying { kind, secs, secret, .. } = std::mem::replace(&mut self.state, State::Idle) else {
            return;
        };
        if matches!(out, RunOutput::Exited { code: 0, .. }) {
            self.ready = Some(DestructRequest {
                kind,
                total: Duration::from_secs(secs as u64),
                password: secret,
            });
        } else {
            self.state = State::Denied("CLEARANCE DENIED");
        }
    }
}
