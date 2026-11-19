//! Pure agent-turn state machine: turns stream endings, tool outcomes and
//! operator answers into `Effect`s for the tab to carry out. No I/O, no clocks.

use crate::llm::{Stop, ToolResult};
use crate::tools::audit::{AuditEntry, Decision};
use crate::config::Elevate;
use crate::tools::exec::{argv_for, ToolOutcome};
use crate::tools::{Invocation, ToolDefs};

#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Spawn { forced_final: bool },
    Run { call_id: String, invocation: Invocation },
    Confirm { call_id: String, prompt: String },
    Note(String),
    LogTool(Vec<ToolResult>),
    Audit(AuditEntry),
    /// A running call was aborted; its command cannot be cancelled, so its
    /// late outcome is still to be audited (see `Orphan::audit`).
    Orphan(Orphan),
    Finished { truncated: bool },
}

/// A call aborted while its command was running.
#[derive(Clone, Debug, PartialEq)]
pub struct Orphan {
    pub call_id: String,
    pub tool: String,
    pub args: serde_json::Value,
    pub invocation: Invocation,
}

impl Orphan {
    /// The audit record of the orphan's real outcome, once it arrives.
    pub fn audit(&self, outcome: ToolOutcome) -> AuditEntry {
        let tier = self.invocation.tier();
        AuditEntry {
            tool: self.tool.clone(),
            args: self.args.clone(),
            tier,
            decision: if tier == 2 { Decision::Confirmed } else { Decision::Auto },
            argv: outcome.argv,
            exit: outcome.exit,
            ms: outcome.ms,
            output: outcome.content,
        }
    }
}

const DENIED: &str = "DENIED BY OPERATOR";
const ABORTED: &str = "ABORTED BY OPERATOR";
/// A confirmed tier-2 command was already handed to the system when aborted.
pub const ABORTED_ISSUED: &str = "ABORTED BY OPERATOR — COMMAND ALREADY ISSUED, OUTCOME UNKNOWN";
const AUDIT_OFFLINE: &str = "AUDIT LOG OFFLINE";
const ROUND_LIMIT: &str = "ROUND LIMIT";
const DUPLICATE_ID: &str = "duplicate call id";

#[derive(Debug)]
enum State {
    /// Tier 2, waiting its turn for confirmation.
    Queued(Invocation),
    /// Tier 2, the operator is being asked.
    Awaiting(Invocation),
    Running { confirmed: bool, inv: Invocation },
    Done,
}

#[derive(Debug)]
struct Slot {
    call: Call,
    label: String,
    tier: u8,
    state: State,
    result: Option<ToolResult>,
}

pub struct Turn {
    max_rounds: u32,
    rounds: u32,
    /// Forced-final requests spawned so far (0, 1 or 2).
    forced: u8,
    /// The open round's calls, in call order; empty when none is open.
    slots: Vec<Slot>,
}

fn rejected_tier(name: &str) -> u8 {
    if matches!(name, "start_service" | "stop_service" | "restart_service") { 2 } else { 1 }
}

impl Turn {
    pub fn new(max_rounds: u32) -> Self {
        // With no rounds allowed, the very first request is already at the cap.
        Turn { max_rounds, rounds: 0, forced: u8::from(max_rounds == 0), slots: Vec::new() }
    }

    pub fn on_done(&mut self, stop: &Stop, calls: Vec<Call>, defs: &ToolDefs, audit_online: bool) -> Vec<Effect> {
        // A round still open when a new stream ends is closed out as aborted.
        let mut out = Vec::new();
        self.abort_open(defs.elevate(), &mut out);
        if *stop != Stop::ToolUse || calls.is_empty() {
            out.push(Effect::Finished { truncated: false });
            return out;
        }
        let forced = self.forced > 0;
        for call in calls {
            let duplicate = self.slots.iter().any(|s| s.call.id == call.id);
            let checked = defs.validate(&call.name, &call.input);
            let (label, tier) = match &checked {
                Ok(inv) => (inv.label(), inv.tier()),
                Err(_) => (call.name.to_uppercase(), rejected_tier(&call.name)),
            };
            let mut slot = Slot { call, label, tier, state: State::Done, result: None };
            match checked {
                _ if duplicate => {
                    slot.resolve_err(Decision::Rejected, format!("NOT AUTHORIZED: {DUPLICATE_ID}"), &mut out)
                }
                _ if forced => slot.resolve_err(Decision::Rejected, format!("NOT AUTHORIZED: {ROUND_LIMIT}"), &mut out),
                Err(reason) => slot.resolve_err(Decision::Rejected, format!("NOT AUTHORIZED: {reason}"), &mut out),
                Ok(_) if tier == 2 && !audit_online => {
                    slot.resolve_err(Decision::Rejected, format!("NOT AUTHORIZED: {AUDIT_OFFLINE}"), &mut out)
                }
                Ok(inv) if tier == 2 => slot.state = State::Queued(inv),
                Ok(inv) => {
                    out.push(Effect::Note(format!("ACCESSING: {}", slot.label)));
                    out.push(Effect::Run { call_id: slot.call.id.clone(), invocation: inv.clone() });
                    slot.state = State::Running { confirmed: false, inv };
                }
            }
            self.slots.push(slot);
        }
        self.advance(&mut out);
        out
    }

