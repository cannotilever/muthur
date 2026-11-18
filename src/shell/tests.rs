use super::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const WINDOWED: Rect = Rect { x: 19, y: 2, width: 61, height: 40 };
const FULL: Rect = Rect { x: 0, y: 0, width: 80, height: 43 };

fn tab(script: &str) -> (ShellTab, HintSlot, mpsc::Receiver<PtyEvent>) {
    let (tx, rx) = mpsc::channel();
    let hints = HintSlot::default();
    let cfg = ShellConfig { command: "sh".into(), args: vec!["-c".into(), script.into()], scrollback: 1000 };
    let t = ShellTab::new(cfg, hints.clone(), move |e| {
        let _ = tx.send(e);
    });
    (t, hints, rx)
}

fn render(t: &mut ShellTab, area: Rect) -> Buffer {
    let mut buf = Buffer::empty(FULL);
    t.render(area, &mut buf);
    buf
}

fn text(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n")
        .collect()
}

/// Feed PTY events into the tab until `done` holds for the rendered screen (2 s max).
fn pump(t: &mut ShellTab, rx: &mpsc::Receiver<PtyEvent>, area: Rect, done: impl Fn(&str) -> bool) -> Buffer {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let buf = render(t, area);
        if done(&text(&buf)) || Instant::now() > deadline {
            return buf;
        }
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(20)) {
            match ev {
                PtyEvent::Output(b) => t.on_output(&b),
                PtyEvent::Exited => t.on_exit(),
            }
        }
    }
}

#[test]
fn echo_reaches_screen() {
    let (mut t, _h, rx) = tab("printf HELLO; sleep 5");
    let buf = pump(&mut t, &rx, WINDOWED, |s| s.contains("HELLO"));
    let row: String = (20..25).map(|x| buf[(x, 3)].symbol().to_string()).collect();
    assert_eq!(row, "HELLO");
    assert!(text(&buf).contains(" SHELL "));
}

#[test]
fn acs_reaches_screen_as_graphics() {
    let (mut t, _h, rx) = tab(r"printf '\033(0lqk\033(B'; sleep 5");
    let buf = pump(&mut t, &rx, FULL, |s| s.contains("┌─┐"));
    assert!(text(&buf).starts_with("┌─┐"));
}

#[test]
fn colours_become_attributes() {
    let (mut t, _h, rx) = tab(r"printf '\033[41mR\033[0m\033[91mB'; sleep 5");
    let buf = pump(&mut t, &rx, FULL, |s| s.starts_with("RB"));
    assert!(buf[(0, 0)].modifier.contains(Modifier::REVERSED));
    assert!(buf[(1, 0)].modifier.contains(Modifier::BOLD));
    assert!(!buf[(1, 0)].modifier.contains(Modifier::REVERSED));
}

#[test]
fn shell_scroll_emits_hint() {
    let (mut t, hints, rx) = tab("for i in $(seq 1 60); do echo L$i; done; read x; echo NEW; sleep 5");
    pump(&mut t, &rx, WINDOWED, |s| s.contains("L60"));
    hints.take();
    t.handle_key(&Key::Enter);
    pump(&mut t, &rx, WINDOWED, |s| s.contains("NEW"));
    let h = hints.take().expect("no scroll hint");
    assert!(h.lines > 0 && h.top == 3 && h.bottom == 41, "{h:?}");
}

#[test]
fn fullscreen_resizes_pty() {
    let (mut t, _h, rx) = tab("sleep 0.3; stty size; sleep 5");
    render(&mut t, WINDOWED);
    assert_eq!(t.size(), (38, 59));
    let buf = pump(&mut t, &rx, FULL, |s| s.contains("43 80"));
    assert_eq!(t.size(), (43, 80));
    assert!(text(&buf).contains("43 80"));
}

#[test]
fn scrollback_pages_and_exits() {
    let (mut t, _h, rx) = tab("seq 1 100; sleep 5");
    pump(&mut t, &rx, WINDOWED, |s| s.contains("100"));
    t.enter_scrollback();
    t.handle_key(&Key::PageUp);
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("SCROLLBACK") && !s.contains("100"), "{s}");
    assert_eq!(t.cursor(), None);
    t.handle_key(&Key::Char('x'));
    let s = text(&render(&mut t, WINDOWED));
    assert!(!s.contains("SCROLLBACK") && s.contains("100"), "{s}");
}

#[test]
fn exited_shows_restart_prompt() {
    let (mut t, _h, rx) = tab("exit 0");
    let buf = pump(&mut t, &rx, WINDOWED, |s| s.contains("SHELL TERMINATED"));
    assert!(text(&buf).contains("PRESS RETURN TO RESTART"));
    assert_eq!(t.cursor(), None);
}

#[test]
fn cursor_tracks_shell() {
    let (mut t, _h, rx) = tab("printf ab; sleep 5");
    pump(&mut t, &rx, WINDOWED, |s| s.contains("ab"));
    assert_eq!(t.cursor(), Some(Position { x: 22, y: 3 }));
}

#[test]
fn activity_flag_set_by_output() {
    let (mut t, _h, rx) = tab("printf z; sleep 5");
    pump(&mut t, &rx, WINDOWED, |s| s.contains('z'));
    assert!(t.take_activity());
    assert!(!t.take_activity());
}

#[test]
fn scrollback_indicator_in_fullscreen() {
    let (mut t, _h, rx) = tab("seq 1 100; sleep 5");
    pump(&mut t, &rx, FULL, |s| s.contains("100"));
    t.enter_scrollback();
    t.handle_key(&Key::PageUp);
    let buf = render(&mut t, FULL);
    let top = text(&buf).lines().next().unwrap().to_string();
    assert!(top.trim_end().ends_with("SCROLLBACK -43"), "{top:?}");
    assert!(buf[(79, 0)].modifier.contains(Modifier::REVERSED));
}
