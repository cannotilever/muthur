use super::*;
use crate::dialect::{Glyph, map_glyph};
use crate::metrics::{Container, Disk, NetRate, Proc, Usage};
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

const WINDOWED: Rect = Rect { x: 19, y: 2, width: 61, height: 40 };
const FULL: Rect = Rect { x: 0, y: 0, width: 80, height: 43 };
const G: u64 = 1 << 30;

pub(crate) fn snap() -> Snapshot {
    Snapshot {
        host: "NOSTROMO".into(),
        uptime_secs: 86400 * 3 + 3600 * 2 + 60 * 5,
        load: [0.42, 0.38, 0.30],
        cpu_total: 42.0,
        cpu_cores: vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0],
        mem: Usage { used: 12 * G, total: 32 * G },
        swap: Usage { used: G, total: 8 * G },
        disks: vec![
            Disk { mount: "/".into(), usage: Usage { used: 700 * G, total: 1000 * G } },
            Disk { mount: "/tank".into(), usage: Usage { used: 1100 * G, total: 2000 * G } },
        ],
        net: NetRate { rx_bps: 4608, tx_bps: 6144 },
        top: vec![
            Proc { pid: 1382, name: "RESTIC".into(), cpu: 121.5, mem: 500 << 20 },
            Proc { pid: 77, name: "FISH".into(), cpu: 1.0, mem: 8 << 20 },
        ],
        failed_units: Some(vec!["AUTORANDR".into()]),
        containers: Some(vec![Container { id: "1".into(), name: "web".into(), status: "UP".into(), ..Default::default() }]),
        container_runtime: Some("docker".into()),
        containers_at: None,
    }
}

fn render(t: &mut UtilityTab, area: Rect) -> Buffer {
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
    text(buf).lines().enumerate().find_map(|(y, l)| {
        l.find(needle).map(|byte| (l[..byte].chars().count() as u16, y as u16))
    })
}

#[test]
fn gauges_in_a_panel_line_up() {
    let mut t = UtilityTab::new();
    t.on_snapshot(snap());
    let buf = render(&mut t, WINDOWED);
    let s = text(&buf);
    let close = |needle: &str| {
        let l = s.lines().find(|l| l.contains(needle)).unwrap();
        l.chars().position(|c| c == ']').unwrap()
    };
    assert_eq!(close("MEM "), close("SWAP "), "{s}");
    assert_eq!(close("/TANK "), close("│/  "), "{s}");
    assert!(s.contains("/TANK [") || s.contains("/TANK ["), "label needs a gap: {s}");
}

#[test]
fn awaiting_before_first_snapshot() {
    let s = text(&render(&mut UtilityTab::new(), WINDOWED));
    assert!(s.contains("AWAITING TELEMETRY"), "{s}");
}

#[test]
fn windowed_shows_all_panels() {
    let mut t = UtilityTab::new();
    t.on_snapshot(snap());
    let buf = render(&mut t, WINDOWED);
    let s = text(&buf);
    for title in ["SYSTEM - NOSTROMO", " CPU ", " MEMORY ", " DISKS ", " NETWORK ", " PROCESSES ", " SERVICES "] {
        assert!(s.contains(title), "missing {title}\n{s}");
    }
    assert!(s.contains("UPTIME 3D 02:05") && s.contains("LOAD 0.42 0.38 0.30"), "{s}");
    assert!(s.contains("12.0G/32.0G") && s.contains("RESTIC") && s.contains("RX 4.5K/S"), "{s}");
    assert!(s.contains("CONTAINERS 1 (SEE CARGO)"), "{s}");
    let (x, y) = find(&buf, "FAILED AUTORANDR").expect("failed unit");
    assert!(buf[(x, y)].modifier.contains(Modifier::REVERSED));
    // Nothing escapes the content pane.
    for y in 0..43 {
        for x in 0..80 {
            if !WINDOWED.contains((x, y).into()) {
                assert_eq!(buf[(x, y)].symbol(), " ", "wrote outside at {x},{y}");
            }
        }
    }
}

#[test]
fn fullscreen_uses_two_columns() {
    let mut t = UtilityTab::new();
    t.on_snapshot(snap());
    let buf = render(&mut t, FULL);
    let (x, _) = find(&buf, " DISKS ").expect("disks");
    assert!(x >= 40, "DISKS at x={x}");
    let (x, _) = find(&buf, " CPU ").expect("cpu");
    assert!(x < 40, "CPU at x={x}");
}

