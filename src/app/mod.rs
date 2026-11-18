//! The TUI: tab focus, prefix commands, fullscreen, chrome.

pub mod chrome;
pub mod tabs;

use crate::input::Key;
use chrono::NaiveDateTime;
use ratatui::Frame;
pub use tabs::{PlaceholderTab, Tab, TabId};

/// What the title bar shows.
pub struct ChromeView {
    pub ship_name: String,
    pub year_offset: i32,
}

enum Overlay {
    Help,
    Message(&'static str),
}

pub struct App {
    pub active: TabId,
    fullscreen: [bool; 3],
    prefix: bool,
    overlay: Option<Overlay>,
    show_real_date: bool,
    /// Set by `^] q`; the run loop exits.
    pub quit: bool,
    /// Set by `^] r`; the run loop clears and repaints everything.
    pub redraw: bool,
    tabs: [Box<dyn Tab>; 3],
    activity: [bool; 3],
    chrome: ChromeView,
}

impl App {
    pub fn new(chrome: ChromeView) -> Self {
        Self::with_tabs(chrome, TabId::ALL.map(|id| Box::new(PlaceholderTab { id }) as Box<dyn Tab>))
    }

    pub fn with_tabs(chrome: ChromeView, tabs: [Box<dyn Tab>; 3]) -> Self {
        Self {
            active: TabId::Utility,
            fullscreen: [false; 3],
            prefix: false,
            overlay: None,
            show_real_date: false,
            quit: false,
            redraw: false,
            tabs,
            activity: [false; 3],
            chrome,
        }
    }

    fn switch(&mut self, id: TabId) {
        self.active = id;
        self.activity[id.index()] = false;
    }

    pub fn handle_key(&mut self, key: Key) {
        if self.overlay.take().is_some() {
            return;
        }
        if self.prefix {
            self.prefix = false;
            // WY-60 users often have caps lock on: accept either case.
            let Key::Char(c) = key else { return };
            match c.to_ascii_lowercase() {
                'f' => self.fullscreen[self.active.index()] ^= true,
                'r' => self.redraw = true,
                'q' => self.quit = true,
                'd' => self.show_real_date ^= true,
                '?' => self.overlay = Some(Overlay::Help),
                ']' => self.tabs[self.active.index()].handle_key(&Key::Ctrl(']')),
                'p' | '[' => self.overlay = Some(Overlay::Message("NOT AVAILABLE")),
                _ => {}
            }
            return;
        }
        match key {
            Key::F(n @ 1..=3) => self.switch(TabId::ALL[n as usize - 1]),
            Key::Ctrl(']') => self.prefix = true,
            k => self.tabs[self.active.index()].handle_key(&k),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, now: NaiveDateTime) {
        let area = frame.area();
        let active = self.active.index();
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if tab.take_activity() && i != active {
                self.activity[i] = true;
            }
        }
        let buf = frame.buffer_mut();
        if self.fullscreen[active] {
            self.tabs[active].render(area, buf);
        } else {
            self.tabs[active].render(chrome::CONTENT, buf);
            let offset = if self.show_real_date { 0 } else { self.chrome.year_offset };
            chrome::title_bar(buf, area.width, &self.chrome.ship_name, &chrome::format_date(now, offset));
            chrome::sidebar(buf, self.active, &self.activity);
            chrome::footer(buf);
        }
        if self.prefix {
            chrome::prefix_strip(buf, area.width, area.height - 1);
        }
        match self.overlay {
            Some(Overlay::Help) => chrome::help(buf, area),
            Some(Overlay::Message(m)) => chrome::message(buf, area, m),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests;
