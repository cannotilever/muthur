//! Tabs: the four folders of the personal terminal.

use crate::input::Key;
use crate::shell::PtyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Position;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Widget};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabId {
    Muthur,
    Utility,
    Cargo,
    Shell,
}

/// How many folders there are.
pub const TABS: usize = TabId::ALL.len();

impl TabId {
    /// Sidebar order, which is also F-key order (F1 first).
    pub const ALL: [TabId; 4] = [TabId::Muthur, TabId::Utility, TabId::Cargo, TabId::Shell];

    pub fn label(self) -> &'static str {
        match self {
            TabId::Muthur => "MU/TH/UR",
            TabId::Utility => "UTILITY",
            TabId::Cargo => "CARGO",
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
    /// Whether the PTY's output is currently going to the wire untranslated
    /// (Shell only): the real terminal then answers queries itself.
    fn set_passthrough(&mut self, _on: bool) {}
    /// A new metrics snapshot (Utility and Cargo).
    fn on_metrics(&mut self, _s: &crate::metrics::Snapshot) {}
    /// The metrics source is failing (Utility and Cargo).
    fn on_metrics_offline(&mut self, _why: &str) {}
    /// The result of container command `_seq` (CARGO only).
    fn on_cargo_done(&mut self, _seq: u64, _out: crate::ops::runner::RunOutput, _ms: u64) {}
    /// A streamed LLM event for request `_req_id` (MU/TH/UR only).
    fn on_llm(&mut self, _req_id: u64, _ev: crate::llm::StreamEvent) {}
    #[cfg(feature = "agent")]
    /// The outcome of tool call `_call_id` started for request `_req_id` (MU/TH/UR only).
    fn on_tool_done(&mut self, _req_id: u64, _call_id: String, _o: crate::tools::ToolOutcome) {}
    #[cfg(feature = "agent")]
    /// The reviewer's verdict on call `_call_id` of request `_req_id` (MU/TH/UR only).
    fn on_review(&mut self, _req_id: u64, _call_id: String, _review: crate::review::Review) {}
    /// Called before every render: true when the tab is not visible as drawn,
    /// because another folder is shown or the help overlay, a message overlay
    /// or the prefix strip is drawn over it.
    fn set_obscured(&mut self, _obscured: bool) {}
    /// A tool prompt (Y/N, password, OVERRIDE) is waiting for the operator (MU/TH/UR only).
    fn awaiting_operator(&self) -> bool {
        false
    }
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
        let text = Rect {
            y: mid,
            height: 2.min(inner.height),
            ..inner
        };
        Paragraph::new(vec![Line::from("MODULE OFFLINE"), Line::from("AWAITING INSTALLATION")])
            .alignment(Alignment::Center)
            .render(text, buf);
    }

    fn handle_key(&mut self, _key: &Key) {}
}
