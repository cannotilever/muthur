use super::*;
use crate::dialect::{Glyph, map_glyph};
use crate::input::Key;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use std::sync::{Arc, Mutex};

fn now() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap().and_hms_opt(14, 2, 0).unwrap()
}

fn chrome() -> ChromeView {
    ChromeView {
        ship_name: "USCSS TEST".into(),
        year_offset: 111,
    }
}

fn draw(app: &mut App) -> Buffer {
    let mut t = Terminal::new(TestBackend::new(80, 43)).unwrap();
    t.draw(|f| app.render(f, now())).unwrap();
    t.backend().buffer().clone()
}

fn row(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()
}

fn keys(app: &mut App, ks: &[Key]) {
    for k in ks {
        app.handle_key(k.clone());
    }
}

#[test]
fn title_bar_shows_ship_and_in_universe_date() {
    let buf = draw(&mut App::new(chrome()));
    let r = row(&buf, 0);
    assert!(r.contains("USCSS TEST") && r.contains("MU/TH/UR 6000"), "{r}");
    assert!(r.contains("17 NOV 2137 14:02"), "{r}");
    assert!(buf[(0, 0)].modifier.contains(Modifier::REVERSED));
    assert!(buf[(79, 0)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn real_date_toggle() {
    let mut app = App::new(chrome());
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('d')]);
    assert!(row(&draw(&mut app), 0).contains("17 NOV 2026 14:02"));
}

#[test]
fn sidebar_lists_folders_and_highlights_active() {
    let buf = draw(&mut App::new(chrome()));
    assert!(row(&buf, 2).contains("FOLDERS"));
    let label_row = |id: TabId| chrome::box_top(id) + 1;
    assert!(row(&buf, label_row(TabId::Utility)).contains("UTILITY"));
    assert!(row(&buf, label_row(TabId::Muthur)).contains("MU/TH/UR"));
    assert!(row(&buf, label_row(TabId::Shell)).contains("SHELL"));
    assert!(buf[(chrome::BOX_X + 2, label_row(TabId::Utility))].modifier.contains(Modifier::REVERSED));
    assert!(!buf[(chrome::BOX_X + 2, label_row(TabId::Shell))].modifier.contains(Modifier::REVERSED));
    // connector from the active box to the content pane
    let mid = chrome::box_top(TabId::Utility) + 2;
    assert_eq!(buf[(chrome::BOX_X + chrome::BOX_W, mid)].symbol(), "─");
}

#[test]
fn f_keys_switch_tabs() {
    let mut app = App::new(chrome());
    keys(&mut app, &[Key::F(4)]);
    assert_eq!(app.active, TabId::Shell);
    let buf = draw(&mut app);
    let label_row = chrome::box_top(TabId::Shell) + 1;
    assert!(buf[(chrome::BOX_X + 2, label_row)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn placeholder_says_offline() {
    let buf = draw(&mut App::new(chrome()));
    let all: String = (0..43).map(|y| row(&buf, y)).collect();
    assert!(all.contains("MODULE OFFLINE"));
}

#[test]
fn fullscreen_is_per_tab_and_starts_windowed() {
    let mut app = App::new(chrome());
    for id in TabId::ALL {
        app.active = id;
        assert!(row(&draw(&mut app), 0).contains("MU/TH/UR 6000"), "{id:?} not windowed");
    }
    keys(&mut app, &[Key::F(2), Key::Ctrl(']'), Key::Char('f')]);
    assert!(!row(&draw(&mut app), 0).contains("MU/TH/UR 6000"));
    keys(&mut app, &[Key::F(4)]);
    assert!(row(&draw(&mut app), 0).contains("MU/TH/UR 6000"));
    keys(&mut app, &[Key::F(2)]);
    assert!(!row(&draw(&mut app), 0).contains("MU/TH/UR 6000"));
}

#[test]
fn prefix_commands_accept_caps_lock() {
    let mut app = App::new(chrome());
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('F')]);
    assert!(!row(&draw(&mut app), 0).contains("MU/TH/UR 6000"));
}

#[test]
fn prefix_strip_appears_and_clears() {
    let mut app = App::new(chrome());
    keys(&mut app, &[Key::Ctrl(']')]);
    assert!(row(&draw(&mut app), 42).starts_with("PREFIX:"));
    keys(&mut app, &[Key::Char('x')]);
    let r = row(&draw(&mut app), 42);
    assert!(!r.starts_with("PREFIX:") && r.contains("FULLSCREEN"), "{r}");
}

#[test]
fn prefix_q_quits_and_r_redraws() {
    let mut app = App::new(chrome());
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('r')]);
    assert!(app.redraw && !app.quit);
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('q')]);
    assert!(app.quit);
}

#[test]
fn prefix_p_is_not_available_yet() {
    let mut app = App::new(chrome());
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('p')]);
    let all: String = (0..43).map(|y| row(&draw(&mut app), y)).collect();
    assert!(all.contains("NOT AVAILABLE"));
    keys(&mut app, &[Key::Char('z')]);
    let all: String = (0..43).map(|y| row(&draw(&mut app), y)).collect();
    assert!(!all.contains("NOT AVAILABLE"));
}

#[test]
fn help_overlay() {
    let mut app = App::new(chrome());
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('?')]);
    let all: String = (0..43).map(|y| row(&draw(&mut app), y)).collect();
    assert!(all.contains("QUIT") && all.contains("BREAK            REINIT SCREEN"), "{all}");
}

/// Records each `set_obscured` call made before a render.
struct Cover(Arc<Mutex<Vec<bool>>>);
impl Tab for Cover {
    fn render(&mut self, _area: Rect, _buf: &mut Buffer) {}
    fn handle_key(&mut self, _key: &Key) {}
    fn set_obscured(&mut self, obscured: bool) {
        self.0.lock().unwrap().push(obscured);
    }
}

#[test]
fn active_tab_told_when_covered() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new(chrome()).with_tab(TabId::Utility, Box::new(Cover(log.clone())));
    let step = |app: &mut App, ks: &[Key]| {
        keys(app, ks);
        draw(app);
        log.lock().unwrap().pop()
    };
    assert_eq!(step(&mut app, &[]), Some(false));
    assert_eq!(step(&mut app, &[Key::Ctrl(']')]), Some(true), "prefix strip");
    assert_eq!(step(&mut app, &[Key::Char('?')]), Some(true), "help overlay");
    assert_eq!(step(&mut app, &[Key::Char('x')]), Some(false), "overlay dismissed");
    assert_eq!(step(&mut app, &[Key::Ctrl(']'), Key::Char('p')]), Some(true), "message overlay");
    assert_eq!(step(&mut app, &[Key::Esc]), Some(false));
    // Hidden behind another folder counts as covered too.
    assert_eq!(step(&mut app, &[Key::F(1)]), Some(true));
    assert_eq!(step(&mut app, &[Key::F(2)]), Some(false));
}

struct Recorder(Arc<Mutex<Vec<Key>>>);
impl Tab for Recorder {
    fn render(&mut self, _area: Rect, _buf: &mut Buffer) {}
    fn handle_key(&mut self, key: &Key) {
        self.0.lock().unwrap().push(key.clone());
    }
}

#[test]
fn other_keys_go_to_active_tab() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new(chrome()).with_tab(TabId::Utility, Box::new(Recorder(log.clone())));
    keys(
        &mut app,
        &[Key::Char('a'), Key::F(1), Key::Char('b'), Key::F(2), Key::Ctrl(']'), Key::Char(']')],
    );
    assert_eq!(*log.lock().unwrap(), vec![Key::Char('a'), Key::Ctrl(']')]);
}

