//! Command runner: argv only (never a shell), bounded by a timeout.

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum RunOutput {
    Exited {
        code: i32,
        stdout: String,
        stderr: String,
    },
    NotFound,
    TimedOut,
}

pub trait Runner: Send + Sync {
    fn run(&self, argv: &[String], timeout: Duration) -> RunOutput;

    /// `run` with stderr folded into stdout. This default loses the
    /// interleaving; `SystemRunner` keeps it by sharing one pipe.
    fn run_merged(&self, argv: &[String], timeout: Duration) -> RunOutput {
        match self.run(argv, timeout) {
            RunOutput::Exited { code, stdout, stderr } => RunOutput::Exited { code, stdout: stdout + &stderr, stderr: String::new() },
            other => other,
        }
    }
}

pub struct SystemRunner;

fn drain<R: Read + Send + 'static>(mut r: R) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

const GRACE: Duration = Duration::from_millis(200);

impl Runner for SystemRunner {
    fn run(&self, argv: &[String], timeout: Duration) -> RunOutput {
        spawn_and_wait(argv, timeout, false)
    }

    fn run_merged(&self, argv: &[String], timeout: Duration) -> RunOutput {
        spawn_and_wait(argv, timeout, true)
    }
}

fn spawn_failed(cmd: &str, e: std::io::Error) -> RunOutput {
    if e.kind() != std::io::ErrorKind::NotFound {
        tracing::warn!(cmd, "tool spawn failed: {e}");
    }
    RunOutput::NotFound
}

/// `merged`: stdout and stderr share one pipe, so their order is kept and
/// everything comes back as `stdout`.
fn spawn_and_wait(argv: &[String], timeout: Duration, merged: bool) -> RunOutput {
    let Some((cmd, args)) = argv.split_first() else {
        return RunOutput::NotFound;
    };
    let mut command = Command::new(cmd);
    command.args(args).env("LC_ALL", "C").process_group(0).stdin(Stdio::null());
    let (mut child, out_rx, err_rx) = if merged {
        let (reader, writer) = match std::io::pipe() {
            Ok(p) => p,
            Err(e) => return spawn_failed(cmd, e),
        };
        let writer2 = match writer.try_clone() {
            Ok(w) => w,
            Err(e) => return spawn_failed(cmd, e),
        };
        command.stdout(Stdio::from(writer)).stderr(Stdio::from(writer2));
        let spawned = command.spawn();
        // The Command holds the parent's copies of the write end: drop them,
        // or the reader never sees EOF.
        drop(command);
        let child = match spawned {
            Ok(c) => c,
            Err(e) => return spawn_failed(cmd, e),
        };
        let (tx, err_rx) = mpsc::channel();
        let _ = tx.send(Vec::new());
        (child, drain(reader), err_rx)
    } else {
        let mut child = match command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => return spawn_failed(cmd, e),
        };
        let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return RunOutput::NotFound;
        };
        (child, drain(out), drain(err))
    };
    let deadline = Instant::now() + timeout;
    let pgid = Pid::from_raw(child.id() as i32);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = killpg(pgid, Signal::SIGKILL);
                let _ = child.wait();
                break None;
            }
        }
    };
    // Stragglers in the group (grandchildren holding the pipes) die too.
    let _ = killpg(pgid, Signal::SIGKILL);
    let wait = deadline.saturating_duration_since(Instant::now()) + GRACE;
    let end = Instant::now() + wait;
    let collect = |rx: &mpsc::Receiver<Vec<u8>>| rx.recv_timeout(end.saturating_duration_since(Instant::now()));
    let (o, e) = (collect(&out_rx), collect(&err_rx));
    match status {
        Some(s) => {
            let text = |r: Result<Vec<u8>, _>| String::from_utf8_lossy(&r.unwrap_or_default()).into_owned();
            RunOutput::Exited { code: s.code().unwrap_or(-1), stdout: text(o), stderr: text(e) }
        }
        None => RunOutput::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn merged_keeps_order() {
        assert_eq!(
            SystemRunner.run_merged(&v(&["sh", "-c", "echo a; echo b >&2; echo c"]), Duration::from_secs(2)),
            RunOutput::Exited { code: 0, stdout: "a\nb\nc\n".into(), stderr: String::new() }
        );
    }

    #[test]
    fn merged_times_out_and_kills() {
        let t = Instant::now();
        assert_eq!(SystemRunner.run_merged(&v(&["sleep", "5"]), Duration::from_millis(200)), RunOutput::TimedOut);
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn merged_not_found() {
        assert_eq!(SystemRunner.run_merged(&v(&["definitely-not-a-binary-muthur"]), Duration::from_secs(1)), RunOutput::NotFound);
    }

    #[test]
    fn system_runner_real_commands() {
        let r = SystemRunner;
        assert_eq!(
            r.run(
                &v(&["sh", "-c", "echo out; echo err >&2; exit 3"]),
                Duration::from_secs(2)
            ),
            RunOutput::Exited {
                code: 3,
                stdout: "out\n".into(),
                stderr: "err\n".into()
            }
        );
        assert_eq!(
            r.run(
                &v(&["definitely-not-a-binary-muthur"]),
                Duration::from_secs(1)
            ),
            RunOutput::NotFound
        );
        let t = Instant::now();
        assert_eq!(
            r.run(&v(&["sleep", "5"]), Duration::from_millis(200)),
            RunOutput::TimedOut
        );
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn grandchild_holding_pipe_does_not_block() {
        let t = Instant::now();
        let _ = SystemRunner.run(&v(&["sh", "-c", "sleep 5 & echo hi"]), Duration::from_millis(300));
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn locale_is_forced_to_c() {
        assert_eq!(
            SystemRunner.run(&v(&["sh", "-c", "echo $LC_ALL"]), Duration::from_secs(2)),
            RunOutput::Exited { code: 0, stdout: "C\n".into(), stderr: String::new() }
        );
    }
}
