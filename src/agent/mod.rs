//! Pure agent-turn state machine: turns stream endings, review verdicts, tool
//! outcomes and operator answers into `Effect`s for the tab to carry out.
//! No I/O, no clocks. Calls are processed one at a time, in call order.

use crate::llm::{Stop, ToolResult};
use crate::ops::AgentId;
use crate::ops::audit::{AuditEntry, Decision};
use crate::review::{PriorCall, Review, ReviewInput, Verdict};
use crate::tools::deny::DenyRules;
use crate::tools::{Gate, Invocation, NetTarget, ToolDefs, ToolOutcome, netgate, shell};

#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Deterministic checks applied before review.
pub struct Policy {
    /// Deny rules for `run`; None skips them.
    pub deny: Option<DenyRules>,
    /// Network targets that count as known (`[agent] net_allow`).
    pub net_allow: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)] // short-lived; carried straight to the tab
pub enum Effect {
    Spawn {
        forced_final: bool,
    },
    /// Ask the reviewer about `call_id`; the verdict comes back through `on_review`.
    Review {
        call_id: String,
        input: ReviewInput,
    },
    /// Y/N prompt: `prompt` is the command or tool label, `note` the reviewer's note.
    Confirm {
        call_id: String,
        prompt: String,
        note: String,
    },
    /// The operator's password for a root call; `given = false` means Esc.
    AskPassword {
        call_id: String,
        prompt: String,
    },
    /// The reviewer denied; the operator may type OVERRIDE. `prompt` is the command or tool label.
    AskOverride {
        call_id: String,
        prompt: String,
        note: String,
    },
    Run {
        call_id: String,
        invocation: Invocation,
    },
    /// Hand `task` to the technical agent; its outcome comes back through `on_delegate`.
    /// `operator_message` is the operator's own line, for the child's authority checks.
    Delegate {
        call_id: String,
        task: String,
        operator_message: String,
    },
    Note(String),
    LogTool(Vec<ToolResult>),
    Audit(AuditEntry),
    /// A running call was aborted; its late outcome is still to be audited
    /// (see `Orphan::audit`).
    Orphan(Orphan),
    Finished {
        truncated: bool,
    },
}

/// How a delegation ended, as the persona's tool result.
#[derive(Clone, Debug, PartialEq)]
pub enum DelegateOutcome {
    Report { lines: Vec<String>, report: String, truncated: bool },
    NoResponse { lines: Vec<String> },
}

impl DelegateOutcome {
    /// The tool-result text for the persona; `officer` is the agent's label.
    pub fn content(&self, officer: &str) -> String {
        let (lines, tail) = match self {
            DelegateOutcome::Report { lines, report, truncated } => (lines, format!("REPORT{}:\n{report}", if *truncated { " (TRUNCATED)" } else { "" })),
            DelegateOutcome::NoResponse { lines } => (lines, format!("NO RESPONSE FROM {officer}")),
        };
        let commands = if lines.is_empty() { "NONE".to_string() } else { lines.join("\n") };
        format!("COMMANDS:\n{commands}\n{tail}")
    }
}

/// A call aborted while its command was running.
#[derive(Clone, Debug, PartialEq)]
pub struct Orphan {
    pub call_id: String,
    pub invocation: Invocation,
    /// The audit record it was running under, without its outcome.
    base: AuditEntry,
}

impl Orphan {
    /// The audit record of the orphan's real outcome, once it arrives, under
    /// the decision it was running with.
    pub fn audit(&self, outcome: ToolOutcome) -> AuditEntry {
        AuditEntry {
            argv: outcome.argv,
            exit: outcome.exit,
            ms: outcome.ms,
            output: outcome.audit_output.unwrap_or(outcome.content),
            ..self.base.clone()
        }
    }
}

const DENIED: &str = "DENIED BY OPERATOR";
const ABORTED: &str = "ABORTED BY OPERATOR";
/// A root command was already handed to the system when aborted.
pub const ABORTED_ISSUED: &str = "ABORTED BY OPERATOR — COMMAND ALREADY ISSUED, OUTCOME UNKNOWN";
const AUDIT_OFFLINE: &str = "AUDIT LOG OFFLINE";
const ROUND_LIMIT: &str = "ROUND LIMIT";
const DUPLICATE_ID: &str = "duplicate call id";
const UNLISTED: &str = "UNLISTED TARGET";
const DENY_OFFLINE: &str = "DENY RULES OFFLINE";

