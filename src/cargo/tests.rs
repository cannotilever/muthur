use super::*;
use crate::app::ARMING;
use crate::metrics::local::ps_argv;
use crate::tools::runner::RunOutput;
use ratatui::style::Modifier;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

const WINDOWED: Rect = Rect { x: 19, y: 2, width: 61, height: 40 };
const FULL: Rect = Rect { x: 0, y: 0, width: 80, height: 43 };

fn snap(containers: Option<Vec<Container>>) -> Snapshot {
    let container_runtime = containers.as_ref().map(|_| "docker".to_string());
    Snapshot { containers, container_runtime, ..Default::default() }
}

fn ship(n: usize) -> Vec<Container> {
    (0..n).map(|i| Container { id: format!("id{i}"), name: format!("crate{i:03}"), status: "UP".into(), ..Default::default() }).collect()
}

fn render(t: &mut CargoTab, area: Rect) -> Buffer {
    let mut buf = Buffer::empty(FULL);
    t.render(area, &mut buf);
    buf
}

fn text(buf: &Buffer) -> String {
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n")
        .collect()
}

fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    text(buf).lines().enumerate().find_map(|(y, l)| l.find(needle).map(|b| (l[..b].chars().count() as u16, y as u16)))
}

#[test]
fn awaiting_then_offline() {
    let mut t = CargoTab::detached();
    assert!(text(&render(&mut t, WINDOWED)).contains("AWAITING TELEMETRY"));
    t.on_metrics_offline("PERMISSION DENIED");
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("TELEMETRY OFFLINE") && s.contains("PERMISSION DENIED"), "{s}");
    t.on_metrics(&snap(Some(ship(1))));
    assert!(!text(&render(&mut t, WINDOWED)).contains("OFFLINE"));
}

#[test]
fn no_runtime_and_empty_hold() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(None));
    assert!(text(&render(&mut t, WINDOWED)).contains("NO CONTAINER RUNTIME"));
    t.on_metrics(&snap(Some(vec![])));
    assert!(text(&render(&mut t, WINDOWED)).contains("HOLD EMPTY"));
}

#[test]
fn lists_containers_with_status_column() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(vec![
        Container { id: "1".into(), name: "web".into(), status: "UP".into(), ..Default::default() },
        Container { id: "2".into(), name: "db".into(), status: "EXITED".into(), ..Default::default() },
    ])));
    let buf = render(&mut t, WINDOWED);
    let s = text(&buf);
    assert!(s.contains(" CARGO - 2 CONTAINERS "), "{s}");
    let (hx, _) = find(&buf, "STATUS").unwrap();
    let (ux, uy) = find(&buf, "UP").unwrap();
    let (ex, ey) = find(&buf, "EXITED").unwrap();
    assert_eq!((ux, ex), (hx, hx), "status column lines up\n{s}");
    assert!(!buf[(ux, uy)].modifier.contains(Modifier::REVERSED));
    assert!(buf[(ex, ey)].modifier.contains(Modifier::REVERSED));
    // Nothing escapes the content pane.
    for y in 0..43 {
        for x in 0..80 {
            if !WINDOWED.contains((x, y).into()) {
                assert_eq!(buf[(x, y)].symbol(), " ", "({x},{y})");
            }
        }
    }
}

#[test]
fn singular_title() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(ship(1))));
    assert!(text(&render(&mut t, WINDOWED)).contains(" CARGO - 1 CONTAINER "));
}

#[test]
fn long_names_are_cut() {
    let mut t = CargoTab::detached();
    let name = "X".repeat(200);
    t.on_metrics(&snap(Some(vec![Container { id: "1".into(), name: name.clone(), status: "UP".into(), ..Default::default() }])));
    let buf = render(&mut t, WINDOWED);
    let (_, y) = find(&buf, "XXXX").unwrap();
    let row = text(&buf).lines().nth(y as usize).unwrap().to_string();
    // 59 inner columns: the '>' gutter, a 6-wide STATUS, one space, the name gets the rest.
    assert_eq!(row.matches('X').count(), 59 - 1 - 6 - 1, "{row}");
    assert!(row.contains("X UP "), "{row}");
}

