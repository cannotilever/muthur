//! Pure agent-turn state machine: turns stream endings, tool outcomes and
//! operator answers into `Effect`s for the tab to carry out. No I/O, no clocks.

use crate::llm::{Stop, ToolResult};
use crate::tools::audit::{AuditEntry, Decision};
use crate::tools::exec::ToolOutcome;
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
    Finished { truncated: bool },
}

const DENIED: &str = "DENIED BY OPERATOR";
const ABORTED: &str = "ABORTED BY OPERATOR";
const AUDIT_OFFLINE: &str = "AUDIT LOG OFFLINE";
const ROUND_LIMIT: &str = "ROUND LIMIT";

#[derive(Debug)]
enum State {
    /// Tier 2, waiting its turn for confirmation.
    Queued(Invocation),
    /// Tier 2, the operator is being asked.
    Awaiting(Invocation),
    Running { confirmed: bool },
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
        Turn { max_rounds, rounds: 0, forced: 0, slots: Vec::new() }
    }

    pub fn on_done(&mut self, stop: &Stop, calls: Vec<Call>, defs: &ToolDefs, audit_online: bool) -> Vec<Effect> {
        if *stop != Stop::ToolUse || calls.is_empty() {
            self.slots.clear();
            return vec![Effect::Finished { truncated: false }];
        }
        let mut out = Vec::new();
        self.slots.clear();
        let forced = self.forced > 0;
        for call in calls {
            let checked = defs.validate(&call.name, &call.input);
            let (label, tier) = match &checked {
                Ok(inv) => (inv.label(), inv.tier()),
                Err(_) => (call.name.to_uppercase(), rejected_tier(&call.name)),
            };
            let mut slot = Slot { call, label, tier, state: State::Done, result: None };
            match checked {
                _ if forced => slot.resolve_err(Decision::Rejected, format!("NOT AUTHORIZED: {ROUND_LIMIT}"), &mut out),
                Err(reason) => slot.resolve_err(Decision::Rejected, format!("NOT AUTHORIZED: {reason}"), &mut out),
                Ok(_) if tier == 2 && !audit_online => {
                    slot.resolve_err(Decision::Rejected, format!("NOT AUTHORIZED: {AUDIT_OFFLINE}"), &mut out)
                }
                Ok(inv) if tier == 2 => slot.state = State::Queued(inv),
                Ok(inv) => {
                    out.push(Effect::Note(format!("ACCESSING: {}", slot.label)));
                    out.push(Effect::Run { call_id: slot.call.id.clone(), invocation: inv });
                    slot.state = State::Running { confirmed: false };
                }
            }
            self.slots.push(slot);
        }
        self.advance(&mut out);
        out
    }

    pub fn on_tool_done(&mut self, call_id: &str, outcome: ToolOutcome) -> Vec<Effect> {
        let Some(slot) = self.slots.iter_mut().find(|s| s.call.id == call_id) else { return vec![] };
        let State::Running { confirmed } = slot.state else { return vec![] };
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

    pub fn on_confirm(&mut self, call_id: &str, yes: bool) -> Vec<Effect> {
        let Some(slot) = self.slots.iter_mut().find(|s| s.call.id == call_id) else { return vec![] };
        if !matches!(slot.state, State::Awaiting(_)) {
            return vec![];
        }
        let State::Awaiting(inv) = std::mem::replace(&mut slot.state, State::Done) else { unreachable!() };
        let mut out = Vec::new();
        if yes {
            out.push(Effect::Note(format!("ACCESSING: {}", slot.label)));
            out.push(Effect::Run { call_id: slot.call.id.clone(), invocation: inv });
            slot.state = State::Running { confirmed: true };
        } else {
            slot.resolve_err(Decision::Denied, DENIED.into(), &mut out);
        }
        self.advance(&mut out);
        out
    }

    pub fn abort(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        if !self.slots.is_empty() {
            for slot in &mut self.slots {
                if slot.result.is_none() {
                    slot.resolve(Decision::Aborted, ABORTED.into(), &mut out);
                }
            }
            let results = self.slots.drain(..).filter_map(|s| s.result).collect();
            out.push(Effect::LogTool(results));
        }
        out.push(Effect::Finished { truncated: false });
        out
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

    /// Resolves with an error result and an audit entry (no outcome attached).
    fn resolve(&mut self, decision: Decision, text: String, out: &mut Vec<Effect>) {
        out.push(Effect::Audit(AuditEntry {
            tool: self.call.name.clone(),
            args: self.call.input.clone(),
            tier: self.tier,
            decision,
            argv: None,
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
    use crate::config::{ServiceLists, ToolsSection};
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
        let e = t.on_confirm("c1", true);
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
        let e = t.on_confirm("c1", false);
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
        assert_eq!(t.on_confirm("c3", true), vec![]);
        let e = t.on_confirm("c1", true);
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
        let e = t.on_confirm("c3", false);
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
            t.abort(),
            vec![
                aborted("ship_status", json!({}), 1),
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
        assert_eq!(Turn::new(8).abort(), vec![Effect::Finished { truncated: false }]);
    }

    #[test]
    fn stale_outcome_ignored() {
        let mut t = Turn::new(8);
        assert_eq!(t.on_tool_done("nope", ok("x")), vec![]);
        tier1_round(&mut t, "c1");
        assert_eq!(t.on_tool_done("c1", ok("again")), vec![]);
    }
}
