//! MU/TH/UR tool turns: carrying out `agent::Effect`s, reviews and the
//! operator's Y/N, password and OVERRIDE prompts.

use super::{Entry, MuthurTab, State};
use crate::agent::{ABORTED_ISSUED, Call, DelegateOutcome, Effect};
use crate::input::Key;
use crate::llm::{Stop, ToolResult};
use crate::review::Review;
use crate::secret::Secret;
use crate::tools::ToolOutcome;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The word the OVERRIDE prompt wants.
const OVERRIDE: &str = "OVERRIDE";
/// Longest text the OVERRIDE prompt keeps.
const TYPED_MAX: usize = 16;

/// An operator answer to a tool prompt.
enum Answer {
    Confirm(bool),
    Password(bool),
    Override(bool),
}

const AUDIT_OFFLINE: &str = "AUDIT LOG OFFLINE";

/// The transcript note redisplayed for a logged tool result.
pub(super) fn tool_note(r: &ToolResult) -> String {
    if r.content.starts_with(ABORTED_ISSUED) {
        return format!("ACCESS INTERRUPTED: {}", r.label);
    }
    let denied = ["NOT AUTHORIZED", "DENIED", "ABORTED"].iter().any(|p| r.content.starts_with(p));
    format!("{}: {}", if denied { "ACCESS DENIED" } else { "ACCESSING" }, r.label)
}

impl MuthurTab {
    /// The stream ended: log the assistant record, then let the turn decide what follows.
    pub(super) fn stream_done(&mut self, stop: Stop, model: Option<String>, raw: Option<Vec<serde_json::Value>>) {
        self.flush_cleaner();
        let a = self.assistant(model, stop.log_str(), raw);
        self.log_assistant(a);
        // Logged; an interrupt later in the turn must not log this text again.
        self.partial_raw.clear();
        self.abort = None;
        let calls = std::mem::take(&mut self.calls);
        let online = self.audit.borrow().is_online();
        let (defs, policy) = if self.direct() { (&self.tech.defs, &self.tech.policy) } else { (&self.tools, &self.policy) };
        let effects = match &mut self.turn {
            Some(t) => t.on_done(&stop, calls, defs, policy, online),
            None => vec![Effect::Finished { truncated: false }],
        };
        if !effects.iter().any(|e| matches!(e, Effect::Finished { .. })) {
            // A tool round follows: the text so far goes above its notes.
            self.settle_segment();
        }
        self.apply(effects, Some(&stop));
    }

    /// A tool call of the stream; the turn takes them when it ends.
    pub(super) fn tool_call(&mut self, id: String, name: String, input: serde_json::Value) {
        self.calls.push(Call { id, name, input });
    }

    /// A tool outcome for request `req_id`. The late outcome of a call aborted
    /// while running is audited and nothing more; other stale ones are ignored.
    pub(super) fn tool_done(&mut self, req_id: u64, call_id: &str, outcome: ToolOutcome) {
        if let Some(i) = self.orphans.iter().position(|(r, o)| *r == req_id && o.call_id == call_id) {
            let (_, orphan) = self.orphans.swap_remove(i);
            self.record_audit(&orphan.audit(outcome));
            return;
        }
        if req_id != self.req_id {
            return;
        }
        let Some(turn) = &mut self.turn else { return };
        self.cancel = None;
        let effects = turn.on_tool_done(call_id, outcome);
        self.apply(effects, None);
    }

    /// The reviewer's verdict for request `req_id`; stale ones, and any while
    /// no review is in flight, are ignored (the turn also checks the call).
    pub(super) fn review_done(&mut self, req_id: u64, call_id: &str, review: Review) {
        if req_id != self.req_id || self.state != State::Reviewing {
            return;
        }
        let online = self.audit.borrow().is_online();
        let Some(turn) = &mut self.turn else { return };
        let effects = turn.on_review(call_id, review.clone(), online);
        if !effects.is_empty() {
            self.review_abort = None;
            // For the pending block of a prompt that may follow.
            self.last_review = Some((call_id.to_string(), review));
        }
        self.apply(effects, None);
    }

    /// A key while a Y/N, password or OVERRIDE prompt is up.
    pub(super) fn prompt_key(&mut self, key: &Key) {
        // Aborting is always safe, so Ctrl-C works even while arming.
        if *key == Key::Ctrl('c') {
            self.interrupt();
            return;
        }
        // Unarmed until seen, then for ARMING after that.
        if !self.state.armed((self.deps.now)()) {
            return;
        }
        let answer = match &mut self.state {
            State::Ask { .. } => match key {
                Key::Char('y' | 'Y') => Answer::Confirm(true),
                Key::Char('n' | 'N') | Key::Esc => Answer::Confirm(false),
                _ => return,
            },
            State::Password { secret, .. } => match key {
                Key::Char(c) if !c.is_control() => return secret.push(*c),
                Key::Backspace => return secret.pop(),
                Key::Enter if !secret.is_empty() => Answer::Password(true),
                Key::Esc => {
                    secret.clear();
                    Answer::Password(false)
                }
                _ => return,
            },
            State::OverridePrompt { typed, .. } => match key {
                Key::Char(c) if !c.is_control() => {
                    if typed.chars().count() < TYPED_MAX {
                        typed.extend(c.to_uppercase());
                    }
                    return;
                }
                Key::Backspace => {
                    typed.pop();
                    return;
                }
                Key::Enter if typed == OVERRIDE => Answer::Override(true),
                Key::Enter => return typed.clear(),
                Key::Esc => Answer::Override(false),
                _ => return,
            },
            _ => return,
        };
        self.answer(answer);
    }

