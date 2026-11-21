//! External commands for metrics sysinfo can't provide: failed systemd units
//! and containers. Every probe has a timeout and degrades to `None`.

use super::Container;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Run `cmd args`, returning stdout if it exits 0 within `timeout`.
/// Missing binaries, failures and timeouts are all `None`.
pub fn run_probe(cmd: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(cmd)
        .args(args)
        // Own process group, so a timeout can kill wrappers' children too
        // (they'd otherwise hold stdout open and stall the reader).
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drain stdout concurrently so a chatty command can't fill the pipe and stall.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().ok()?;
    match status {
        Some(s) if s.success() => Some(String::from_utf8_lossy(&out).into_owned()),
        _ => {
            tracing::debug!(cmd, "probe failed or timed out");
            None
        }
    }
}

/// `systemctl --failed --plain --no-legend` → unit names, uppercased,
/// `.service` dropped.
pub fn parse_failed_units(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(|u| u.strip_suffix(".service").unwrap_or(u).to_uppercase())
        .collect()
}

/// `ps --format '{{.ID}}\t{{.Names}}\t{{.Status}}'` → containers. Lines
/// without all three fields cannot be acted on and are skipped.
pub fn parse_containers(out: &str) -> Vec<Container> {
    out.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let [id, name, detail] = f[..] else {
                return None;
            };
            let detail = detail.trim();
            Some(Container {
                id: id.trim().to_string(),
                name: name.trim().to_string(),
                status: detail.split_whitespace().next().unwrap_or("?").to_uppercase(),
                detail: detail.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn parses_failed_units() {
        assert_eq!(
            parse_failed_units("autorandr.service loaded failed failed autorandr hook\nfoo.mount loaded failed failed x\n\n"),
            vec!["AUTORANDR", "FOO.MOUNT"]
        );
    }

    #[test]
    fn parses_containers() {
        let out = "a1b2\tweb\tUp 3 hours\nc3d4\tMyDB\tExited (137) 2 hours ago\ne5\told\tCreated\nshort\tline\n\n";
        assert_eq!(
            parse_containers(out),
            vec![
                Container {
                    id: "a1b2".into(),
                    name: "web".into(),
                    status: "UP".into(),
                    detail: "Up 3 hours".into()
                },
                Container {
                    id: "c3d4".into(),
                    name: "MyDB".into(),
                    status: "EXITED".into(),
                    detail: "Exited (137) 2 hours ago".into()
                },
                Container {
                    id: "e5".into(),
                    name: "old".into(),
                    status: "CREATED".into(),
                    detail: "Created".into()
                },
            ]
        );
    }

    #[test]
    fn probe_returns_stdout() {
        assert_eq!(run_probe("sh", &["-c", "echo hi"], Duration::from_secs(2)).as_deref(), Some("hi\n"));
    }

    #[test]
    fn missing_binary_is_none() {
        assert_eq!(run_probe("definitely-not-a-binary-muthur", &[], Duration::from_secs(1)), None);
    }

    #[test]
    fn failure_is_none() {
        assert_eq!(run_probe("sh", &["-c", "exit 1"], Duration::from_secs(1)), None);
    }

    #[test]
    fn timeout_is_none() {
        let t = Instant::now();
        assert_eq!(run_probe("sleep", &["5"], Duration::from_millis(200)), None);
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn timeout_kills_grandchildren_holding_stdout() {
        let t = Instant::now();
        assert_eq!(run_probe("sh", &["-c", "sleep 5 & wait"], Duration::from_millis(200)), None);
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    }

    #[test]
    fn large_output_does_not_deadlock() {
        let out = run_probe("sh", &["-c", "seq 1 100000"], Duration::from_secs(5)).expect("output");
        assert!(out.ends_with("100000\n"));
    }
}
