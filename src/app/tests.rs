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
    ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 }
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
    keys(&mut app, &[Key::F(3)]);
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
    keys(&mut app, &[Key::F(1), Key::Ctrl(']'), Key::Char('f')]);
    assert!(!row(&draw(&mut app), 0).contains("MU/TH/UR 6000"));
    keys(&mut app, &[Key::F(3)]);
    assert!(row(&draw(&mut app), 0).contains("MU/TH/UR 6000"));
    keys(&mut app, &[Key::F(1)]);
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
    assert!(all.contains("QUIT"), "{all}");
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
    let mut app = App::with_tabs(
        chrome(),
        [
            Box::new(Recorder(log.clone())),
            Box::new(PlaceholderTab { id: TabId::Muthur }),
            Box::new(PlaceholderTab { id: TabId::Shell }),
        ],
    );
    keys(&mut app, &[Key::Char('a'), Key::F(2), Key::Char('b'), Key::F(1), Key::Ctrl(']'), Key::Char(']')]);
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
