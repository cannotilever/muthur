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
