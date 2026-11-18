//! Command runner: argv only (never a shell), bounded by a timeout.

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
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

fn drain<R: Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

impl Runner for SystemRunner {
    fn run(&self, argv: &[String], timeout: Duration) -> RunOutput {
        let Some((cmd, args)) = argv.split_first() else {
            return RunOutput::NotFound;
        };
        let mut child = match Command::new(cmd)
            .args(args)
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
        let (out_t, err_t) = (drain(out), drain(err));
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let stdout = String::from_utf8_lossy(&out_t.join().unwrap_or_default()).into_owned();
        let stderr = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
        match status {
            // Death by signal has no exit code; report -1.
            Some(s) => RunOutput::Exited {
                code: s.code().unwrap_or(-1),
                stdout,
                stderr,
            },
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
}
