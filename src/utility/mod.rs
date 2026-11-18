//! The UTILITY folder: a system monitor in boxed panels.
//!
//! Two columns when there is room (fullscreen), one stacked column otherwise.
//! Panels are stacked top to bottom; the last in a column takes what is left;
//! a panel that doesn't fit at its minimum height is dropped, never squashed.

pub mod widgets;

use crate::app::Tab;
use crate::input::Key;
use crate::metrics::{History, Snapshot, human_bytes, human_rate, human_uptime};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use widgets::{gauge_line, panel, sparkline};

/// Samples kept for the sparklines (2 minutes at the default 2 s interval).
const HISTORY: usize = 60;
/// Narrowest column a per-core bar gets.
const CORE_COL: u16 = 18;
const MAX_DISKS: usize = 6;
const TWO_COLUMNS_FROM: u16 = 78;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    System,
    Cpu,
    Memory,
    Disks,
    Network,
    Processes,
    Services,
}

pub struct UtilityTab {
    snap: Option<Snapshot>,
    cpu: History,
    net: History,
    last_failed: usize,
    activity: bool,
}

impl Default for UtilityTab {
    fn default() -> Self {
        Self::new()
    }
}

fn reverse() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

/// `used/total` for each usage, right-aligned to the longest so bars line up.
fn aligned_details(usages: &[crate::metrics::Usage]) -> Vec<String> {
    let raw: Vec<String> = usages.iter().map(|u| format!("{}/{}", human_bytes(u.used), human_bytes(u.total))).collect();
    let w = raw.iter().map(String::len).max().unwrap_or(0);
    raw.into_iter().map(|d| format!("{d:>w$}")).collect()
}

impl UtilityTab {
    pub fn new() -> Self {
        Self { snap: None, cpu: History::new(HISTORY), net: History::new(HISTORY), last_failed: 0, activity: false }
    }

    pub fn on_snapshot(&mut self, s: Snapshot) {
        self.cpu.push(s.cpu_total);
        self.net.push((s.net.rx_bps + s.net.tx_bps) as f32);
        let failed = s.failed_units.as_ref().map_or(0, Vec::len);
        if failed > self.last_failed {
            self.activity = true;
        }
        self.last_failed = failed;
        self.snap = Some(s);
    }

    fn core_columns(inner_w: u16) -> u16 {
        (inner_w / CORE_COL).max(1)
    }

    fn services_lines(s: &Snapshot) -> Vec<(String, Style)> {
        let mut lines = Vec::new();
        match &s.failed_units {
            None => lines.push(("UNITS N/A".to_string(), Style::new())),
            Some(u) if u.is_empty() => lines.push(("ALL SYSTEMS NOMINAL".to_string(), Style::new())),
            Some(u) => lines.extend(u.iter().map(|n| (format!("FAILED {n}"), reverse()))),
        }
        match &s.containers {
            None => lines.push(("CONTAINERS N/A".to_string(), Style::new())),
            Some(c) if c.is_empty() => lines.push(("CONTAINERS NONE".to_string(), Style::new())),
            Some(c) => {
                lines.push(("CONTAINERS".to_string(), Style::new()));
                lines.extend(c.iter().map(|c| (format!("{} {}", c.name, c.status), Style::new())));
            }
        }
        lines
    }

    /// Content rows (excluding borders) a panel wants at this width.
    fn content_height(&self, kind: Kind, s: &Snapshot, inner_w: u16) -> u16 {
        match kind {
            Kind::System => 1,
            Kind::Cpu => 2 + (s.cpu_cores.len() as u16).div_ceil(Self::core_columns(inner_w)),
            Kind::Memory => 2,
            Kind::Disks => s.disks.len().clamp(1, MAX_DISKS) as u16,
            Kind::Network => 2,
            Kind::Processes => 1 + s.top.len() as u16,
            Kind::Services => Self::services_lines(s).len() as u16,
        }
    }

    fn title(kind: Kind, s: &Snapshot) -> String {
        match kind {
            Kind::System => format!("SYSTEM - {}", s.host),
            Kind::Cpu => "CPU".into(),
            Kind::Memory => "MEMORY".into(),
            Kind::Disks => "DISKS".into(),
            Kind::Network => "NETWORK".into(),
            Kind::Processes => "PROCESSES".into(),
            Kind::Services => "SERVICES".into(),
        }
    }

