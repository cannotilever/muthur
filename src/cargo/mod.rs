//! The CARGO folder: the container manifest, one container per row, and
//! start / restart / stop for the selected one.
//!
//! Fed by the same snapshots as UTILITY; containers not `UP` are listed
//! first. The arrows move a selection (Shift-F15/F16 a page, Home the top);
//! the list scrolls to keep it visible. U/R/D act on the selected container
//! (R and D only after an armed Y); L opens the container's last 200 log
//! lines full-pane (L again refetches, Esc goes back). Commands run through the injected
//! `CargoDeps::run` and come back as `on_cargo_done`; every one is audited.

mod action;
mod list;
mod logview;

use crate::app::Tab;
use crate::input::Key;
use crate::metrics::local::ps_argv;
use crate::metrics::probes::parse_containers;
use crate::metrics::{Container, Snapshot};
use crate::ops::audit::{AuditLog, SharedAudit, shared};
use crate::ops::runner::RunOutput;
use crate::utility::widgets::message_panel;
use action::{Prompt, Verb, aborted_entry, audit_entry, failure};
use list::{Selection, draw_list};
use logview::LogView;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use std::time::{Duration, Instant};

const HINT: &str = "U START  R RESTART  D STOP  L LOGS";
const NO_RUNTIME: &str = "NO CONTAINER RUNTIME";
const REFRESH_TIMEOUT: Duration = Duration::from_secs(2);
/// Tier-2 actions (restart, stop) are refused without an audit log, as
/// the agent's root calls and overrides are.
const AUDIT_OFFLINE: &str = "AUDIT LOG OFFLINE";

/// What CARGO needs from the outside world, injected so tests can fake it.
pub struct CargoDeps {
    /// Start `argv` in the background (`seq`, argv, timeout, merged
    /// stdout+stderr); the result comes back as `Tab::on_cargo_done`.
    pub run: Box<dyn FnMut(u64, Vec<String>, Duration, bool)>,
    pub now: Box<dyn Fn() -> Instant>,
    pub timestamp: Box<dyn Fn() -> String>,
    /// Scroll hints for the backend (the log view scrolls by line).
    pub hints: crate::scroll::HintSlot,
}

enum State {
    Awaiting,
    /// Telemetry failed before any list arrived.
    Offline(String),
    /// `None`: no container runtime answers on this host.
    Manifest(Option<Vec<Container>>),
}

/// The one action (U/R/D/L) in flight.
struct Pending {
    seq: u64,
    verb: Verb,
    id: String,
    name: String,
    argv: Vec<String>,
}

pub struct CargoTab {
    deps: CargoDeps,
    audit: SharedAudit,
    state: State,
    /// The runtime that produced the list (`docker`/`podman`).
    runtime: Option<String>,
    sel: Selection,
    /// Telemetry failed after a list arrived: the list stays usable.
    offline: bool,
    prompt: Option<Prompt>,
    action: Option<Pending>,
    /// Log fetches whose view was closed: still audited when they finish.
    discarded: Vec<Pending>,
    /// A snapshot came without a container list after one was shown (the
    /// runtime did not answer the probe): the last list stays.
    not_answering: bool,
    /// `seq` of the `ps` refresh in flight, and when it was started.
    refresh: Option<(u64, Instant)>,
    /// When the list on screen was probed (snapshot probe or refresh start).
    list_at: Option<Instant>,
    seq: u64,
    /// Progress, result or error (reverse) for the message line; cleared by
    /// the next key.
    message: Option<(String, bool)>,
    /// Open over the list while `Some`.
    logs: Option<LogView>,
    obscured: bool,
    activity: bool,
}

impl CargoTab {
    pub fn new(deps: CargoDeps, audit: SharedAudit) -> Self {
        Self {
            deps,
            audit,
            state: State::Awaiting,
            runtime: None,
            sel: Selection::new(),
            offline: false,
            prompt: None,
            action: None,
            discarded: Vec::new(),
            not_answering: false,
            refresh: None,
            list_at: None,
            seq: 0,
            message: None,
            logs: None,
            obscured: false,
            activity: false,
        }
    }

    /// A tab that runs nothing and audits nowhere (tests, previews).
    pub fn detached() -> Self {
        let deps = CargoDeps {
            run: Box::new(|_, _, _, _| {}),
            now: Box::new(Instant::now),
            timestamp: Box::new(String::new),
            hints: Default::default(),
        };
        Self::new(deps, shared(AuditLog::offline()))
    }

    pub fn on_offline(&mut self, why: &str) {
        match self.state {
            State::Manifest(_) => self.offline = true,
            _ => self.state = State::Offline(why.to_string()),
        }
    }

    pub fn on_snapshot(&mut self, s: &Snapshot) {
        self.offline = false;
        // Snapshots repeat a cached probe between container probes: one taken
        // before the list on screen (a post-action refresh) must not undo it.
        if let (Some(at), Some(mine)) = (s.containers_at, self.list_at)
            && at < mine
        {
            return;
        }
        // A probe that timed out (daemon busy): keep the list and the runtime.
        if s.containers.is_none() && matches!(self.state, State::Manifest(Some(_))) {
            self.not_answering = true;
            return;
        }
        self.not_answering = false;
        self.runtime = s.container_runtime.clone();
        self.list_at = s.containers_at;
        self.set_list(s.containers.clone());
    }