#[test]
fn renders_only_dialect_safe_glyphs() {
    let mut app = App::new(chrome());
    for ks in [vec![], vec![Key::Ctrl(']')], vec![Key::Ctrl(']'), Key::Char('?')]] {
        keys(&mut app, &ks);
        let buf = draw(&mut app);
        for c in buf.content() {
            assert!(c.symbol() == "?" || map_glyph(c.symbol()) != Glyph::Ascii(b'?'), "unmappable {:?}", c.symbol());
        }
    }
}

// ---- Shell integration (real `sh` on a PTY) ----

use crate::scroll::HintSlot;
use crate::shell::{PtyEvent, ShellConfig, ShellTab};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn shell_app(args: &[&str]) -> (App, mpsc::Receiver<PtyEvent>) {
    let (tx, rx) = mpsc::channel();
    let cfg = ShellConfig {
        command: "sh".into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        scrollback: 100,
    };
    let shell = ShellTab::new(cfg, HintSlot::default(), move |e| {
        let _ = tx.send(e);
    });
    (
        App::assemble(
            chrome(),
            fake_muthur().0,
            crate::utility::UtilityTab::new(),
            crate::cargo::CargoTab::detached(),
            shell,
        ),
        rx,
    )
}

fn screen_text(buf: &Buffer) -> String {
    (0..buf.area.height).map(|y| row(buf, y) + "\n").collect()
}

fn pump(app: &mut App, rx: &mpsc::Receiver<PtyEvent>, done: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let s = screen_text(&draw(app));
        if done(&s) || Instant::now() > deadline {
            return s;
        }
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(20)) {
            app.on_pty(ev);
        }
    }
}

#[test]
fn typing_reaches_shell() {
    let (mut app, rx) = shell_app(&[]);
    keys(&mut app, &[Key::F(4)]);
    pump(&mut app, &rx, |s| s.contains('$'));
    for c in "echo M\"\"U".chars() {
        app.handle_key(Key::Char(c));
    }
    app.handle_key(Key::Enter);
    let s = pump(&mut app, &rx, |s| {
        s.lines().any(|l| l.chars().skip(20).take(59).collect::<String>().trim() == "MU")
    });
    assert!(s.lines().any(|l| l.chars().skip(20).take(59).collect::<String>().trim() == "MU"), "{s}");
}

#[test]
fn prefix_p_toggles_passthrough_only_on_shell() {
    let (mut app, _rx) = shell_app(&["-c", "sleep 5"]);
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('p')]);
    assert!(!app.passthrough);
    assert!(screen_text(&draw(&mut app)).contains("NOT AVAILABLE"));
    keys(&mut app, &[Key::Char('z'), Key::F(4), Key::Ctrl(']'), Key::Char('p')]);
    assert!(app.passthrough);
    assert!(!row(&draw(&mut app), 0).contains("MU/TH/UR 6000"), "shell not forced fullscreen");
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('p')]);
    assert!(!app.passthrough && app.redraw);
}

#[test]
fn prefix_bracket_enters_scrollback_only_on_shell() {
    let (mut app, rx) = shell_app(&["-c", "seq 1 80; sleep 5"]);
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('[')]);
    assert!(screen_text(&draw(&mut app)).contains("NOT AVAILABLE"));
    keys(&mut app, &[Key::Char('z'), Key::F(4)]);
    pump(&mut app, &rx, |s| s.contains("80"));
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('['), Key::PageUp]);
    assert!(screen_text(&draw(&mut app)).contains("SCROLLBACK"));
}

#[test]
fn double_prefix_sends_literal() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new(chrome()).with_tab(TabId::Utility, Box::new(Recorder(log.clone())));
    keys(&mut app, &[Key::Ctrl(']'), Key::Ctrl(']')]);
    assert_eq!(*log.lock().unwrap(), vec![Key::Ctrl(']')]);
}

#[test]
fn shell_restart_after_exit() {
    let (mut app, rx) = shell_app(&["-c", "echo RUN; exit 0"]);
    keys(&mut app, &[Key::F(4)]);
    pump(&mut app, &rx, |s| s.contains("SHELL TERMINATED"));
    app.handle_key(Key::Enter);
    let s = pump(&mut app, &rx, |s| s.contains("RUN") && !s.contains("TERMINATED"));
    assert!(s.contains("RUN") && !s.contains("TERMINATED"), "{s}");
}

#[test]
fn cursor_follows_shell() {
    let (mut app, rx) = shell_app(&["-c", "printf ab; sleep 5"]);
    assert_eq!(app.cursor(), None);
    keys(&mut app, &[Key::F(4)]);
    pump(&mut app, &rx, |s| s.contains("ab"));
    assert_eq!(app.cursor(), Some(ratatui::layout::Position { x: 22, y: 3 }));
    keys(&mut app, &[Key::Ctrl(']')]);
    assert_eq!(app.cursor(), None, "prefix strip hides the cursor");
}

#[test]
fn shell_exit_ends_passthrough() {
    let (mut app, _rx) = shell_app(&["-c", "sleep 5"]);
    keys(&mut app, &[Key::F(4)]);
    draw(&mut app);
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('p')]);
    app.on_pty(PtyEvent::Exited);
    assert!(!app.passthrough && app.redraw);
}

#[test]
fn f_keys_ignored_in_passthrough() {
    let (mut app, _rx) = shell_app(&["-c", "sleep 5"]);
    keys(&mut app, &[Key::F(4)]);
    draw(&mut app);
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('p'), Key::F(2)]);
    assert_eq!(app.active, TabId::Shell);
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('p')]);
    assert!(!app.passthrough);
}

// ---- MU/TH/UR integration ----

pub(crate) fn fake_muthur() -> (crate::muthur::MuthurTab, tempfile::TempDir) {
    use crate::muthur::{MuthurDeps, MuthurTab};
    let dir = tempfile::tempdir().unwrap();
    let log = crate::chatlog::ChatLog::open(dir.path().join("chat.jsonl")).unwrap();
    let cfg = crate::config::MuthurSection {
        api_key: Some("sk-test".into()),
        ..Default::default()
    };
    let deps = MuthurDeps {
        spawn: Box::new(|_, _, _| Box::new(|| {})),
        now: Box::new(Instant::now),
        ship_time: Box::new(|| "18 NOV 2137 14:02".into()),
        timestamp: Box::new(|| "2026-11-18T14:02:00Z".into()),
        #[cfg(feature = "agent")]
        run_tool: Box::new(|_, _, _, _, _| {}),
        #[cfg(feature = "agent")]
        review: Box::new(|_, _, _| Box::new(|| {})),
    };
    let audit = crate::ops::audit::shared(crate::ops::audit::AuditLog::open(dir.path().join("audit.jsonl")).unwrap());
    #[cfg(feature = "agent")]
    let tab = {
        let tools = crate::tools::ToolDefs::persona(&crate::config::ToolsSection::default());
        let tech = fake_tech(&cfg);
        MuthurTab::new(cfg, tools, "USCSS TEST", log, audit, deps, tech)
    };
    #[cfg(not(feature = "agent"))]
    let tab = MuthurTab::new(cfg, "USCSS TEST", log, audit, deps);
    (tab, dir)
}

#[cfg(feature = "agent")]
/// A technical agent with `run`, the default `[agent]`, a passing probe and reviewer.
pub(crate) fn fake_tech(cfg: &crate::config::MuthurSection) -> crate::muthur::AgentDeps {
    let agent = crate::config::AgentSection::default();
    crate::muthur::AgentDeps {
        defs: crate::tools::ToolDefs::technical(&agent, true),
        cfg: cfg.technical_cfg(),
        prompt: crate::muthur::tech_prompt(&agent),
        facts: Box::new(|| "HOST: test".into()),
        policy: crate::agent::Policy {
            deny: Some(crate::tools::deny::test_rules()),
            net_allow: Vec::new(),
        },
        label: agent.label.clone(),
        max_rounds: agent.max_rounds,
        delegate_report: crate::config::DelegateReport::Relayed,
        probe: crate::tools::shell::Probe::Ready,
        reviewer_ready: true,
    }
}