    pub fn on_tool_done(&mut self, call_id: &str, outcome: ToolOutcome) -> Vec<Effect> {
        let Some(slot) = self.live_slot(call_id) else { return vec![] };
        let State::Running { confirmed, .. } = slot.state else { return vec![] };
        let decision = if confirmed { Decision::Confirmed } else { Decision::Auto };
        let mut out = vec![Effect::Audit(AuditEntry {
            tool: slot.call.name.clone(),
            args: slot.call.input.clone(),
            tier: slot.tier,
            decision,
            argv: outcome.argv,
            exit: outcome.exit,
            ms: outcome.ms,
            output: outcome.content.clone(),
        })];
        slot.result = Some(slot.make_result(outcome.content, outcome.is_error));
        slot.state = State::Done;
        self.advance(&mut out);
        out
    }

    /// The operator's answer. `audit_online` is checked again here: a tier-2
    /// command never runs unless its audit record can be written.
    pub fn on_confirm(&mut self, call_id: &str, yes: bool, audit_online: bool) -> Vec<Effect> {
        let Some(slot) = self.live_slot(call_id) else { return vec![] };
        if !matches!(slot.state, State::Awaiting(_)) {
            return vec![];
        }
        let State::Awaiting(inv) = std::mem::replace(&mut slot.state, State::Done) else { unreachable!() };
        let mut out = Vec::new();
        if yes && !audit_online {
            slot.resolve_err(Decision::Rejected, format!("NOT AUTHORIZED: {AUDIT_OFFLINE}"), &mut out);
        } else if yes {
            out.push(Effect::Note(format!("ACCESSING: {}", slot.label)));
            out.push(Effect::Run { call_id: slot.call.id.clone(), invocation: inv.clone() });
            slot.state = State::Running { confirmed: true, inv };
        } else {
            slot.resolve_err(Decision::Denied, DENIED.into(), &mut out);
        }
        self.advance(&mut out);
        out
    }

    /// Ctrl-C. `elevate` names the command line an interrupted tier-2 call was issued with.
    pub fn abort(&mut self, elevate: Elevate) -> Vec<Effect> {
        let mut out = Vec::new();
        self.abort_open(elevate, &mut out);
        out.push(Effect::Finished { truncated: false });
        out
    }

    /// The unresolved slot for `call_id`. Rejected duplicates are resolved on
    /// arrival, so only the first call with a given id can match.
    fn live_slot(&mut self, call_id: &str) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.call.id == call_id && !matches!(s.state, State::Done))
    }

    /// Resolves every unresolved call of the open round as aborted, then logs
    /// the round. Does nothing when no round is open. Running calls cannot be
    /// cancelled: each becomes an `Orphan`, and a confirmed tier-2 one is
    /// recorded as issued with its outcome unknown.
    fn abort_open(&mut self, elevate: Elevate, out: &mut Vec<Effect>) {
        if self.slots.is_empty() {
            return;
        }
        for slot in &mut self.slots {
            if slot.result.is_some() {
                continue;
            }
            let running = match std::mem::replace(&mut slot.state, State::Done) {
                State::Running { confirmed, inv } => Some((confirmed, inv)),
                _ => None,
            };
            match running {
                Some((true, inv)) => {
                    out.push(Effect::Note(format!("ACCESS INTERRUPTED: {}", slot.label)));
                    slot.resolve_with(Decision::Aborted, argv_for(&inv, elevate), ABORTED_ISSUED.into(), out);
                    out.push(Effect::Orphan(slot.orphan(inv)));
                }
                Some((false, inv)) => {
                    slot.resolve(Decision::Aborted, ABORTED.into(), out);
                    out.push(Effect::Orphan(slot.orphan(inv)));
                }
                None => slot.resolve(Decision::Aborted, ABORTED.into(), out),
            }
        }
        let results = self.slots.drain(..).filter_map(|s| s.result).collect();
        out.push(Effect::LogTool(results));
    }

    pub fn awaiting_confirm(&self) -> Option<(&str, &str)> {
        self.slots
            .iter()
            .find(|s| matches!(s.state, State::Awaiting(_)))
            .map(|s| (s.call.id.as_str(), s.label.as_str()))
    }

    pub fn tools_running(&self) -> bool {
        self.slots.iter().any(|s| matches!(s.state, State::Running { .. }))
    }

    /// Asks for the next queued confirmation if none is pending; finishes the
    /// round once every call has a result.
    fn advance(&mut self, out: &mut Vec<Effect>) {
        if self.slots.is_empty() {
            return;
        }
        if self.awaiting_confirm().is_none()
            && let Some(slot) = self.slots.iter_mut().find(|s| matches!(s.state, State::Queued(_)))
        {
            let State::Queued(inv) = std::mem::replace(&mut slot.state, State::Done) else { unreachable!() };
            slot.state = State::Awaiting(inv);
            out.push(Effect::Confirm { call_id: slot.call.id.clone(), prompt: slot.label.clone() });
        }
        if self.slots.iter().any(|s| s.result.is_none()) {
            return;
        }
        let results = self.slots.drain(..).filter_map(|s| s.result).collect();
        out.push(Effect::LogTool(results));
        if self.forced > 0 {
            // A ToolUse in a forced-final response: one more forced request, then truncate.
            if self.forced >= 2 {
                out.push(Effect::Finished { truncated: true });
                return;
            }
            self.forced += 1;
            out.push(Effect::Spawn { forced_final: true });
            return;
        }
        self.rounds += 1;
        let forced_final = self.rounds >= self.max_rounds;
        if forced_final {
            self.forced = 1;
        }
        out.push(Effect::Spawn { forced_final });
    }
}

