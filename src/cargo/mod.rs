//! The CARGO folder: the container manifest, one container per row.
//!
//! Fed by the same snapshots as UTILITY; containers not `UP` are listed
//! first. The arrows move a selection (Shift-F15/F16 a page, Home the top);
//! the list scrolls to keep it visible.

mod list;

use crate::app::Tab;
use crate::input::Key;
use crate::metrics::{Container, Snapshot};
use crate::tools::audit::{AuditLog, SharedAudit, shared};
use crate::utility::widgets::message_panel;
use list::{Selection, draw_list};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use std::time::{Duration, Instant};

const HINT: &str = "U START  R RESTART  D STOP  L LOGS";
const NO_RUNTIME: &str = "NO CONTAINER RUNTIME";

/// What CARGO needs from the outside world, injected so tests can fake it.
pub struct CargoDeps {
    /// Start `argv` in the background (`seq`, argv, timeout, merged
    /// stdout+stderr); the result comes back as `Tab::on_cargo_done`.
    pub run: Box<dyn FnMut(u64, Vec<String>, Duration, bool)>,
    pub now: Box<dyn Fn() -> Instant>,
    pub timestamp: Box<dyn Fn() -> String>,
}

enum State {
    Awaiting,
    Offline(String),
    /// `None`: no container runtime answers on this host.
    Manifest(Option<Vec<Container>>),
}

pub struct CargoTab {
    #[expect(dead_code, reason = "container actions use it")]
    deps: CargoDeps,
    #[expect(dead_code, reason = "container actions use it")]
    audit: SharedAudit,
    state: State,
    /// The runtime that produced the list (`docker`/`podman`).
    runtime: Option<String>,
    sel: Selection,
}

impl CargoTab {
    pub fn new(deps: CargoDeps, audit: SharedAudit) -> Self {
        Self { deps, audit, state: State::Awaiting, runtime: None, sel: Selection::new() }
    }

    /// A tab that runs nothing and audits nowhere (tests, previews).
    pub fn detached() -> Self {
        let deps = CargoDeps { run: Box::new(|_, _, _, _| {}), now: Box::new(Instant::now), timestamp: Box::new(String::new) };
        Self::new(deps, shared(AuditLog::offline()))
    }

    pub fn on_offline(&mut self, why: &str) {
        self.state = State::Offline(why.to_string());
    }

    /// Anything not `UP` sorts first so it is never scrolled out of sight;
    /// the sort is stable, so each group keeps the runtime's order.
    pub fn on_snapshot(&mut self, s: &Snapshot) {
        let mut c = s.containers.clone();
        if let Some(c) = &mut c {
            c.sort_by_key(|k| k.status == "UP");
            self.sel.sync(c);
        }
        self.runtime = s.container_runtime.clone();
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
}

impl Tab for CargoTab {
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let detail = self.detail_line();
        let hint = if self.runtime.is_some() { HINT } else { NO_RUNTIME };
        let lines: Vec<Line<'static>> = match &self.state {
            State::Awaiting => vec![Line::from("AWAITING TELEMETRY")],
            State::Offline(why) => vec![Line::from("TELEMETRY OFFLINE"), Line::from(why.clone())],
            State::Manifest(None) => vec![Line::from(NO_RUNTIME)],
            State::Manifest(Some(c)) if c.is_empty() => vec![Line::from("HOLD EMPTY")],
            State::Manifest(Some(c)) => return draw_list(&mut self.sel, c, None, (&detail, false), hint, area, buf),
        };
        message_panel("CARGO", lines, area, buf);
    }

    fn handle_key(&mut self, key: &Key) {
        let State::Manifest(Some(c)) = &self.state else { return };
        match key {
            Key::Up => self.sel.up(c),
            Key::Down => self.sel.down(c),
            Key::PageUp => self.sel.page_up(c),
            Key::PageDown => self.sel.page_down(c),
            Key::Home => self.sel.home(c),
            _ => {}
        }
    }

    fn on_metrics(&mut self, s: &Snapshot) {
        self.on_snapshot(s);
    }

    fn on_metrics_offline(&mut self, why: &str) {
        self.on_offline(why);
    }
}

#[cfg(test)]
mod tests;