#[cfg(feature = "agent")]
type Ran = std::rc::Rc<std::cell::RefCell<Vec<String>>>;

#[cfg(feature = "agent")]
/// `fake_muthur` with a hand-driven clock and a record of started tool calls.
fn fake_muthur_clocked() -> (crate::muthur::MuthurTab, tempfile::TempDir, std::rc::Rc<std::cell::Cell<Instant>>, Ran) {
    use crate::muthur::{MuthurDeps, MuthurTab};
    let dir = tempfile::tempdir().unwrap();
    let log = crate::chatlog::ChatLog::open(dir.path().join("chat.jsonl")).unwrap();
    let cfg = crate::config::MuthurSection {
        api_key: Some("sk-test".into()),
        ..Default::default()
    };
    let clock = std::rc::Rc::new(std::cell::Cell::new(Instant::now()));
    let ran: Ran = Default::default();
    let (c, r) = (clock.clone(), ran.clone());
    let deps = MuthurDeps {
        spawn: Box::new(|_, _, _| Box::new(|| {})),
        now: Box::new(move || c.get()),
        ship_time: Box::new(|| "18 NOV 2137 14:02".into()),
        timestamp: Box::new(|| "2026-11-18T14:02:00Z".into()),
        run_tool: Box::new(move |_, id, _, _, _| r.borrow_mut().push(id)),
        review: Box::new(|_, _, _| Box::new(|| {})),
    };
    let tools = crate::tools::ToolDefs::persona(&crate::config::ToolsSection::default());
    let audit = crate::ops::audit::AuditLog::open(dir.path().join("audit.jsonl")).unwrap();
    let tech = fake_tech(&cfg);
    (
        MuthurTab::new(cfg, tools, "USCSS TEST", log, crate::ops::audit::shared(audit), deps, tech),
        dir,
        clock,
        ran,
    )
}

#[cfg(feature = "agent")]
#[test]
fn prompt_under_help_overlay_rearms_after_dismissal() {
    use crate::llm::{Stop, StreamEvent};
    use crate::review::{Review, Risk, Verdict};
    let (m, _dir, clock, ran) = fake_muthur_clocked();
    let mut app = App::new(chrome()).with_tab(TabId::Muthur, Box::new(m));
    let tick = |ms: u64| clock.set(clock.get() + Duration::from_millis(ms));
    keys(&mut app, &[Key::F(1)]);
    for line in ["/direct", "restart nginx"] {
        keys(&mut app, &line.chars().map(Key::Char).collect::<Vec<_>>());
        keys(&mut app, &[Key::Enter]);
    }
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('?')]);
    let input = serde_json::json!({"command": "systemctl restart nginx", "reason": "asked", "as_root": false, "timeout_secs": 0});
    app.on_llm(
        1,
        StreamEvent::ToolCall {
            id: "c1".into(),
            name: "run".into(),
            input: input.clone(),
        },
    );
    let raw = vec![serde_json::json!({"type":"tool_use","id":"c1","name":"run","input":input})];
    app.on_llm(
        1,
        StreamEvent::Done {
            stop: Stop::ToolUse,
            model: Some("m".into()),
            raw: Some(raw),
        },
    );
    let ask = Review {
        verdict: Verdict::Ask,
        risk: Risk::Medium,
        note: "restarts a service".into(),
        model: "r".into(),
        ms: 1,
        error: None,
    };
    app.on_review(1, "c1".into(), ask);
    // The Y/N prompt is raised under the help overlay and rendered there for a while.
    for _ in 0..30 {
        draw(&mut app);
        tick(100);
    }
    keys(&mut app, &[Key::Esc]); // dismisses the overlay only
    keys(&mut app, &[Key::Char('y')]);
    assert!(ran.borrow().is_empty(), "prompt never seen must not be armed");
    draw(&mut app);
    tick(100);
    draw(&mut app);
    tick(500);
    keys(&mut app, &[Key::Char('y')]);
    assert!(ran.borrow().is_empty(), "y inside the arming delay after dismissal");
    tick(600);
    keys(&mut app, &[Key::Char('y')]);
    assert_eq!(*ran.borrow(), vec!["c1".to_string()]);
}

#[test]
fn activity_marker_for_background_reply() {
    use crate::llm::{Stop, StreamEvent};
    let (m, _dir) = fake_muthur();
    let (shell_tx, _rx) = mpsc::channel();
    let cfg = ShellConfig {
        command: "sh".into(),
        args: vec!["-c".into(), "sleep 2".into()],
        scrollback: 10,
    };
    let shell = ShellTab::new(cfg, HintSlot::default(), move |e| {
        let _ = shell_tx.send(e);
    });
    let mut app = App::assemble(chrome(), m, crate::utility::UtilityTab::new(), crate::cargo::CargoTab::detached(), shell);
    keys(&mut app, &[Key::F(1), Key::Char('H'), Key::Char('I'), Key::Enter, Key::F(2)]);
    app.on_llm(1, StreamEvent::Text("OK".into()));
    app.on_llm(
        1,
        StreamEvent::Done {
            stop: Stop::End,
            model: None,
            raw: None,
        },
    );
    let buf = draw(&mut app);
    let top = chrome::box_top(TabId::Muthur);
    assert_eq!(buf[(chrome::BOX_X + chrome::BOX_W - 2, top + 1)].symbol(), "*");
}

#[test]
fn hidden_reveal_does_not_drive_tick() {
    use crate::llm::{Stop, StreamEvent};
    let (m, _dir) = fake_muthur();
    let shell = ShellTab::new(
        ShellConfig {
            command: "sh".into(),
            args: vec!["-c".into(), "sleep 2".into()],
            scrollback: 10,
        },
        HintSlot::default(),
        |_| {},
    );
    let mut app = App::assemble(chrome(), m, crate::utility::UtilityTab::new(), crate::cargo::CargoTab::detached(), shell);
    keys(&mut app, &[Key::F(1), Key::Char('H'), Key::Enter, Key::F(2)]);
    app.on_llm(1, StreamEvent::Text("OK".into()));
    app.on_llm(
        1,
        StreamEvent::Done {
            stop: Stop::End,
            model: None,
            raw: None,
        },
    );
    assert_eq!(app.tick_interval(), Duration::from_secs(1));
    app.handle_key(Key::F(1));
    assert!(app.tick_interval() < Duration::from_secs(1));
}

// ---- Boot splash ----

use crate::metrics::Snapshot;
use crate::splash::{Interface, Splash, SplashInfo};

fn splash_info() -> SplashInfo {
    SplashInfo {
        ship_name: "USCSS TEST".into(),
        year: 2137,
        link: "38400 BAUD  XON/XOFF".into(),
        interface: Interface::Online,
    }
}
fn fresh_splash() -> Splash {
    Splash::new(splash_info(), Instant::now())
}
fn stale_splash() -> Splash {
    let mut s = Splash::new(splash_info(), Instant::now() - Duration::from_secs(60));
    s.on_metrics(&Snapshot::default());
    s
}

#[derive(Default)]
struct SpyState {
    keys: Vec<Key>,
    obscured: Option<bool>,
    metrics: usize,
    activity: bool,
    passthrough: Vec<bool>,
    cargo_done: Vec<u64>,
}
struct Spy(Arc<Mutex<SpyState>>);
impl Tab for Spy {
    fn render(&mut self, _area: Rect, _buf: &mut Buffer) {}
    fn on_cargo_done(&mut self, seq: u64, _out: crate::ops::runner::RunOutput, _ms: u64) {
        self.0.lock().unwrap().cargo_done.push(seq);
    }
    fn handle_key(&mut self, key: &Key) {
        self.0.lock().unwrap().keys.push(key.clone());
    }
    fn set_obscured(&mut self, o: bool) {
        self.0.lock().unwrap().obscured = Some(o);
    }
    fn on_metrics(&mut self, _s: &Snapshot) {
        self.0.lock().unwrap().metrics += 1;
    }
    fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.0.lock().unwrap().activity)
    }
    fn on_pty(&mut self, _ev: PtyEvent) {}
    fn set_passthrough(&mut self, on: bool) {
        self.0.lock().unwrap().passthrough.push(on);
    }
}

