//! MU/TH/UR tool turns: carrying out `agent::Effect`s and the operator override.

use super::{unable, MuthurTab, State, NOTE_TRUNCATED};
use crate::agent::{Effect, ABORTED_ISSUED};
use crate::input::Key;
use crate::llm::{LlmError, Stop, ToolResult};
use crate::tools::exec::ToolOutcome;

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
        let effects = match &mut self.turn {
            Some(t) => t.on_done(&stop, calls, &self.tools, self.audit.borrow().is_online()),
            None => vec![Effect::Finished { truncated: false }],
        };
        if !effects.iter().any(|e| matches!(e, Effect::Finished { .. })) {
            // A tool round follows: the text so far goes above its notes.
            self.settle_segment();
        }
        self.apply(effects, Some(&stop));
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
        let effects = turn.on_tool_done(call_id, outcome);
        self.apply(effects, None);
    }

    pub(super) fn override_key(&mut self, key: &Key) {
        let State::Override { call_id, armed_at, .. } = &self.state else { return };
        // Aborting is always safe, so Ctrl-C works even while arming.
        if *key == Key::Ctrl('c') {
            self.interrupt();
            return;
        }
        // Unarmed until seen, then for ARMING after that.
        if armed_at.is_none_or(|t| (self.deps.now)() < t) {
            return;
        }
        let id = call_id.clone();
        match key {
            Key::Char('y' | 'Y') => self.answer(&id, true),
            Key::Char('n' | 'N') | Key::Esc => self.answer(&id, false),
            _ => {}
        }
    }

    fn answer(&mut self, call_id: &str, yes: bool) {
        let Some(turn) = &mut self.turn else {
            self.state = State::Ready;
            return;
        };
        self.state = State::Tools;
        let effects = turn.on_confirm(call_id, yes, self.audit.borrow().is_online());
        self.apply(effects, None);
    }

    /// Ctrl-C mid-inquiry: cancel the request, close the turn, mark it interrupted.
    pub(super) fn interrupt(&mut self) {
        if let Some(abort) = self.abort.take() {
            abort();
        }
        self.abort_turn();
        self.end_early(None);
    }

    /// Resolve any open round as aborted (logged and audited); the caller ends the inquiry.
    pub(super) fn abort_turn(&mut self) {
        if let Some(mut turn) = self.turn.take() {
            let effects = turn.abort(self.tools.elevate()).into_iter().filter(|e| !matches!(e, Effect::Finished { .. })).collect();
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
                    (self.deps.run_tool)(self.req_id, call_id, invocation);
                    // A pending override prompt keeps the status row.
                    if !matches!(self.state, State::Tools | State::Override { .. }) {
                        self.state = State::Tools;
                        self.started = Some((self.deps.now)());
                    }
                }
                Effect::Confirm { call_id, prompt } => {
                    // Armed only from renders, so a prompt raised in the background still gets its delay.
                    self.state = State::Override { call_id, prompt, drawn: false, armed_at: None };
                    self.activity = true;
                }
                Effect::Note(s) => {
                    if self.revealing {
                        self.settle_segment();
                    }
                    self.transcript.push(super::Entry::Note(s));
                }
                Effect::LogTool(results) => {
                    let ts = (self.deps.timestamp)();
                    if let Err(e) = self.log.append_tool(&results, &ts) {
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

    fn record_audit(&mut self, entry: &crate::tools::audit::AuditEntry) {
        let ts = (self.deps.timestamp)();
        if let Err(e) = self.audit.borrow_mut().append(entry, &ts) {
            tracing::error!("audit log: cannot record tool call: {e}");
            self.status_flash = Some(AUDIT_OFFLINE.into());
        }
    }

    /// The M4 completion path.
    fn finish(&mut self, stop: Option<&Stop>, truncated: bool) {
        match stop {
            Some(Stop::Refusal) => self.status_flash = Some(unable(LlmError::Restricted.reason())),
            Some(Stop::MaxTokens) => self.pending_notes.push(NOTE_TRUNCATED.into()),
            _ => {}
        }
        if truncated {
            self.pending_notes.push(NOTE_TRUNCATED.into());
        }
        self.abort = None;
        self.turn = None;
        self.state = State::Ready;
        self.activity = true;
        self.settle();
    }
}