impl Slot {
    fn make_result(&self, content: String, is_error: bool) -> ToolResult {
        ToolResult {
            id: self.call.id.clone(),
            name: self.call.name.clone(),
            content,
            is_error,
            label: self.label.clone(),
        }
    }

    fn orphan(&self, invocation: Invocation) -> Orphan {
        Orphan { call_id: self.call.id.clone(), tool: self.call.name.clone(), args: self.call.input.clone(), invocation }
    }

    /// Resolves with an error result and an audit entry (no outcome attached).
    fn resolve(&mut self, decision: Decision, text: String, out: &mut Vec<Effect>) {
        self.resolve_with(decision, None, text, out);
    }

    /// `resolve`, with the argv of a command already issued.
    fn resolve_with(&mut self, decision: Decision, argv: Option<Vec<String>>, text: String, out: &mut Vec<Effect>) {
        out.push(Effect::Audit(AuditEntry {
            tool: self.call.name.clone(),
            args: self.call.input.clone(),
            tier: self.tier,
            decision,
            argv,
            exit: None,
            ms: 0,
            output: text.clone(),
        }));
        self.result = Some(self.make_result(text, true));
        self.state = State::Done;
    }

    /// `resolve`, preceded by the `ACCESS DENIED` note.
    fn resolve_err(&mut self, decision: Decision, text: String, out: &mut Vec<Effect>) {
        out.push(Effect::Note(format!("ACCESS DENIED: {}", self.label)));
        self.resolve(decision, text, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Elevate, ServiceLists, ToolsSection};
    use crate::tools::Verb;
    use serde_json::{Value, json};

    fn defs_restart(l: &[&str]) -> ToolDefs {
        ToolDefs::new(&ToolsSection {
            services: ServiceLists { restart: l.iter().map(|s| s.to_string()).collect(), ..Default::default() },
            ..Default::default()
        })
    }
    fn defs() -> ToolDefs {
        defs_restart(&["nginx"])
    }
    fn ok(content: &str) -> ToolOutcome {
        ToolOutcome {
            content: content.into(),
            is_error: false,
            argv: Some(vec!["systemctl".into(), "status".into()]),
            exit: Some(json!(0)),
            ms: 12,
        }
    }
    fn call(id: &str, name: &str, input: Value) -> Call {
        Call { id: id.into(), name: name.into(), input }
    }
    fn restart(id: &str) -> Call {
        call(id, "restart_service", json!({"unit": "nginx"}))
    }
    fn restart_inv() -> Invocation {
        Invocation::Service { verb: Verb::Restart, unit: "nginx.service".into() }
    }
    fn status(id: &str) -> Call {
        call(id, "service_status", json!({"unit": "nginx"}))
    }
    fn result(id: &str, name: &str, content: &str, is_error: bool, label: &str) -> ToolResult {
        ToolResult { id: id.into(), name: name.into(), content: content.into(), is_error, label: label.into() }
    }
    fn audits(effects: &[Effect]) -> Vec<&AuditEntry> {
        effects.iter().filter_map(|e| if let Effect::Audit(a) = e { Some(a) } else { None }).collect()
    }
    /// Runs one tier-1 round to completion; returns the effects of the finishing `on_tool_done`.
    fn tier1_round(t: &mut Turn, id: &str) -> Vec<Effect> {
        let e = t.on_done(&Stop::ToolUse, vec![status(id)], &defs(), true);
        assert!(matches!(e.last(), Some(Effect::Run { .. })), "{e:?}");
        t.on_tool_done(id, ok("active"))
    }

    #[test]
    fn final_answer_finishes() {
        assert_eq!(Turn::new(8).on_done(&Stop::End, vec![], &defs(), true), vec![Effect::Finished { truncated: false }]);
        assert_eq!(Turn::new(8).on_done(&Stop::ToolUse, vec![], &defs(), true), vec![Effect::Finished { truncated: false }]);
        assert_eq!(
            Turn::new(8).on_done(&Stop::MaxTokens, vec![status("c1")], &defs(), true),
            vec![Effect::Finished { truncated: false }]
        );
    }

    #[test]
    fn tier1_runs_then_spawns() {
        let mut t = Turn::new(8);
        let e = t.on_done(&Stop::ToolUse, vec![status("c1")], &defs(), true);
        assert_eq!(
            e,
            vec![
                Effect::Note("ACCESSING: SERVICE STATUS NGINX.SERVICE".into()),
                Effect::Run { call_id: "c1".into(), invocation: Invocation::ServiceStatus { unit: "nginx.service".into() } },
            ]
        );
        assert!(t.tools_running());
        assert_eq!(t.awaiting_confirm(), None);
        let e = t.on_tool_done("c1", ok("active"));
        assert_eq!(
            e,
            vec![
                Effect::Audit(AuditEntry {
                    tool: "service_status".into(),
                    args: json!({"unit": "nginx"}),
                    tier: 1,
                    decision: Decision::Auto,
                    argv: Some(vec!["systemctl".into(), "status".into()]),
                    exit: Some(json!(0)),
                    ms: 12,
                    output: "active".into(),
                }),
                Effect::LogTool(vec![result("c1", "service_status", "active", false, "SERVICE STATUS NGINX.SERVICE")]),
                Effect::Spawn { forced_final: false },
            ]
        );
        assert!(!t.tools_running());
    }

    #[test]
    fn tier2_confirm_yes_and_no() {
        // Yes.
        let mut t = Turn::new(8);
        let e = t.on_done(&Stop::ToolUse, vec![restart("c1")], &defs(), true);
        assert_eq!(e, vec![Effect::Confirm { call_id: "c1".into(), prompt: "RESTART NGINX.SERVICE".into() }]);
        assert_eq!(t.awaiting_confirm(), Some(("c1", "RESTART NGINX.SERVICE")));
        assert!(!t.tools_running());
        let e = t.on_confirm("c1", true, true);
        assert_eq!(
            e,
            vec![
                Effect::Note("ACCESSING: RESTART NGINX.SERVICE".into()),
                Effect::Run { call_id: "c1".into(), invocation: restart_inv() },
            ]
        );
        assert_eq!(t.awaiting_confirm(), None);
        assert!(t.tools_running());
        let e = t.on_tool_done("c1", ok(""));
        let a = audits(&e);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].decision, Decision::Confirmed);
        assert_eq!(a[0].tier, 2);
        assert!(matches!(e.last(), Some(Effect::Spawn { forced_final: false })));

