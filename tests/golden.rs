//! Whole-screen snapshots: App → Wy60Backend bytes → vt100 emulator → text.
//! Regenerate with `UPDATE_GOLDEN=1 cargo test --test golden`, then READ the files.

use muthur::app::{App, ChromeView};
use muthur::backend::Wy60Backend;
use muthur::dialect::Vt100Dialect;
use muthur::input::Key;
use muthur::strict::StrictWriter;
use ratatui::Terminal;
use std::num::NonZeroU16;

type Clock = std::rc::Rc<std::cell::Cell<std::time::Instant>>;

fn muthur_tab(path: &std::path::Path, prefill: Option<(&str, &str)>) -> (muthur::muthur::MuthurTab, Clock) {
    use muthur::chatlog::{Assistant, ChatLog};
    use muthur::config::{MuthurSection, Provider, ServiceLists, ToolsSection};
    use muthur::muthur::{MuthurDeps, MuthurTab};
    use muthur::tools::{ToolDefs, audit::AuditLog};
    let mut log = ChatLog::open(path.to_path_buf()).unwrap();
    if let Some((q, a)) = prefill {
        log.append_user(q, "ts").unwrap();
        let a = Assistant { provider: Provider::Anthropic, model: "m".into(), text: a.into(), stop: "end".into(), raw: None };
        log.append_assistant(a, "ts").unwrap();
    }
    let clock: Clock = std::rc::Rc::new(std::cell::Cell::new(std::time::Instant::now()));
    let c = clock.clone();
    let deps = MuthurDeps {
        spawn: Box::new(|_, _| Box::new(|| {})),
        now: Box::new(move || c.get()),
        ship_time: Box::new(|| "17 NOV 2137 14:02".into()),
        timestamp: Box::new(|| "2026-11-17T14:02:00Z".into()),
        run_tool: Box::new(|_, _, _| {}),
    };
    let cfg = MuthurSection { api_key: Some("sk-test".into()), ..Default::default() };
    let tools = ToolDefs::new(&ToolsSection {
        services: ServiceLists { restart: vec!["nginx".into()], ..Default::default() },
        ..Default::default()
    });
    let audit = AuditLog::open(path.with_file_name("audit.jsonl")).unwrap();
    (MuthurTab::new(cfg, tools, "USCSS TEST", log, muthur::tools::audit::shared(audit), deps), clock)
}

fn scene(name: &str, keys: &[Key]) {
    let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap().and_hms_opt(14, 2, 0).unwrap();
    let mut app = App::new(ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 });
    let w = StrictWriter::new(Vec::new(), Vt100Dialect);
    let mut t = Terminal::new(Wy60Backend::new(w, Vt100Dialect, 80, 43)).unwrap();
    t.clear().unwrap();
    for k in keys {
        app.handle_key(k.clone());
    }
    t.draw(|f| app.render(f, now)).unwrap();
    let strict = t.backend().writer();
    assert!(strict.violations.is_empty(), "{name}: {:?}", strict.violations);

    let mut p = vt100::Parser::new(NonZeroU16::new(43).unwrap(), NonZeroU16::new(80).unwrap(), 0);
    p.process(&strict.inner);
    let text = p.screen().contents();
    let path = format!("{}/tests/golden/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &text).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing {path}; run with UPDATE_GOLDEN=1"));
    assert_eq!(text, want, "{name} differs from golden");
}

#[test]
fn windowed_utility() {
    scene("windowed_utility", &[]);
}

/// Draw `app` through `StrictWriter` and compare with `tests/golden/<name>.txt`.
fn check_app(name: &str, app: &mut App) {
    let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap().and_hms_opt(14, 2, 0).unwrap();
    let w = StrictWriter::new(Vec::new(), Vt100Dialect);
    let mut t = Terminal::new(Wy60Backend::new(w, Vt100Dialect, 80, 43)).unwrap();
    t.clear().unwrap();
    t.draw(|f| app.render(f, now)).unwrap();
    let strict = t.backend().writer();
    assert!(strict.violations.is_empty(), "{name}: {:?}", strict.violations);
    let mut p = vt100::Parser::new(NonZeroU16::new(43).unwrap(), NonZeroU16::new(80).unwrap(), 0);
    p.process(&strict.inner);
    let text = p.screen().contents();
    let path = format!("{}/tests/golden/{name}.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &text).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing {path}; run with UPDATE_GOLDEN=1"));
    assert_eq!(text, want, "{name} differs from golden");
}

