//! The TUI: tab focus, prefix commands, fullscreen, chrome.

pub mod chrome;
pub mod tabs;

use crate::input::Key;
use crate::metrics::Snapshot;
use crate::muthur::MuthurTab;
use crate::shell::{PtyEvent, ShellTab};
use crate::splash::Splash;
use crate::cargo::CargoTab;
use crate::utility::UtilityTab;
use chrono::NaiveDateTime;
use ratatui::Frame;
use ratatui::layout::Position;
pub use tabs::{PlaceholderTab, TABS, Tab, TabId};

/// How long a Y/N prompt must be on screen before Y counts (MU/TH/UR, CARGO).
/// Keys sooner than this after the prompt is seen are ignored (see
/// `muthur::State::Override`, `cargo::action::Prompt`).
/// Break arrives as NUL in raw mode (`BRKINT`/`PARMRK` off), decoded as `^@`.
pub const BREAK: Key = Key::Ctrl('@');

/// After Break, how long the line must stay quiet before the re-init.
pub const BREAK_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

pub const ARMING: std::time::Duration = std::time::Duration::from_secs(1);

/// Builds a fresh boot splash, started at the given instant (for Break).
pub type SplashFactory = Box<dyn Fn(std::time::Instant) -> Splash>;

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
    fullscreen: [bool; TABS],
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
    /// Builds a fresh splash for Break (re-initialise the screen).
    splash_factory: Option<SplashFactory>,
    /// The current splash was started by Break: activity behind it is news.
    splash_is_replay: bool,
    /// Seeds a replayed splash.
    last_snapshot: Option<Snapshot>,
    metrics_offline: bool,
    /// After Break: re-initialise when the line has been quiet until then.
    settle_until: Option<std::time::Instant>,
    /// The splash reached `READY.`; the run loop sends BEL.
    bell: bool,
    tabs: [Box<dyn Tab>; TABS],
    activity: [bool; TABS],
    chrome: ChromeView,
}

impl App {
    pub fn new(chrome: ChromeView) -> Self {
        Self::with_tabs(chrome, TabId::ALL.map(|id| Box::new(PlaceholderTab { id }) as Box<dyn Tab>))
    }

    /// The real tabs, in `TabId::ALL` order.
    pub fn assemble(chrome: ChromeView, muthur: MuthurTab, utility: UtilityTab, cargo: CargoTab, shell: ShellTab) -> Self {
        Self::with_tabs(chrome, [Box::new(muthur), Box::new(utility), Box::new(cargo), Box::new(shell)])
    }

    /// Replace one folder's tab (tests: a real tab among placeholders).
    pub fn with_tab(mut self, id: TabId, tab: Box<dyn Tab>) -> Self {
        self.tabs[id.index()] = tab;
        self
    }

    pub fn with_splash(mut self, splash: Splash) -> Self {
        self.splash = Some(splash);
        self
    }

    pub fn with_splash_factory(mut self, f: SplashFactory) -> Self {
        self.splash_factory = Some(f);
        self
    }