fn spy_app(state: &Arc<Mutex<SpyState>>, id: TabId) -> App {
    App::new(chrome()).with_tab(id, Box::new(Spy(state.clone())))
}

#[test]
fn splash_draws_instead_of_chrome() {
    let mut app = App::new(chrome()).with_splash(fresh_splash());
    let buf = draw(&mut app);
    assert!(row(&buf, 24).contains("MU/TH/UR 6000"), "{}", row(&buf, 24));
    assert!(!row(&buf, 0).contains("USCSS TEST"));
    assert!(app.splash_active());
}

#[test]
fn key_skips_splash_and_is_not_delivered() {
    let st = Arc::new(Mutex::new(SpyState::default()));
    let mut app = spy_app(&st, TabId::Utility).with_splash(fresh_splash());
    app.handle_key(Key::Char('x'));
    assert!(!app.splash_active() && app.redraw);
    assert!(st.lock().unwrap().keys.is_empty());
}

#[test]
fn ctrl_c_skips_too() {
    let mut app = App::new(chrome()).with_splash(fresh_splash());
    app.handle_key(Key::Ctrl('c'));
    assert!(!app.splash_active() && !app.quit);
}

#[test]
fn finished_splash_hands_over_with_redraw() {
    let mut app = App::new(chrome()).with_splash(stale_splash());
    let buf = draw(&mut app);
    assert!(!app.splash_active() && app.redraw);
    assert!(!row(&buf, 0).contains("USCSS TEST"), "chrome must not paint over the final splash frame");
    let all: String = (0..43).map(|y| row(&buf, y)).collect();
    assert!(all.contains("INTERFACE"), "{all}");
}

#[test]
fn key_batch_is_consumed_whole_while_splash_active() {
    let mut app = App::new(chrome()).with_splash(fresh_splash());
    app.handle_keys(vec![Key::Char(' '), Key::Ctrl(']')]);
    assert!(!app.splash_active());
    assert!(!app.prefix, "rest of the batch must not reach the TUI");
    app.handle_keys(vec![Key::Ctrl(']')]);
    assert!(app.prefix);
}

#[test]
fn tick_interval_follows_splash() {
    let mut app = App::new(chrome()).with_splash(fresh_splash());
    assert!(app.tick_interval() <= Duration::from_millis(100));
    app.handle_key(Key::Char(' '));
    assert!(app.tick_interval() > Duration::from_millis(100));
}

#[test]
fn quit_after_splash_quits_on_finish_and_on_skip() {
    let mut app = App::new(chrome()).with_splash(stale_splash());
    app.quit_after_splash = true;
    draw(&mut app);
    assert!(app.quit && !app.redraw);
    let mut app = App::new(chrome()).with_splash(fresh_splash());
    app.quit_after_splash = true;
    app.handle_key(Key::Char(' '));
    assert!(app.quit && !app.redraw);
}

#[test]
fn metrics_reach_splash_and_utility() {
    let st = Arc::new(Mutex::new(SpyState::default()));
    let mut app = spy_app(&st, TabId::Utility).with_splash(Splash::new(splash_info(), Instant::now() - Duration::from_millis(2500)));
    app.on_metrics(Snapshot {
        cpu_cores: vec![0.0; 16],
        ..Default::default()
    });
    assert_eq!(st.lock().unwrap().metrics, 1);
    let buf = draw(&mut app);
    let all: String = (0..43).map(|y| row(&buf, y)).collect();
    assert!(all.contains("16 CORES"), "{all}");
}

#[test]
fn tabs_are_obscured_during_splash() {
    let st = Arc::new(Mutex::new(SpyState::default()));
    let mut app = spy_app(&st, TabId::Utility).with_splash(fresh_splash());
    draw(&mut app);
    assert_eq!(st.lock().unwrap().obscured, Some(true));
}

#[test]
fn no_activity_markers_after_splash() {
    let st = Arc::new(Mutex::new(SpyState::default()));
    let mut app = spy_app(&st, TabId::Muthur).with_splash(fresh_splash());
    st.lock().unwrap().activity = true;
    draw(&mut app); // splash frame must not consume or light activity
    app.handle_key(Key::Char('x'));
    let buf = draw(&mut app);
    let top = chrome::box_top(TabId::Muthur);
    assert_ne!(buf[(chrome::BOX_X + chrome::BOX_W - 2, top + 1)].symbol(), "*");
}

#[test]
fn no_splash_is_unchanged() {
    assert!(!App::new(chrome()).splash_active());
}

#[test]
fn shell_learns_passthrough_before_each_pty_event() {
    let st = Arc::new(Mutex::new(SpyState::default()));
    let mut app = spy_app(&st, TabId::Shell);
    app.on_pty(PtyEvent::Output(b"x".to_vec()));
    app.passthrough = true;
    app.on_pty(PtyEvent::Output(b"y".to_vec()));
    assert_eq!(st.lock().unwrap().passthrough, vec![false, true]);
}

#[test]
fn natural_finish_passes_the_bell_on_once() {
    let mut app = App::new(chrome()).with_splash(stale_splash());
    draw(&mut app);
    assert!(app.take_bell());
    assert!(!app.take_bell());
}

#[test]
fn skipping_before_ready_does_not_ring() {
    let mut app = App::new(chrome()).with_splash(fresh_splash());
    draw(&mut app);
    keys(&mut app, &[Key::Char('x')]);
    draw(&mut app);
    assert!(!app.take_bell());
}

#[test]
fn f_keys_follow_sidebar_order() {
    assert_eq!(TabId::ALL.map(TabId::label), ["MU/TH/UR", "UTILITY", "CARGO", "SHELL"]);
    let mut app = App::new(chrome());
    assert_eq!(app.active, TabId::Utility, "starts on UTILITY");
    for (n, id) in [(1, TabId::Muthur), (2, TabId::Utility), (3, TabId::Cargo), (4, TabId::Shell)] {
        keys(&mut app, &[Key::F(n)]);
        assert_eq!(app.active, id);
    }
    let buf = draw(&mut app);
    assert!(row(&buf, chrome::box_top(TabId::Cargo) + 1).contains("CARGO"));
    assert!(chrome::box_top(TabId::Shell) + chrome::BOX_H < chrome::FOOTER_ROW);
}

#[test]
fn metrics_and_outage_reach_utility_and_cargo() {
    let u = Arc::new(Mutex::new(SpyState::default()));
    let c = Arc::new(Mutex::new(SpyState::default()));
    let mut app = spy_app(&u, TabId::Utility).with_tab(TabId::Cargo, Box::new(Spy(c.clone())));
    app.on_metrics(Snapshot::default());
    assert_eq!((u.lock().unwrap().metrics, c.lock().unwrap().metrics), (1, 1));

    let mut app = App::new(chrome())
        .with_tab(TabId::Utility, Box::new(crate::utility::UtilityTab::new()))
        .with_tab(TabId::Cargo, Box::new(crate::cargo::CargoTab::detached()));
    app.on_metrics_offline("NO /PROC");
    for id in [TabId::Utility, TabId::Cargo] {
        app.active = id;
        let all = screen_text(&draw(&mut app));
        assert!(all.contains("TELEMETRY OFFLINE") && all.contains("NO /PROC"), "{id:?}\n{all}");
    }
}

