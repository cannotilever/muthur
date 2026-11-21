use super::logo::Logo;
use super::*;
use crate::config::FlowControl;
use crate::metrics::{Snapshot, Usage};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::time::{Duration, Instant};

fn info() -> SplashInfo {
    SplashInfo {
        ship_name: "USCSS OMICRON".into(),
        year: 2137,
        link: link_label(38400, FlowControl::XonXoff),
        interface: Interface::Online,
    }
}

fn snap() -> Snapshot {
    Snapshot {
        cpu_cores: vec![0.0; 16],
        mem: Usage {
            used: 0,
            total: (31.2 * (1u64 << 30) as f64) as u64,
        },
        uptime_secs: 3 * 86400 + 4 * 3600 + 12 * 60,
        failed_units: Some(vec![]),
        ..Default::default()
    }
}

fn text(s: &Splash) -> String {
    let area = Rect::new(0, 0, 80, 43);
    let mut buf = Buffer::empty(area);
    s.render(area, &mut buf);
    rows(&buf, area).join("\n")
}

fn rows(buf: &Buffer, area: Rect) -> Vec<String> {
    (area.top()..area.bottom())
        .map(|y| (area.left()..area.right()).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect()
}

fn ms(t0: Instant, n: u64) -> Instant {
    t0 + Duration::from_millis(n)
}

/// Step in 10 ms increments until `f` is true; returns the elapsed ms.
fn step_until(s: &mut Splash, t0: Instant, limit: u64, f: impl Fn(&Splash) -> bool) -> u64 {
    let mut n = 0;
    while n <= limit {
        s.advance(ms(t0, n));
        if f(s) {
            return n;
        }
        n += 10;
    }
    panic!("condition not reached within {limit} ms");
}

fn done_at(s: &mut Splash, t0: Instant) -> u64 {
    step_until(s, t0, 20_000, |s| s.is_done())
}

#[test]
fn link_label_formats() {
    assert_eq!(link_label(38400, FlowControl::XonXoff), "38400 BAUD  XON/XOFF");
    assert_eq!(link_label(9600, FlowControl::None), "9600 BAUD  NONE");
    assert_eq!(link_label(9600, FlowControl::RtsCts), "9600 BAUD  RTS/CTS");
}

#[test]
fn first_frame_is_logo_and_header_only() {
    let t0 = Instant::now();
    let s = Splash::new(info(), t0);
    let out = text(&s);
    assert!(out.lines().nth(24).unwrap().trim() == "MU/TH/UR 6000 — INTERFACE 2137");
    assert!(!out.contains("SYSTEM CHECK"));
}

#[test]
fn lines_wait_for_logo_settle() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    s.advance(ms(t0, 799));
    assert!(!text(&s).contains("SYSTEM CHECK"));
    s.advance(ms(t0, 900));
    assert!(text(&s).contains("SYSTEM CHECK"));
}

#[test]
fn full_run_formats_every_fact() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    s.advance(ms(t0, 60_000));
    let out = text(&s);
    let row = |n: usize| out.lines().nth(n - 1).unwrap().to_string();
    assert_eq!(row(27).trim_end(), format!("        SYSTEM CHECK {} OK", ".".repeat(25)));
    for want in ["USCSS OMICRON", "16 CORES", "31.2G", "3D 04:12", "38400 BAUD  XON/XOFF", "NOMINAL", "[ONLINE]"] {
        assert!(out.contains(want), "{want}\n{out}");
    }
    assert_eq!(row(43).trim(), "READY.");
}

#[test]
fn render_keeps_final_frame_after_done() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    s.advance(ms(t0, 60_000));
    s.advance(ms(t0, 70_000));
    assert!(s.is_done());
    let out = text(&s);
    assert!(out.contains("[ONLINE]"));
    assert_eq!(out.lines().nth(42).unwrap().trim(), "READY.");
}

#[test]
fn ready_holds_two_and_a_half_seconds_then_done() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    let t = step_until(&mut s, t0, 20_000, |s| text(s).lines().nth(42).unwrap().trim() == "READY.");
    assert!(!s.is_done());
    // The hold runs from the step's due time, up to one 10 ms tick before `t`.
    s.advance(ms(t0, t + 2489));
    assert!(!s.is_done());
    s.advance(ms(t0, t + 2500));
    assert!(s.is_done());
}

#[test]
fn total_time_with_warm_snapshot_is_about_seven_and_a_half_seconds() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    let d = done_at(&mut s, t0);
    assert!((6_500..=8_500).contains(&d), "{d}");
}