#[derive(Debug)]
enum State {
    /// Validated, waiting for the calls before it.
    Pending(Invocation),
    Reviewing(Invocation),
    Confirming(Invocation),
    /// `overridden`: reached through OVERRIDE.
    Password {
        inv: Invocation,
        overridden: bool,
    },
    Override(Invocation),
    /// Handed to the technical agent; waiting for its outcome.
    Delegating,
    Running {
        inv: Invocation,
        decision: Decision,
    },
    Done,
}

#[derive(Debug)]
struct Slot {
    call: Call,
    /// The invocation's label, or the tool name for an invalid call.
    label: String,
    /// The validated invocation, if the call was valid.
    inv: Option<Invocation>,
    unknown_target: bool,
    deny_rule: Option<String>,
    review: Option<Review>,
    /// Technical agent: the start line as drawn, without the REVIEW line.
    line: Option<String>,
    state: State,
    result: Option<ToolResult>,
    /// Set in a delegated turn: the officer's label, the delegation id,
    /// and the condensed line announced for this call.
    delegation: Option<SlotDelegation>,
}

#[derive(Debug)]
struct SlotDelegation {
    officer: String,
    by: String,
    line: Option<String>,
}

/// A delegated turn's condensed lines announced so far.
struct Delegation {
    lines: Vec<String>,
}

/// The persona-facing line of a delegated call: the start line without its
/// `RUN  ` / `RUN# ` prefix, under the officer's label.
pub fn condensed(officer: &str, start_line: &str) -> String {
    let rest = start_line
        .strip_prefix("RUN  ")
        .or_else(|| start_line.strip_prefix("RUN# "))
        .unwrap_or(start_line);
    format!("{officer}: {rest}")
}

pub struct Turn {
    max_rounds: u32,
    rounds: u32,
    /// Forced-final requests spawned so far (0, 1 or 2).
    forced: u8,
    agent: AgentId,
    operator_message: String,
    /// Set for a turn run on MU/TH/UR's behalf.
    delegation: Option<Delegation>,
    /// The delegated task, for the reviewer only; never an authority.
    delegated_task: Option<String>,
    /// The officer's label and the delegation id, copied into each slot.
    slot_delegation: Option<(String, String)>,
    /// Every call of this turn with a result, across rounds, for the reviewer.
    prior: Vec<PriorCall>,
    /// The open round's calls, in call order; empty when none is open.
    slots: Vec<Slot>,
}

/// The line that replaces a finished `run`'s EXIT line, or the EXIT line itself.
fn after_line(o: &ToolOutcome) -> String {
    let c = &o.content;
    if c.starts_with(ABORTED_ISSUED) {
        return "OUTCOME UNKNOWN".into();
    }
    if c.starts_with(ABORTED) {
        return "ABORTED".into();
    }
    if !c.starts_with("EXIT ") {
        return c.lines().next().unwrap_or("").to_string();
    }
    let exit = o.exit.as_ref().and_then(serde_json::Value::as_i64).map_or("?".to_string(), |n| n.to_string());
    let text = o.audit_output.as_deref().unwrap_or(c);
    // The capture keeps only the head and tail of a big output, so its count
    // is a floor.
    let more = if text.contains(" BYTES OMITTED …]\n") { "+" } else { "" };
    format!("EXIT {exit}  {}{more} LINES  {:.1} S", text.lines().count(), o.ms as f64 / 1000.0)
}

impl Slot {
    fn active(&self) -> bool {
        !matches!(self.state, State::Pending(_) | State::Done)
    }

    fn prompting(&self) -> bool {
        matches!(self.state, State::Confirming(_) | State::Password { .. } | State::Override(_))
    }

    /// The audit record under `decision`, without an outcome.
    fn base_audit(&self, agent: AgentId, decision: Decision) -> AuditEntry {
        let (as_root, reason) = match &self.inv {
            Some(Invocation::Run { as_root, reason, .. }) => (*as_root, Some(reason.clone())),
            _ => (false, None),
        };
        AuditEntry {
            agent: Some(agent),
            as_root,
            reason,
            deny_rule: self.deny_rule.clone(),
            review: self.review.as_ref().map(Review::record),
            delegated_by: self.delegation.as_ref().map(|d| d.by.clone()),
            ..AuditEntry::new(&self.call.name, self.call.input.clone(), decision)
        }
    }

