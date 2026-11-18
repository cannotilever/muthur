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
        let Some((cmd, args)) = argv.split_first() else {
            return RunOutput::NotFound;
        };
        let mut child = match Command::new(cmd)
            .args(args)
            .env("LC_ALL", "C")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(cmd, "tool spawn failed: {e}");
                }
                return RunOutput::NotFound;
            }
        };
        let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return RunOutput::NotFound;
        };
        let (out_rx, err_rx) = (drain(out), drain(err));
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
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