        // No (second turn).
        let mut t = Turn::new(8);
        t.on_done(&Stop::ToolUse, vec![restart("c1")], &defs(), true);
        let e = t.on_confirm("c1", false, true);
        assert_eq!(
            e,
            vec![
                Effect::Note("ACCESS DENIED: RESTART NGINX.SERVICE".into()),
                Effect::Audit(AuditEntry {
                    tool: "restart_service".into(),
                    args: json!({"unit": "nginx"}),
                    tier: 2,
                    decision: Decision::Denied,
                    argv: None,
                    exit: None,
                    ms: 0,
                    output: "DENIED BY OPERATOR".into(),
                }),
                Effect::LogTool(vec![result("c1", "restart_service", "DENIED BY OPERATOR", true, "RESTART NGINX.SERVICE")]),
                Effect::Spawn { forced_final: false },
            ]
        );
        assert_eq!(t.awaiting_confirm(), None);
    }

    #[test]
    fn parallel_calls_tier2_confirmed_one_at_a_time() {
        let mut t = Turn::new(8);
        let calls = vec![restart("c1"), call("c2", "ship_status", json!({})), restart("c3")];
        let e = t.on_done(&Stop::ToolUse, calls, &defs(), true);
        assert_eq!(
            e,
            vec![
                Effect::Note("ACCESSING: SHIP STATUS".into()),
                Effect::Run { call_id: "c2".into(), invocation: Invocation::ShipStatus },
                Effect::Confirm { call_id: "c1".into(), prompt: "RESTART NGINX.SERVICE".into() },
            ]
        );
        assert_eq!(t.awaiting_confirm(), Some(("c1", "RESTART NGINX.SERVICE")));
        // Answering for the wrong call does nothing.
        assert_eq!(t.on_confirm("c3", true, true), vec![]);
        let e = t.on_confirm("c1", true, true);
        assert_eq!(
            e,
            vec![
                Effect::Note("ACCESSING: RESTART NGINX.SERVICE".into()),
                Effect::Run { call_id: "c1".into(), invocation: restart_inv() },
                Effect::Confirm { call_id: "c3".into(), prompt: "RESTART NGINX.SERVICE".into() },
            ]
        );
        assert_eq!(t.awaiting_confirm(), Some(("c3", "RESTART NGINX.SERVICE")));
        // Results arrive out of order.
        let e = t.on_tool_done("c2", ok("ship"));
        assert_eq!(audits(&e).len(), 1);
        assert!(!e.iter().any(|x| matches!(x, Effect::LogTool(_) | Effect::Spawn { .. })));
        let e = t.on_tool_done("c1", ok("restarted"));
        assert!(!e.iter().any(|x| matches!(x, Effect::LogTool(_) | Effect::Spawn { .. })));
        let e = t.on_confirm("c3", false, true);
        let n = e.len();
        assert!(matches!(e[n - 3], Effect::Audit(AuditEntry { decision: Decision::Denied, .. })), "{e:?}");
        let Effect::LogTool(r) = &e[n - 2] else { panic!("{e:?}") };
        let ids: Vec<&str> = r.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["c1", "c2", "c3"]);
        assert_eq!(r[1].content, "ship");
        assert_eq!(r[2].content, "DENIED BY OPERATOR");
        assert_eq!(e[n - 1], Effect::Spawn { forced_final: false });
    }

    #[test]
    fn invalid_call_gets_result_and_loop_continues() {
        let mut t = Turn::new(8);
        let e = t.on_done(&Stop::ToolUse, vec![call("c1", "service_status", json!({}))], &defs(), true);
        assert_eq!(
            e,
            vec![
                Effect::Note("ACCESS DENIED: SERVICE_STATUS".into()),
                Effect::Audit(AuditEntry {
                    tool: "service_status".into(),
                    args: json!({}),
                    tier: 1,
                    decision: Decision::Rejected,
                    argv: None,
                    exit: None,
                    ms: 0,
                    output: "NOT AUTHORIZED: missing unit".into(),
                }),
                Effect::LogTool(vec![result("c1", "service_status", "NOT AUTHORIZED: missing unit", true, "SERVICE_STATUS")]),
                Effect::Spawn { forced_final: false },
            ]
        );
        // A rejected service verb is audited as tier 2.
        let mut t = Turn::new(8);
        let e = t.on_done(&Stop::ToolUse, vec![call("c1", "stop_service", json!({"unit": "nginx"}))], &defs(), true);
        assert_eq!(e[0], Effect::Note("ACCESS DENIED: STOP_SERVICE".into()));
        assert_eq!(audits(&e)[0].tier, 2);
    }

    #[test]
    fn audit_offline_refuses_tier2() {
        let mut t = Turn::new(8);
        let e = t.on_done(&Stop::ToolUse, vec![restart("c1")], &defs(), false);
        assert!(!e.iter().any(|x| matches!(x, Effect::Confirm { .. } | Effect::Run { .. })), "{e:?}");
        assert_eq!(e[0], Effect::Note("ACCESS DENIED: RESTART NGINX.SERVICE".into()));
        let a = audits(&e);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].decision, Decision::Rejected);
        assert_eq!(a[0].tier, 2);
        assert_eq!(a[0].output, "NOT AUTHORIZED: AUDIT LOG OFFLINE");
        assert_eq!(
            e[e.len() - 2],
            Effect::LogTool(vec![result(
                "c1",
                "restart_service",
                "NOT AUTHORIZED: AUDIT LOG OFFLINE",
                true,
                "RESTART NGINX.SERVICE"
            )])
        );
        assert_eq!(e[e.len() - 1], Effect::Spawn { forced_final: false });
        assert_eq!(t.awaiting_confirm(), None);
        // Tier 1 still runs with the audit log offline.
        let mut t = Turn::new(8);
        let e = t.on_done(&Stop::ToolUse, vec![status("c1")], &defs(), false);
        assert!(matches!(e.last(), Some(Effect::Run { .. })));
    }

    #[test]
    fn round_cap_forces_final_then_truncates() {
        let mut t = Turn::new(2);
        assert_eq!(tier1_round(&mut t, "c1").last(), Some(&Effect::Spawn { forced_final: false }));
        assert_eq!(tier1_round(&mut t, "c2").last(), Some(&Effect::Spawn { forced_final: true }));
        // ToolUse in the forced final: rejected, logged, one more forced request.
        let e = t.on_done(&Stop::ToolUse, vec![status("c3")], &defs(), true);
        assert!(!e.iter().any(|x| matches!(x, Effect::Run { .. })), "{e:?}");
        assert_eq!(audits(&e)[0].decision, Decision::Rejected);
        let n = e.len();
        assert_eq!(
            e[n - 2],
            Effect::LogTool(vec![result(
                "c3",
                "service_status",
                "NOT AUTHORIZED: ROUND LIMIT",
                true,
                "SERVICE STATUS NGINX.SERVICE"
            )])
        );
        assert_eq!(e[n - 1], Effect::Spawn { forced_final: true });
        // Again: logged, then truncated.
        let e = t.on_done(&Stop::ToolUse, vec![status("c4")], &defs(), true);
        let n = e.len();
        let Effect::LogTool(r) = &e[n - 2] else { panic!("{e:?}") };
        assert_eq!(r[0].content, "NOT AUTHORIZED: ROUND LIMIT");
        assert_eq!(e[n - 1], Effect::Finished { truncated: true });
    }

    #[test]
    fn abort_while_running_and_confirming() {
        let mut t = Turn::new(8);
        t.on_done(&Stop::ToolUse, vec![call("c1", "ship_status", json!({})), restart("c2")], &defs(), true);
        assert!(t.tools_running());
        assert_eq!(t.awaiting_confirm(), Some(("c2", "RESTART NGINX.SERVICE")));
        let aborted = |tool: &str, args: Value, tier: u8| {
            Effect::Audit(AuditEntry {
                tool: tool.into(),
                args,
                tier,
                decision: Decision::Aborted,
                argv: None,
                exit: None,
                ms: 0,
                output: "ABORTED BY OPERATOR".into(),
            })
        };
        assert_eq!(
            t.abort(Elevate::Sudo),
            vec![
                aborted("ship_status", json!({}), 1),
                Effect::Orphan(Orphan {
                    call_id: "c1".into(),
                    tool: "ship_status".into(),
                    args: json!({}),
                    invocation: Invocation::ShipStatus,
                }),
                aborted("restart_service", json!({"unit": "nginx"}), 2),
                Effect::LogTool(vec![
                    result("c1", "ship_status", "ABORTED BY OPERATOR", true, "SHIP STATUS"),
                    result("c2", "restart_service", "ABORTED BY OPERATOR", true, "RESTART NGINX.SERVICE"),
                ]),
                Effect::Finished { truncated: false },
            ]
        );
        assert!(!t.tools_running());
        assert_eq!(t.awaiting_confirm(), None);
        // The late outcome of the aborted call is ignored.
        assert_eq!(t.on_tool_done("c1", ok("late")), vec![]);
        // With no open round, abort just finishes.
        assert_eq!(Turn::new(8).abort(Elevate::Sudo), vec![Effect::Finished { truncated: false }]);
    }

    #[test]
    fn abort_while_confirmed_tier2_runs_is_interrupted_not_denied() {
        let mut t = Turn::new(8);
        t.on_done(&Stop::ToolUse, vec![restart("c1")], &defs(), true);
        t.on_confirm("c1", true, true);
        assert!(t.tools_running());
        let e = t.abort(Elevate::Doas);
        let unknown = "ABORTED BY OPERATOR — COMMAND ALREADY ISSUED, OUTCOME UNKNOWN";
        let argv: Vec<String> =
            ["doas", "-n", "/usr/bin/systemctl", "restart", "nginx.service"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            e,
            vec![
                Effect::Note("ACCESS INTERRUPTED: RESTART NGINX.SERVICE".into()),
                Effect::Audit(AuditEntry {
                    tool: "restart_service".into(),
                    args: json!({"unit": "nginx"}),
                    tier: 2,
                    decision: Decision::Aborted,
                    argv: Some(argv.clone()),
                    exit: None,
                    ms: 0,
                    output: unknown.into(),
                }),
                Effect::Orphan(Orphan {
                    call_id: "c1".into(),
                    tool: "restart_service".into(),
                    args: json!({"unit": "nginx"}),
                    invocation: restart_inv(),
                }),
                Effect::LogTool(vec![result("c1", "restart_service", unknown, true, "RESTART NGINX.SERVICE")]),
                Effect::Finished { truncated: false },
            ]
        );
        // The late real outcome becomes a second audit entry for the orphan.
        let Effect::Orphan(o) = &e[2] else { panic!() };
        let late = ToolOutcome {
            content: "COMPLETED: RESTART NGINX.SERVICE".into(),
            is_error: false,
            argv: Some(argv.clone()),
            exit: Some(json!(0)),
            ms: 900,
        };
        assert_eq!(
            o.audit(late),
            AuditEntry {
                tool: "restart_service".into(),
                args: json!({"unit": "nginx"}),
                tier: 2,
                decision: Decision::Confirmed,
                argv: Some(argv),
                exit: Some(json!(0)),
                ms: 900,
                output: "COMPLETED: RESTART NGINX.SERVICE".into(),
            }
        );
        // The turn itself ignores it.
        assert_eq!(t.on_tool_done("c1", ok("late")), vec![]);
        // A tier-1 orphan's late outcome is audited as auto.
        let o = Orphan { call_id: "c2".into(), tool: "ship_status".into(), args: json!({}), invocation: Invocation::ShipStatus };
        assert_eq!((o.audit(ok("x")).decision, o.audit(ok("x")).tier), (Decision::Auto, 1));
    }

    #[test]
    fn audit_offline_at_confirmation_refuses_tier2() {
        let mut t = Turn::new(8);
        t.on_done(&Stop::ToolUse, vec![restart("c1")], &defs(), true);
        assert_eq!(t.awaiting_confirm(), Some(("c1", "RESTART NGINX.SERVICE")));
        // The audit log went offline while the operator was being asked.
        let e = t.on_confirm("c1", true, false);
        assert!(!e.iter().any(|x| matches!(x, Effect::Run { .. })), "{e:?}");
        assert_eq!(e[0], Effect::Note("ACCESS DENIED: RESTART NGINX.SERVICE".into()));
        let a = audits(&e);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].decision, Decision::Rejected);
        assert_eq!(a[0].output, "NOT AUTHORIZED: AUDIT LOG OFFLINE");
        let Effect::LogTool(r) = &e[e.len() - 2] else { panic!("{e:?}") };
        assert_eq!(r[0].content, "NOT AUTHORIZED: AUDIT LOG OFFLINE");
        assert_eq!(e[e.len() - 1], Effect::Spawn { forced_final: false });
        assert!(!t.tools_running());
        // Saying no while offline is still a plain denial.
        let mut t = Turn::new(8);
        t.on_done(&Stop::ToolUse, vec![restart("c1")], &defs(), true);
        assert_eq!(audits(&t.on_confirm("c1", false, false))[0].decision, Decision::Denied);
    }

    #[test]
    fn duplicate_tier1_ids_rejected_after_first() {
        for id in ["c1", ""] {
            let mut t = Turn::new(8);
            let e = t.on_done(&Stop::ToolUse, vec![status(id), call(id, "ship_status", json!({}))], &defs(), true);
            assert_eq!(
                e,
                vec![
                    Effect::Note("ACCESSING: SERVICE STATUS NGINX.SERVICE".into()),
                    Effect::Run { call_id: id.into(), invocation: Invocation::ServiceStatus { unit: "nginx.service".into() } },
                    Effect::Note("ACCESS DENIED: SHIP STATUS".into()),
                    Effect::Audit(AuditEntry {
                        tool: "ship_status".into(),
                        args: json!({}),
                        tier: 1,
                        decision: Decision::Rejected,
                        argv: None,
                        exit: None,
                        ms: 0,
                        output: "NOT AUTHORIZED: duplicate call id".into(),
                    }),
                ]
            );
            let e = t.on_tool_done(id, ok("active"));
            assert_eq!(audits(&e)[0].decision, Decision::Auto);
            assert_eq!(
                e[e.len() - 2],
                Effect::LogTool(vec![
                    result(id, "service_status", "active", false, "SERVICE STATUS NGINX.SERVICE"),
                    result(id, "ship_status", "NOT AUTHORIZED: duplicate call id", true, "SHIP STATUS"),
                ])
            );
            assert_eq!(e[e.len() - 1], Effect::Spawn { forced_final: false });
        }
    }

    #[test]
    fn duplicate_id_does_not_block_tier2() {
        for id in ["c1", ""] {
            let mut t = Turn::new(8);
            let e = t.on_done(&Stop::ToolUse, vec![restart(id), status(id)], &defs(), true);
            assert!(!e.iter().any(|x| matches!(x, Effect::Run { .. })), "{e:?}");
            assert_eq!(e[0], Effect::Note("ACCESS DENIED: SERVICE STATUS NGINX.SERVICE".into()));
            assert_eq!(audits(&e)[0].output, "NOT AUTHORIZED: duplicate call id");
            assert_eq!(e.last(), Some(&Effect::Confirm { call_id: id.into(), prompt: "RESTART NGINX.SERVICE".into() }));
            assert_eq!(t.awaiting_confirm(), Some((id, "RESTART NGINX.SERVICE")));
            assert_eq!(t.on_confirm(id, true, true)[1], Effect::Run { call_id: id.into(), invocation: restart_inv() });
            let e = t.on_tool_done(id, ok("restarted"));
            assert_eq!(audits(&e)[0].decision, Decision::Confirmed);
            let Effect::LogTool(r) = &e[e.len() - 2] else { panic!("{e:?}") };
            assert_eq!(r.len(), 2);
            assert_eq!(r[0].content, "restarted");
            assert_eq!(r[1].content, "NOT AUTHORIZED: duplicate call id");
            assert_eq!(e[e.len() - 1], Effect::Spawn { forced_final: false });
        }
    }

    #[test]
    fn new_stream_closes_open_round_as_aborted() {
        let mut t = Turn::new(8);
        t.on_done(&Stop::ToolUse, vec![status("c1"), restart("c2")], &defs(), true);
        let e = t.on_done(&Stop::ToolUse, vec![call("c3", "ship_status", json!({}))], &defs(), true);
        assert_eq!(audits(&e).iter().take(2).map(|a| a.decision).collect::<Vec<_>>(), [Decision::Aborted; 2]);
        // The running tier-1 call is orphaned; the queued tier-2 call never ran.
        assert!(matches!(&e[1], Effect::Orphan(o) if o.call_id == "c1"), "{e:?}");
        let e = &e[1..];
        assert_eq!(
            e[2],
            Effect::LogTool(vec![
                result("c1", "service_status", "ABORTED BY OPERATOR", true, "SERVICE STATUS NGINX.SERVICE"),
                result("c2", "restart_service", "ABORTED BY OPERATOR", true, "RESTART NGINX.SERVICE"),
            ])
        );
        assert_eq!(
            e[3..],
            [Effect::Note("ACCESSING: SHIP STATUS".into()), Effect::Run { call_id: "c3".into(), invocation: Invocation::ShipStatus }]
        );
        assert_eq!(t.awaiting_confirm(), None);
        assert_eq!(t.on_tool_done("c1", ok("late")), vec![]);
        // A final answer over an open round: logged as aborted, then Finished.
        let e = t.on_done(&Stop::End, vec![], &defs(), true);
        assert_eq!(audits(&e).len(), 1);
        assert!(matches!(&e[1], Effect::Orphan(o) if o.call_id == "c3"), "{e:?}");
        assert_eq!(e[2], Effect::LogTool(vec![result("c3", "ship_status", "ABORTED BY OPERATOR", true, "SHIP STATUS")]));
        assert_eq!(e[3], Effect::Finished { truncated: false });
    }

    #[test]
    fn max_rounds_zero_and_one() {
        // 0: the first ToolUse already gets the forced-final treatment.
        let mut t = Turn::new(0);
        let e = t.on_done(&Stop::ToolUse, vec![status("c1")], &defs(), true);
        assert!(!e.iter().any(|x| matches!(x, Effect::Run { .. })), "{e:?}");
        assert_eq!(audits(&e)[0].output, "NOT AUTHORIZED: ROUND LIMIT");
        assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: true }));
        let e = t.on_done(&Stop::ToolUse, vec![status("c2")], &defs(), true);
        assert_eq!(e.last(), Some(&Effect::Finished { truncated: true }));
        // 1: one real round, then forced.
        let mut t = Turn::new(1);
        assert_eq!(tier1_round(&mut t, "c1").last(), Some(&Effect::Spawn { forced_final: true }));
        let e = t.on_done(&Stop::ToolUse, vec![status("c2")], &defs(), true);
        assert_eq!(audits(&e)[0].output, "NOT AUTHORIZED: ROUND LIMIT");
        assert_eq!(e.last(), Some(&Effect::Spawn { forced_final: true }));
        let e = t.on_done(&Stop::ToolUse, vec![status("c3")], &defs(), true);
        assert_eq!(e.last(), Some(&Effect::Finished { truncated: true }));
    }

    #[test]
    fn stale_outcome_ignored() {
        let mut t = Turn::new(8);
        assert_eq!(t.on_tool_done("nope", ok("x")), vec![]);
        tier1_round(&mut t, "c1");
        assert_eq!(t.on_tool_done("c1", ok("again")), vec![]);
    }
}