#[test]
fn long_manifest_pages() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(ship(100))));
    // 40 rows - 2 borders - heading - detail line - hint line = 35 per page.
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("CARGO - 100 CONTAINERS (1-35)") && s.contains(">CRATE000") && s.contains("CRATE034") && !s.contains("CRATE035"), "{s}");
    t.handle_key(&Key::PageDown);
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("(36-70)") && s.contains(">CRATE035") && !s.contains("CRATE034"), "{s}");
    t.handle_key(&Key::PageDown);
    t.handle_key(&Key::PageDown); // clamps at the end
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("(66-100)") && s.contains(">CRATE099"), "{s}");
    t.handle_key(&Key::Home);
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("(1-35)") && s.contains(">CRATE000"), "{s}");
    t.handle_key(&Key::PageUp); // clamps at the top
    assert!(text(&render(&mut t, WINDOWED)).contains("(1-35)"));
}

#[test]
fn shrinking_manifest_clamps_scroll() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(ship(100))));
    render(&mut t, WINDOWED);
    t.handle_key(&Key::PageDown);
    t.handle_key(&Key::PageDown);
    t.on_metrics(&snap(Some(ship(5))));
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("CRATE000") && s.contains("CRATE004") && !s.contains("("), "{s}");
}

#[test]
fn fullscreen_fits_more() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(ship(100))));
    assert!(text(&render(&mut t, FULL)).contains("(1-38)"));
}

#[test]
fn trouble_sorts_to_the_top_in_runtime_order() {
    let mut t = CargoTab::detached();
    let k = |name: &str, status: &str| Container { id: name.into(), name: name.into(), status: status.into(), ..Default::default() };
    t.on_metrics(&snap(Some(vec![k("alpha", "UP"), k("bravo", "EXITED"), k("charlie", "UP"), k("delta", "RESTARTING"), k("echo", "UP")])));
    let buf = render(&mut t, WINDOWED);
    let rows: Vec<u16> = ["BRAVO ", "DELTA ", "ALPHA ", "CHARLIE ", "ECHO "].iter().map(|n| find(&buf, n).unwrap().1).collect();
    assert!(rows.windows(2).all(|w| w[0] < w[1]), "{rows:?}\n{}", text(&buf));
}

#[test]
fn names_display_upper_case() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(vec![Container { id: "1".into(), name: "web".into(), status: "UP".into(), ..Default::default() }])));
    assert!(text(&render(&mut t, WINDOWED)).contains("WEB"));
}

#[test]
fn arrows_move_selection_marker() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(ship(3))));
    let buf = render(&mut t, WINDOWED);
    let (x, y) = find(&buf, ">CRATE000").unwrap();
    assert!(buf[(x + 1, y)].modifier.contains(Modifier::UNDERLINED));
    assert!(!buf[(x + 1, y + 1)].modifier.contains(Modifier::UNDERLINED));
    t.handle_key(&Key::Down);
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains(">CRATE001") && !s.contains(">CRATE000"), "{s}");
    t.handle_key(&Key::Up);
    t.handle_key(&Key::Up); // clamps at the top
    assert!(text(&render(&mut t, WINDOWED)).contains(">CRATE000"));
}

#[test]
fn selection_follows_id_across_resort() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(ship(5))));
    t.handle_key(&Key::Down);
    t.handle_key(&Key::Down); // CRATE002
    let mut l = ship(5);
    l[4].status = "EXITED".into(); // sorts first, shifts rows
    t.on_metrics(&snap(Some(l)));
    assert!(text(&render(&mut t, WINDOWED)).contains(">CRATE002"));
}

#[test]
fn selection_clamps_when_selected_vanishes() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(ship(5))));
    for _ in 0..4 {
        t.handle_key(&Key::Down);
    }
    t.on_metrics(&snap(Some(ship(2))));
    assert!(text(&render(&mut t, WINDOWED)).contains(">CRATE001"));
    t.on_metrics(&snap(Some(vec![])));
    assert!(text(&render(&mut t, WINDOWED)).contains("HOLD EMPTY"));
    t.handle_key(&Key::Down); // no panic
    t.handle_key(&Key::PageDown);
    t.on_metrics(&snap(Some(ship(3))));
    assert!(text(&render(&mut t, WINDOWED)).contains(">CRATE000"));
}