#[test]
fn fact_line_waits_then_no_data_once() {
    let t0 = Instant::now();
    let mut warm = Splash::new(info(), t0);
    warm.on_metrics(&snap());
    let warm_total = done_at(&mut warm, t0);
    let mut s = Splash::new(info(), t0);
    let d = done_at(&mut s, t0);
    assert!(d < warm_total + 1_600, "{d} vs {warm_total}");
    assert!(d >= warm_total + 1_400, "waited once: {d} vs {warm_total}");
    let out = text(&s);
    for label in ["PROCESSOR", "MEMORY", "UPTIME", "SYSTEM UNITS"] {
        let line = out.lines().find(|l| l.contains(label)).unwrap();
        assert!(line.trim_end().ends_with("NO DATA"), "{line}");
    }
}

#[test]
fn metrics_offline_skips_the_wait() {
    let t0 = Instant::now();
    let mut warm = Splash::new(info(), t0);
    warm.on_metrics(&snap());
    let warm_total = done_at(&mut warm, t0);
    let mut s = Splash::new(info(), t0);
    s.on_metrics_offline();
    let d = done_at(&mut s, t0);
    assert!(d.abs_diff(warm_total) <= 100, "{d} vs {warm_total}");
    assert_eq!(text(&s).matches("NO DATA").count(), 4);
}

#[test]
fn snapshot_during_wait_resumes_immediately() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    let full = format!("PROCESSOR {} ", ".".repeat(28));
    let t = step_until(&mut s, t0, 20_000, |s| text(s).contains(&full));
    // Genuinely waiting: advance further with no snapshot, nothing resolves.
    s.advance(ms(t0, t + 200));
    assert!(!text(&s).contains("NO DATA") && !text(&s).contains("CORES"));
    s.on_metrics(&snap());
    s.advance(ms(t0, t + 210));
    assert!(text(&s).contains("16 CORES"), "{}", text(&s));
    // The following line starts from now, not from the wait's start.
    assert!(!text(&s).contains("MEMORY"));
}

#[test]
fn one_late_advance_reaches_done() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    s.advance(ms(t0, 60_000));
    assert!(s.is_done());
}

#[test]
fn long_values_are_truncated() {
    let t0 = Instant::now();
    let name: String = ('A'..='Z').cycle().take(60).collect();
    let mut s = Splash::new(
        SplashInfo {
            ship_name: name.clone(),
            ..info()
        },
        t0,
    );
    s.on_metrics(&snap());
    s.advance(ms(t0, 60_000));
    let out = text(&s);
    let row = out.lines().nth(27).unwrap();
    assert_eq!(row.chars().count(), 80);
    assert!(row.ends_with(&name[..33]), "{row}");
}

#[test]
fn offline_reason_is_uppercased() {
    let t0 = Instant::now();
    let mut s = Splash::new(
        SplashInfo {
            interface: Interface::Offline("no key".into()),
            ..info()
        },
        t0,
    );
    s.on_metrics(&snap());
    s.advance(ms(t0, 60_000));
    assert!(text(&s).contains("[OFFLINE: NO KEY]"));
}

#[test]
fn snapshot_after_timeout_fills_later_lines() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    let t = step_until(&mut s, t0, 20_000, |s| text(s).contains("NO DATA"));
    s.on_metrics(&snap());
    s.advance(ms(t0, t + 20_000));
    let out = text(&s);
    let line = |l: &str| out.lines().find(|x| x.contains(l)).unwrap().to_string();
    assert!(line("PROCESSOR").trim_end().ends_with("NO DATA"));
    assert!(line("MEMORY").contains("31.2G"));
}

#[test]
fn failed_units_variants() {
    let t0 = Instant::now();
    let mut a = Splash::new(info(), t0);
    a.on_metrics(&Snapshot {
        failed_units: Some(vec!["a".into(), "b".into()]),
        ..snap()
    });
    a.advance(ms(t0, 60_000));
    assert!(text(&a).contains("2 FAILED"));
    let mut b = Splash::new(info(), t0);
    b.on_metrics(&Snapshot { failed_units: None, ..snap() });
    b.advance(ms(t0, 60_000));
    assert!(text(&b).contains("NOT MONITORED"));
}

#[test]
fn offline_interface() {
    let t0 = Instant::now();
    let mut s = Splash::new(
        SplashInfo {
            interface: Interface::Offline("NO ACCESS CODE".into()),
            ..info()
        },
        t0,
    );
    s.on_metrics(&snap());
    s.advance(ms(t0, 60_000));
    assert!(text(&s).contains("[OFFLINE: NO ACCESS CODE]"));
}

#[test]
fn uptime_under_a_day() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&Snapshot {
        uptime_secs: 4 * 3600 + 12 * 60,
        ..snap()
    });
    s.advance(ms(t0, 60_000));
    let out = text(&s);
    let line = out.lines().find(|l| l.contains("UPTIME")).unwrap();
    assert!(line.trim_end().ends_with("04:12") && !line.contains('D'), "{line}");
}

