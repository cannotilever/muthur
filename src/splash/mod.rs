//! Boot splash: the Weyland-Yutani logo and system check.

pub mod logo;

use crate::config::FlowControl;
use crate::dialect::LineAttr;
use crate::metrics::{Snapshot, human_bytes, human_uptime};
use crate::muthur::reveal::Reveal;
use logo::Logo;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::time::{Duration, Instant};

pub const LOGO_SETTLE: Duration = Duration::from_millis(800);
pub const CPS: u32 = 400;
pub const VALUE_PAUSE: Duration = Duration::from_millis(400);
pub const FACT_WAIT: Duration = Duration::from_millis(1500);
pub const READY_HOLD: Duration = Duration::from_millis(2500);

const NO_DATA: &str = "NO DATA";
/// 0-based x of the first boot-line column (column 9).
const LINE_X: u16 = 8;
/// 0-based x where dots end (exclusive): column 46.
const DOTS_END: usize = 46;
const LOGO_TOP: u16 = 1;
const LOGO_BAND: usize = 22;
const HEADER_Y: u16 = 24;
const LINES_Y: u16 = 26;
const READY_Y: u16 = 42;

pub enum Interface {
    Online,
    Offline(String),
}

pub struct SplashInfo {
    pub ship_name: String,
    pub year: i32,
    pub link: String,
    pub interface: Interface,
}

impl SplashInfo {
    pub fn from_config(cfg: &crate::config::Config, ship_name: &str, now: chrono::NaiveDateTime) -> Self {
        use chrono::Datelike;
        Self {
            ship_name: ship_name.to_string(),
            year: now.year() + cfg.chrome.year_offset,
            link: link_label(cfg.link.baud, cfg.link.flow),
            interface: match cfg.muthur.readiness() {
                Ok(()) => Interface::Online,
                Err(e) => Interface::Offline(e.reason().to_string()),
            },
        }
    }
}

pub fn link_label(baud: u32, flow: FlowControl) -> String {
    let f = match flow {
        FlowControl::XonXoff => "XON/XOFF",
        FlowControl::RtsCts => "RTS/CTS",
        FlowControl::None => "NONE",
    };
    format!("{baud} BAUD  {f}")
}

#[derive(Clone, Copy)]
enum Fact {
    Processor,
    Memory,
    Uptime,
    Units,
}

enum Step {
    Type(String),
    Pause(Duration),
    Fact(Fact),
    Ready,
}

pub struct Splash {
    logo: Logo,
    header: String,
    steps: Vec<Step>,
    next: usize,
    /// Virtual time at which the next step starts.
    cursor: Instant,
    reveal: Reveal,
    snapshot: Option<Snapshot>,
    offline: bool,
    timed_out: bool,
    waiting: bool,
    ready_at: Option<Instant>,
    /// `READY.` appeared and the bell has not been taken yet.
    bell: bool,
    done: bool,
}

fn line_prefix(label: &str) -> String {
    let dots = DOTS_END.saturating_sub(LINE_X as usize + label.chars().count() + 1);
    format!("{label} {} ", ".".repeat(dots))
}

impl Splash {
    pub fn new(info: SplashInfo, now: Instant) -> Self {
        Self::with_logo(info, Logo::shipped().clone(), now)
    }

    pub fn with_logo(info: SplashInfo, logo: Logo, now: Instant) -> Self {
        let iface = match &info.interface {
            Interface::Online => "[ONLINE]".to_string(),
            Interface::Offline(r) => format!("[OFFLINE: {}]", r.to_uppercase()),
        };
        let mut steps = Vec::new();
        let mut line = |label: &str, value: Option<String>, fact: Option<Fact>| {
            steps.push(Step::Type(line_prefix(label)));
            match (value, fact) {
                (Some(v), _) => steps.push(Step::Type(format!("{v}\n"))),
                (None, Some(f)) => steps.push(Step::Fact(f)),
                (None, None) => unreachable!(),
            }
            steps.push(Step::Pause(VALUE_PAUSE));
        };
        line("SYSTEM CHECK", Some("OK".into()), None);
        line("VESSEL", Some(info.ship_name.to_uppercase()), None);
        line("PROCESSOR", None, Some(Fact::Processor));
        line("MEMORY", None, Some(Fact::Memory));
        line("UPTIME", None, Some(Fact::Uptime));
        line("LINK", Some(info.link.to_uppercase()), None);
        line("SYSTEM UNITS", None, Some(Fact::Units));
        line("INTERFACE", Some(iface), None);
        steps.push(Step::Ready);
        Self {
            logo,
            header: format!("MU/TH/UR 6000 \u{2014} INTERFACE {}", info.year),
            steps,
            next: 0,
            cursor: now + LOGO_SETTLE,
            reveal: Reveal::new(CPS),
            snapshot: None,
            offline: false,
            timed_out: false,
            waiting: false,
            ready_at: None,
            bell: false,
            done: false,
        }
    }