    /// The technical start line: `RUN  <cmd>  [<tag>]`, `RUN# <cmd>  [ROOT, <tag>]`,
    /// `<label>  [<tag>]`, or just the label for a review-free tool.
    fn start_line(&self, tag: Option<&str>) -> String {
        match (&self.inv, tag) {
            (Some(Invocation::Run { command, as_root: true, .. }), Some(t)) => format!("RUN# {command}  [ROOT, {t}]"),
            (Some(Invocation::Run { command, .. }), Some(t)) => format!("RUN  {command}  [{t}]"),
            (_, Some(t)) => format!("{}  [{t}]", self.label),
            (_, None) => self.label.clone(),
        }
    }

    /// The tag of a call about to run, from its review; None when review-free.
    fn run_tag(&self, decision: Decision) -> Option<String> {
        let r = self.review.as_ref()?;
        Some(match (decision, r.verdict) {
            (Decision::Overridden, _) => "OVERRIDDEN".into(),
            (_, Verdict::Allow) => "ALLOW".into(),
            (_, _) => format!("ASK: {}", r.risk.upper()),
        })
    }

    /// Technical agent: the start line and, for reviewed calls, the REVIEW line.
    fn announce(&mut self, agent: AgentId, tag: Option<&str>, out: &mut Vec<Effect>) {
        if agent != AgentId::Tech {
            return;
        }
        let line = self.start_line(tag);
        if let Some(d) = &mut self.delegation {
            let c = condensed(&d.officer, &line);
            out.push(Effect::Note(c.clone()));
            d.line = Some(c);
        } else {
            out.push(Effect::Note(line.clone()));
            if let Some(r) = &self.review {
                out.push(Effect::Note(format!("     REVIEW: {} -- {}", r.risk.upper(), r.note)));
            }
        }
        self.line = Some(line);
    }

    fn make_result(&self, content: String, is_error: bool) -> ToolResult {
        ToolResult {
            id: self.call.id.clone(),
            name: self.call.name.clone(),
            content,
            is_error,
            label: self.line.clone().unwrap_or_else(|| self.label.clone()),
        }
    }

    fn resolved(&mut self, result: ToolResult, decision: Decision, exit: Option<serde_json::Value>, prior: &mut Vec<PriorCall>) {
        prior.push(PriorCall {
            tool: self.call.name.clone(),
            args: self.call.input.clone(),
            decision,
            exit,
        });
        self.result = Some(result);
        self.state = State::Done;
    }

    fn run(&mut self, agent: AgentId, inv: Invocation, decision: Decision, out: &mut Vec<Effect>) {
        if agent == AgentId::Tech {
            let tag = self.run_tag(decision);
            self.announce(agent, tag.as_deref(), out);
        } else {
            out.push(Effect::Note(format!("ACCESSING: {}", self.label)));
        }
        out.push(Effect::Run {
            call_id: self.call.id.clone(),
            invocation: inv.clone(),
        });
        self.state = State::Running { inv, decision };
    }

    /// Resolves without running: notes, audit, an error result.
    fn refuse(&mut self, agent: AgentId, decision: Decision, text: String, tag: &str, prior: &mut Vec<PriorCall>, out: &mut Vec<Effect>) {
        if agent == AgentId::Tech {
            self.announce(agent, Some(tag), out);
        } else if decision != Decision::Aborted {
            out.push(Effect::Note(format!("ACCESS DENIED: {}", self.label)));
        }
        out.push(Effect::Audit(AuditEntry {
            output: text.clone(),
            ..self.base_audit(agent, decision)
        }));
        let r = self.make_result(text, true);
        self.resolved(r, decision, None, prior);
    }

    fn refuse_offline(&mut self, agent: AgentId, prior: &mut Vec<PriorCall>, out: &mut Vec<Effect>) {
        let tag = format!("DENIED: {AUDIT_OFFLINE}");
        self.refuse(agent, Decision::Rejected, format!("NOT AUTHORIZED: {AUDIT_OFFLINE}"), &tag, prior, out);
    }

