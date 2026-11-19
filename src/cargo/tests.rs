use super::*;
use ratatui::style::Modifier;

const WINDOWED: Rect = Rect { x: 19, y: 2, width: 61, height: 40 };
const FULL: Rect = Rect { x: 0, y: 0, width: 80, height: 43 };

fn snap(containers: Option<Vec<Container>>) -> Snapshot {
    Snapshot { containers, ..Default::default() }
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
    let mut t = CargoTab::new();
    assert!(text(&render(&mut t, WINDOWED)).contains("AWAITING TELEMETRY"));
    t.on_metrics_offline("PERMISSION DENIED");
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("TELEMETRY OFFLINE") && s.contains("PERMISSION DENIED"), "{s}");
    t.on_metrics(&snap(Some(ship(1))));
    assert!(!text(&render(&mut t, WINDOWED)).contains("OFFLINE"));
}

#[test]
fn no_runtime_and_empty_hold() {
    let mut t = CargoTab::new();
    t.on_metrics(&snap(None));
    assert!(text(&render(&mut t, WINDOWED)).contains("NO CONTAINER RUNTIME"));
    t.on_metrics(&snap(Some(vec![])));
    assert!(text(&render(&mut t, WINDOWED)).contains("HOLD EMPTY"));
}

#[test]
fn lists_containers_with_status_column() {
    let mut t = CargoTab::new();
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
    let mut t = CargoTab::new();
    t.on_metrics(&snap(Some(ship(1))));
    assert!(text(&render(&mut t, WINDOWED)).contains(" CARGO - 1 CONTAINER "));
}

#[test]
fn long_names_are_cut() {
    let mut t = CargoTab::new();
    let name = "X".repeat(200);
    t.on_metrics(&snap(Some(vec![Container { id: "1".into(), name: name.clone(), status: "UP".into(), ..Default::default() }])));
    let buf = render(&mut t, WINDOWED);
    let (_, y) = find(&buf, "XXXX").unwrap();
    let row = text(&buf).lines().nth(y as usize).unwrap().to_string();
    // 59 inner columns: a 6-wide STATUS, one space, the name gets the rest.
    assert_eq!(row.matches('X').count(), 59 - 6 - 1, "{row}");
    assert!(row.contains("X UP "), "{row}");
}

#[test]
fn long_manifest_pages() {
    let mut t = CargoTab::new();
    t.on_metrics(&snap(Some(ship(100))));
    // 40 rows - 2 borders - 1 heading = 37 per page.
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("CARGO - 100 CONTAINERS (1-37)") && s.contains("CRATE036") && !s.contains("CRATE037"), "{s}");
    t.handle_key(&Key::PageDown);
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("(38-74)") && s.contains("CRATE037") && !s.contains("CRATE036"), "{s}");
    t.handle_key(&Key::PageDown);
    t.handle_key(&Key::PageDown); // clamps at the end
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("(64-100)") && s.contains("CRATE099"), "{s}");
    t.handle_key(&Key::Up);
    assert!(text(&render(&mut t, WINDOWED)).contains("(63-99)"));
    t.handle_key(&Key::Home);
    assert!(text(&render(&mut t, WINDOWED)).contains("(1-37)"));
    t.handle_key(&Key::PageUp); // clamps at the top
    assert!(text(&render(&mut t, WINDOWED)).contains("(1-37)"));
}

#[test]
fn shrinking_manifest_clamps_scroll() {
    let mut t = CargoTab::new();
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
    let mut t = CargoTab::new();
    t.on_metrics(&snap(Some(ship(100))));
    assert!(text(&render(&mut t, FULL)).contains("(1-40)"));
}

#[test]
fn trouble_sorts_to_the_top_in_runtime_order() {
    let mut t = CargoTab::new();
    let k = |name: &str, status: &str| Container { id: name.into(), name: name.into(), status: status.into(), ..Default::default() };
    t.on_metrics(&snap(Some(vec![k("A", "UP"), k("B", "EXITED"), k("C", "UP"), k("D", "RESTARTING"), k("E", "UP")])));
    let buf = render(&mut t, WINDOWED);
    let rows: Vec<u16> = ["B ", "D ", "A ", "C ", "E "].iter().map(|n| find(&buf, &format!("│{n}")).unwrap().1).collect();
    assert!(rows.windows(2).all(|w| w[0] < w[1]), "{rows:?}\n{}", text(&buf));
}

#[test]
fn names_display_upper_case() {
    let mut t = CargoTab::new();
    t.on_metrics(&snap(Some(vec![Container { id: "1".into(), name: "web".into(), status: "UP".into(), ..Default::default() }])));
    assert!(text(&render(&mut t, WINDOWED)).contains("WEB"));
}