#[test]
fn many_cores_fit() {
    let mut t = UtilityTab::new();
    t.on_snapshot(Snapshot { cpu_cores: (0..32).map(|i| i as f32 * 3.0).collect(), ..snap() });
    let mut buf = Buffer::empty(FULL);
    for c in buf.content.iter_mut() {
        c.set_symbol("#");
    }
    t.render(WINDOWED, &mut buf);
    assert!(text(&buf).contains("C31"), "{}", text(&buf));
    for y in 0..43 {
        for x in 0..80 {
            if !WINDOWED.contains((x, y).into()) {
                assert_eq!(buf[(x, y)].symbol(), "#", "wrote outside at {x},{y}");
            }
        }
    }
}

#[test]
fn failed_units_survive_a_big_machine() {
    let mut t = UtilityTab::new();
    let top: Vec<Proc> = (0..5).map(|i| Proc { pid: i, name: format!("P{i}"), cpu: 1.0, mem: 1 }).collect();
    t.on_snapshot(Snapshot { cpu_cores: vec![5.0; 32], top, ..snap() });
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("FAILED AUTORANDR"), "{s}");
}

#[test]
fn long_mount_names_stay_distinct() {
    let mut t = UtilityTab::new();
    let u = Usage { used: G, total: 2 * G };
    t.on_snapshot(Snapshot {
        disks: vec![Disk { mount: "/mnt/backup".into(), usage: u }, Disk { mount: "/mnt/media".into(), usage: u }],
        ..snap()
    });
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("/MNT/BACKUP") && s.contains("/MNT/MEDIA"), "{s}");
}

#[test]
fn offline_telemetry_is_shown_and_clears() {
    let mut t = UtilityTab::new();
    t.on_offline("PERMISSION DENIED");
    let s = text(&render(&mut t, WINDOWED));
    assert!(s.contains("TELEMETRY OFFLINE") && s.contains("PERMISSION DENIED"), "{s}");
    t.on_snapshot(snap());
    assert!(!text(&render(&mut t, WINDOWED)).contains("OFFLINE"));
}

#[test]
fn services_panel_handles_none() {
    let mut t = UtilityTab::new();
    t.on_snapshot(Snapshot { failed_units: None, containers: None, ..snap() });
    let s = text(&render(&mut t, FULL));
    assert!(s.contains("UNITS N/A") && !s.contains("CONTAINERS"), "{s}");
}

#[test]
fn nominal_when_nothing_failed() {
    let mut t = UtilityTab::new();
    t.on_snapshot(Snapshot { failed_units: Some(vec![]), containers: Some(vec![]), ..snap() });
    let s = text(&render(&mut t, FULL));
    assert!(s.contains("ALL SYSTEMS NOMINAL") && s.contains("CONTAINERS NONE"), "{s}");
}

#[test]
fn no_swap_says_none() {
    let mut t = UtilityTab::new();
    t.on_snapshot(Snapshot { swap: Usage::default(), ..snap() });
    assert!(text(&render(&mut t, FULL)).contains("SWAP  NONE"));
}

#[test]
fn activity_on_new_failure() {
    let mut t = UtilityTab::new();
    t.on_snapshot(Snapshot { failed_units: Some(vec![]), ..snap() });
    assert!(!t.take_activity());
    t.on_snapshot(snap());
    assert!(t.take_activity());
    assert!(!t.take_activity());
    t.on_snapshot(snap());
    assert!(!t.take_activity());
}

#[test]
fn renders_only_dialect_safe_glyphs() {
    let mut t = UtilityTab::new();
    for _ in 0..5 {
        t.on_snapshot(snap());
    }
    for area in [WINDOWED, FULL] {
        let buf = render(&mut t, area);
        for c in buf.content() {
            assert!(c.symbol() == "?" || map_glyph(c.symbol()) != Glyph::Ascii(b'?'), "{:?}", c.symbol());
        }
    }
}

#[test]
#[ignore]
fn print_layouts() {
    let mut t = UtilityTab::new();
    for i in 0..30 {
        let mut s = snap();
        s.cpu_total = (i * 7 % 100) as f32;
        s.net.rx_bps = (i * 997 % 9000) as u64;
        t.on_snapshot(s);
    }
    for a in [WINDOWED, FULL] {
        println!("{}", text(&render(&mut t, a)));
    }
}

#[test]
fn many_containers_are_one_summary_line() {
    let many: Vec<Container> = (0..60).map(|i| Container { id: format!("{i}"), name: format!("svc{i}"), status: "UP".into(), ..Default::default() }).collect();
    let mut t = UtilityTab::new();
    t.on_snapshot(Snapshot { containers: Some(many), ..snap() });
    for area in [WINDOWED, FULL] {
        let s = text(&render(&mut t, area));
        assert!(s.contains("CONTAINERS 60 (SEE CARGO)"), "{s}");
        assert!(!s.contains("SVC0"), "{s}");
        assert!(s.contains(" DISKS ") && s.contains(" PROCESSES "), "{s}");
    }
}
