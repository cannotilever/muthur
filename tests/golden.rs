//! Whole-screen snapshots: App → Wy60Backend bytes → vt100 emulator → text.
//! Regenerate with `UPDATE_GOLDEN=1 cargo test --test golden`, then READ the files.

use muthur::app::{App, ChromeView};
use muthur::backend::Wy60Backend;
use muthur::dialect::Vt100Dialect;
use muthur::input::Key;
use muthur::strict::StrictWriter;
use ratatui::Terminal;
use std::num::NonZeroU16;

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

#[test]
fn fullscreen_shell() {
    scene("fullscreen_shell", &[Key::F(3), Key::Ctrl(']'), Key::Char('f')]);
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
    let mut app = App::assemble(
        ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 },
        muthur::utility::UtilityTab::new(),
        shell,
    );
    let w = StrictWriter::new(Vec::new(), Vt100Dialect);
    let mut t = Terminal::new(Wy60Backend::new(w, Vt100Dialect, 80, 43).with_hints(hints)).unwrap();
    t.clear().unwrap();
    app.handle_key(Key::F(3));
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
    use muthur::app::{PlaceholderTab, TabId};
    use muthur::metrics::{Container, Disk, NetRate, Proc, Snapshot, Usage};
    use muthur::utility::UtilityTab;
    const G: u64 = 1 << 30;
    let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap().and_hms_opt(14, 2, 0).unwrap();
    let mut app = App::with_tabs(
        ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 },
        [
            Box::new(UtilityTab::new()),
            Box::new(PlaceholderTab { id: TabId::Muthur }),
            Box::new(PlaceholderTab { id: TabId::Shell }),
        ],
    );
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
            containers: Some(vec![Container { name: "WEB".into(), status: "UP".into() }]),
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
