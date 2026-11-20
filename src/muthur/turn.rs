//! MU/TH/UR tool turns: carrying out `agent::Effect`s, reviews and the
//! operator's Y/N, password and OVERRIDE prompts.

use super::{Entry, MuthurTab, State};
use crate::agent::{ABORTED_ISSUED, Call, DelegateOutcome, Effect, Turn};
use crate::config::DelegateReport;
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

/// The screen line of a delegation that got no response.
pub(super) fn no_response(label: &str) -> String {
    format!("NO RESPONSE FROM {label}")
}

/// A technical turn running a task the persona delegated.
pub(super) struct Child {
    /// The persona's `delegate` call this answers (not unique: servers may repeat ids).
    call_id: String,
    /// The harness-unique delegation id: the child's log lane and its audit `delegated_by`.
    pub(super) delegation_id: String,
    turn: Turn,
    /// The text of the child's last reply: its report.
    report: String,
}

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
        let child = self.child.is_some();
        if !child {
            self.flush_cleaner();
        }
        let a = self.assistant(model, stop.log_str(), raw);
        self.log_assistant(a);
        if let Some(c) = &mut self.child {
            c.report = self.partial_raw.clone();
        }
        // Logged; an interrupt later in the turn must not log this text again.
        self.partial_raw.clear();
        self.abort = None;
        let calls = std::mem::take(&mut self.calls);
        let online = self.audit.borrow().is_online();
        let (defs, policy) = if self.tech_active() { (&self.tech.defs, &self.tech.policy) } else { (&self.tools, &self.policy) };
        let turn = match &mut self.child {
            Some(c) => Some(&mut c.turn),
            None => self.turn.as_mut(),
        };
        let effects = match turn {
            Some(t) => t.on_done(&stop, calls, defs, policy, online),
            None => vec![Effect::Finished { truncated: false }],
        };
        if !child && !effects.iter().any(|e| matches!(e, Effect::Finished { .. })) {
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
        let Some(effects) = self.active_turn().map(|t| t.on_tool_done(call_id, outcome)) else { return };
        self.cancel = None;
        self.apply(effects, None);
    }

    /// The reviewer's verdict for request `req_id`; stale ones, and any while
    /// no review is in flight, are ignored (the turn also checks the call).
    pub(super) fn review_done(&mut self, req_id: u64, call_id: &str, review: Review) {
        if req_id != self.req_id || self.state != State::Reviewing {
            return;
        }
        let online = self.audit.borrow().is_online();
        let Some(effects) = self.active_turn().map(|t| t.on_review(call_id, review.clone(), online)) else { return };
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
        let Some(turn) = self.active_turn() else {
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
        self.abort_child();
        self.abort_turn();
        self.end_early(None);
    }

    /// The child's turn while a delegation runs, else the inquiry's own.
    fn active_turn(&mut self) -> Option<&mut Turn> {
        match &mut self.child {
            Some(c) => Some(&mut c.turn),
            None => self.turn.as_mut(),
        }
    }

    /// Ctrl-C during a delegation: log any reply in progress and close the
    /// child's open round as aborted, in its own lane; the persona's waiting
    /// call is then aborted with the rest of its turn.
    fn abort_child(&mut self) {
        if self.child.is_none() {
            return;
        }
        if !self.partial_raw.is_empty() {
            let a = self.assistant(None, "interrupted", None);
            self.log_assistant(a);
            self.partial_raw.clear();
        }
        self.close_child();
    }

    /// Resolve the child's open round as aborted (in its lane) and take it.
    fn close_child(&mut self) -> Option<Child> {
        let effects = self.child.as_mut()?.turn.abort().into_iter().filter(|e| !matches!(e, Effect::Finished { .. })).collect();
        self.apply(effects, None);
        self.child.take()
    }

    /// A child request failed (stream error or window build, `cause`): the
    /// persona's call gets no response and its turn goes on.
    pub(super) fn child_failed(&mut self, cause: &str) {
        if let Some(c) = &self.child {
            tracing::warn!(delegation = %c.delegation_id, "delegated request failed: {cause}");
        }
        let stop = if self.partial_raw.is_empty() { "error" } else { "interrupted" };
        let a = self.assistant(None, stop, None);
        self.log_assistant(a);
        self.partial_raw.clear();
        self.abort = None;
        let Some(child) = self.close_child() else { return };
        let lines = child.turn.lines().to_vec();
        self.resume_persona(&child.call_id, DelegateOutcome::NoResponse { lines });
    }

    /// The child finished: show the report in verbatim mode and hand it to the persona.
    fn finish_child(&mut self, truncated: bool) {
        let Some(Child { call_id, turn, report, .. }) = self.child.take() else { return };
        let label = self.tech.label.clone();
        if self.tech.delegate_report == DelegateReport::Verbatim && !report.trim().is_empty() {
            self.transcript.push(Entry::Harness(format!("{label} REPORTS:")));
            self.transcript.extend(report.trim_end().lines().map(|l| Entry::Harness(l.to_string())));
        }
        let lines = turn.lines().to_vec();
        self.resume_persona(&call_id, DelegateOutcome::Report { lines, report, truncated });
    }

    /// Resolve the persona's delegated call and carry on with its turn. A
    /// delegation that got no response says so on screen.
    fn resume_persona(&mut self, call_id: &str, outcome: DelegateOutcome) {
        let label = self.tech.label.clone();
        if matches!(outcome, DelegateOutcome::NoResponse { .. }) {
            self.transcript.push(Entry::Harness(no_response(&label)));
        }
        let effects = self.turn.as_mut().map(|t| t.on_delegate(call_id, outcome, &label)).unwrap_or_default();
        if effects.is_empty() {
            // No call was waiting: never leave the tab stuck mid-inquiry.
            debug_assert!(false, "persona turn ignored the delegation outcome for {call_id}");
            self.finish(None, false);
            return;
        }
        self.state = State::Tools;
        self.apply(effects, None);
    }

    /// Hand `task` to a new child turn, or answer at once when the technical agent cannot be reached.
    fn delegate(&mut self, call_id: String, task: String, operator_message: String) {
        let label = self.tech.label.clone();
        if let Some(c) = &mut self.child {
            // A child cannot delegate (it has no such tool); never nest.
            let effects = c.turn.on_delegate(&call_id, DelegateOutcome::NoResponse { lines: vec![] }, &label);
            self.apply(effects, None);
            return;
        }
        if self.revealing {
            self.settle_segment();
        }
        if let Err(e) = self.tech.cfg.readiness() {
            tracing::warn!("cannot delegate: technical agent not ready: {e:?}");
            self.resume_persona(&call_id, DelegateOutcome::NoResponse { lines: vec![] });
            return;
        }
        self.ensure_tech_system();
        let ts = (self.deps.timestamp)();
        let verbatim = self.tech.delegate_report == DelegateReport::Verbatim;
        let delegation_id = match self.log.append_delegation(&call_id, &task, verbatim, &ts) {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("chat log: cannot record delegation: {e}");
                self.resume_persona(&call_id, DelegateOutcome::NoResponse { lines: vec![] });
                return;
            }
        };
        self.transcript.push(Entry::Harness(format!("DELEGATING TO {label}")));
        let turn = Turn::delegated(self.tech.max_rounds, operator_message, task, label, delegation_id.clone());
        self.child = Some(Child { call_id, delegation_id, turn, report: String::new() });
        self.spawn_request(self.tech.max_rounds == 0);
    }

    /// The child running now, by delegation id: the owner of a batch of effects.
    fn child_id(&self) -> Option<String> {
        self.child.as_ref().map(|c| c.delegation_id.clone())
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
        // The effects belong to the child running now, or to the persona's turn.
        // A persona batch holds at most one Delegate, and it comes last (calls run
        // one at a time), so stopping when the owner changes skips nothing of its.
        let owner = self.child_id();
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
                Effect::Delegate { call_id, task, operator_message } => {
                    self.delegate(call_id, task, operator_message);
                    if self.turn.is_none() {
                        break;
                    }
                }
                Effect::Note(s) => {
                    if self.revealing {
                        self.settle_segment();
                    }
                    self.transcript.push(if self.tech_active() { Entry::Harness(s) } else { Entry::Note(s) });
                }
                Effect::LogTool(results) => {
                    let ts = (self.deps.timestamp)();
                    if let Err(e) = self.log.append_tool(&results, &ts, &self.lane()) {
                        tracing::error!("chat log: cannot record tool results: {e}");
                    }
                }
                Effect::Audit(entry) => self.record_audit(&entry),
                // Its command is still running; its outcome comes back for this request.
                Effect::Orphan(o) => self.orphans.push((self.req_id, o)),
                Effect::Finished { truncated } if self.child.is_some() => {
                    self.finish_child(truncated || stop == Some(&Stop::MaxTokens));
                }
                Effect::Finished { truncated } => self.finish(stop, truncated),
            }
            // A child finished or failed (and maybe another started): the rest was its.
            if self.child_id() != owner {
                break;
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