/// CARGO shown windowed with 50 containers (unit 03 EXITED, so it sorts
/// first and starts selected; unit 07 has a long name). Returns the `seq` of
/// every command the tab started, and the directory holding its audit log.
fn cargo_app() -> (App, std::rc::Rc<std::cell::RefCell<Vec<u64>>>, tempfile::TempDir) {
    use muthur::app::TabId;
    use muthur::cargo::{CargoDeps, CargoTab};
    use muthur::metrics::{Container, Snapshot};
    use muthur::tools::audit::{AuditLog, shared};
    let dir = tempfile::tempdir().unwrap();
    let seqs: std::rc::Rc<std::cell::RefCell<Vec<u64>>> = Default::default();
    let rec = seqs.clone();
    let deps = CargoDeps {
        run: Box::new(move |seq, _, _, _| rec.borrow_mut().push(seq)),
        now: Box::new(std::time::Instant::now),
        timestamp: Box::new(String::new),
        hints: Default::default(),
    };
    let mut app = App::new(ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 })
        .with_tab(TabId::Cargo, Box::new(CargoTab::new(deps, shared(AuditLog::open(dir.path().join("audit.jsonl")).unwrap()))));
    let mut containers: Vec<Container> =
        (0..50).map(|i| Container { id: format!("{i:012x}"), name: format!("refinery-unit-{i:02}"), status: "UP".into(), detail: "Up 3 hours".into() }).collect();
    containers[3].status = "EXITED".into();
    containers[3].detail = "Exited (137) 2 hours ago".into();
    containers[7].name = "AN-EXCEPTIONALLY-LONG-CONTAINER-NAME-THAT-WILL-NOT-FIT-IN-THE-PANE".into();
    app.on_metrics(Snapshot { containers: Some(containers), container_runtime: Some("docker".into()), ..Default::default() });
    app.handle_key(Key::F(3));
    (app, seqs, dir)
}

#[test]
fn windowed_cargo() {
    check_app("windowed_cargo", &mut cargo_app().0);
}

#[test]
fn cargo_stop_prompt() {
    let (mut app, _, _dir) = cargo_app();
    for k in [Key::Down, Key::Down, Key::Char('D')] {
        app.handle_key(k);
    }
    check_app("cargo_stop_prompt", &mut app);
}

#[test]
fn cargo_logs() {
    use muthur::tools::runner::RunOutput;
    let (mut app, seqs, _dir) = cargo_app();
    app.handle_key(Key::Char('L'));
    let mut log: String = (0..38).map(|i| format!("2137-11-17T14:{i:02}:00Z refinery: batch {i} nominal\n")).collect();
    log.push_str(&format!("2137-11-17T14:38:00Z refinery: {}\n", "very long line ".repeat(10)));
    log.push_str("2137-11-17T14:39:00Z health check \x1b[32mOK\x1b[0m\n");
    let seq = *seqs.borrow().last().unwrap();
    app.on_cargo_done(seq, RunOutput::Exited { code: 0, stdout: log, stderr: String::new() }, 5);
    check_app("cargo_logs", &mut app);
}

#[test]
fn fullscreen_shell() {
    scene("fullscreen_shell", &[Key::F(4), Key::Ctrl(']'), Key::Char('f')]);
}

#[test]
fn prefix_strip() {
    scene("prefix_strip", &[Key::Ctrl(']')]);
}

#[test]
fn shell_windowed_echo() {
    use muthur::scroll::HintSlot;
    use muthur::shell::{ShellConfig, ShellTab};
    use std::time::{Duration, Instant};

    let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap().and_hms_opt(14, 2, 0).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let hints = HintSlot::default();
    let cfg = ShellConfig {
        command: "sh".into(),
        args: vec!["-c".into(), "printf 'MUTHUR SHELL OK\\n'; sleep 5".into()],
        scrollback: 100,
    };
    let shell = ShellTab::new(cfg, hints.clone(), move |e| {
        let _ = tx.send(e);
    });
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::assemble(
        ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 },
        muthur_tab(&dir.path().join("c.jsonl"), None).0,
        muthur::utility::UtilityTab::new(),
        muthur::cargo::CargoTab::detached(),
        shell,
    );
    let w = StrictWriter::new(Vec::new(), Vt100Dialect);
    let mut t = Terminal::new(Wy60Backend::new(w, Vt100Dialect, 80, 43).with_hints(hints)).unwrap();
    t.clear().unwrap();
    app.handle_key(Key::F(4));
    let mut p = vt100::Parser::new(NonZeroU16::new(43).unwrap(), NonZeroU16::new(80).unwrap(), 0);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut sent = 0;
    while Instant::now() < deadline && !p.screen().contents().contains("MUTHUR SHELL OK") {
        t.draw(|f| app.render(f, now)).unwrap();
        let bytes = &t.backend().writer().inner;
        p.process(&bytes[sent..]);
        sent = bytes.len();
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(20)) {
            app.on_pty(ev);
        }
    }
    let strict = t.backend().writer();
    assert!(strict.violations.is_empty(), "{:?}", strict.violations);
    let text = p.screen().contents();
    let path = format!("{}/tests/golden/shell_windowed_echo.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &text).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing {path}; run with UPDATE_GOLDEN=1"));
    assert_eq!(text, want);
}

