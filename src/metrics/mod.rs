//! System metrics: the data model shared by the Utility tab and (M5) the
//! MU/TH/UR tools, plus formatting helpers.

pub mod local;
pub mod probes;

use std::collections::VecDeque;

/// One reading of the whole system.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Snapshot {
    pub host: String,
    pub uptime_secs: u64,
    pub load: [f64; 3],
    /// Percent, 0–100 (may exceed 100 transiently; consumers clamp).
    pub cpu_total: f32,
    pub cpu_cores: Vec<f32>,
    pub mem: Usage,
    pub swap: Usage,
    pub disks: Vec<Disk>,
    pub net: NetRate,
    pub top: Vec<Proc>,
    /// `None` when unavailable (no systemctl).
    pub failed_units: Option<Vec<String>>,
    /// `None` when no container runtime answers.
    pub containers: Option<Vec<Container>>,
    /// The runtime command that produced `containers` (`docker`/`podman`).
    pub container_runtime: Option<String>,
}

/// Bytes used of bytes total.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Usage {
    pub used: u64,
    pub total: u64,
}

impl Usage {
    pub fn percent(&self) -> f32 {
        if self.total == 0 { 0.0 } else { self.used as f32 * 100.0 / self.total as f32 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Disk {
    pub mount: String,
    pub usage: Usage,
}

/// Bytes per second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct NetRate {
    pub rx_bps: u64,
    pub tx_bps: u64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
    pub cpu: f32,
    pub mem: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Container {
    /// Short ID: what commands target.
    pub id: String,
    /// As the runtime reports it; display sites upper-case it.
    pub name: String,
    /// First word of `detail`, upper-cased (`UP`, `EXITED`, …).
    pub status: String,
    /// Full status text, e.g. `Exited (137) 2 hours ago`.
    pub detail: String,
}

pub trait MetricsSource {
    fn snapshot(&mut self) -> anyhow::Result<Snapshot>;
}

/// `1536` → `1.5K`: binary units, one decimal, uppercase.
pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 6] = ["K", "M", "G", "T", "P", "E"];
    if b < 1024 {
        return format!("{b}B");
    }
    let mut v = b as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1}{}", UNITS[unit])
}

pub fn human_rate(bps: u64) -> String {
    format!("{}/S", human_bytes(bps))
}

/// `12D 04:13`, or `04:13` under a day.
pub fn human_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    if d > 0 { format!("{d}D {h:02}:{m:02}") } else { format!("{h:02}:{m:02}") }
}

/// A fixed-size window of recent samples, oldest first.
#[derive(Clone, Debug)]
pub struct History {
    cap: usize,
    values: VecDeque<f32>,
}

impl History {
    pub fn new(cap: usize) -> Self {
        Self { cap, values: VecDeque::with_capacity(cap) }
    }

    pub fn push(&mut self, v: f32) {
        if self.values.len() == self.cap {
            self.values.pop_front();
        }
        self.values.push_back(v);
    }

    pub fn values(&self) -> impl Iterator<Item = f32> + '_ {
        self.values.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes() {
        assert_eq!(human_bytes(0), "0B");
        assert_eq!(human_bytes(999), "999B");
        assert_eq!(human_bytes(1536), "1.5K");
        assert_eq!(human_bytes(16_106_127_360), "15.0G");
    }

    #[test]
    fn rate() {
        assert_eq!(human_rate(2048), "2.0K/S");
    }

    #[test]
    fn uptime() {
        assert_eq!(human_uptime(3600 * 4 + 60 * 13), "04:13");
        assert_eq!(human_uptime(86400 * 12 + 3600 * 4 + 60 * 13), "12D 04:13");
    }

    #[test]
    fn percent_handles_zero_total() {
        assert_eq!(Usage { used: 5, total: 0 }.percent(), 0.0);
        assert_eq!(Usage { used: 1, total: 4 }.percent(), 25.0);
    }

    #[test]
    fn history_caps() {
        let mut h = History::new(3);
        for v in 0..5 {
            h.push(v as f32);
        }
        assert_eq!(h.values().collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
    }
}