#[test]
fn cargo_done_reaches_cargo() {
    let st = Arc::new(Mutex::new(SpyState::default()));
    let mut app = spy_app(&st, TabId::Cargo);
    app.on_cargo_done(7, crate::ops::runner::RunOutput::NotFound, 3);
    assert_eq!(st.lock().unwrap().cargo_done, vec![7]);
}

// ---- Break: re-initialise the screen ----

const BREAK: Key = Key::Ctrl('@');

/// A factory that counts its calls and builds splashes started `ago` in the past.
fn counting_factory(ago: Duration) -> (SplashFactory, std::rc::Rc<std::cell::Cell<usize>>) {
    let n = std::rc::Rc::new(std::cell::Cell::new(0));
    let calls = n.clone();
    let f = move |now: Instant| {
        calls.set(calls.get() + 1);
        Splash::new(splash_info(), now - ago)
    };
    (Box::new(f), n)
}

/// Break, then let the line settle.
fn brk(app: &mut App) {
    let t = Instant::now();
    app.handle_keys_at(vec![BREAK], t);
    app.poll_settle(t + BREAK_SETTLE);
}

#[test]
fn break_waits_for_the_line_to_settle() {
    let (f, _) = counting_factory(Duration::ZERO);
    let mut app = App::new(chrome()).with_splash_factory(f);
    let t = Instant::now();
    app.handle_keys_at(vec![BREAK], t);
    assert!(
        app.settling() && !app.redraw && !app.splash_active(),
        "nothing is sent while the terminal is busy"
    );
    app.poll_settle(t + BREAK_SETTLE - Duration::from_millis(1));
    assert!(app.settling() && !app.redraw);
    app.poll_settle(t + BREAK_SETTLE);
    assert!(!app.settling() && app.redraw && app.splash_active());
}

#[test]
fn input_while_settling_is_dropped_and_restarts_the_wait() {
    let st = Arc::new(Mutex::new(SpyState::default()));
    let mut app = spy_app(&st, TabId::Utility);
    let t = Instant::now();
    app.handle_keys_at(vec![BREAK], t);
    let t2 = t + Duration::from_millis(300);
    app.handle_keys_at(vec![Key::Char('x')], t2); // echo / line noise
    app.poll_settle(t + BREAK_SETTLE);
    assert!(app.settling(), "noise restarts the wait");
    let t3 = t2 + Duration::from_millis(300);
    app.handle_keys_at(vec![BREAK], t3); // another NUL from power-on noise
    app.poll_settle(t2 + BREAK_SETTLE);
    assert!(app.settling());
    app.poll_settle(t3 + BREAK_SETTLE);
    assert!(!app.settling() && app.redraw);
    assert!(st.lock().unwrap().keys.is_empty(), "nothing received while settling reaches a tab");
}

#[test]
fn settling_wakes_the_loop_often() {
    let mut app = App::new(chrome());
    app.handle_key(BREAK);
    assert!(app.tick_interval() <= Duration::from_millis(50));
}

#[test]
fn break_without_factory_only_redraws() {
    let mut app = App::new(chrome());
    brk(&mut app);
    assert!(!app.splash_active() && app.redraw);
}

#[test]
fn break_during_splash_restarts_it() {
    let (f, n) = counting_factory(Duration::ZERO);
    let mut app = App::new(chrome()).with_splash(fresh_splash()).with_splash_factory(f);
    brk(&mut app);
    assert!(app.splash_active(), "Break must not skip the splash");
    assert_eq!(n.get(), 1, "a fresh splash replaces the old one");
}

#[test]
fn break_clears_prefix_and_overlay() {
    let mut app = App::new(chrome());
    keys(&mut app, &[Key::Ctrl(']')]);
    brk(&mut app);
    assert!(!app.prefix && app.redraw, "Break acts even after ^]");
    app.redraw = false;
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('?')]);
    brk(&mut app);
    assert!(app.overlay.is_none() && app.redraw, "Break acts under an overlay");
}

#[test]
fn break_ends_passthrough() {
    let st = Arc::new(Mutex::new(SpyState::default()));
    let mut app = spy_app(&st, TabId::Shell);
    app.active = TabId::Shell;
    app.passthrough = true;
    brk(&mut app);
    assert!(!app.passthrough && app.redraw);
    assert_eq!(st.lock().unwrap().passthrough, vec![false], "the shell learns at once");
    assert!(st.lock().unwrap().keys.is_empty(), "Break is not sent to the shell");
}

#[test]
fn activity_survives_replayed_splash() {
    let st = Arc::new(Mutex::new(SpyState::default()));
    let (f, _) = counting_factory(Duration::ZERO);
    let mut app = spy_app(&st, TabId::Muthur).with_splash_factory(f);
    st.lock().unwrap().activity = true;
    draw(&mut app); // lights the MU/TH/UR marker
    brk(&mut app);
    assert!(app.splash_active());
    draw(&mut app);
    app.handle_key(Key::Char('x')); // skip the replay
    let buf = draw(&mut app);
    let top = chrome::box_top(TabId::Muthur);
    assert_eq!(buf[(chrome::BOX_X + chrome::BOX_W - 2, top + 1)].symbol(), "*");
}

#[test]
fn replayed_splash_is_seeded_with_last_snapshot() {
    let (f, _) = counting_factory(Duration::from_millis(2500));
    let mut app = App::new(chrome()).with_splash_factory(f);
    app.on_metrics(Snapshot {
        cpu_cores: vec![0.0; 16],
        ..Default::default()
    });
    brk(&mut app);
    let all = screen_text(&draw(&mut app));
    assert!(all.contains("16 CORES") && !all.contains("NO DATA"), "{all}");
}

#[test]
fn replayed_splash_is_seeded_offline() {
    let (f, _) = counting_factory(Duration::from_millis(2500));
    let mut app = App::new(chrome()).with_splash_factory(f);
    app.on_metrics_offline("NO /PROC");
    brk(&mut app);
    let all = screen_text(&draw(&mut app));
    assert!(all.contains("NO DATA"), "{all}");
}

#[test]
fn batch_with_break_is_dropped_whole() {
    let st = Arc::new(Mutex::new(SpyState::default()));
    let mut app = spy_app(&st, TabId::Utility);
    let t = Instant::now();
    app.handle_keys_at(vec![Key::Char('y'), BREAK, Key::Char('x')], t);
    app.poll_settle(t + BREAK_SETTLE);
    assert!(app.redraw);
    assert!(st.lock().unwrap().keys.is_empty(), "noise around the NUL never reaches a tab");
}

// ── Standby ─────────────────────────────────────────────────────────────────

const D: Duration = Duration::from_secs(600);
const S: Duration = Duration::from_secs(1);

/// An app with standby after `D`, its countdown started at the returned instant.
fn standby_app() -> (App, Instant) {
    let mut app = App::new(chrome()).with_standby(D);
    let t0 = Instant::now();
    app.poll_standby(t0, now());
    (app, t0)
}

fn all_rows(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height).map(|y| row(buf, y)).collect()
}

#[test]
fn standby_begins_at_timeout() {
    let (mut app, t0) = standby_app();
    app.poll_standby(t0 + D - S, now());
    assert!(!app.standby_active());
    app.poll_standby(t0 + D, now());
    assert!(app.standby_active() && app.redraw);
}

#[test]
fn key_resets_countdown() {
    let (mut app, t0) = standby_app();
    let t1 = t0 + Duration::from_secs(300);
    app.handle_keys_at(vec![Key::Char('a')], t1);
    app.poll_standby(t0 + D, now());
    assert!(!app.standby_active());
    app.poll_standby(t1 + D, now());
    assert!(app.standby_active());
}

#[test]
fn standby_off_by_default() {
    let mut app = App::new(chrome());
    let t0 = Instant::now();
    app.poll_standby(t0, now());
    app.poll_standby(t0 + Duration::from_secs(36_000), now());
    assert!(!app.standby_active());
}

