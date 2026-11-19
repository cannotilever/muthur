//! The CARGO folder: the container manifest, one container per row.
//!
//! Fed by the same snapshots as UTILITY; containers not `UP` are listed
//! first. A list longer than the pane scrolls
//! with Shift-F15/F16 (a page) and the arrows (a row).

use crate::app::Tab;
use crate::input::Key;
use crate::metrics::{Container, Snapshot};
use crate::utility::widgets::{message_panel, panel};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Widget;

/// Narrowest the STATUS column gets (its heading).
const MIN_STATUS_W: usize = 6;

enum State {
    Awaiting,
    Offline(String),
    /// `None`: no container runtime answers on this host.
    Manifest(Option<Vec<Container>>),
}

pub struct CargoTab {
    state: State,
    view: View,
}

/// Where the manifest is scrolled to.
struct View {
    /// First container row shown.
    scroll: usize,
    /// Container rows the last render had room for (a page).
    page: usize,
}

impl Default for CargoTab {
    fn default() -> Self {
        Self::new()
    }
}

impl CargoTab {
    pub fn new() -> Self {
        Self { state: State::Awaiting, view: View { scroll: 0, page: 1 } }
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
        }
        self.state = State::Manifest(c);
    }

    fn count(&self) -> usize {
        match &self.state {
            State::Manifest(Some(c)) => c.len(),
            _ => 0,
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.count().saturating_sub(self.view.page);
        self.view.scroll = self.view.scroll.saturating_add_signed(delta).min(max);
    }
}

impl View {
    fn draw_manifest(&mut self, c: &[Container], area: Rect, buf: &mut Buffer) {
        // Borders and the heading row.
        self.page = area.height.saturating_sub(3).max(1) as usize;
        self.scroll = self.scroll.min(c.len().saturating_sub(self.page));
        let shown = &c[self.scroll..c.len().min(self.scroll + self.page)];
        let noun = if c.len() == 1 { "CONTAINER" } else { "CONTAINERS" };
        let mut title = format!("CARGO - {} {noun}", c.len());
        if shown.len() < c.len() {
            title += &format!(" ({}-{})", self.scroll + 1, self.scroll + shown.len());
        }
        let block = panel(&title);
        let inner = block.inner(area);
        block.render(area, buf);

        let w = inner.width as usize;
        let status_w = c.iter().map(|c| c.status.chars().count()).max().unwrap_or(0).clamp(MIN_STATUS_W, w / 2);
        let name_w = w.saturating_sub(status_w + 1);
        let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>();
        buf.set_string(inner.x, inner.y, format!("{:<name_w$} {}", "NAME", "STATUS"), Style::new());
        for (i, k) in shown.iter().enumerate() {
            let y = inner.y + 1 + i as u16;
            buf.set_string(inner.x, y, cut(&k.name.to_uppercase(), name_w), Style::new());
            let style = if k.status == "UP" { Style::new() } else { Style::new().add_modifier(Modifier::REVERSED) };
            buf.set_string(inner.x + (name_w + 1) as u16, y, cut(&k.status, status_w), style);
        }
    }
}

impl Tab for CargoTab {
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let lines: Vec<Line<'static>> = match &self.state {
            State::Awaiting => vec![Line::from("AWAITING TELEMETRY")],
            State::Offline(why) => vec![Line::from("TELEMETRY OFFLINE"), Line::from(why.clone())],
            State::Manifest(None) => vec![Line::from("NO CONTAINER RUNTIME")],
            State::Manifest(Some(c)) if c.is_empty() => vec![Line::from("HOLD EMPTY")],
            State::Manifest(Some(c)) => return self.view.draw_manifest(c, area, buf),
        };
        message_panel("CARGO", lines, area, buf);
    }

    fn handle_key(&mut self, key: &Key) {
        let page = self.view.page as isize;
        match key {
            Key::PageUp => self.scroll_by(-page),
            Key::PageDown => self.scroll_by(page),
            Key::Up => self.scroll_by(-1),
            Key::Down => self.scroll_by(1),
            Key::Home => self.view.scroll = 0,
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