    /// Anything not `UP` sorts first so it is never scrolled out of sight;
    /// the sort is stable, so each group keeps the runtime's order.
    fn set_list(&mut self, mut c: Option<Vec<Container>>) {
        if let Some(c) = &mut c {
            c.sort_by_key(|k| k.status == "UP");
            self.sel.sync(c);
        }
        if let Some(p) = &self.prompt
            && !c.as_ref().is_some_and(|c| c.iter().any(|k| k.id == p.id))
        {
            self.message = Some((format!("{} GONE", p.name.to_uppercase()), false));
            self.prompt = None;
        }
        self.state = State::Manifest(c);
    }

    fn list(&self) -> &[Container] {
        match &self.state {
            State::Manifest(Some(c)) => c,
            _ => &[],
        }
    }

    /// `NAME: FULL STATUS` for the selected container.
    fn detail_line(&self) -> String {
        match self.sel.selected(self.list()) {
            Some(k) if k.detail.is_empty() => k.name.to_uppercase(),
            Some(k) => format!("{}: {}", k.name, k.detail).to_uppercase(),
            None => String::new(),
        }
    }

    fn message_line(&self, width: usize) -> (String, bool) {
        if let Some(p) = &self.prompt {
            return (p.text(width), false);
        }
        if let Some(m) = &self.message {
            return m.clone();
        }
        if self.offline {
            return ("TELEMETRY OFFLINE".into(), false);
        }
        if self.not_answering {
            return ("RUNTIME NOT ANSWERING".into(), false);
        }
        (self.detail_line(), false)
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// U/R/D/L on the selected container.
    fn request(&mut self, verb: Verb) {
        let (Some(_), Some(k)) = (&self.runtime, self.sel.selected(self.list())) else {
            return;
        };
        if self.action.is_some() {
            self.message = Some(("BUSY".into(), false));
            return;
        }
        let (id, name) = (k.id.clone(), k.name.clone());
        if verb.confirm() && !self.audit.borrow().is_online() {
            self.message = Some((AUDIT_OFFLINE.into(), true));
            return;
        }
        if verb.confirm() {
            self.prompt = Some(Prompt::new(verb, &id, &name));
        } else {
            self.start(verb, id, name);
        }
    }

    fn start(&mut self, verb: Verb, id: String, name: String) {
        let Some(rt) = self.runtime.clone() else {
            return;
        };
        let argv = verb.argv(&rt, &id);
        let seq = self.next_seq();
        (self.deps.run)(seq, argv.clone(), verb.timeout(), verb.merged());
        self.message = Some((format!("{} {} ...", verb.progress(), name.to_uppercase()), false));
        self.action = Some(Pending { seq, verb, id, name, argv });
    }

    fn logs_key(&mut self, key: &Key) {
        match key {
            Key::Esc => {
                self.logs = None;
                self.discard_log_fetch();
            }
            Key::Char('l' | 'L') if self.action.is_none() => {
                if let Some(v) = &self.logs {
                    let (id, name) = (v.id.clone(), v.name().to_string());
                    self.start(Verb::Logs, id, name);
                }
            }
            k => {
                if let Some(v) = &mut self.logs {
                    v.key(k);
                }
            }
        }
    }

    /// A log fetch in flight is for a view that is gone: its result is only
    /// audited, and other commands may start meanwhile.
    fn discard_log_fetch(&mut self) {
        if self.action.as_ref().is_some_and(|p| p.verb == Verb::Logs)
            && let Some(p) = self.action.take()
        {
            self.discarded.push(p);
        }
    }

    fn prompt_key(&mut self, key: &Key) {
        let Some(p) = &self.prompt else { return };
        match key {
            Key::Char('n' | 'N') | Key::Esc => self.prompt = None,
            Key::Char('y' | 'Y') if p.armed() => {
                let Some(p) = self.prompt.take() else { return };
                if !self.audit.borrow().is_online() {
                    self.message = Some((AUDIT_OFFLINE.into(), true));
                    return;
                }
                self.start(p.verb, p.id, p.name);
            }
            _ => {}
        }
    }

    /// False when the entry could not be written.
    fn record_audit(&mut self, p: &Pending, out: &RunOutput, ms: u64) -> bool {
        self.append_audit(&audit_entry(p.verb, &p.id, &p.name, p.argv.clone(), out, ms))
    }

    fn append_audit(&mut self, entry: &crate::ops::audit::AuditEntry) -> bool {
        let ts = (self.deps.timestamp)();
        if let Err(e) = self.audit.borrow_mut().append(entry, &ts) {
            tracing::error!("audit log: cannot record container command: {e}");
            return false;
        }
        true
    }

    fn action_done(&mut self, p: Pending, out: RunOutput, ms: u64) {
        let audited = self.record_audit(&p, &out, ms);
        // The App lights `*` only when CARGO is not the folder on screen.
        self.activity = true;
        if p.verb == Verb::Logs {
            match (failure(p.verb, &out), out) {
                (None, RunOutput::Exited { stdout, .. }) => {
                    self.message = None; // the progress line is done with
                    self.logs = Some(LogView::new(p.id, p.name.to_uppercase(), &stdout));
                }
                (why, _) => {
                    self.logs = None;
                    self.message = why.map(|w| (w, true));
                }
            }
            return;
        }
        self.message = Some(match failure(p.verb, &out) {
            Some(why) => (why, true),
            None if !audited => (format!("{} {} - {AUDIT_OFFLINE}", p.name.to_uppercase(), p.verb.done()), true),
            None => (format!("{} {}", p.name.to_uppercase(), p.verb.done()), false),
        });
        // Show the new state now rather than at the next 10 s probe.
        if let Some(rt) = self.runtime.clone() {
            let seq = self.next_seq();
            self.refresh = Some((seq, (self.deps.now)()));
            (self.deps.run)(seq, ps_argv(&rt), REFRESH_TIMEOUT, false);
        }
    }
}

impl Tab for CargoTab {
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let (message, reverse) = self.message_line(area.width.saturating_sub(2) as usize);
        let hint = if self.runtime.is_some() { HINT } else { NO_RUNTIME };
        let busy = self
            .action
            .as_ref()
            .filter(|p| p.verb != Verb::Logs)
            .map(|p| (p.id.clone(), p.verb.progress().to_string()));
        let lines: Vec<Line<'static>> = match &self.state {
            State::Awaiting => vec![Line::from("AWAITING TELEMETRY")],
            State::Offline(why) => vec![Line::from("TELEMETRY OFFLINE"), Line::from(why.clone())],
            State::Manifest(None) => vec![Line::from(NO_RUNTIME)],
            State::Manifest(Some(_)) if self.logs.is_some() => {
                if let Some(v) = &mut self.logs {
                    v.render(area, buf, &self.deps.hints);
                }
                return;
            }
            State::Manifest(Some(c)) if c.is_empty() => {
                let mut l = vec![Line::from("HOLD EMPTY")];
                if let Some((m, _)) = &self.message {
                    l.push(Line::from(m.clone()));
                }
                l
            }
            State::Manifest(Some(c)) => {
                let over = busy.as_ref().map(|(id, s)| (id.as_str(), s.as_str()));
                draw_list(&mut self.sel, c, over, (&message, reverse), hint, area, buf);
                // Arm the prompt once it has been drawn uncovered.
                if let Some(p) = &mut self.prompt
                    && !self.obscured
                {
                    p.drawn = true;
                }
                return;
            }
        };
        message_panel("CARGO", lines, area, buf);
    }

    fn handle_key(&mut self, key: &Key) {
        if self.prompt.is_some() {
            return self.prompt_key(key);
        }
        if self.logs.is_some() {
            return self.logs_key(key);
        }
        self.message = None;
        let State::Manifest(Some(c)) = &self.state else {
            return;
        };
        match key {
            // Changed your mind while the logs are coming: drop them.
            Key::Esc => self.discard_log_fetch(),
            Key::Up => self.sel.up(c),
            Key::Down => self.sel.down(c),
            Key::PageUp => self.sel.page_up(c),
            Key::PageDown => self.sel.page_down(c),
            Key::Home => self.sel.home(c),
            Key::Char(ch) => {
                if let Some(verb) = Verb::key(*ch) {
                    self.request(verb);
                }
            }
            _ => {}
        }
    }

    fn on_metrics(&mut self, s: &Snapshot) {
        self.on_snapshot(s);
    }

    fn on_metrics_offline(&mut self, why: &str) {
        self.on_offline(why);
    }

    fn on_cargo_done(&mut self, seq: u64, out: RunOutput, ms: u64) {
        if let Some((rseq, started)) = self.refresh
            && rseq == seq
        {
            self.refresh = None;
            if let RunOutput::Exited { code: 0, stdout, .. } = &out {
                self.list_at = Some(started);
                self.set_list(Some(parse_containers(stdout)));
            }
            return;
        }
        if self.action.as_ref().is_some_and(|p| p.seq == seq)
            && let Some(p) = self.action.take()
        {
            self.action_done(p, out, ms);
        } else if let Some(i) = self.discarded.iter().position(|p| p.seq == seq) {
            let p = self.discarded.remove(i);
            self.record_audit(&p, &out, ms);
        }
    }

    fn set_obscured(&mut self, obscured: bool) {
        self.obscured = obscured;
        if obscured && let Some(p) = &mut self.prompt {
            p.drawn = false;
        }
    }

    fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity)
    }
}

/// Commands still running when muthur quits are recorded as aborted: what
/// they did is unknown, but that they were issued is not lost.
impl Drop for CargoTab {
    fn drop(&mut self) {
        let pending: Vec<Pending> = self.action.take().into_iter().chain(self.discarded.drain(..)).collect();
        for p in pending {
            self.append_audit(&aborted_entry(p.verb, &p.id, &p.name, p.argv));
        }
    }
}

#[cfg(test)]
mod tests;