#[test]
fn no_standby_during_splash() {
    let mut app = App::new(chrome()).with_standby(D).with_splash(fresh_splash());
    let t0 = Instant::now();
    app.poll_standby(t0, now());
    app.poll_standby(t0 + D, now());
    assert!(!app.standby_active());
}

#[test]
fn no_standby_in_passthrough() {
    let (mut app, t0) = standby_app();
    app.passthrough = true;
    app.poll_standby(t0 + D, now());
    assert!(!app.standby_active());
}

#[test]
fn no_standby_while_settling() {
    let (mut app, t0) = standby_app();
    app.handle_keys_at(vec![BREAK], t0 + S);
    app.poll_standby(t0 + D + S, now());
    assert!(!app.standby_active());
}

#[test]
fn wake_key_is_swallowed() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new(chrome()).with_standby(D).with_tab(TabId::Utility, Box::new(Recorder(log.clone())));
    let t0 = Instant::now();
    app.poll_standby(t0, now());
    app.poll_standby(t0 + D, now());
    app.redraw = false;
    app.handle_keys_at(vec![Key::Char('y'), Key::Ctrl(']'), Key::Char('q')], t0 + D + S);
    assert!(log.lock().unwrap().is_empty());
    assert!(!app.quit && !app.standby_active() && app.redraw);
    keys(&mut app, &[Key::Char('z')]);
    assert_eq!(*log.lock().unwrap(), vec![Key::Char('z')], "prefix not left open");
}

#[test]
fn wake_restarts_countdown() {
    let (mut app, t0) = standby_app();
    app.poll_standby(t0 + D, now());
    let t1 = t0 + D + Duration::from_secs(30);
    app.handle_keys_at(vec![Key::Char('x')], t1);
    app.poll_standby(t1 + D - S, now());
    assert!(!app.standby_active());
    app.poll_standby(t1 + D, now());
    assert!(app.standby_active());
}

#[test]
fn break_while_asleep_ends_standby_and_settles() {
    let (mut app, t0) = standby_app();
    app.poll_standby(t0 + D, now());
    app.handle_keys_at(vec![Key::Char('x'), BREAK], t0 + D + S);
    assert!(!app.standby_active() && app.settling());
}

#[test]
fn entering_standby_closes_prefix_and_overlay() {
    // The prefix strip, then the help overlay, open when the countdown ends.
    for ks in [vec![Key::Ctrl(']')], vec![Key::Ctrl(']'), Key::Char('?')]] {
        let (mut app, t0) = standby_app();
        keys(&mut app, &ks);
        app.poll_standby(t0 + D, now());
        let rows = all_rows(&draw(&mut app));
        assert!(rows[27].contains("STANDBY SINCE 17 NOV 2137 14:02"), "{}", rows[27]);
        assert!(rows.iter().all(|r| !r.contains("PREFIX") && !r.contains("QUIT")), "{rows:#?}");
    }
}

#[test]
fn standby_frame_replaces_chrome() {
    use crate::dialect::LineAttr;
    let (mut app, t0) = standby_app();
    app.poll_standby(t0 + D, now());
    let buf = draw(&mut app);
    assert!(!row(&buf, 0).contains("USCSS TEST"));
    assert!(row(&buf, 26).contains("INTERFACE STANDBY"));
    assert_eq!(
        app.line_attrs(Rect::new(0, 0, 80, 43)),
        vec![(13, LineAttr::DoubleTop), (14, LineAttr::DoubleBottom)]
    );
}

#[test]
fn standby_date_honours_real_date_toggle() {
    let (mut app, t0) = standby_app();
    keys(&mut app, &[Key::Ctrl(']'), Key::Char('d')]);
    app.poll_standby(t0 + D, now());
    assert!(row(&draw(&mut app), 27).contains("17 NOV 2026 14:02"));
}

#[test]
fn no_bell_while_asleep() {
    let (mut app, t0) = standby_app();
    app.poll_standby(t0 + D, now());
    for _ in 0..3 {
        draw(&mut app);
    }
    assert!(!app.take_bell());
}