#[test]
fn utility_fullscreen() {
    use muthur::app::TabId;
    use muthur::metrics::{Container, Disk, NetRate, Proc, Snapshot, Usage};
    use muthur::utility::UtilityTab;
    const G: u64 = 1 << 30;
    let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap().and_hms_opt(14, 2, 0).unwrap();
    let mut app = App::new(ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 })
        .with_tab(TabId::Utility, Box::new(UtilityTab::new()));
    for i in 0..20u64 {
        app.on_metrics(Snapshot {
            host: "NOSTROMO".into(),
            uptime_secs: 86400 * 3 + 7500,
            load: [0.42, 0.38, 0.30],
            cpu_total: (i * 13 % 100) as f32,
            cpu_cores: (0..16).map(|c| (c * 6) as f32).collect(),
            mem: Usage { used: 12 * G, total: 32 * G },
            swap: Usage { used: G, total: 8 * G },
            disks: vec![
                Disk { mount: "/".into(), usage: Usage { used: 700 * G, total: 1000 * G } },
                Disk { mount: "/boot".into(), usage: Usage { used: G / 2, total: G } },
                Disk { mount: "/tank".into(), usage: Usage { used: 1100 * G, total: 2000 * G } },
            ],
            net: NetRate { rx_bps: i * 400, tx_bps: 6144 },
            top: vec![
                Proc { pid: 1382, name: "RESTIC".into(), cpu: 121.5, mem: 500 << 20 },
                Proc { pid: 77, name: "FISH".into(), cpu: 1.0, mem: 8 << 20 },
            ],
            failed_units: Some(vec!["AUTORANDR".into()]),
            containers: Some(vec![Container { id: "1".into(), name: "web".into(), status: "UP".into(), ..Default::default() }]),
            container_runtime: Some("docker".into()),
            containers_at: None,
        });
    }
    app.handle_key(Key::Ctrl(']'));
    app.handle_key(Key::Char('f'));
    let w = StrictWriter::new(Vec::new(), Vt100Dialect);
    let mut t = Terminal::new(Wy60Backend::new(w, Vt100Dialect, 80, 43)).unwrap();
    t.clear().unwrap();
    t.draw(|f| app.render(f, now)).unwrap();
    let strict = t.backend().writer();
    assert!(strict.violations.is_empty(), "{:?}", strict.violations);
    let mut p = vt100::Parser::new(NonZeroU16::new(43).unwrap(), NonZeroU16::new(80).unwrap(), 0);
    p.process(&strict.inner);
    let text = p.screen().contents();
    let path = format!("{}/tests/golden/utility_fullscreen.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &text).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing {path}; run with UPDATE_GOLDEN=1"));
    assert_eq!(text, want);
}

#[test]
fn muthur_windowed() {
    use muthur::llm::StreamEvent;
    let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap().and_hms_opt(14, 2, 0).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (tab, clock) = muthur_tab(&dir.path().join("chat.jsonl"), Some(("HI", "GREETINGS.")));
    let mut app = App::new(ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 })
        .with_tab(muthur::app::TabId::Muthur, Box::new(tab));
    app.handle_key(Key::F(1));
    for c in "REACTOR STATUS".chars() {
        app.handle_key(Key::Char(c));
    }
    app.handle_key(Key::Enter);
    app.on_llm(1, StreamEvent::Text("NO REACTOR TELEMETRY AVAILABLE \u{2014} SENSOR ACCESS NOT INSTALLED.".into()));
    let mut t = Terminal::new(Wy60Backend::new(StrictWriter::new(Vec::new(), Vt100Dialect), Vt100Dialect, 80, 43)).unwrap();
    t.clear().unwrap();
    t.draw(|f| app.render(f, now)).unwrap(); // starts the reveal clock
    clock.set(clock.get() + std::time::Duration::from_millis(400));
    t.draw(|f| app.render(f, now)).unwrap();
    let strict = t.backend().writer();
    assert!(strict.violations.is_empty(), "{:?}", strict.violations);
    let mut p = vt100::Parser::new(NonZeroU16::new(43).unwrap(), NonZeroU16::new(80).unwrap(), 0);
    p.process(&strict.inner);
    let text = p.screen().contents();
    // The reveal cursor is a reverse-video space right after the 20th character.
    assert!(p.screen().cell(11, 41).unwrap().inverse(), "no block cursor after partial reply");
    let path = format!("{}/tests/golden/muthur_windowed.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &text).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing {path}; run with UPDATE_GOLDEN=1"));
    assert_eq!(text, want);
}

