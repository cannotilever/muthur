//! Arming the self-destruct: the duration strip. Enter hands App a request;
//! the password is asked for on the destruct screen's clearance stage.

use crate::destruct::{DestructRequest, Kind as DKind};
use crate::input::Key;
use crate::ops::audit::SharedAudit;
use std::time::Duration;

const STEPS: [u32; 6] = [10, 30, 60, 120, 300, 600];

pub struct UtilityDeps {
    pub audit: SharedAudit,
    pub destruct_seconds: u32,
}

pub(super) enum State {
    Idle,
    Duration { kind: DKind, idx: usize },
    Denied(&'static str),
}

pub(super) struct Arm {
    deps: Option<UtilityDeps>,
    pub(super) state: State,
    steps: Vec<u32>,
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
            ready: None,
        }
    }

    pub(super) fn take(&mut self) -> Option<DestructRequest> {
        self.ready.take()
    }

    pub(super) fn set_obscured(&mut self, obscured: bool) {
        if obscured {
            // A request not taken while shown must never start later.
            self.ready = None;
            if matches!(self.state, State::Duration { .. }) {
                self.state = State::Idle;
            }
        }
    }

    /// The strip text, if a prompt (or denial) is up.
    pub(super) fn strip(&self) -> Option<String> {
        Some(match &self.state {
            State::Idle => return None,
            State::Duration { kind, idx } => format!("{} IN {}  <-/-> ADJUST  ENTER CONFIRM  ESC CANCEL", kind.label(), mmss(self.steps[*idx])),
            State::Denied(t) => (*t).into(),
        })
    }

    /// Returns true when the key was consumed.
    pub(super) fn key(&mut self, key: &Key) -> bool {
        // The hand-over is over: a request not taken by now is stale.
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
                        self.ready = Some(DestructRequest {
                            kind,
                            total: Duration::from_secs(u64::from(self.steps[idx])),
                        });
                        return true;
                    }
                    _ => {}
                }
                self.state = State::Duration { kind, idx };
                true
            }
        }
    }
}
