//! Metrics for the machine MUTHUR runs on.

use super::probes::{parse_containers, parse_failed_units, run_probe};
use super::{Container, Disk, MetricsSource, NetRate, Proc, Snapshot, Usage};
use serde::Deserialize;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use sysinfo::{Disks, Networks, ProcessesToUpdate, System};

/// External probes are slow; refresh them at most this often.
const SLOW_PROBE_EVERY: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const CONTAINER_FORMAT: &str = "{{.Names}}\t{{.Status}}";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerRuntime {
    #[default]
    Auto,
    Docker,
    Podman,
    None,
}

/// Filesystem types that are never real storage.
pub fn default_exclude() -> Vec<String> {
    ["tmpfs", "devtmpfs", "efivarfs", "overlay", "squashfs", "proc", "sysfs", "cgroup2", "ramfs", "autofs"]
        .map(String::from)
        .to_vec()
}

pub struct LocalSource {
    sys: System,
    disks: Disks,
    networks: Networks,
    runtime: ContainerRuntime,
    exclude_fs: Vec<String>,
    last_sample: Option<Instant>,
    last_probe: Option<Instant>,
    failed_units: Option<Vec<String>>,
    containers: Option<Vec<Container>>,
}

fn containers_from(cmd: &str) -> Option<Vec<Container>> {
    run_probe(cmd, &["ps", "--format", CONTAINER_FORMAT], PROBE_TIMEOUT).map(|o| parse_containers(&o))
}

impl LocalSource {
    pub fn new(runtime: ContainerRuntime, exclude_fs: Vec<String>) -> anyhow::Result<Self> {
        Ok(Self {
            sys: System::new_all()?,
            disks: Disks::new_with_refreshed_list()?,
            networks: Networks::new_with_refreshed_list()?,
            runtime,
            exclude_fs,
            last_sample: None,
            last_probe: None,
            failed_units: None,
            containers: None,
        })
    }

    fn refresh_probes(&mut self) {
        if self.last_probe.is_some_and(|t| t.elapsed() < SLOW_PROBE_EVERY) {
            return;
        }
        self.last_probe = Some(Instant::now());
        self.failed_units = run_probe("systemctl", &["--failed", "--plain", "--no-legend"], PROBE_TIMEOUT)
            .map(|o| parse_failed_units(&o));
        self.containers = match self.runtime {
            ContainerRuntime::None => None,
            ContainerRuntime::Docker => containers_from("docker"),
            ContainerRuntime::Podman => containers_from("podman"),
            ContainerRuntime::Auto => containers_from("docker").or_else(|| containers_from("podman")),
        };
    }

    fn disks(&self) -> Vec<Disk> {
        let mut seen = HashSet::new();
        let mut out: Vec<Disk> = self
            .disks
            .iter()
            .filter(|d| d.total_space() > 0)
            .filter(|d| !self.exclude_fs.iter().any(|x| d.file_system() == x.as_str()))
            .filter(|d| seen.insert(d.name().to_owned()))
            .map(|d| Disk {
                mount: d.mount_point().to_string_lossy().into_owned(),
                usage: Usage { used: d.total_space() - d.available_space(), total: d.total_space() },
            })
            .collect();
        out.sort_by(|a, b| (a.mount != "/", &a.mount).cmp(&(b.mount != "/", &b.mount)));
        out
    }

    fn top(&self) -> Vec<Proc> {
        let mut procs: Vec<Proc> = self
            .sys
            .processes()
            .values()
            // On Linux sysinfo also lists every thread; keep whole processes only.
            .filter(|p| p.thread_kind().is_none())
            .map(|p| Proc {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().to_uppercase().chars().take(15).collect(),
                cpu: p.cpu_usage(),
                mem: p.memory(),
            })
            .collect();
        procs.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.mem.cmp(&a.mem)));
        procs.truncate(5);
        procs
    }
}

impl MetricsSource for LocalSource {
    fn snapshot(&mut self) -> anyhow::Result<Snapshot> {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.sys.refresh_processes(ProcessesToUpdate::All, true);
        self.disks.refresh(true);
        self.networks.refresh(true);
        let now = Instant::now();
        let secs = self.last_sample.map(|t| now.duration_since(t).as_secs_f64()).filter(|s| *s > 0.0);
        self.last_sample = Some(now);
        let (rx, tx) = self
            .networks
            .iter()
            .filter(|(name, _)| name.as_os_str() != "lo")
            .fold((0u64, 0u64), |(r, t), (_, d)| (r + d.received(), t + d.transmitted()));
        let net = match secs {
            Some(s) => NetRate { rx_bps: (rx as f64 / s) as u64, tx_bps: (tx as f64 / s) as u64 },
            None => NetRate::default(),
        };
        self.refresh_probes();
        let load = System::load_average()?;
        Ok(Snapshot {
            host: System::host_name().unwrap_or_default().to_uppercase(),
            uptime_secs: System::uptime()?,
            load: [load.one, load.five, load.fifteen],
            cpu_total: self.sys.global_cpu_usage(),
            cpu_cores: self.sys.cpus().iter().map(|c| c.usage()).collect(),
            mem: Usage { used: self.sys.used_memory(), total: self.sys.total_memory() },
            swap: Usage { used: self.sys.used_swap(), total: self.sys.total_swap() },
            disks: self.disks(),
            net,
            top: self.top(),
            failed_units: self.failed_units.clone(),
            containers: self.containers.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_has_basics() {
        let mut s = LocalSource::new(ContainerRuntime::None, default_exclude()).unwrap();
        let a = s.snapshot().unwrap();
        assert!(!a.host.is_empty() && a.host == a.host.to_uppercase());
        assert!(!a.cpu_cores.is_empty() && a.mem.total > 0 && a.uptime_secs > 0);
        assert!(a.disks.iter().any(|d| d.mount == "/"), "{:?}", a.disks);
        assert_eq!(a.disks[0].mount, "/");
        assert!(a.top.len() <= 5);
        assert!(a.top.iter().all(|p| p.name.len() <= 15 && p.name == p.name.to_uppercase()));
        assert_eq!(a.containers, None);
    }

    #[test]
    fn excluded_filesystems_are_dropped() {
        let mut s = LocalSource::new(ContainerRuntime::None, default_exclude()).unwrap();
        let a = s.snapshot().unwrap();
        assert!(a.disks.iter().all(|d| !d.mount.starts_with("/proc") && !d.mount.starts_with("/sys")), "{:?}", a.disks);
    }

    #[test]
    fn top_lists_processes_not_threads() {
        // A busy multi-threaded process so threads would crowd the list.
        let busy = std::thread::spawn(|| {
            let t0 = std::time::Instant::now();
            let workers: Vec<_> = (0..4)
                .map(|_| std::thread::spawn(move || while t0.elapsed().as_millis() < 600 {}))
                .collect();
            workers.into_iter().for_each(|w| w.join().unwrap());
        });
        let mut s = LocalSource::new(ContainerRuntime::None, default_exclude()).unwrap();
        s.snapshot().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let a = s.snapshot().unwrap();
        busy.join().unwrap();
        let listed: HashSet<u32> = std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
            .collect();
        for p in &a.top {
            assert!(listed.contains(&p.pid), "{p:?} is a thread, not a process");
        }
    }

    #[test]
    fn second_snapshot_has_rates() {
        let mut s = LocalSource::new(ContainerRuntime::None, default_exclude()).unwrap();
        s.snapshot().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let b = s.snapshot().unwrap();
        assert!(b.net.rx_bps < u64::MAX / 2 && b.cpu_total >= 0.0);
    }
}