#[test]
fn tick_interval_is_fast_while_typing() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    s.advance(ms(t0, 0));
    assert_eq!(s.tick_interval(), Duration::from_millis(100));
    s.advance(ms(t0, 900));
    assert_eq!(s.tick_interval(), Duration::from_millis(50));
}

#[test]
fn logo_is_centred_and_blocks_render_as_full_block() {
    let t0 = Instant::now();
    let s = Splash::with_logo(info(), Logo::parse("██\n"), t0);
    let out = text(&s);
    let row: Vec<&str> = out.lines().nth(11).unwrap().split("").filter(|c| !c.is_empty()).collect();
    assert_eq!(row[39], "█");
    assert_eq!(row[40], "█");
    assert_eq!(out.matches('█').count(), 2);
}

#[test]
fn renders_inside_a_larger_area_without_panic() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    s.advance(ms(t0, 60_000));
    let big = Rect::new(0, 0, 100, 50);
    let mut buf = Buffer::empty(big);
    s.render(big, &mut buf);
    let r = rows(&buf, big);
    let header = r[24].find("MU/TH/UR 6000").unwrap();
    assert!(header == 35, "header centred on 100: {header}");

    let buf_area = Rect::new(0, 0, 80, 43);
    let mut buf = Buffer::empty(buf_area);
    s.render(Rect::new(0, 0, 80, 20), &mut buf);
    for y in 20..43 {
        for x in 0..80 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x},{y})");
        }
    }
}

fn info_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 11, 19).unwrap().and_hms_opt(0, 0, 0).unwrap()
}

#[test]
fn info_from_default_config_is_offline_without_key() {
    let cfg = crate::config::Config::default();
    let i = SplashInfo::from_config(&cfg, "USCSS X", info_date());
    assert_eq!(i.year, 2137);
    assert_eq!(i.link, "38400 BAUD  XON/XOFF");
    assert!(matches!(i.interface, Interface::Offline(ref r) if r == "NO ACCESS CODE"));
}

#[test]
fn info_online_with_key() {
    let mut cfg = crate::config::Config::default();
    cfg.muthur.api_key = Some("k".into());
    let i = SplashInfo::from_config(&cfg, "USCSS X", info_date());
    assert!(matches!(i.interface, Interface::Online));
}

#[test]
fn lines_linger_400_ms_after_each_value() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    let ok = step_until(&mut s, t0, 20_000, |s| text(s).contains(" OK"));
    s.advance(ms(t0, ok + 380));
    assert!(!text(&s).contains("VESSEL"), "next line started before the 400 ms pause");
    s.advance(ms(t0, ok + 420));
    assert!(text(&s).contains("VESSEL"));
}

#[test]
fn bell_rings_once_when_ready_appears() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    s.advance(ms(t0, 2_000));
    assert!(!s.take_bell(), "no bell before READY.");
    step_until(&mut s, t0, 20_000, |s| text(s).lines().nth(42).unwrap().trim() == "READY.");
    assert!(s.take_bell());
    assert!(!s.take_bell(), "rings once");
    s.advance(ms(t0, 60_000));
    assert!(!s.take_bell());
}

#[test]
fn one_late_advance_still_rings() {
    let t0 = Instant::now();
    let mut s = Splash::new(info(), t0);
    s.on_metrics(&snap());
    s.advance(ms(t0, 60_000));
    assert!(s.is_done());
    assert!(s.take_bell());
}

#[test]
fn wordmark_is_a_double_height_pair_centred_on_half_width() {
    let t0 = Instant::now();
    let s = Splash::with_logo(info(), Logo::parse("██\n^W Y\nAB\n"), t0);
    let area = Rect::new(0, 0, 80, 43);
    // 4 screen rows in the 22-row band from row 1: top at row 10.
    assert_eq!(s.line_attrs(area), vec![(11, LineAttr::DoubleTop), (12, LineAttr::DoubleBottom)]);
    let out = text(&s);
    let r: Vec<&str> = out.lines().collect();
    assert_eq!(r[10].trim(), "██");
    // "W Y" centred in 40 double-width columns: x = 18.
    assert_eq!(&r[11][..21], &format!("{}W Y", " ".repeat(18)));
    assert_eq!(r[11], r[12], "both halves carry the text");
    assert_eq!(r[13].trim(), "AB");
}

#[test]
fn double_row_that_does_not_fit_is_dropped() {
    let t0 = Instant::now();
    let s = Splash::with_logo(info(), Logo::parse("^W Y\n"), t0);
    // Top lands on row 11; a 12-row area has no room for its bottom half.
    let area = Rect::new(0, 0, 80, 12);
    assert!(s.line_attrs(area).is_empty());
    let mut buf = Buffer::empty(area);
    s.render(area, &mut buf);
    assert!(!rows(&buf, area).concat().contains('W'));
}
