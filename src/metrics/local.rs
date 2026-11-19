//! Metrics for the machine MUTHUR runs on.

use super::probes::{parse_containers, parse_failed_units, run_probe};
use super::{Container, Disk, MetricsSource, NetRate, Proc, Snapshot, Usage};
use serde::Deserialize;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use sysinfo::{Disks, Networks, ProcessesToUpdate, System, ThreadKind};

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
    // Network and FUSE filesystems are excluded too: a hung mount would block
    // statvfs and freeze the whole collector.
    [
        "tmpfs", "devtmpfs", "efivarfs", "overlay", "squashfs", "proc", "sysfs", "cgroup2", "ramfs", "autofs",
        "fuse", "9p", "ceph", "glusterfs", "nfs", "nfs4", "cifs", "smb3",
    ]
        .map(String::from)
        .to_vec()
}

/// `fs` matches an entry exactly or as a subtype (`fuse` excludes `fuse.sshfs`).
pub fn is_excluded(fs: &str, exclude: &[String]) -> bool {
    exclude.iter().any(|x| fs == x || fs.strip_prefix(x.as_str()).is_some_and(|rest| rest.starts_with('.')))
}

/// Interfaces to count: physical ones only, so traffic isn't counted again on
/// veth/bridge/bond devices. Falls back to everything but `lo` when no
/// interface looks physical (containers, unusual VMs).
pub fn select_interfaces<'a>(names: &[&'a str], physical: impl Fn(&str) -> bool) -> Vec<&'a str> {
    let named: Vec<&str> = names.iter().copied().filter(|n| *n != "lo").collect();
    let phys: Vec<&str> = named.iter().copied().filter(|n| physical(n)).collect();
    if phys.is_empty() { named } else { phys }
}

fn is_physical(iface: &str) -> bool {
    std::path::Path::new("/sys/class/net").join(iface).join("device").exists()
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

/// `--all`: stopped and crashed containers are what CARGO must not hide.
const CONTAINER_ARGS: [&str; 4] = ["ps", "--all", "--format", CONTAINER_FORMAT];

fn containers_from(cmd: &str) -> Option<Vec<Container>> {
    run_probe(cmd, &CONTAINER_ARGS, PROBE_TIMEOUT).map(|o| parse_containers(&o))
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
            .filter(|d| !is_excluded(&d.file_system().to_string_lossy(), &self.exclude_fs))
            .filter(|d| seen.insert(d.name().to_owned()))
            .map(|d| Disk {
                mount: d.mount_point().to_string_lossy().into_owned(),
                usage: Usage { used: d.total_space() - d.available_space(), total: d.total_space() },
            })
            .collect();
        out.sort_by(|a, b| (a.mount != "/", &a.mount).cmp(&(b.mount != "/", &b.mount)));
        out
    }

    /// Every process eligible for the top list: whole processes (sysinfo also
    /// lists each userland thread on Linux) plus kernel threads, which are
    /// often the real consumers on a NAS (md raid, kswapd, kworker).
    pub fn candidates(&self) -> Vec<Proc> {
        self.sys
            .processes()
            .values()
            .filter(|p| p.thread_kind() != Some(ThreadKind::Userland))
            .map(|p| Proc {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().to_uppercase().chars().take(15).collect(),
                cpu: p.cpu_usage(),
                mem: p.memory(),
            })
            .collect()
    }

    fn top(&self) -> Vec<Proc> {
        let mut procs = self.candidates();
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
        let names: Vec<String> = self.networks.keys().map(|n| n.to_string_lossy().into_owned()).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let counted: HashSet<&str> = select_interfaces(&refs, is_physical).into_iter().collect();
        let (rx, tx) = self
            .networks
            .iter()
            .filter(|(name, _)| counted.contains(name.to_string_lossy().as_ref()))
            .fold((0u64, 0u64), |(r, t), (_, d)| (r + d.received(), t + d.transmitted()));
        let net = match secs {
            Some(s) => NetRate { rx_bps: (rx as f64 / s) as u64, tx_bps: (tx as f64 / s) as u64 },
            None => NetRate::default(),
        };
        self.refresh_probes();
        // Degrade field by field: a sandbox without /proc/loadavg still gets the rest.
        let load = System::load_average().unwrap_or_default();
        Ok(Snapshot {
            host: System::host_name().unwrap_or_default().to_uppercase(),
            uptime_secs: System::uptime().unwrap_or_default(),
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
    fn container_probe_includes_stopped() {
        assert!(CONTAINER_ARGS.contains(&"--all"), "{CONTAINER_ARGS:?}");
    }

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
    fn kernel_threads_are_candidates() {
        let mut s = LocalSource::new(ContainerRuntime::None, default_exclude()).unwrap();
        s.snapshot().unwrap();
        assert!(s.candidates().iter().any(|p| p.pid == 2), "kthreadd (pid 2) missing");
    }

    #[test]
    fn physical_interfaces_only() {
        let names = ["lo", "eth0", "docker0", "veth12", "br-1", "bond0", "enp3s0"];
        let phys = |n: &str| n == "eth0" || n == "enp3s0";
        assert_eq!(select_interfaces(&names, phys), vec!["eth0", "enp3s0"]);
        assert_eq!(select_interfaces(&["lo", "veth0", "tun0"], |_| false), vec!["veth0", "tun0"]);
    }

    #[test]
    fn fuse_and_network_filesystems_excluded() {
        let ex = default_exclude();
        for fs in ["fuse.sshfs", "fuse.rclone", "fuseblk_not", "9p", "ceph", "glusterfs", "tmpfs"] {
            let want = fs != "fuseblk_not";
            assert_eq!(is_excluded(fs, &ex), want, "{fs}");
        }
        assert!(!is_excluded("xfs", &ex) && !is_excluded("ext4", &ex));
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