    /// Root calls need the operator's password, and an audit log to record them.
    fn ask_password(&mut self, agent: AgentId, inv: Invocation, overridden: bool, audit_online: bool, prior: &mut Vec<PriorCall>, out: &mut Vec<Effect>) {
        if !audit_online {
            return self.refuse_offline(agent, prior, out);
        }
        out.push(Effect::AskPassword {
            call_id: self.call.id.clone(),
            prompt: self.label.clone(),
        });
        self.state = State::Password { inv, overridden };
    }

    fn finish(&mut self, agent: AgentId, outcome: ToolOutcome, prior: &mut Vec<PriorCall>, out: &mut Vec<Effect>) {
        let State::Running { inv, decision } = std::mem::replace(&mut self.state, State::Done) else {
            return;
        };
        let argv = match &inv {
            Invocation::Run { command, as_root, .. } => {
                if agent == AgentId::Tech && self.delegation.is_none() {
                    out.push(Effect::Note(after_line(&outcome)));
                }
                Some(shell::argv(command, *as_root))
            }
            _ => outcome.argv,
        };
        out.push(Effect::Audit(AuditEntry {
            argv,
            exit: outcome.exit.clone(),
            ms: outcome.ms,
            output: outcome.audit_output.unwrap_or_else(|| outcome.content.clone()),
            ..self.base_audit(agent, decision)
        }));
        let r = self.make_result(outcome.content, outcome.is_error);
        self.resolved(r, decision, outcome.exit, prior);
    }

    /// Aborts this unresolved call. A running command cannot be cancelled here:
    /// it becomes an `Orphan`, and a root one is recorded as already issued.
    fn abort(&mut self, agent: AgentId, prior: &mut Vec<PriorCall>, out: &mut Vec<Effect>) {
        let State::Running { inv, decision } = std::mem::replace(&mut self.state, State::Done) else {
            return self.refuse(agent, Decision::Aborted, ABORTED.into(), "ABORTED", prior, out);
        };
        let tech = agent == AgentId::Tech;
        let (text, argv) = match &inv {
            Invocation::Run { command, as_root: true, .. } => {
                if self.delegation.is_none() {
                    out.push(Effect::Note(if tech {
                        "OUTCOME UNKNOWN".into()
                    } else {
                        format!("ACCESS INTERRUPTED: {}", self.label)
                    }));
                }
                (ABORTED_ISSUED, Some(shell::argv(command, true)))
            }
            // Every tech-mode call shows its RUN line, so it needs an end line.
            _ if tech => {
                if self.delegation.is_none() {
                    out.push(Effect::Note("ABORTED".into()));
                }
                (ABORTED, None)
            }
            _ => (ABORTED, None),
        };
        out.push(Effect::Audit(AuditEntry {
            argv,
            output: text.into(),
            ..self.base_audit(agent, Decision::Aborted)
        }));
        let base = self.base_audit(agent, decision);
        out.push(Effect::Orphan(Orphan {
            call_id: self.call.id.clone(),
            invocation: inv,
            base,
        }));
        let r = self.make_result(text.into(), true);
        self.resolved(r, Decision::Aborted, None, prior);
    }
}

