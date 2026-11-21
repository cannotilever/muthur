//! The TUI: tab focus, prefix commands, fullscreen, chrome.

pub mod chrome;
pub mod tabs;

use crate::cargo::CargoTab;
use crate::destruct::record::Recorder;
use crate::destruct::{Destruct, DestructDeps, DestructRequest, Effect, Outcome, SudoRun};
use crate::input::Key;
use crate::metrics::Snapshot;
use crate::muthur::MuthurTab;
use crate::shell::{PtyEvent, ShellTab};
use crate::splash::Splash;
use crate::standby::Standby;
use crate::utility::UtilityTab;
use chrono::NaiveDateTime;
use ratatui::Frame;
use ratatui::layout::Position;
pub use tabs::{PlaceholderTab, TABS, Tab, TabId};

/// How long a Y/N prompt must be on screen before Y counts (MU/TH/UR, CARGO).
/// Keys sooner than this after the prompt is seen are ignored (see
/// `muthur::State::Ask`, `cargo::action::Prompt`).
/// Break arrives as NUL in raw mode (`BRKINT`/`PARMRK` off), decoded as `^@`.
pub const BREAK: Key = Key::Ctrl('@');

/// After Break, how long the line must stay quiet before the re-init.
pub const BREAK_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

pub const ARMING: std::time::Duration = std::time::Duration::from_secs(1);

/// Builds a fresh boot splash, started at the given instant (for Break).
pub type SplashFactory = Box<dyn Fn(std::time::Instant) -> Splash>;

/// How long the execute command (`systemctl poweroff`/`reboot`) may take.
const EXECUTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long the clearance check (`sudo -S -k true`) may take.
const VERIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Seqs of App's own sudo commands (clearance checks and execute commands).
const EXEC_SEQ_BASE: u64 = 1 << 63;