/// Reports a tool prompt while its flag is set.
struct Prompter(Arc<std::sync::atomic::AtomicBool>);
impl Tab for Prompter {
    fn render(&mut self, _area: Rect, _buf: &mut Buffer) {}
    fn handle_key(&mut self, _key: &Key) {}
    fn awaiting_operator(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn prompter_app() -> (App, Instant, Arc<std::sync::atomic::AtomicBool>) {
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut app = App::new(chrome()).with_standby(D).with_tab(TabId::Muthur, Box::new(Prompter(flag.clone())));
    let t0 = Instant::now();
    app.poll_standby(t0, now());
    (app, t0, flag)
}

#[test]
fn prompt_wakes_standby_and_rings() {
    let (mut app, t0, flag) = prompter_app();
    app.poll_standby(t0 + D, now());
    assert!(app.standby_active());
    app.redraw = false;
    flag.store(true, std::sync::atomic::Ordering::SeqCst);
    app.poll_standby(t0 + D + S, now());
    assert!(!app.standby_active() && app.redraw && app.take_bell());
}

#[test]
fn prompt_showing_blocks_standby() {
    let (mut app, t0, flag) = prompter_app();
    flag.store(true, std::sync::atomic::Ordering::SeqCst);
    app.poll_standby(t0 + D, now());
    assert!(!app.standby_active());
    flag.store(false, std::sync::atomic::Ordering::SeqCst);
    app.poll_standby(t0 + D + S, now());
    assert!(app.standby_active(), "the countdown still runs from the last key");
}

#[test]
fn break_restarts_countdown() {
    let (mut app, t0) = standby_app();
    app.poll_standby(t0 + D, now());
    let tb = t0 + D + S;
    app.handle_keys_at(vec![BREAK], tb);
    let after = tb + Duration::from_secs(5);
    app.poll_settle(after);
    app.poll_standby(after, now());
    assert!(!app.standby_active(), "Break woke standby but it re-entered at once");
    app.poll_standby(after + D, now());
    assert!(app.standby_active(), "the countdown runs from the re-init");
}

/// Wants a fast redraw cadence (a reply animation).
struct Animating;
impl Tab for Animating {
    fn render(&mut self, _area: Rect, _buf: &mut Buffer) {}
    fn handle_key(&mut self, _key: &Key) {}
    fn tick_interval(&self) -> Option<Duration> {
        Some(Duration::from_millis(100))
    }
}

#[test]
fn standby_ignores_the_hidden_tabs_cadence() {
    let mut app = App::new(chrome()).with_standby(D).with_tab(TabId::Utility, Box::new(Animating));
    let t0 = Instant::now();
    app.poll_standby(t0, now());
    assert_eq!(app.tick_interval(), Duration::from_millis(100));
    app.poll_standby(t0 + D, now());
    assert_eq!(app.tick_interval(), Duration::from_secs(1), "nothing animates on the standby screen");
}

// ---- Self-destruct countdown ----

use crate::destruct::{DestructDeps, DestructRequest, Kind};
use crate::ops::runner::RunOutput;
use crate::secret::Secret;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct ArmerState {
    request: Option<DestructRequest>,
    sudo_done: Vec<u64>,
}

/// A Utility stand-in: the first key it receives arms a countdown.
struct Armer(Rc<RefCell<ArmerState>>, Option<Duration>);
impl Tab for Armer {
    fn render(&mut self, _area: Rect, _buf: &mut Buffer) {}
    fn handle_key(&mut self, _key: &Key) {
        if let Some(total) = self.1.take() {
            let mut password = Secret::new();
            password.push('p');
            password.push('w');
            self.0.borrow_mut().request = Some(DestructRequest {
                kind: Kind::Shutdown,
                total,
                password,
            });
        }
    }
    fn take_destruct(&mut self) -> Option<DestructRequest> {
        self.0.borrow_mut().request.take()
    }
    fn on_sudo_done(&mut self, seq: u64, _out: RunOutput) {
        self.0.borrow_mut().sudo_done.push(seq);
    }
}

type Calls = Rc<RefCell<Vec<(u64, Vec<String>, String)>>>;

struct Rig {
    app: App,
    t0: Instant,
    calls: Calls,
    armer: Rc<RefCell<ArmerState>>,
    dir: tempfile::TempDir,
}

impl Rig {
    fn audit(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir.path().join("audit.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn exits(&self) -> Vec<String> {
        self.audit().iter().map(|e| e["exit"].as_str().unwrap().to_string()).collect()
    }
    fn strip(&mut self) -> String {
        row(&draw(&mut self.app), 42)
    }
}

/// An app whose Utility tab arms a `secs`-second countdown on the first key,
/// armed (by a key batch at the returned `t0`) unless `arm` is false.
fn destruct_rig(secs: u64, standby: Option<Duration>, arm: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let audit = crate::ops::audit::shared(crate::ops::audit::AuditLog::open(dir.path().join("audit.jsonl")).unwrap());
    let calls: Calls = Rc::default();
    let c = calls.clone();
    let deps = DestructDeps {
        run: Box::new(move |seq, argv, _timeout, pw: Secret| c.borrow_mut().push((seq, argv, pw.expose().to_string()))),
        audit,
        timestamp: Box::new(|| "2026-11-20T00:00:00Z".into()),
    };
    let armer = Rc::new(RefCell::new(ArmerState::default()));
    let mut app = App::new(chrome())
        .with_tab(TabId::Utility, Box::new(Armer(armer.clone(), Some(Duration::from_secs(secs)))))
        .with_destruct(deps);
    if let Some(s) = standby {
        app = app.with_standby(s);
    }
    let t0 = Instant::now();
    if arm {
        app.handle_keys_at(vec![Key::Enter], t0);
    }
    Rig { app, t0, calls, armer, dir }
}

#[test]
fn request_takes_over_screen() {
    let mut r = destruct_rig(60, None, false);
    r.app.redraw = false;
    r.app.handle_keys_at(vec![Key::Enter], r.t0);
    assert!(r.app.destruct_active() && r.app.redraw);
    assert!(row(&draw(&mut r.app), 0).contains("EMERGENCY DESTRUCT SYSTEM"));
    assert_eq!(r.app.tick_interval(), crate::destruct::TICK);
    assert!(r.app.cursor().is_none());
}

#[test]
fn fkeys_and_prefix_locked_out() {
    let mut r = destruct_rig(60, None, true);
    r.app.handle_keys_at(vec![Key::F(1), Key::Ctrl(']'), Key::Char('f')], r.t0);
    assert_eq!(r.app.active, TabId::Utility);
    let s = r.strip();
    assert!(!s.starts_with("PREFIX:"), "{s}");
    assert!(s.trim_end().ends_with('f'), "{s}");
    assert!(row(&draw(&mut r.app), 0).contains("EMERGENCY DESTRUCT SYSTEM"), "not fullscreened away");
}

#[test]
fn standby_never_starts_during_countdown() {
    let mut r = destruct_rig(600, Some(S), true);
    r.app.poll_standby(r.t0 + Duration::from_secs(5), now());
    assert!(!r.app.standby_active());
    let esc = r.t0 + Duration::from_secs(6);
    r.app.handle_keys_at(vec![Key::Esc], esc);
    assert!(!r.app.destruct_active());
    r.app.poll_standby(esc + S - Duration::from_millis(1), now());
    assert!(!r.app.standby_active());
    r.app.poll_standby(esc + S, now());
    assert!(r.app.standby_active());
}

#[test]
fn no_bell_during_countdown() {
    let mut r = destruct_rig(60, None, true);
    r.app.bell = true;
    assert!(!r.app.take_bell());
}

#[test]
fn zero_runs_systemctl_with_password() {
    let mut r = destruct_rig(10, None, true);
    r.app.poll_destruct(r.t0 + Duration::from_secs(5));
    assert!(r.calls.borrow().is_empty());
    r.app.poll_destruct(r.t0 + Duration::from_secs(10));
    r.app.poll_destruct(r.t0 + Duration::from_secs(11));
    let calls = r.calls.borrow().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, ["sudo", "-S", "-k", "-p", "", "systemctl", "poweroff"]);
    assert_eq!(calls[0].2, "pw");
    let audit = r.audit();
    assert_eq!(r.exits(), ["executed"]);
    assert_eq!(audit[0]["tool"], "destruct_shutdown");
    assert_eq!(audit[0]["args"]["seconds"], 10);
    assert_eq!(audit[0]["as_root"], true);
    assert_eq!(audit[0]["decision"], "confirmed");
    assert_eq!(audit[0]["argv"][6], "poweroff");
    let raw = std::fs::read_to_string(r.dir.path().join("audit.jsonl")).unwrap();
    assert!(!raw.contains("pw"), "{raw}");
    assert!(r.app.destruct_active(), "holds on 00000 while the host goes down");
}

#[test]
fn failed_execute_shows_reason_and_records() {
    let mut r = destruct_rig(10, None, true);
    r.app.poll_destruct(r.t0 + Duration::from_secs(10));
    let seq = r.calls.borrow()[0].0;
    r.app.on_sudo_done(
        seq,
        RunOutput::Exited {
            code: 1,
            stdout: String::new(),
            stderr: "sudo: 1 incorrect password attempt\n".into(),
        },
    );
    let s = r.strip();
    assert!(s.contains("UNABLE TO COMPLY -- SUDO: 1 INCORRECT PASSWORD ATTEMPT"), "{s}");
    assert!(r.armer.borrow().sudo_done.is_empty());
    r.app.active = TabId::Cargo;
    r.app.handle_keys_at(vec![Key::Char('x')], r.t0 + Duration::from_secs(12));
    assert!(!r.app.destruct_active() && r.app.redraw);
    assert_eq!(r.app.active, TabId::Utility);
    assert_eq!(r.exits(), ["executed", "failed"]);
    let audit = r.audit();
    assert_eq!(audit[1]["decision"], "confirmed");
    assert_eq!(audit[1]["output"], "SUDO: 1 INCORRECT PASSWORD ATTEMPT");
}

#[test]
fn esc_records_aborted() {
    let mut r = destruct_rig(60, None, true);
    r.app.redraw = false;
    r.app.handle_keys_at(vec![Key::Esc], r.t0 + S);
    assert!(!r.app.destruct_active() && r.app.redraw);
    assert_eq!(r.exits(), ["aborted"]);
    assert_eq!(r.audit()[0]["decision"], "aborted");
    let rows = all_rows(&draw(&mut r.app));
    assert!(rows.iter().all(|l| !l.contains("ABORT")), "{rows:#?}");
    assert!(rows[0].contains("USCSS TEST"));
    drop(std::mem::replace(&mut r.app, App::new(chrome())));
    assert_eq!(r.exits(), ["aborted"], "nothing more on drop");
}

#[test]
fn break_while_frozen_records_frozen() {
    let mut r = destruct_rig(10, None, true);
    let t = r.t0 + S;
    r.app.handle_keys_at(vec![Key::Char('x'), Key::Enter], t);
    r.app.poll_destruct(r.t0 + Duration::from_millis(9950));
    let brk_at = r.t0 + Duration::from_secs(11);
    r.app.handle_keys_at(vec![BREAK], brk_at);
    assert!(!r.app.destruct_active() && r.app.settling());
    assert_eq!(r.exits(), ["frozen"]);
    r.app.poll_settle(brk_at + BREAK_SETTLE);
    assert!(r.app.redraw);
    assert!(row(&draw(&mut r.app), 0).contains("USCSS TEST"));
    assert!(r.calls.borrow().is_empty());
}

#[test]
fn drop_mid_countdown_records_aborted() {
    let r = destruct_rig(60, None, true);
    let Rig { app, dir, .. } = r;
    drop(app);
    let raw = std::fs::read_to_string(dir.path().join("audit.jsonl")).unwrap();
    assert_eq!(raw.lines().count(), 1, "{raw}");
    assert!(raw.contains("\"exit\":\"aborted\""), "{raw}");
}

#[test]
fn other_sudo_results_go_to_utility() {
    let mut r = destruct_rig(60, None, false);
    r.app.on_sudo_done(
        7,
        RunOutput::Exited {
            code: 0,
            stdout: String::new(),
            stderr: String::new(),
        },
    );
    assert_eq!(r.armer.borrow().sudo_done, [7]);
}

#[test]
fn request_without_deps_is_dropped() {
    let armer = Rc::new(RefCell::new(ArmerState::default()));
    let mut app = App::new(chrome()).with_tab(TabId::Utility, Box::new(Armer(armer.clone(), Some(Duration::from_secs(60)))));
    app.handle_keys_at(vec![Key::Enter], Instant::now());
    assert!(!app.destruct_active());
    assert!(armer.borrow().request.is_none(), "taken and dropped");
}

#[test]
fn quit_after_destruct_quits_on_done() {
    let mut r = destruct_rig(60, None, true);
    r.app.quit_after_destruct = true;
    r.app.handle_keys_at(vec![Key::Esc], r.t0 + S);
    assert!(r.app.quit);
}

#[test]
fn preview_has_no_recorder() {
    let mut r = destruct_rig(60, None, false);
    r.app
        .start_destruct(crate::destruct::Destruct::preview(Kind::Reboot, Duration::from_secs(10), r.t0, 1));
    assert!(r.app.destruct_active());
    r.app.poll_destruct(r.t0 + Duration::from_secs(11));
    r.app.handle_keys_at(vec![Key::Esc], r.t0 + Duration::from_secs(12));
    assert!(!r.app.destruct_active());
    assert!(r.audit().is_empty() && r.calls.borrow().is_empty());
}

#[test]
fn quit_after_destruct_quits_on_break() {
    let mut r = destruct_rig(60, None, true);
    r.app.quit_after_destruct = true;
    r.app.handle_keys_at(vec![BREAK], r.t0 + S);
    assert!(r.app.quit && !r.app.destruct_active());
}

/// An app with the real Utility tab, its verify command recorded in the
/// returned calls; the clock drives the arming delay.
fn real_utility_app() -> (App, Calls, Rc<std::cell::Cell<Instant>>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let audit = crate::ops::audit::shared(crate::ops::audit::AuditLog::open(dir.path().join("audit.jsonl")).unwrap());
    let runs: Calls = Rc::default();
    let r = runs.clone();
    let clock = Rc::new(std::cell::Cell::new(Instant::now()));
    let c = clock.clone();
    let util = crate::utility::UtilityTab::with_destruct(crate::utility::UtilityDeps {
        run: Box::new(move |seq, argv, _to, pw: Secret| r.borrow_mut().push((seq, argv, pw.expose().to_string()))),
        now: Box::new(move || c.get()),
        audit: audit.clone(),
        destruct_seconds: 60,
    });
    let app = App::new(chrome()).with_tab(TabId::Utility, Box::new(util)).with_destruct(DestructDeps {
        run: Box::new(|_, _, _, _| {}),
        audit,
        timestamp: Box::new(|| "t".into()),
    });
    (app, runs, clock, dir)
}

/// Arm a shutdown on the real Utility tab up to VERIFYING; returns the verify seq.
fn submit_password(app: &mut App, runs: &Calls, clock: &std::cell::Cell<Instant>, t: Instant) -> u64 {
    app.handle_keys_at(vec![Key::Char('s'), Key::Enter], t);
    draw(app);
    draw(app);
    clock.set(clock.get() + ARMING + Duration::from_millis(100));
    app.handle_keys_at(vec![Key::Char('p'), Key::Char('w'), Key::Enter], t);
    let calls = runs.borrow();
    assert_eq!(calls.len(), 1, "verify ran");
    assert_eq!(calls[0].1.last().unwrap(), "true");
    calls[0].0
}

fn ok() -> RunOutput {
    RunOutput::Exited {
        code: 0,
        stdout: String::new(),
        stderr: String::new(),
    }
}

#[test]
fn verified_password_starts_countdown_at_once() {
    let (mut app, runs, clock, _dir) = real_utility_app();
    let seq = submit_password(&mut app, &runs, &clock, Instant::now());
    assert!(!app.destruct_active());
    app.on_sudo_done(seq, ok());
    assert!(app.destruct_active(), "no further key needed");
}

#[test]
fn verify_result_after_leaving_utility_never_starts() {
    let (mut app, runs, clock, _dir) = real_utility_app();
    let t = Instant::now();
    let seq = submit_password(&mut app, &runs, &clock, t);
    app.handle_keys_at(vec![Key::F(3)], t);
    app.on_sudo_done(seq, ok());
    assert!(!app.destruct_active());
    draw(&mut app);
    app.handle_keys_at(vec![Key::F(2)], t + Duration::from_secs(3600));
    draw(&mut app);
    app.handle_keys_at(vec![Key::Char('x')], t + Duration::from_secs(3601));
    assert!(!app.destruct_active(), "a stale request never starts");
}

#[test]
fn verify_result_under_prefix_never_starts() {
    let (mut app, runs, clock, _dir) = real_utility_app();
    let t = Instant::now();
    let seq = submit_password(&mut app, &runs, &clock, t);
    app.handle_keys_at(vec![Key::Ctrl(']')], t);
    app.on_sudo_done(seq, ok());
    assert!(!app.destruct_active());
    app.handle_keys_at(vec![Key::Char('r'), Key::Char('x')], t);
    assert!(!app.destruct_active());
}

#[test]
fn break_while_executing_still_records_failed() {
    let mut r = destruct_rig(10, None, true);
    r.app.poll_destruct(r.t0 + Duration::from_secs(10));
    let seq = r.calls.borrow()[0].0;
    r.app.handle_keys_at(vec![BREAK], r.t0 + Duration::from_secs(11));
    assert!(!r.app.destruct_active());
    assert_eq!(r.exits(), ["executed"]);
    r.app.on_sudo_done(
        seq,
        RunOutput::Exited {
            code: 1,
            stdout: String::new(),
            stderr: "Failed to power off\n".into(),
        },
    );
    assert_eq!(r.exits(), ["executed", "failed"]);
    assert_eq!(r.audit()[1]["output"], "FAILED TO POWER OFF");
    assert!(r.armer.borrow().sudo_done.is_empty(), "not routed to Utility");
    drop(std::mem::replace(&mut r.app, App::new(chrome())));
    assert_eq!(r.exits(), ["executed", "failed"], "nothing more on drop");
}

#[test]
fn break_while_executing_then_success_records_nothing_more() {
    let mut r = destruct_rig(10, None, true);
    r.app.poll_destruct(r.t0 + Duration::from_secs(10));
    let seq = r.calls.borrow()[0].0;
    r.app.handle_keys_at(vec![BREAK], r.t0 + Duration::from_secs(11));
    r.app.on_sudo_done(seq, ok());
    drop(std::mem::replace(&mut r.app, App::new(chrome())));
    assert_eq!(r.exits(), ["executed"]);
}

#[test]
fn key_ending_countdown_stops_the_batch() {
    let mut r = destruct_rig(60, None, true);
    r.app.handle_keys_at(vec![Key::Esc, Key::F(3)], r.t0 + S);
    assert!(!r.app.destruct_active());
    assert_eq!(r.app.active, TabId::Utility);
}