impl Turn {
    pub fn new(max_rounds: u32, agent: AgentId, operator_message: String) -> Self {
        // With no rounds allowed, the very first request is already at the cap.
        Turn {
            max_rounds,
            rounds: 0,
            forced: u8::from(max_rounds == 0),
            agent,
            operator_message,
            delegation: None,
            delegated_task: None,
            slot_delegation: None,
            prior: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// A technical turn run for MU/TH/UR. `operator_message` is the operator's
    /// own line and the only authority; `task` is MU/TH/UR's wording, shown to
    /// the reviewer apart and as untrusted text. `delegated_by` is the
    /// delegation id (the persona's call id, then a harness-unique suffix).
    pub fn delegated(max_rounds: u32, operator_message: String, task: String, officer: String, delegated_by: String) -> Self {
        let mut t = Turn::new(max_rounds, AgentId::Tech, operator_message);
        t.delegated_task = Some(task);
        t.delegation = Some(Delegation { lines: Vec::new() });
        t.slot_delegation = Some((officer, delegated_by));
        t
    }

    /// The condensed lines of closed rounds; complete once the turn has emitted
    /// `Finished`. Empty for a turn that is not delegated.
    pub fn lines(&self) -> &[String] {
        self.delegation.as_ref().map_or(&[], |d| &d.lines)
    }

    /// Drains the finished round's results, keeping its condensed lines.
    fn close_round(&mut self) -> Vec<ToolResult> {
        if let Some(d) = &mut self.delegation {
            d.lines
                .extend(self.slots.iter_mut().filter_map(|s| s.delegation.as_mut().and_then(|x| x.line.take())));
        }
        self.slots.drain(..).filter_map(|s| s.result).collect()
    }

    /// A stream ended. Every call is validated and deny-checked now; the valid
    /// ones then go through review and prompts one at a time, in call order.
    pub fn on_done(&mut self, stop: &Stop, calls: Vec<Call>, defs: &ToolDefs, policy: &Policy, audit_online: bool) -> Vec<Effect> {
        // A round still open when a new stream ends is closed out as aborted.
        let mut out = Vec::new();
        self.abort_open(&mut out);
        if *stop != Stop::ToolUse || calls.is_empty() {
            out.push(Effect::Finished { truncated: false });
            return out;
        }
        let _ = audit_online; // checked when a root call or OVERRIDE is raised
        let forced = self.forced > 0;
        for call in calls {
            let duplicate = self.slots.iter().any(|s| s.call.id == call.id);
            let checked = defs.validate(&call.name, &call.input);
            let mut slot = Slot {
                label: call.name.to_uppercase(),
                call,
                inv: None,
                unknown_target: false,
                deny_rule: None,
                review: None,
                line: None,
                state: State::Done,
                result: None,
                delegation: self.slot_delegation.as_ref().map(|(officer, by)| SlotDelegation {
                    officer: officer.clone(),
                    by: by.clone(),
                    line: None,
                }),
            };
            if let Ok(inv) = &checked {
                slot.label = inv.label();
                slot.unknown_target = match inv.target() {
                    Some(NetTarget::Host(h)) => !netgate::known_host(&h, &self.operator_message, &policy.net_allow),
                    Some(NetTarget::Url(u)) => !netgate::known_url(&u, &self.operator_message, &policy.net_allow),
                    None => false,
                };
                slot.inv = Some(inv.clone());
            }
            let rule = match (&checked, &policy.deny) {
                (Ok(Invocation::Run { command, as_root, .. }), Some(d)) => d.check(command, *as_root),
                _ => None,
            };
            // Defence in depth: startup always builds the tech agent's rules.
            let rules_offline = matches!(checked, Ok(Invocation::Run { .. })) && defs.agent() == AgentId::Tech && policy.deny.is_none();
            let refusal = match checked {
                _ if duplicate => Some(DUPLICATE_ID.to_string()),
                _ if forced => Some(ROUND_LIMIT.to_string()),
                Err(reason) => Some(reason),
                Ok(_) if rules_offline => Some(DENY_OFFLINE.to_string()),
                Ok(_) if rule.is_some() => {
                    slot.deny_rule = rule.clone();
                    rule
                }
                Ok(inv) => {
                    slot.state = State::Pending(inv);
                    None
                }
            };
            if let Some(reason) = refusal {
                let tag = format!("DENIED: {reason}");
                slot.refuse(
                    self.agent,
                    Decision::Rejected,
                    format!("NOT AUTHORIZED: {reason}"),
                    &tag,
                    &mut self.prior,
                    &mut out,
                );
            }
            self.slots.push(slot);
        }
        self.advance(&mut out);
        out
    }

    /// The reviewer's verdict for the call under review.
    pub fn on_review(&mut self, call_id: &str, review: Review, audit_online: bool) -> Vec<Effect> {
        let Some(i) = self.live(call_id, |s| matches!(s, State::Reviewing(_))) else {
            return vec![];
        };
        let agent = self.agent;
        let slot = &mut self.slots[i];
        let State::Reviewing(inv) = std::mem::replace(&mut slot.state, State::Done) else {
            return vec![];
        };
        let mut review = review;
        if slot.unknown_target && review.verdict == Verdict::Allow {
            review.verdict = Verdict::Ask;
            review.note = format!("{} -- {UNLISTED}", review.note);
        }
        slot.review = Some(review.clone());
        let mut out = Vec::new();
        let prior = &mut self.prior;
        match review.verdict {
            Verdict::Allow | Verdict::Ask if inv.as_root() => slot.ask_password(agent, inv, false, audit_online, prior, &mut out),
            Verdict::Allow => slot.run(agent, inv, Decision::Auto, &mut out),
            Verdict::Ask => {
                out.push(Effect::Confirm {
                    call_id: slot.call.id.clone(),
                    prompt: slot.label.clone(),
                    note: review.note,
                });
                slot.state = State::Confirming(inv);
            }
            Verdict::Deny if !audit_online => slot.refuse_offline(agent, prior, &mut out),
            Verdict::Deny => {
                out.push(Effect::AskOverride {
                    call_id: slot.call.id.clone(),
                    prompt: slot.label.clone(),
                    note: review.note,
                });
                slot.state = State::Override(inv);
            }
        }
        self.advance(&mut out);
        out
    }

    /// The operator's Y/N for an unprivileged `ask`.
    pub fn on_confirm(&mut self, call_id: &str, yes: bool, audit_online: bool) -> Vec<Effect> {
        let _ = audit_online; // unprivileged: runs without the audit log
        let Some(i) = self.live(call_id, |s| matches!(s, State::Confirming(_))) else {
            return vec![];
        };
        let agent = self.agent;
        let slot = &mut self.slots[i];
        let State::Confirming(inv) = std::mem::replace(&mut slot.state, State::Done) else {
            return vec![];
        };
        let mut out = Vec::new();
        if yes {
            slot.run(agent, inv, Decision::Confirmed, &mut out);
        } else {
            slot.refuse(agent, Decision::Denied, DENIED.into(), DENIED, &mut self.prior, &mut out);
        }
        self.advance(&mut out);
        out
    }

    /// The password prompt was answered (`given`) or refused with Esc.
    pub fn on_password(&mut self, call_id: &str, given: bool, audit_online: bool) -> Vec<Effect> {
        let Some(i) = self.live(call_id, |s| matches!(s, State::Password { .. })) else {
            return vec![];
        };
        let agent = self.agent;
        let slot = &mut self.slots[i];
        let State::Password { inv, overridden } = std::mem::replace(&mut slot.state, State::Done) else {
            return vec![];
        };
        let mut out = Vec::new();
        if !given {
            slot.refuse(agent, Decision::Denied, DENIED.into(), DENIED, &mut self.prior, &mut out);
        } else if !audit_online {
            slot.refuse_offline(agent, &mut self.prior, &mut out);
        } else {
            let decision = if overridden { Decision::Overridden } else { Decision::Confirmed };
            slot.run(agent, inv, decision, &mut out);
        }
        self.advance(&mut out);
        out
    }

    /// The operator typed OVERRIDE (`yes`) or refused with Esc.
    pub fn on_override(&mut self, call_id: &str, yes: bool, audit_online: bool) -> Vec<Effect> {
        let Some(i) = self.live(call_id, |s| matches!(s, State::Override(_))) else {
            return vec![];
        };
        let agent = self.agent;
        let slot = &mut self.slots[i];
        let State::Override(inv) = std::mem::replace(&mut slot.state, State::Done) else {
            return vec![];
        };
        let mut out = Vec::new();
        let prior = &mut self.prior;
        if !yes {
            let note = slot.review.as_ref().map(|r| r.note.clone()).unwrap_or_default();
            slot.refuse(
                agent,
                Decision::Denied,
                format!("DENIED BY REVIEWER: {note}"),
                "DENIED BY REVIEWER",
                prior,
                &mut out,
            );
        } else if !audit_online {
            slot.refuse_offline(agent, prior, &mut out);
        } else if inv.as_root() {
            slot.ask_password(agent, inv, true, audit_online, prior, &mut out);
        } else {
            slot.run(agent, inv, Decision::Overridden, &mut out);
        }
        self.advance(&mut out);
        out
    }

    pub fn on_tool_done(&mut self, call_id: &str, outcome: ToolOutcome) -> Vec<Effect> {
        let Some(i) = self.live(call_id, |s| matches!(s, State::Running { .. })) else {
            return vec![];
        };
        let mut out = Vec::new();
        self.slots[i].finish(self.agent, outcome, &mut self.prior, &mut out);
        self.advance(&mut out);
        out
    }

    /// The delegation finished: resolves the waiting call and moves on.
    pub fn on_delegate(&mut self, call_id: &str, outcome: DelegateOutcome, officer: &str) -> Vec<Effect> {
        let Some(i) = self.live(call_id, |s| matches!(s, State::Delegating)) else {
            return vec![];
        };
        let mut out = Vec::new();
        let content = outcome.content(officer);
        let is_error = matches!(outcome, DelegateOutcome::NoResponse { .. });
        let slot = &mut self.slots[i];
        out.push(Effect::Audit(AuditEntry {
            output: content.clone(),
            ..slot.base_audit(self.agent, Decision::Auto)
        }));
        let r = slot.make_result(content, is_error);
        slot.resolved(r, Decision::Auto, None, &mut self.prior);
        self.advance(&mut out);
        out
    }

    /// The call id of the slot waiting on a delegation.
    pub fn delegating(&self) -> Option<&str> {
        self.slots
            .iter()
            .find(|s| matches!(s.state, State::Delegating) && s.result.is_none())
            .map(|s| s.call.id.as_str())
    }

    /// Ctrl-C: every unresolved call is aborted, the round logged, the turn finished.
    pub fn abort(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        self.abort_open(&mut out);
        out.push(Effect::Finished { truncated: false });
        out
    }

    /// The call whose Y/N, password or OVERRIDE prompt is up.
    pub fn pending_prompt(&self) -> Option<&str> {
        self.slots.iter().find(|s| s.prompting()).map(|s| s.call.id.as_str())
    }

    pub fn running(&self) -> bool {
        self.slots.iter().any(|s| matches!(s.state, State::Running { .. }))
    }

    pub fn reviewing(&self) -> bool {
        self.slots.iter().any(|s| matches!(s.state, State::Reviewing(_)))
    }

    /// The unresolved slot for `call_id` in a state `want` accepts. Rejected
    /// duplicates are resolved on arrival, so only the first call with an id can match.
    fn live(&self, call_id: &str, want: impl Fn(&State) -> bool) -> Option<usize> {
        self.slots.iter().position(|s| s.call.id == call_id && s.result.is_none() && want(&s.state))
    }

    /// Resolves every unresolved call of the open round as aborted, then logs
    /// the round. Does nothing when no round is open.
    fn abort_open(&mut self, out: &mut Vec<Effect>) {
        if self.slots.is_empty() {
            return;
        }
        for slot in &mut self.slots {
            if slot.result.is_none() {
                slot.abort(self.agent, &mut self.prior, out);
            }
        }
        let results = self.close_round();
        out.push(Effect::LogTool(results));
    }

    /// Starts the next pending call when none is active; finishes the round
    /// once every call has a result.
    fn advance(&mut self, out: &mut Vec<Effect>) {
        if self.slots.is_empty() {
            return;
        }
        if !self.slots.iter().any(Slot::active)
            && let Some(i) = self.slots.iter().position(|s| matches!(s.state, State::Pending(_)))
        {
            self.start(i, out);
        }
        if self.slots.iter().any(|s| s.result.is_none()) {
            return;
        }
        let results = self.close_round();
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

    /// Review-free tools run at once; the others go to the reviewer.
    fn start(&mut self, i: usize, out: &mut Vec<Effect>) {
        let slot = &mut self.slots[i];
        let State::Pending(inv) = std::mem::replace(&mut slot.state, State::Done) else {
            return;
        };
        match inv.gate() {
            Gate::Free => slot.run(self.agent, inv, Decision::Auto, out),
            Gate::Delegate => {
                let Invocation::Delegate { task } = &inv else {
                    return;
                };
                out.push(Effect::Delegate {
                    call_id: slot.call.id.clone(),
                    task: task.clone(),
                    operator_message: self.operator_message.clone(),
                });
                slot.state = State::Delegating;
            }
            Gate::Reviewed => {
                let input = ReviewInput {
                    operator_message: self.operator_message.clone(),
                    tool: slot.call.name.clone(),
                    args: slot.call.input.clone(),
                    unknown_target: slot.unknown_target,
                    prior: self.prior.clone(),
                    delegated_task: self.delegated_task.clone(),
                };
                out.push(Effect::Review {
                    call_id: slot.call.id.clone(),
                    input,
                });
                slot.state = State::Reviewing(inv);
            }
        }
    }
}

#[cfg(test)]
mod tests;