/// `DestructDeps`, with the timestamp shareable with each run's `Recorder`.
struct Deps {
    run: SudoRun,
    audit: crate::ops::audit::SharedAudit,
    timestamp: std::rc::Rc<dyn Fn() -> String>,
}

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
    /// The splash reached `READY.`, or a prompt woke standby; the run loop sends BEL.
    bell: bool,
    /// Standby begins after this long without a key; `None` = never.
    standby_after: Option<std::time::Duration>,
    /// When the countdown to standby started: the last key batch, or the
    /// first `poll_standby`.
    last_key: Option<std::time::Instant>,
    /// The static standby screen, while it is shown.
    standby: Option<Standby>,
    /// The self-destruct countdown, while it is shown.
    destruct: Option<Destruct>,
    /// Audits the running countdown: created once clearance is passed (`None`
    /// before that, and for a preview).
    recorder: Option<Recorder>,
    /// How a real countdown runs its command and audits; `None` = requests are refused.
    destruct_deps: Option<Deps>,
    /// The seq of the clearance check, until its result arrives.
    verify_seq: Option<u64>,
    /// The seq of the execute command, until its result arrives.
    exec_seq: Option<u64>,
    /// A countdown ended by Break while its execute command still ran: the
    /// command's seq, recorder and argv, so a failure is still audited.
    orphan: Option<(u64, Recorder, Vec<String>)>,
    next_seq: u64,
    /// `muthur destruct`: quit as soon as the countdown ends.
    pub quit_after_destruct: bool,
    tabs: [Box<dyn Tab>; TABS],
    activity: [bool; TABS],
    chrome: ChromeView,
    /// The screen as of the last render (where the destruct cursor goes).
    screen: ratatui::layout::Rect,
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

    /// Show the standby screen after `after` without a key (off by default).
    pub fn with_standby(mut self, after: std::time::Duration) -> Self {
        self.standby_after = Some(after);
        self
    }

    /// Let a tab's armed request start a real countdown.
    pub fn with_destruct(mut self, deps: DestructDeps) -> Self {
        self.destruct_deps = Some(Deps {
            run: deps.run,
            audit: deps.audit,
            timestamp: deps.timestamp.into(),
        });
        self
    }

    pub fn destruct_active(&self) -> bool {
        self.destruct.is_some()
    }

    /// Take over the screen with `d` (a preview runs without a recorder).
    pub fn start_destruct(&mut self, d: Destruct) {
        tracing::info!(kind = d.kind().label(), "destruct: screen up");
        self.destruct = Some(d);
        self.prefix = false;
        self.overlay = None;
        self.standby = None;
        self.redraw = true;
    }

    /// A request from the active tab: the destruct screen, at its clearance stage.
    fn begin_destruct(&mut self, req: DestructRequest) {
        if self.destruct_deps.is_none() {
            tracing::error!("destruct: request dropped (not wired)");
            return;
        }
        self.start_destruct(Destruct::new(req, quiz_seed()));
    }

    /// Advance the countdown; called on every loop wake-up.
    pub fn poll_destruct(&mut self, now: std::time::Instant) {
        if let Some(e) = self.destruct.as_mut().and_then(|d| d.tick(now)) {
            self.destruct_effect(e, now);
        }
    }

    fn destruct_effect(&mut self, e: Effect, now: std::time::Instant) {
        match e {
            Effect::Verify(password) => {
                let Some(deps) = &mut self.destruct_deps else { return };
                let seq = self.next_seq;
                self.next_seq += 1;
                self.verify_seq = Some(seq);
                tracing::info!("destruct: checking clearance");
                (deps.run)(seq, crate::destruct::sudo_argv(&["true"]), VERIFY_TIMEOUT, password);
            }
            Effect::Execute(password) => {
                let Some(d) = &self.destruct else { return };
                let argv = execute_argv(d);
                if let Some(r) = &mut self.recorder {
                    r.record(Outcome::Executed, Some(argv.clone()), "");
                }
                let Some(deps) = &mut self.destruct_deps else { return };
                let seq = self.next_seq;
                self.next_seq += 1;
                self.exec_seq = Some(seq);
                tracing::warn!(?argv, "destruct: executing");
                (deps.run)(seq, argv, EXECUTE_TIMEOUT, password);
            }
            Effect::Done(o) => {
                self.end_destruct(o);
                self.redraw = true;
                self.last_key = Some(now);
            }
        }
    }

    /// Drop the countdown (zeroizing any password) and audit how it ended.
    /// Esc, the freeze, a failure acknowledged, a denial, or Break. Before
    /// clearance there is no recorder, so nothing is audited.
    fn end_destruct(&mut self, o: Outcome) {
        let Some(d) = self.destruct.take() else { return };
        tracing::info!(outcome = ?o, "destruct: ended");
        // A clearance check still running is answered into nothing.
        self.verify_seq = None;
        if let Some(r) = self.recorder.take() {
            match self.exec_seq.take() {
                // Break while executing: the command may still fail.
                Some(seq) => self.orphan = Some((seq, r, execute_argv(&d))),
                None => r.finish(o),
            }
        }
        self.switch(TabId::Utility);
        if self.quit_after_destruct {
            self.quit = true;
        }
    }

    /// The result of sudo command `seq`: the clearance check's or the execute command's.
    pub fn on_sudo_done(&mut self, seq: u64, out: crate::ops::runner::RunOutput) {
        self.on_sudo_done_at(seq, out, std::time::Instant::now());
    }

    /// `on_sudo_done`, with a verified countdown starting at `now`.
    pub fn on_sudo_done_at(&mut self, seq: u64, out: crate::ops::runner::RunOutput, now: std::time::Instant) {
        if self.orphan.as_ref().is_some_and(|(s, ..)| *s == seq) {
            let Some((_, mut r, argv)) = self.orphan.take() else { return };
            if let Some(reason) = crate::destruct::failure_reason(&out) {
                tracing::error!(%reason, "destruct: execute failed (after Break)");
                r.record(Outcome::Failed, Some(argv), &reason);
            }
            return;
        }
        if self.verify_seq.is_some_and(|s| s == seq) {
            self.verify_seq = None;
            self.on_verified(&out, now);
            return;
        }
        if self.exec_seq != Some(seq) {
            tracing::debug!(seq, "destruct: stale sudo result ignored");
            return;
        }
        self.exec_seq = None;
        let Some(d) = &mut self.destruct else { return };
        d.on_executed(&out);
        if let crate::destruct::Phase::Failed(reason) = &d.phase {
            tracing::error!(%reason, "destruct: execute failed");
            let argv = execute_argv(d);
            if let Some(r) = &mut self.recorder {
                r.record(Outcome::Failed, Some(argv), reason);
            }
        }
    }

    /// The clearance check's result: on success the countdown starts, and
    /// only from then on is the run audited.
    fn on_verified(&mut self, out: &crate::ops::runner::RunOutput, now: std::time::Instant) {
        let Some(d) = &mut self.destruct else { return };
        d.on_verified(out, now);
        if !matches!(d.phase, crate::destruct::Phase::Running) {
            tracing::warn!("destruct: clearance denied");
            return;
        }
        tracing::info!(kind = d.kind().label(), "destruct: countdown started");
        if let Some(deps) = &self.destruct_deps {
            self.recorder = Some(Recorder::new(d.kind(), d.total.as_secs(), deps.audit.clone(), deps.timestamp.clone()));
        }
    }

    pub fn standby_active(&self) -> bool {
        self.standby.is_some()
    }

    /// A tool prompt is waiting for the operator: it keeps the TUI awake.
    fn awaiting_operator(&self) -> bool {
        self.tabs[TabId::Muthur.index()].awaiting_operator()
    }

    /// Enter standby once the countdown has run out. Called on every loop
    /// wake-up; `wall` dates the `STANDBY SINCE` caption.
    pub fn poll_standby(&mut self, now: std::time::Instant, wall: NaiveDateTime) {
        if self.destruct.is_some() {
            return;
        }
        let Some(last) = self.last_key else {
            self.last_key = Some(now);
            return;
        };
        let Some(after) = self.standby_after else { return };
        // A prompt needs the operator: wake and call them.
        if self.standby.is_some() && self.awaiting_operator() {
            self.standby = None;
            self.redraw = true;
            self.bell = true;
            self.last_key = Some(now);
            tracing::info!("standby: woke (prompt)");
            return;
        }
        if self.standby.is_some() || now < last + after || self.splash.is_some() || self.passthrough || self.settling() || self.awaiting_operator() {
            return;
        }
        let offset = if self.show_real_date { 0 } else { self.chrome.year_offset };
        self.standby = Some(Standby::new(chrome::format_date(wall, offset)));
        self.prefix = false;
        self.overlay = None;
        self.redraw = true;
        tracing::info!("standby: entered");
    }

    /// True once after the splash reached `READY.`.
    pub fn take_bell(&mut self) -> bool {
        if self.destruct.is_some() {
            return false;
        }
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

    #[cfg(feature = "agent")]
    pub fn on_tool_done(&mut self, req_id: u64, call_id: String, outcome: crate::tools::ToolOutcome) {
        self.tabs[TabId::Muthur.index()].on_tool_done(req_id, call_id, outcome);
    }

    #[cfg(feature = "agent")]
    pub fn on_review(&mut self, req_id: u64, call_id: String, review: crate::review::Review) {
        self.tabs[TabId::Muthur.index()].on_review(req_id, call_id, review);
    }

    /// The active tab's redraw cadence; 1 s when it is idle.
    pub fn tick_interval(&self) -> std::time::Duration {
        if self.settling() {
            return std::time::Duration::from_millis(50);
        }
        if self.destruct.is_some() {
            return crate::destruct::TICK;
        }
        if let Some(s) = &self.splash {
            return s.tick_interval();
        }
        // Nothing animates on the standby screen; the tabs behind it are hidden.
        if self.standby.is_some() {
            return std::time::Duration::from_secs(1);
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

    /// The active tab's cursor, unless something is drawn over it; on the
    /// destruct screen, its password field's.
    pub fn cursor(&self) -> Option<Position> {
        if let Some(d) = &self.destruct {
            return d.cursor(self.screen);
        }
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
            standby_after: None,
            last_key: None,
            standby: None,
            destruct: None,
            recorder: None,
            destruct_deps: None,
            verify_seq: None,
            exec_seq: None,
            orphan: None,
            next_seq: EXEC_SEQ_BASE,
            quit_after_destruct: false,
            tabs,
            activity: [false; TABS],
            chrome,
            screen: ratatui::layout::Rect::new(0, 0, 80, 43),
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
        self.last_key = Some(now);
        // The key that wakes standby does nothing else.
        if self.standby.take().is_some() {
            tracing::info!("standby: woke (key)");
            self.redraw = true;
            return;
        }
        if self.splash.is_some() {
            self.end_splash();
            return;
        }
        for k in keys {
            let counting = self.destruct.is_some();
            self.handle_key_at(k, now);
            // The key that ends the countdown is the last one of its batch.
            if counting && self.destruct.is_none() {
                return;
            }
            // A request starts the destruct screen at once; the rest of the
            // batch was typed at the Utility strip, not at the password field.
            if !counting && let Some(req) = self.tabs[self.active.index()].take_destruct() {
                self.begin_destruct(req);
                if self.destruct.is_some() {
                    return;
                }
            }
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
        if let Some(o) = self.destruct.as_ref().map(Destruct::outcome_so_far) {
            self.end_destruct(o);
        }
        self.prefix = false;
        self.overlay = None;
        if self.standby.take().is_some() {
            tracing::info!("standby: woke (break)");
        }
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
        // The screen is back: the countdown to standby starts again.
        self.last_key = Some(now);
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
        self.handle_key_at(key, std::time::Instant::now());
    }

    fn handle_key_at(&mut self, key: Key, now: std::time::Instant) {
        if key == BREAK || self.settle_until.is_some() || self.standby.is_some() {
            self.handle_keys_at(vec![key], now);
            return;
        }
        if self.splash.is_some() {
            self.end_splash();
            return;
        }
        // The countdown owns the keyboard: no folders, no prefix.
        if let Some(d) = &mut self.destruct {
            if let Some(e) = d.key(&key, now) {
                self.destruct_effect(e, now);
            }
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

    /// DECDHL rows for the next frame (only the splash and standby use them).
    pub fn line_attrs(&self, area: ratatui::layout::Rect) -> Vec<(u16, crate::dialect::LineAttr)> {
        if let Some(d) = &self.destruct {
            return d.line_attrs(area);
        }
        if let Some(s) = &self.standby {
            return s.line_attrs(area);
        }
        self.splash.as_ref().map(|s| s.line_attrs(area)).unwrap_or_default()
    }

    pub fn render(&mut self, frame: &mut Frame, now: NaiveDateTime) {
        let area = frame.area();
        self.screen = area;
        if self.destruct.is_some() || self.standby.is_some() {
            // Collect activity so the markers show afterwards.
            let active = self.active.index();
            for (i, tab) in self.tabs.iter_mut().enumerate() {
                tab.set_obscured(true);
                if tab.take_activity() && i != active {
                    self.activity[i] = true;
                }
            }
        }
        if let Some(d) = &mut self.destruct {
            let t = std::time::Instant::now();
            d.render(t, area, frame.buffer_mut());
            d.rendered(t);
            if let Some(p) = d.cursor(area) {
                frame.set_cursor_position(p);
            }
            return;
        }
        if let Some(sb) = &self.standby {
            // Draw nothing that changes.
            sb.render(area, frame.buffer_mut());
            return;
        }
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

/// The execute command for `d`'s kind.
fn execute_argv(d: &Destruct) -> Vec<String> {
    crate::destruct::sudo_argv(&["systemctl", d.kind().verb()])
}

/// Seeds the arithmetic step: `Instant` has no readable value, the wall clock does.
fn quiz_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() ^ u64::from(d.subsec_nanos()) << 32)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod tests;
