//! The TUI: tab focus, prefix commands, fullscreen, chrome.

pub mod chrome;
pub mod tabs;

use crate::input::Key;
use crate::metrics::Snapshot;
use crate::muthur::MuthurTab;
use crate::shell::{PtyEvent, ShellTab};
use crate::splash::Splash;
use crate::utility::UtilityTab;
use chrono::NaiveDateTime;
use ratatui::Frame;
use ratatui::layout::Position;
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
    /// `^] p` on the Shell tab: PTY bytes go to the wire untranslated.
    pub passthrough: bool,
    /// `muthur splash`: quit as soon as the splash ends or is skipped.
    pub quit_after_splash: bool,
    splash: Option<Splash>,
    tabs: [Box<dyn Tab>; 3],
    activity: [bool; 3],
    chrome: ChromeView,
}

impl App {
    pub fn new(chrome: ChromeView) -> Self {
        Self::with_tabs(chrome, TabId::ALL.map(|id| Box::new(PlaceholderTab { id }) as Box<dyn Tab>))
    }

    /// The three real tabs.
    pub fn assemble(chrome: ChromeView, utility: UtilityTab, muthur: MuthurTab, shell: ShellTab) -> Self {
        Self::with_tabs(chrome, [Box::new(utility), Box::new(muthur), Box::new(shell)])
    }

    pub fn with_splash(mut self, splash: Splash) -> Self {
        self.splash = Some(splash);
        self
    }

    pub fn splash_active(&self) -> bool {
        self.splash.is_some()
    }

    fn end_splash(&mut self) {
        self.splash = None;
        // Whatever happened behind the splash is not news.
        for tab in self.tabs.iter_mut() {
            tab.take_activity();
        }
        self.activity = [false; 3];
        if self.quit_after_splash {
            self.quit = true;
        } else {
            self.redraw = true;
        }
    }

    pub fn on_llm(&mut self, req_id: u64, ev: crate::llm::StreamEvent) {
        self.tabs[TabId::Muthur.index()].on_llm(req_id, ev);
    }

    pub fn on_tool_done(&mut self, req_id: u64, call_id: String, outcome: crate::tools::exec::ToolOutcome) {
        self.tabs[TabId::Muthur.index()].on_tool_done(req_id, call_id, outcome);
    }

    /// The active tab's redraw cadence; 1 s when it is idle.
    pub fn tick_interval(&self) -> std::time::Duration {
        if let Some(s) = &self.splash {
            return s.tick_interval();
        }
        // Hidden tabs only advance when rendered, so their animation must not
        // drive the cadence; the 1 s tick still lights their activity marker.
        self.tabs[self.active.index()].tick_interval().unwrap_or(std::time::Duration::from_secs(1))
    }

    pub fn on_metrics(&mut self, s: Snapshot) {
        if let Some(sp) = &mut self.splash {
            sp.on_metrics(&s);
        }
        self.tabs[TabId::Utility.index()].on_metrics(&s);
    }

    pub fn on_metrics_offline(&mut self, why: &str) {
        if let Some(sp) = &mut self.splash {
            sp.on_metrics_offline();
        }
        self.tabs[TabId::Utility.index()].on_metrics_offline(why);
    }

    pub fn on_pty(&mut self, ev: PtyEvent) {
        if ev == PtyEvent::Exited && self.passthrough {
            self.passthrough = false;
            self.redraw = true;
        }
        self.tabs[TabId::Shell.index()].on_pty(ev);
    }

    /// The active tab's cursor, unless something is drawn over it.
    pub fn cursor(&self) -> Option<Position> {
        if self.overlay.is_some() || self.prefix {
            return None;
        }
        self.tabs[self.active.index()].cursor()
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
            passthrough: false,
            quit_after_splash: false,
            splash: None,
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
        if self.splash.is_some() {
            self.end_splash();
            return;
        }
        if self.overlay.take().is_some() {
            return;
        }
        if self.prefix {
            self.prefix = false;
            let tab = &mut self.tabs[self.active.index()];
            // ^] ^] sends a literal ^], like tmux/screen.
            if key == Key::Ctrl(']') {
                tab.handle_key(&key);
                return;
            }
            // WY-60 users often have caps lock on: accept either case.
            let Key::Char(c) = key else { return };
            match c.to_ascii_lowercase() {
                'f' => self.fullscreen[self.active.index()] ^= true,
                'r' => self.redraw = true,
                'q' => self.quit = true,
                'd' => self.show_real_date ^= true,
                '?' => self.overlay = Some(Overlay::Help),
                ']' => self.tabs[self.active.index()].handle_key(&Key::Ctrl(']')),
                'p' if self.passthrough => {
                    self.passthrough = false;
                    self.redraw = true;
                }
                'p' if tab.supports_passthrough() => {
                    self.passthrough ^= true;
                    if self.passthrough {
                        self.fullscreen[self.active.index()] = true;
                    } else {
                        self.redraw = true;
                    }
                }
                '[' if tab.enter_scrollback() => {}
                'p' | '[' => self.overlay = Some(Overlay::Message("NOT AVAILABLE ON THIS FOLDER")),
                _ => {}
            }
            return;
        }
        match key {
            // The screen belongs to the shell in passthrough: no tab switching.
            Key::F(1..=3) if self.passthrough => {}
            Key::F(n @ 1..=3) => self.switch(TabId::ALL[n as usize - 1]),
            Key::Ctrl(']') => self.prefix = true,
            k => self.tabs[self.active.index()].handle_key(&k),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, now: NaiveDateTime) {
        let area = frame.area();
        if let Some(sp) = &mut self.splash {
            sp.advance(std::time::Instant::now());
            if !sp.is_done() {
                for tab in self.tabs.iter_mut() {
                    tab.set_obscured(true);
                }
                sp.render(area, frame.buffer_mut());
                return;
            }
            self.end_splash();
        }
        let active = self.active.index();
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if tab.take_activity() && i != active {
                self.activity[i] = true;
            }
        }
        // Hidden or covered tabs must not treat this frame as seen (override arming).
        let covered = self.overlay.is_some() || self.prefix;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            tab.set_obscured(i != active || covered);
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
        if let Some(p) = self.cursor() {
            frame.set_cursor_position(p);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
