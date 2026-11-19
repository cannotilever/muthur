//! Container commands: what each key runs, the Y/N prompt, and how a result
//! reads on the message line and in the audit log.

use crate::tools::audit::{AuditEntry, Decision};
use crate::tools::cap;
use crate::tools::runner::RunOutput;
use serde_json::json;
use std::time::{Duration, Instant};

const ACTION_TIMEOUT: Duration = Duration::from_secs(30);
const LOGS_TIMEOUT: Duration = Duration::from_secs(10);
/// How much log `L` fetches.
pub(super) const LOG_TAIL: &str = "200";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Verb {
    Start,
    Restart,
    Stop,
    Logs,
}

impl Verb {
    /// U / R / D / L, either case (WY-60 users often have caps lock on).
    pub(super) fn key(c: char) -> Option<Verb> {
        Some(match c.to_ascii_lowercase() {
            'u' => Verb::Start,
            'r' => Verb::Restart,
            'd' => Verb::Stop,
            'l' => Verb::Logs,
            _ => return None,
        })
    }

    pub(super) fn word(self) -> &'static str {
        match self {
            Verb::Start => "start",
            Verb::Restart => "restart",
            Verb::Stop => "stop",
            Verb::Logs => "logs",
        }
    }

    pub(super) fn progress(self) -> &'static str {
        match self {
            Verb::Start => "STARTING",
            Verb::Restart => "RESTARTING",
            Verb::Stop => "STOPPING",
            Verb::Logs => "FETCHING LOGS",
        }
    }

    pub(super) fn done(self) -> &'static str {
        match self {
            Verb::Start => "STARTED",
            Verb::Restart => "RESTARTED",
            Verb::Stop => "STOPPED",
            Verb::Logs => "LOGS FETCHED",
        }
    }

    /// Taking a container down needs an armed Y.
    pub(super) fn confirm(self) -> bool {
        matches!(self, Verb::Restart | Verb::Stop)
    }

    /// Log output arrives on both streams and must keep its order.
    pub(super) fn merged(self) -> bool {
        self == Verb::Logs
    }

    pub(super) fn argv(self, rt: &str, id: &str) -> Vec<String> {
        let a: &[&str] = match self {
            Verb::Logs => &[rt, "logs", "--tail", LOG_TAIL, id],
            v => &[rt, v.word(), id],
        };
        a.iter().map(|s| s.to_string()).collect()
    }

    pub(super) fn timeout(self) -> Duration {
        if self == Verb::Logs { LOGS_TIMEOUT } else { ACTION_TIMEOUT }
    }
}

/// A raised `<VERB> <NAME>? Y/N`, pinned to the container it was raised for.
/// Armed like MU/TH/UR's override prompt: `drawn` on the first unobscured
/// render, `armed_at` on the next; covering the tab resets both.
pub(super) struct Prompt {
    pub(super) verb: Verb,
    pub(super) id: String,
    pub(super) name: String,
    pub(super) drawn: bool,
    pub(super) armed_at: Option<Instant>,
}

impl Prompt {
    pub(super) fn new(verb: Verb, id: &str, name: &str) -> Self {
        Self { verb, id: id.into(), name: name.into(), drawn: false, armed_at: None }
    }

    pub(super) fn text(&self) -> String {
        format!("{} {}? Y/N", self.verb.word(), self.name).to_uppercase()
    }

    pub(super) fn armed(&self, now: Instant) -> bool {
        self.armed_at.is_some_and(|t| now >= t)
    }
}

/// The message-line text for a failed command; `None` when it succeeded.
pub(super) fn failure(verb: Verb, out: &RunOutput) -> Option<String> {
    let verb = verb.word().to_uppercase();
    Some(match out {
        RunOutput::Exited { code: 0, .. } => return None,
        RunOutput::Exited { code, stdout, stderr } => {
            let first = |s: &str| s.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string);
            let why = first(stderr).or_else(|| first(stdout)).unwrap_or_else(|| format!("EXIT {code}"));
            format!("{verb} FAILED: {why}").to_uppercase()
        }
        RunOutput::NotFound => "RUNTIME NOT FOUND".into(),
        RunOutput::TimedOut => format!("{verb} TIMED OUT"),
    })
}

pub(super) fn audit_entry(verb: Verb, id: &str, name: &str, argv: Vec<String>, out: &RunOutput, ms: u64) -> AuditEntry {
    let (exit, output) = match out {
        RunOutput::Exited { code, stdout, stderr } => (json!(code), format!("{stdout}{stderr}")),
        RunOutput::NotFound => (json!("not_found"), String::new()),
        RunOutput::TimedOut => (json!("timeout"), String::new()),
    };
    AuditEntry {
        tool: format!("cargo_{}", verb.word()),
        args: json!({ "id": id, "name": name }),
        tier: if verb.confirm() { 2 } else { 1 },
        decision: if verb.confirm() { Decision::Confirmed } else { Decision::Auto },
        argv: Some(argv),
        exit: Some(exit),
        ms,
        output: cap(output),
    }
}