    pub fn on_metrics(&mut self, s: &Snapshot) {
        self.snapshot = Some(s.clone());
    }

    pub fn on_metrics_offline(&mut self) {
        self.offline = true;
    }

    fn fact_value(&self, f: Fact) -> Option<String> {
        let s = self.snapshot.as_ref()?;
        Some(match f {
            Fact::Processor => format!("{} CORES", s.cpu_cores.len()),
            Fact::Memory => human_bytes(s.mem.total),
            Fact::Uptime => human_uptime(s.uptime_secs),
            Fact::Units => match &s.failed_units {
                None => "NOT MONITORED".into(),
                Some(u) if u.is_empty() => "NOMINAL".into(),
                Some(u) => format!("{} FAILED", u.len()),
            },
        })
    }

    /// Run every step that is due at `now`. Returns true if the display changed.
    pub fn advance(&mut self, now: Instant) -> bool {
        let mut changed = false;
        while self.next < self.steps.len() && now >= self.cursor {
            match &self.steps[self.next] {
                Step::Type(t) => {
                    let n = t.chars().count() as u32;
                    self.reveal.push(t, self.cursor);
                    self.cursor += Duration::from_secs_f64(f64::from(n) / f64::from(CPS));
                }
                Step::Pause(d) => self.cursor += *d,
                Step::Fact(f) => {
                    let f = *f;
                    let (text, wait_cost) = if let Some(v) = self.fact_value(f) {
                        (v, Duration::ZERO)
                    } else if self.offline || self.timed_out {
                        (NO_DATA.to_string(), Duration::ZERO)
                    } else if now >= self.cursor + FACT_WAIT {
                        self.timed_out = true;
                        (NO_DATA.to_string(), FACT_WAIT)
                    } else {
                        self.waiting = true;
                        break;
                    };
                    self.cursor += wait_cost;
                    let n = text.chars().count() as u32 + 1;
                    // Typed from the wait's start so the value shows at once;
                    // later lines resume from now, not from the wait's start.
                    self.reveal.push(&format!("{text}\n"), self.cursor);
                    if self.waiting {
                        self.cursor = self.cursor.max(now);
                    }
                    self.waiting = false;
                    self.cursor += Duration::from_secs_f64(f64::from(n) / f64::from(CPS));
                }
                Step::Ready => {
                    self.ready_at = Some(self.cursor);
                    self.bell = true;
                }
            }
            self.next += 1;
            changed = true;
        }
        if self.reveal.advance(now) {
            changed = true;
        }
        if let Some(r) = self.ready_at
            && !self.done
            && now >= r + READY_HOLD
        {
            self.done = true;
            changed = true;
        }
        changed
    }

    /// True once, when `READY.` has appeared: the caller rings the bell.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell)
    }

    pub fn tick_interval(&self) -> Duration {
        if self.next > 0 && self.ready_at.is_none() {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(100)
        }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The logo's first screen row: centred in the logo band.
    fn logo_top(&self) -> i32 {
        i32::from(LOGO_TOP) + (LOGO_BAND.saturating_sub(self.logo.height()) / 2) as i32
    }

    /// The DECDHL rows this frame needs (absolute screen rows).
    pub fn line_attrs(&self, area: Rect) -> Vec<(u16, LineAttr)> {
        logo::line_attrs(&self.logo, area, self.logo_top())
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let put = |buf: &mut Buffer, x: i32, y: i32, s: &str| {
            if y < 0 || y >= i32::from(area.height) {
                return;
            }
            for (i, ch) in s.chars().enumerate() {
                let cx = x + i as i32;
                if cx < 0 || cx >= i32::from(area.width) {
                    continue;
                }
                let mut b = [0u8; 4];
                buf[(area.x + cx as u16, area.y + y as u16)].set_symbol(ch.encode_utf8(&mut b));
            }
        };
        logo::draw(&self.logo, area, self.logo_top(), buf);
        // Header.
        let hx = (i32::from(area.width) - self.header.chars().count() as i32) / 2;
        put(buf, hx, i32::from(HEADER_Y), &self.header);
        // Boot lines.
        for (i, l) in self.reveal.visible().split('\n').enumerate() {
            put(buf, i32::from(LINE_X), i32::from(LINES_Y) + i as i32, l);
        }
        if self.ready_at.is_some() {
            put(buf, i32::from(LINE_X), i32::from(READY_Y), "READY.");
        }
    }
}

#[cfg(test)]
mod tests;
