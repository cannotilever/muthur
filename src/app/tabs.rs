//! Tabs: the three folders of the personal terminal.

use crate::input::Key;
use crate::shell::PtyEvent;
use ratatui::layout::Position;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Widget};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabId {
    Utility,
    Muthur,
    Shell,
}

impl TabId {
    pub const ALL: [TabId; 3] = [TabId::Utility, TabId::Muthur, TabId::Shell];

    pub fn label(self) -> &'static str {
        match self {
            TabId::Utility => "UTILITY",
            TabId::Muthur => "MU/TH/UR",
            TabId::Shell => "SHELL",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

pub trait Tab {
    /// Draw into `area` (the content pane, or the whole screen in fullscreen).
    fn render(&mut self, area: Rect, buf: &mut Buffer);
    fn handle_key(&mut self, key: &Key);
    /// True once if something happened since the last call (sidebar marker).
    fn take_activity(&mut self) -> bool {
        false
    }
    /// Where the terminal cursor belongs while this tab is shown.
    fn cursor(&self) -> Option<Position> {
        None
    }
    /// Output or exit from this tab's PTY (Shell only).
    fn on_pty(&mut self, _ev: PtyEvent) {}
    /// Enter scrollback; false if the tab has none.
    fn enter_scrollback(&mut self) -> bool {
        false
    }
    fn supports_passthrough(&self) -> bool {
        false
    }
    /// A new metrics snapshot (Utility only).
    fn on_metrics(&mut self, _s: &crate::metrics::Snapshot) {}
    /// The metrics source is failing (Utility only).
    fn on_metrics_offline(&mut self, _why: &str) {}
    /// A streamed LLM event for request `_req_id` (MU/TH/UR only).
    fn on_llm(&mut self, _req_id: u64, _ev: crate::llm::StreamEvent) {}
    /// The outcome of tool call `_call_id` started for request `_req_id` (MU/TH/UR only).
    fn on_tool_done(&mut self, _req_id: u64, _call_id: String, _o: crate::tools::exec::ToolOutcome) {}
    /// How often the tab wants a redraw while animating; `None` when idle.
    fn tick_interval(&self) -> Option<Duration> {
        None
    }
}

/// Stand-in until a tab's milestone lands.
pub struct PlaceholderTab {
    pub id: TabId,
}

impl Tab for PlaceholderTab {
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::bordered().title(format!(" {} ", self.id.label()));
        let inner = block.inner(area);
        block.render(area, buf);
        let mid = inner.y + inner.height.saturating_sub(2) / 2;
        let text = Rect { y: mid, height: 2.min(inner.height), ..inner };
        Paragraph::new(vec![Line::from("MODULE OFFLINE"), Line::from("AWAITING INSTALLATION")])
            .alignment(Alignment::Center)
            .render(text, buf);
    }

    fn handle_key(&mut self, _key: &Key) {}
}