    /// Hands the operator's answer to the turn. A given password is kept for
    /// the root `Run` that follows, and dropped (wiped) if none does.
    fn answer(&mut self, answer: Answer) {
        let state = std::mem::replace(&mut self.state, State::Tools);
        let call_id = match state {
            State::Ask { call_id, .. } | State::OverridePrompt { call_id, .. } => call_id,
            State::Password { call_id, secret, .. } => {
                if matches!(answer, Answer::Password(true)) {
                    self.secret = Some((call_id.clone(), secret));
                }
                call_id
            }
            _ => return,
        };
        let online = self.audit.borrow().is_online();
        let Some(turn) = &mut self.turn else {
            self.secret = None;
            self.state = State::Ready;
            return;
        };
        let effects = match answer {
            Answer::Confirm(yes) => turn.on_confirm(&call_id, yes, online),
            Answer::Password(given) => turn.on_password(&call_id, given, online),
            Answer::Override(yes) => turn.on_override(&call_id, yes, online),
        };
        if effects.is_empty() {
            // The turn had no such prompt open: never leave the tab stuck in Tools.
            debug_assert!(false, "turn ignored the answer for call {call_id}");
            self.secret = None;
            self.interrupt();
            return;
        }
        self.apply(effects, None);
        self.secret = None;
    }

    /// Ctrl-C mid-inquiry: wipe any password, cancel the review, the command
    /// and the request, close the turn and mark it interrupted.
    pub(super) fn interrupt(&mut self) {
        if let State::Password { secret, .. } = &mut self.state {
            secret.clear();
        }
        self.secret = None;
        if let Some(abort) = self.review_abort.take() {
            abort();
        }
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        if let Some(abort) = self.abort.take() {
            abort();
        }
        self.abort_turn();
        self.end_early(None);
    }

    /// Resolve any open round as aborted (logged and audited); the caller ends the inquiry.
    pub(super) fn abort_turn(&mut self) {
        if let Some(mut turn) = self.turn.take() {
            let effects = turn.abort().into_iter().filter(|e| !matches!(e, Effect::Finished { .. })).collect();
            self.apply(effects, None);
        }
    }

    /// Carry out the turn's effects in order. `stop` is the stop of the stream
    /// that produced them, for the completion handling on `Finished`.
    fn apply(&mut self, effects: Vec<Effect>, stop: Option<&Stop>) {
        for e in effects {
            match e {
                Effect::Spawn { forced_final } => {
                    if self.revealing {
                        self.settle_segment();
                    }
                    self.spawn_request(forced_final);
                    // A failed window build ended the turn (end_early); nothing after it applies.
                    if self.turn.is_none() {
                        break;
                    }
                }
                Effect::Run { call_id, invocation } => {
                    // Only a root run gets the password just given for it.
                    let secret = match self.secret.take() {
                        Some((id, s)) if id == call_id && invocation.as_root() => Some(s),
                        _ => None,
                    };
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.cancel = Some(cancel.clone());
                    (self.deps.run_tool)(self.req_id, call_id, invocation, secret, cancel);
                    // Calls run one at a time: the status row now shows this one.
                    self.state = State::Tools;
                    self.started = Some((self.deps.now)());
                }
                Effect::Review { call_id, input } => {
                    self.review_abort = Some((self.deps.review)(self.req_id, call_id, input));
                    self.state = State::Reviewing;
                    self.started = Some((self.deps.now)());
                }
                // Prompts arm only from renders, so one raised in the background still gets its delay.
                Effect::Confirm { call_id, prompt, note } => {
                    self.state = State::Ask { call_id, prompt, note, drawn: false, armed_at: None };
                    self.activity = true;
                }
                Effect::AskPassword { call_id, prompt } => {
                    // Type-ahead must not be mistaken for (or end up next to) the password.
                    self.clear_input();
                    self.state = State::Password { call_id, prompt, secret: Secret::new(), drawn: false, armed_at: None };
                    self.activity = true;
                }
                Effect::AskOverride { call_id, prompt, note } => {
                    self.state = State::OverridePrompt { call_id, prompt, note, typed: String::new(), drawn: false, armed_at: None };
                    self.activity = true;
                }
                // Temporary: Task 8 replaces this with the real child turn.
                Effect::Delegate { call_id, .. } => {
                    let label = self.tech.label.clone();
                    let Some(turn) = self.turn.as_mut() else { continue };
                    let effects = turn.on_delegate(&call_id, DelegateOutcome::NoResponse { lines: vec![] }, &label);
                    self.apply(effects, None);
                    if self.turn.is_none() {
                        break;
                    }
                }
                Effect::Note(s) => {
                    if self.revealing {
                        self.settle_segment();
                    }
                    self.transcript.push(if self.direct() { Entry::Harness(s) } else { Entry::Note(s) });
                }
                Effect::LogTool(results) => {
                    let ts = (self.deps.timestamp)();
                    if let Err(e) = self.log.append_tool(&results, &ts, self.agent()) {
                        tracing::error!("chat log: cannot record tool results: {e}");
                    }
                }
                Effect::Audit(entry) => self.record_audit(&entry),
                // Its command is still running; its outcome comes back for this request.
                Effect::Orphan(o) => self.orphans.push((self.req_id, o)),
                Effect::Finished { truncated } => self.finish(stop, truncated),
            }
        }
    }

    fn record_audit(&mut self, entry: &crate::ops::audit::AuditEntry) {
        let ts = (self.deps.timestamp)();
        if let Err(e) = self.audit.borrow_mut().append(entry, &ts) {
            tracing::error!("audit log: cannot record tool call: {e}");
            self.status_flash = Some(AUDIT_OFFLINE.into());
        }
    }
}