#[test]
fn muthur_override() {
    use muthur::llm::{Stop, StreamEvent};
    let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap().and_hms_opt(14, 2, 0).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (tab, _clock) = muthur_tab(&dir.path().join("chat.jsonl"), None);
    let mut app = App::new(ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 })
        .with_tab(muthur::app::TabId::Muthur, Box::new(tab));
    app.handle_key(Key::F(1));
    for c in "RESTART NGINX".chars() {
        app.handle_key(Key::Char(c));
    }
    app.handle_key(Key::Enter);
    let input = serde_json::json!({"unit": "nginx"});
    app.on_llm(1, StreamEvent::ToolCall { id: "c1".into(), name: "restart_service".into(), input: input.clone() });
    let raw = vec![serde_json::json!({"type":"tool_use","id":"c1","name":"restart_service","input":input})];
    app.on_llm(1, StreamEvent::Done { stop: Stop::ToolUse, model: Some("m".into()), raw: Some(raw) });
    let mut t = Terminal::new(Wy60Backend::new(StrictWriter::new(Vec::new(), Vt100Dialect), Vt100Dialect, 80, 43)).unwrap();
    t.clear().unwrap();
    t.draw(|f| app.render(f, now)).unwrap();
    let strict = t.backend().writer();
    assert!(strict.violations.is_empty(), "{:?}", strict.violations);
    let mut p = vt100::Parser::new(NonZeroU16::new(43).unwrap(), NonZeroU16::new(80).unwrap(), 0);
    p.process(&strict.inner);
    let text = p.screen().contents();
    let row = text.lines().position(|l| l.contains("OVERRIDE REQUIRED")).expect("no override row") as u16;
    let line = text.lines().nth(row as usize).unwrap();
    let col = line.chars().position(|c| c == 'O').unwrap() as u16;
    assert!(p.screen().cell(row, col).unwrap().inverse(), "override row is not reverse video");
    assert!(text.contains("> RESTART NGINX"), "{text}");
    assert!(text.contains("OVERRIDE REQUIRED -- RESTART NGINX.SERVICE -- CONFIRM Y/N"), "{text}");
    let path = format!("{}/tests/golden/muthur_override.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &text).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing {path}; run with UPDATE_GOLDEN=1"));
    assert_eq!(text, want);
}

fn splash_for_golden() -> muthur::splash::Splash {
    use muthur::metrics::{Snapshot, Usage};
    use muthur::splash::{Interface, Splash, SplashInfo};
    const G: u64 = 1 << 30;
    let info = SplashInfo {
        ship_name: "USCSS TEST".into(),
        year: 2137,
        link: "38400 BAUD  XON/XOFF".into(),
        interface: Interface::Online,
    };
    let mut s = Splash::new(info, std::time::Instant::now() - std::time::Duration::from_secs(60));
    s.on_metrics(&Snapshot {
        host: "NOSTROMO".into(),
        uptime_secs: 86400 * 3 + 4 * 3600 + 12 * 60,
        cpu_cores: vec![0.0; 16],
        mem: Usage { used: 12 * G, total: 31 * G + G / 5 },
        failed_units: Some(vec![]),
        ..Default::default()
    });
    s
}

#[test]
fn splash() {
    let mut s = splash_for_golden();
    s.advance(std::time::Instant::now());
    let w = StrictWriter::new(Vec::new(), Vt100Dialect);
    let mut t = Terminal::new(Wy60Backend::new(w, Vt100Dialect, 80, 43)).unwrap();
    t.clear().unwrap();
    t.draw(|f| s.render(f.area(), f.buffer_mut())).unwrap();
    let strict = t.backend().writer();
    assert!(strict.violations.is_empty(), "{:?}", strict.violations);
    let mut p = vt100::Parser::new(NonZeroU16::new(43).unwrap(), NonZeroU16::new(80).unwrap(), 0);
    p.process(&strict.inner);
    let text = p.screen().contents();
    let path = format!("{}/tests/golden/splash.txt", env!("CARGO_MANIFEST_DIR"));
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &text).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing {path}; run with UPDATE_GOLDEN=1"));
    assert_eq!(text, want, "splash differs from golden");
}

#[test]
fn splash_logo_frame_fits_the_wire_budget() {
    use muthur::splash::{Interface, Splash, SplashInfo};
    let info = SplashInfo { ship_name: "USCSS TEST".into(), year: 2137, link: "38400 BAUD  XON/XOFF".into(), interface: Interface::Online };
    let s = Splash::new(info, std::time::Instant::now());
    let mut t = Terminal::new(Wy60Backend::new(Vec::new(), Vt100Dialect, 80, 43)).unwrap();
    t.clear().unwrap();
    let before = t.backend().writer().len();
    t.draw(|f| s.render(f.area(), f.buffer_mut())).unwrap();
    let n = t.backend().writer().len() - before;
    println!("logo frame bytes: {n}");
    assert!(n <= 3072, "logo frame is {n} bytes");
}