    fn draw_panel(&self, kind: Kind, s: &Snapshot, area: Rect, buf: &mut Buffer) {
        let block = panel(&Self::title(kind, s));
        let inner = block.inner(area);
        block.render(area, buf);
        let w = inner.width;
        let mut lines: Vec<Line<'static>> = Vec::new();
        match kind {
            Kind::System => lines.push(Line::from(format!(
                "UPTIME {}   LOAD {:.2} {:.2} {:.2}",
                human_uptime(s.uptime_secs),
                s.load[0],
                s.load[1],
                s.load[2]
            ))),
            Kind::Cpu => {
                lines.push(gauge_line("ALL", s.cpu_total, "", w));
                let cols = Self::core_columns(w);
                let col_w = w / cols;
                for row in s.cpu_cores.chunks(cols as usize).enumerate() {
                    let (r, cores) = row;
                    let text: String = cores
                        .iter()
                        .enumerate()
                        .map(|(c, pct)| {
                            let n = r * cols as usize + c;
                            let g = gauge_line(&format!("C{n:02}"), *pct, "", col_w.saturating_sub(1)).to_string();
                            format!("{g:<width$}", width = col_w as usize)
                        })
                        .collect();
                    lines.push(Line::from(text));
                }
                lines.push(Line::from(sparkline(&self.cpu.values().collect::<Vec<_>>(), 100.0, w)));
            }
            Kind::Memory => {
                let d = aligned_details(&[s.mem, s.swap]);
                lines.push(gauge_line("MEM", s.mem.percent(), &d[0], w));
                lines.push(if s.swap.total == 0 {
                    Line::from("SWAP  NONE")
                } else {
                    gauge_line("SWAP", s.swap.percent(), &d[1], w)
                });
            }
            Kind::Disks => {
                if s.disks.is_empty() {
                    lines.push(Line::from("NO DISKS"));
                }
                let disks: Vec<_> = s.disks.iter().take(MAX_DISKS).collect();
                let details = aligned_details(&disks.iter().map(|d| d.usage).collect::<Vec<_>>());
                for (d, detail) in disks.iter().zip(&details) {
                    lines.push(gauge_line(&d.mount.to_uppercase(), d.usage.percent(), detail, w));
                }
            }
            Kind::Network => {
                lines.push(Line::from(format!("RX {}   TX {}", human_rate(s.net.rx_bps), human_rate(s.net.tx_bps))));
                let hist: Vec<f32> = self.net.values().collect();
                let max = hist.iter().copied().fold(0.0, f32::max);
                lines.push(Line::from(sparkline(&hist, max, w)));
            }
            Kind::Processes => {
                lines.push(Line::from(format!("{:<7} {:<15} {:>6} {:>7}", "PID", "NAME", "CPU%", "MEM")));
                for p in &s.top {
                    lines.push(Line::from(format!(
                        "{:<7} {:<15} {:>6.1} {:>7}",
                        p.pid,
                        p.name,
                        p.cpu,
                        human_bytes(p.mem)
                    )));
                }
            }
            Kind::Services => {
                for (text, style) in Self::services_lines(s) {
                    lines.push(Line::styled(text, style));
                }
            }
        }
        for (i, line) in lines.iter().take(inner.height as usize).enumerate() {
            buf.set_line(inner.x, inner.y + i as u16, line, w);
        }
    }

    /// Stack `kinds` in `col`; the last one grows into what is left.
    fn draw_column(&self, kinds: &[Kind], s: &Snapshot, col: Rect, buf: &mut Buffer) {
        let mut y = col.y;
        let bottom = col.y + col.height;
        for (i, &kind) in kinds.iter().enumerate() {
            let want = self.content_height(kind, s, col.width.saturating_sub(2)) + 2;
            let last = i == kinds.len() - 1;
            let room = bottom.saturating_sub(y);
            if want > room && !(last && room >= 3) {
                break; // doesn't fit at its minimum: drop it and everything below
            }
            let h = if last { room } else { want };
            self.draw_panel(kind, s, Rect::new(col.x, y, col.width, h), buf);
            y += h;
        }
    }
}

impl Tab for UtilityTab {
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let Some(s) = &self.snap else {
            let block = panel("UTILITY");
            let inner = block.inner(area);
            block.render(area, buf);
            let mid = Rect { y: inner.y + inner.height / 2, height: 1.min(inner.height), ..inner };
            Paragraph::new("AWAITING TELEMETRY").alignment(Alignment::Center).render(mid, buf);
            return;
        };
        if area.width >= TWO_COLUMNS_FROM {
            let left_w = area.width / 2;
            let left = Rect { width: left_w, ..area };
            let right = Rect { x: area.x + left_w, width: area.width - left_w, ..area };
            self.draw_column(&[Kind::System, Kind::Cpu, Kind::Memory, Kind::Network], s, left, buf);
            self.draw_column(&[Kind::Disks, Kind::Processes, Kind::Services], s, right, buf);
        } else {
            let all = [Kind::System, Kind::Cpu, Kind::Memory, Kind::Disks, Kind::Network, Kind::Processes, Kind::Services];
            self.draw_column(&all, s, area, buf);
        }
    }

    fn handle_key(&mut self, _key: &Key) {}

    fn on_metrics(&mut self, s: &Snapshot) {
        self.on_snapshot(s.clone());
    }

    fn take_activity(&mut self) -> bool {
        std::mem::take(&mut self.activity)
    }
}

#[cfg(test)]
mod tests;