#[test]
fn detail_and_hint_lines() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(vec![Container { id: "1".into(), name: "db".into(), status: "EXITED".into(), detail: "Exited (137) 2 hours ago".into() }])));
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("DB: EXITED (137) 2 HOURS AGO") && s.contains("U START  R RESTART  D STOP  L LOGS"), "{s}");
    t.on_metrics(&Snapshot { container_runtime: None, ..snap(Some(ship(1))) });
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("NO CONTAINER RUNTIME") && !s.contains("U START"), "{s}");
}

#[test]
fn paging_keeps_selection_visible() {
    let mut t = CargoTab::detached();
    t.on_metrics(&snap(Some(ship(100))));
    render(&mut t, WINDOWED);
    t.handle_key(&Key::PageDown);
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("(36-70)") && s.contains(">CRATE035"), "{s}");
    for _ in 0..35 {
        t.handle_key(&Key::Down);
    }
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("(37-71)") && s.contains(">CRATE070"), "{s}");
}

// --- Actions -------------------------------------------------------------

type Calls = Rc<RefCell<Vec<(u64, Vec<String>, Duration, bool)>>>;

struct H {
    t: CargoTab,
    calls: Calls,
    clock: Rc<Cell<Instant>>,
    audit_path: PathBuf,
    _dir: tempfile::TempDir,
}

fn c(id: &str, name: &str, status: &str) -> Container {
    Container { id: id.into(), name: name.into(), status: status.into(), detail: String::new() }
}

/// A tab with a recording `run`, a hand-driven clock and a real audit file;
/// runtime "docker", `list` delivered and rendered once.
fn harness(list: Vec<Container>) -> H {
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let audit = shared(AuditLog::open(audit_path.clone()).unwrap());
    let calls: Calls = Rc::default();
    let clock = Rc::new(Cell::new(Instant::now()));
    let (rec, clk) = (calls.clone(), clock.clone());
    let deps = CargoDeps {
        run: Box::new(move |seq, argv, timeout, merged| rec.borrow_mut().push((seq, argv, timeout, merged))),
        now: Box::new(move || clk.get()),
        timestamp: Box::new(|| "ts".into()),
    };
    let mut t = CargoTab::new(deps, audit);
    t.on_metrics(&snap(Some(list)));
    render(&mut t, WINDOWED);
    H { t, calls, clock, audit_path, _dir: dir }
}