    /// True once after the splash reached `READY.`.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell)
    }

    pub fn splash_active(&self) -> bool {
        self.splash.is_some()
    }

    fn end_splash(&mut self) {
        self.splash = None;
        // Whatever happened behind the boot splash is not news; behind a
        // replay (Break) it is, and `render` picks it up.
        if !std::mem::take(&mut self.splash_is_replay) {
            for tab in self.tabs.iter_mut() {
                tab.take_activity();
            }
            self.activity = [false; TABS];
        }
        if self.quit_after_splash {
            self.quit = true;
        } else {
            self.redraw = true;
        }
    }

    pub fn on_llm(&mut self, req_id: u64, ev: crate::llm::StreamEvent) {
        self.tabs[TabId::Muthur.index()].on_llm(req_id, ev);
    }

    pub fn on_cargo_done(&mut self, seq: u64, out: crate::ops::runner::RunOutput, ms: u64) {
        self.tabs[TabId::Cargo.index()].on_cargo_done(seq, out, ms);
    }

    pub fn on_tool_done(&mut self, req_id: u64, call_id: String, outcome: crate::tools::ToolOutcome) {
        self.tabs[TabId::Muthur.index()].on_tool_done(req_id, call_id, outcome);
    }

    /// The active tab's redraw cadence; 1 s when it is idle.
    pub fn tick_interval(&self) -> std::time::Duration {
        if self.settling() {
            return std::time::Duration::from_millis(50);
        }
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
        for id in [TabId::Utility, TabId::Cargo] {
            self.tabs[id.index()].on_metrics(&s);
        }
        self.last_snapshot = Some(s);
        self.metrics_offline = false;
    }

    pub fn on_metrics_offline(&mut self, why: &str) {
        if let Some(sp) = &mut self.splash {
            sp.on_metrics_offline();
        }
        self.metrics_offline = true;
        for id in [TabId::Utility, TabId::Cargo] {
            self.tabs[id.index()].on_metrics_offline(why);
        }
    }

    pub fn on_pty(&mut self, ev: PtyEvent) {
        if ev == PtyEvent::Exited && self.passthrough {
            self.passthrough = false;
            self.redraw = true;
        }
        let shell = &mut self.tabs[TabId::Shell.index()];
        shell.set_passthrough(self.passthrough);
        shell.on_pty(ev);
    }

    /// The active tab's cursor, unless something is drawn over it.
    pub fn cursor(&self) -> Option<Position> {
        if self.overlay.is_some() || self.prefix {
            return None;
        }
        self.tabs[self.active.index()].cursor()
    }

    pub fn with_tabs(chrome: ChromeView, tabs: [Box<dyn Tab>; TABS]) -> Self {
        Self {
            active: TabId::Utility,
            fullscreen: [false; TABS],
            prefix: false,
            overlay: None,
            show_real_date: false,
            quit: false,
            redraw: false,
            passthrough: false,
            quit_after_splash: false,
            splash: None,
            splash_factory: None,
            splash_is_replay: false,
            last_snapshot: None,
            metrics_offline: false,
            settle_until: None,
            bell: false,
            tabs,
            activity: [false; TABS],
            chrome,
        }
    }

    fn switch(&mut self, id: TabId) {
        self.active = id;
        self.activity[id.index()] = false;
    }

    pub fn handle_keys(&mut self, keys: Vec<Key>) {
        self.handle_keys_at(keys, std::time::Instant::now());
    }

    /// A batch with Break, or any batch while the line settles after one, is
    /// dropped whole: around a Break the WY-60 sends line noise and, while it
    /// powers up, echoes our own output back. A batch that arrives during the
    /// splash is consumed whole too: the first key skips it.
    pub fn handle_keys_at(&mut self, keys: Vec<Key>, now: std::time::Instant) {
        if keys.contains(&BREAK) {
            self.on_break(now);
            return;
        }
        if self.settle_until.is_some() {
            tracing::debug!(n = keys.len(), "break: input dropped while settling");
            self.settle_until = Some(now + BREAK_SETTLE);
            return;
        }
        if self.splash.is_some() {
            self.end_splash();
            return;
        }
        for k in keys {
            self.handle_key(k);
        }
    }

    /// True while waiting for the line to go quiet after Break: nothing may be
    /// drawn, because the terminal is still busy and would lose or echo it.
    pub fn settling(&self) -> bool {
        self.settle_until.is_some()
    }

    /// Re-initialise once the line has been quiet for `BREAK_SETTLE`.
    pub fn poll_settle(&mut self, now: std::time::Instant) {
        if self.settle_until.is_some_and(|t| now >= t) {
            self.settle_until = None;
            self.reinit(now);
        }
    }

    /// Break: the terminal was power-cycled or needs a reset. Leave passthrough
    /// and any prefix/overlay now; re-initialise once the line settles.
    fn on_break(&mut self, now: std::time::Instant) {
        self.prefix = false;
        self.overlay = None;
        if self.passthrough {
            self.passthrough = false;
            self.tabs[TabId::Shell.index()].set_passthrough(false);
        }
        self.settle_until = Some(now + BREAK_SETTLE);
    }

    /// Clear, reset the terminal, replay the splash (if configured), repaint.
    fn reinit(&mut self, now: std::time::Instant) {
        tracing::info!(splash = self.splash_factory.is_some(), "break: re-initialising screen");
        self.redraw = true;
        if let Some(f) = &self.splash_factory {
            let mut sp = f(now);
            if let Some(s) = &self.last_snapshot {
                sp.on_metrics(s);
            } else if self.metrics_offline {
                sp.on_metrics_offline();
            }
            self.splash = Some(sp);
            self.splash_is_replay = true;
        }
    }

    pub fn handle_key(&mut self, key: Key) {
        if key == BREAK || self.settle_until.is_some() {
            self.handle_keys(vec![key]);
            return;
        }
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
            Key::F(1..=4) if self.passthrough => {}
            Key::F(n @ 1..=4) => self.switch(TabId::ALL[n as usize - 1]),
            Key::Ctrl(']') => self.prefix = true,
            k => self.tabs[self.active.index()].handle_key(&k),
        }
    }

    /// DECDHL rows for the next frame (only the splash uses them).
    pub fn line_attrs(&self, area: ratatui::layout::Rect) -> Vec<(u16, crate::dialect::LineAttr)> {
        self.splash.as_ref().map(|s| s.line_attrs(area)).unwrap_or_default()
    }

    pub fn render(&mut self, frame: &mut Frame, now: NaiveDateTime) {
        let area = frame.area();
        if let Some(sp) = &mut self.splash {
            sp.advance(std::time::Instant::now());
            self.bell |= sp.take_bell();
            if !sp.is_done() {
                for tab in self.tabs.iter_mut() {
                    tab.set_obscured(true);
                }
                sp.render(area, frame.buffer_mut());
                return;
            }
            // Finished: paint the final frame, hand over on the next one.
            sp.render(area, frame.buffer_mut());
            self.end_splash();
            return;
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