impl H {
    fn key(&mut self, k: Key) {
        self.t.handle_key(&k);
    }
    fn screen(&mut self) -> String {
        text(&render(&mut self.t, WINDOWED))
    }
    /// Two unobscured renders, then the arming delay.
    fn arm(&mut self) {
        render(&mut self.t, WINDOWED);
        render(&mut self.t, WINDOWED);
        self.clock.set(self.clock.get() + ARMING);
    }
    fn last(&self) -> (u64, Vec<String>, Duration, bool) {
        self.calls.borrow().last().cloned().expect("no command run")
    }
    fn argv(&self) -> Vec<String> {
        self.last().1
    }
    fn n_calls(&self) -> usize {
        self.calls.borrow().len()
    }
    /// The last command finished with `out`.
    fn done(&mut self, out: RunOutput) {
        let seq = self.last().0;
        self.t.on_cargo_done(seq, out, 12);
    }
    fn audit(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(&self.audit_path)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn exited(code: i32, stdout: &str, stderr: &str) -> RunOutput {
    RunOutput::Exited { code, stdout: stdout.into(), stderr: stderr.into() }
}

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn start_runs_at_once_with_id_and_runtime() {
    let mut h = harness(vec![c("a1", "web", "EXITED")]);
    h.key(Key::Char('U'));
    let (_, argv, timeout, merged) = h.last();
    assert_eq!((argv, timeout, merged), (v(&["docker", "start", "a1"]), Duration::from_secs(30), false));
    let s = h.screen();
    assert!(s.contains("STARTING WEB ...") && s.contains("STARTING..."), "{s}");
}

#[test]
fn restart_and_stop_need_armed_y() {
    let mut h = harness(vec![c("a1", "web", "UP")]);
    h.key(Key::Char('r'));
    assert!(h.screen().contains("RESTART WEB? Y/N"));
    assert_eq!(h.n_calls(), 0);
    h.key(Key::Char('y'));
    assert_eq!(h.n_calls(), 0, "Y before arming");
    h.arm();
    h.key(Key::Char('Y'));
    assert_eq!(h.argv(), v(&["docker", "restart", "a1"]));
    h.done(exited(0, "a1\n", ""));
    h.key(Key::Char('d'));
    assert!(h.screen().contains("STOP WEB? Y/N"));
    h.arm();
    h.key(Key::Char('y'));
    assert_eq!(h.argv(), v(&["docker", "stop", "a1"]));
}

#[test]
fn n_and_esc_cancel_any_time() {
    for k in [Key::Char('n'), Key::Char('N'), Key::Esc] {
        let mut h = harness(vec![c("a1", "web", "UP")]);
        h.key(Key::Char('d'));
        h.key(k.clone()); // before arming: cancelling is always safe
        let s = h.screen();
        assert!(!s.contains("Y/N"), "{k:?}\n{s}");
        h.arm();
        h.key(Key::Char('y'));
        assert_eq!(h.n_calls(), 0, "{k:?}");
        assert!(h.audit().is_empty());
    }
}

#[test]
fn obscured_render_clears_arming() {
    let mut h = harness(vec![c("a1", "web", "UP")]);
    h.key(Key::Char('d'));
    h.arm();
    h.t.set_obscured(true);
    h.t.set_obscured(false);
    h.key(Key::Char('y'));
    assert_eq!(h.n_calls(), 0);
    h.arm();
    h.key(Key::Char('y'));
    assert_eq!(h.argv(), v(&["docker", "stop", "a1"]));
}

#[test]
fn confirm_targets_pinned_id_after_resort() {
    let mut h = harness(vec![c("a1", "web", "UP"), c("b2", "db", "UP")]);
    h.key(Key::Down);
    h.key(Key::Char('d'));
    assert!(h.screen().contains("STOP DB? Y/N"));
    h.arm();
    h.t.on_metrics(&snap(Some(vec![c("c3", "new", "EXITED"), c("a1", "web", "UP"), c("b2", "db", "UP")])));
    h.key(Key::Char('Y'));
    assert_eq!(h.argv(), v(&["docker", "stop", "b2"]));
}

#[test]
fn vanished_target_drops_prompt() {
    let mut h = harness(vec![c("a1", "web", "UP"), c("b2", "db", "UP")]);
    h.key(Key::Down);
    h.key(Key::Char('d'));
    h.t.on_metrics(&snap(Some(vec![c("a1", "web", "UP")])));
    assert!(h.screen().contains("DB GONE"));
    h.arm();
    h.key(Key::Char('y'));
    assert_eq!(h.n_calls(), 0);
}

#[test]
fn busy_while_in_flight() {
    let mut h = harness(vec![c("a1", "web", "UP"), c("b2", "db", "UP")]);
    h.key(Key::Char('U'));
    h.key(Key::Char('R'));
    assert!(h.screen().contains("BUSY"));
    assert_eq!(h.n_calls(), 1);
    h.key(Key::Down);
    assert!(h.screen().contains(">DB"));
}

#[test]
fn success_message_then_refresh() {
    let mut h = harness(vec![c("a1", "web", "EXITED")]);
    h.key(Key::Char('U'));
    h.done(exited(0, "a1\n", ""));
    let s = h.screen();
    assert!(s.contains("WEB STARTED") && !s.contains("STARTING..."), "{s}");
    let (_, argv, timeout, merged) = h.last();
    assert_eq!((argv, timeout, merged), (ps_argv("docker"), Duration::from_secs(2), false));
    h.done(exited(0, "a1\tweb\tUp 1 second\n", ""));
    let buf = render(&mut h.t, WINDOWED);
    let (_, y) = find(&buf, ">WEB").unwrap();
    let row = text(&buf).lines().nth(y as usize).unwrap().to_string();
    assert!(row.contains(" UP ") && !row.contains("EXITED"), "{row}");
    h.key(Key::Down); // any key clears the result
    assert!(h.screen().contains("WEB: UP 1 SECOND"));
}

#[test]
fn errors_clear_busy_and_show_reverse() {
    let cases = [
        (exited(1, "", "Error: no such container\n"), "STOP FAILED: ERROR: NO SUCH CONTAINER"),
        (RunOutput::NotFound, "RUNTIME NOT FOUND"),
        (RunOutput::TimedOut, "STOP TIMED OUT"),
    ];
    for (out, msg) in cases {
        let mut h = harness(vec![c("a1", "web", "UP")]);
        h.key(Key::Char('D'));
        h.arm();
        h.key(Key::Char('Y'));
        h.done(out);
        let buf = render(&mut h.t, WINDOWED);
        let (x, y) = find(&buf, msg).unwrap_or_else(|| panic!("{msg}\n{}", text(&buf)));
        assert!(buf[(x, y)].modifier.contains(Modifier::REVERSED), "{msg}");
        h.key(Key::Char('U'));
        assert_eq!(h.argv(), v(&["docker", "start", "a1"]), "{msg}: not busy any more");
    }
}

#[test]
fn stale_seq_ignored() {
    let mut h = harness(vec![c("a1", "web", "UP")]);
    h.key(Key::Char('U'));
    h.t.on_cargo_done(999, exited(0, "", ""), 1);
    assert!(h.screen().contains("STARTING WEB ..."));
    assert_eq!(h.n_calls(), 1);
}

#[test]
fn actions_ignored_when_empty() {
    let mut h = harness(vec![]);
    for k in "URDLurdl".chars() {
        h.key(Key::Char(k));
    }
    assert_eq!(h.n_calls(), 0);
    let mut h = harness(vec![c("a1", "web", "UP")]);
    h.t.on_metrics(&Snapshot { container_runtime: None, ..snap(Some(vec![c("a1", "web", "UP")])) });
    for k in "URDLurdl".chars() {
        h.key(Key::Char(k));
    }
    h.arm();
    h.key(Key::Char('y'));
    assert_eq!(h.n_calls(), 0);
}

#[test]
fn audit_entries_per_action() {
    let mut h = harness(vec![c("a1", "web", "UP")]);
    h.key(Key::Char('r'));
    h.key(Key::Char('n')); // cancelled: not audited
    h.key(Key::Char('d'));
    h.arm();
    h.key(Key::Char('y'));
    h.done(exited(0, "a1\n", ""));
    h.done(exited(0, "a1\tweb\tExited (0) now\n", "")); // the refresh: not audited
    h.key(Key::Char('u'));
    h.done(exited(0, "a1\n", ""));
    let a = h.audit();
    assert_eq!(a.len(), 2, "{a:?}");
    assert_eq!((&a[0]["tool"], &a[0]["decision"], &a[0]["tier"]), (&"cargo_stop".into(), &"confirmed".into(), &2.into()));
    assert_eq!(a[0]["argv"], serde_json::json!(["docker", "stop", "a1"]));
    assert_eq!(a[0]["args"], serde_json::json!({"id": "a1", "name": "web"}));
    assert_eq!(a[0]["exit"], serde_json::json!(0));
    assert_eq!((&a[1]["tool"], &a[1]["decision"], &a[1]["tier"]), (&"cargo_start".into(), &"auto".into(), &1.into()));
}

#[test]
fn offline_keeps_list_and_actions() {
    let mut h = harness(vec![c("a1", "web", "UP")]);
    h.t.on_metrics_offline("X");
    let s = h.screen();
    assert!(s.contains(">WEB") && s.contains("TELEMETRY OFFLINE"), "{s}");
    h.key(Key::Char('U'));
    assert_eq!(h.argv(), v(&["docker", "start", "a1"]));
}

#[test]
fn background_done_lights_activity() {
    let mut h = harness(vec![c("a1", "web", "UP")]);
    h.key(Key::Char('U'));
    assert!(!h.t.take_activity());
    h.t.set_obscured(true);
    h.done(exited(0, "a1\n", ""));
    assert!(h.t.take_activity());
    assert!(!h.t.take_activity());
}
