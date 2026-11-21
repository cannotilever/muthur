//! Command runner: argv only (never a shell), bounded by a timeout.

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum RunOutput {
    Exited { code: i32, stdout: String, stderr: String },
    NotFound,
    TimedOut,
    Cancelled,
}

/// How a child is stopped on timeout or cancel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kill {
    /// SIGKILL the process group.
    Now,
    /// SIGTERM the group, wait this long for exit, then SIGKILL.
    Graceful(Duration),
}

pub struct RunSpec<'a> {
    pub argv: &'a [String],
    pub timeout: Duration,
    pub merged: bool,
    pub stdin: Option<&'a [u8]>,
    pub kill: Kill,
    pub cancel: Option<&'a AtomicBool>,
    /// Bound each captured stream to about this many bytes (head + tail).
    pub keep: Option<usize>,
}

impl<'a> RunSpec<'a> {
    pub fn new(argv: &'a [String], timeout: Duration) -> Self {
        Self {
            argv,
            timeout,
            merged: false,
            stdin: None,
            kill: Kill::Now,
            cancel: None,
            keep: None,
        }
    }
}

pub trait Runner: Send + Sync {
    fn run(&self, argv: &[String], timeout: Duration) -> RunOutput;

    /// `run` with stderr folded into stdout. This default loses the
    /// interleaving; `SystemRunner` keeps it by sharing one pipe.
    fn run_merged(&self, argv: &[String], timeout: Duration) -> RunOutput {
        match self.run(argv, timeout) {
            RunOutput::Exited { code, stdout, stderr } => RunOutput::Exited {
                code,
                stdout: stdout + &stderr,
                stderr: String::new(),
            },
            other => other,
        }
    }

    /// Full-featured run. The default ignores stdin, kill, cancel and keep.
    fn run_spec(&self, spec: &RunSpec) -> RunOutput {
        if spec.merged {
            self.run_merged(spec.argv, spec.timeout)
        } else {
            self.run(spec.argv, spec.timeout)
        }
    }
}

pub struct SystemRunner;

/// Bytes captured from one stream. With `keep`, hold only the first and last
/// `keep / 2` bytes, cut on UTF-8 boundaries, with the `head_tail` marker for
/// the rest.
struct Capture {
    keep: Option<usize>,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}

impl Capture {
    fn new(keep: Option<usize>) -> Self {
        Self {
            keep,
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
        }
    }

    fn push(&mut self, mut data: &[u8]) {
        self.total += data.len();
        let Some(keep) = self.keep else {
            self.head.extend_from_slice(data);
            return;
        };
        let half = keep / 2; // tail size
        let head_cap = keep - half;
        let room = head_cap - self.head.len();
        let (h, rest) = data.split_at(room.min(data.len()));
        self.head.extend_from_slice(h);
        data = rest;
        if data.len() >= half {
            self.tail.clear();
            data = &data[data.len() - half..];
        } else {
            let over = (self.tail.len() + data.len()).saturating_sub(half);
            self.tail.drain(..over);
        }
        self.tail.extend(data);
    }

    /// Everything captured so far.
    fn bytes(&mut self) -> Vec<u8> {
        let tail = self.tail.make_contiguous();
        if self.keep.is_none_or(|k| self.total <= k) {
            let mut all = self.head.clone();
            all.extend_from_slice(tail);
            return all;
        }
        // Trim to char boundaries; trimmed bytes count as dropped.
        let hend = complete_prefix(&self.head);
        let mut tstart = 0;
        while tstart < tail.len() && !is_boundary(tail, tstart) {
            tstart += 1;
        }
        let dropped = self.total - hend - (tail.len() - tstart);
        let mut out = self.head[..hend].to_vec();
        out.extend_from_slice(crate::ops::omitted(dropped).as_bytes());
        out.extend_from_slice(&tail[tstart..]);
        out
    }
}

/// A stream being read on its own thread. What it has read so far stays
/// readable even if the stream never reaches EOF.
struct Drain {
    cap: Arc<Mutex<Capture>>,
    done: mpsc::Receiver<()>,
}

impl Drain {
    /// Wait until `end` for EOF, then return what was read.
    fn collect(&self, end: Instant) -> Vec<u8> {
        let _ = self.done.recv_timeout(end.saturating_duration_since(Instant::now()));
        self.cap.lock().unwrap_or_else(|e| e.into_inner()).bytes()
    }
}

fn drain<R: Read + Send + 'static>(mut r: R, keep: Option<usize>) -> Drain {
    let cap = Arc::new(Mutex::new(Capture::new(keep)));
    let (tx, done) = mpsc::channel();
    let c = cap.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => c.lock().unwrap_or_else(|e| e.into_inner()).push(&chunk[..n]),
            }
        }
        let _ = tx.send(());
    });
    Drain { cap, done }
}

#[cfg(test)]
fn drain_bounded(r: &mut impl Read, keep: usize) -> Vec<u8> {
    let mut c = Capture::new(Some(keep));
    let mut buf = Vec::new();
    let _ = r.read_to_end(&mut buf);
    for piece in buf.chunks(8192) {
        c.push(piece);
    }
    c.bytes()
}

/// Length of `b` without a trailing, cut-off multi-byte sequence.
fn complete_prefix(b: &[u8]) -> usize {
    for back in 1..=3.min(b.len()) {
        let i = b.len() - back;
        let need = match b[i] {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            0x80..=0xBF => continue,
            _ => return b.len(),
        };
        return if need > back { i } else { b.len() };
    }
    b.len()
}

fn is_boundary(b: &[u8], i: usize) -> bool {
    i >= b.len() || (b[i] & 0xC0) != 0x80
}

/// How long to wait for the pipes to close after the child exits.
const AFTER_EXIT: Duration = Duration::from_secs(1);

impl Runner for SystemRunner {
    fn run(&self, argv: &[String], timeout: Duration) -> RunOutput {
        self.run_spec(&RunSpec::new(argv, timeout))
    }

    fn run_merged(&self, argv: &[String], timeout: Duration) -> RunOutput {
        let mut spec = RunSpec::new(argv, timeout);
        spec.merged = true;
        self.run_spec(&spec)
    }

    fn run_spec(&self, spec: &RunSpec) -> RunOutput {
        spawn_and_wait(spec)
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
fn spawn_and_wait(spec: &RunSpec) -> RunOutput {
    let (timeout, merged, keep) = (spec.timeout, spec.merged, spec.keep);
    let Some((cmd, args)) = spec.argv.split_first() else {
        return RunOutput::NotFound;
    };
    let mut command = Command::new(cmd);
    command.args(args).env("LC_ALL", "C");
    // A new session, not just a process group: the child gets no controlling
    // terminal, so it cannot open /dev/tty to reach the operator's serial
    // line. The pgid still equals the pid, so `killpg` works as before.
    // SAFETY: setsid is async-signal-safe and touches no parent state.
    unsafe {
        command.pre_exec(|| nix::unistd::setsid().map(drop).map_err(std::io::Error::from));
    }
    command.stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() });
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
        (child, drain(reader, keep), drain(std::io::empty(), None))
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
        (child, drain(out, keep), drain(err, keep))
    };
    let deadline = Instant::now() + timeout;
    let pgid = Pid::from_raw(child.id() as i32);
    let sin = child.stdin.take();
    let (status, cancelled) = std::thread::scope(|sc| {
        // The write runs off-thread so a child that never reads stdin cannot
        // stall the timeout or cancel. Killing the child breaks the pipe.
        if let (Some(bytes), Some(mut sin)) = (spec.stdin, sin) {
            sc.spawn(move || {
                let _ = sin.write_all(bytes);
            });
        }
        let mut cancelled = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) => {
                    cancelled = spec.cancel.is_some_and(|c| c.load(Ordering::SeqCst));
                    if !cancelled && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                }
                Err(_) => {}
            }
            let mut reaped = false;
            if let Kill::Graceful(g) = spec.kill {
                let _ = killpg(pgid, Signal::SIGTERM);
                let end = Instant::now() + g;
                while Instant::now() < end && !reaped {
                    reaped = matches!(child.try_wait(), Ok(Some(_)));
                    if !reaped {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            // Never signal a reaped child's pgid: it may have been recycled.
            if !reaped {
                let _ = killpg(pgid, Signal::SIGKILL);
                let _ = child.wait();
            }
            break None;
        };
        // Stragglers in the group (grandchildren holding the pipes) die too,
        // which also closes any stdin pipe the writer is blocked on.
        let _ = killpg(pgid, Signal::SIGKILL);
        (status, cancelled)
    });
    match status {
        Some(s) => {
            // The group is dead, so the pipes close at once unless a process
            // outside our reach (another user, another session) still holds
            // them. Take what arrived rather than waiting out the timeout.
            let end = Instant::now() + AFTER_EXIT;
            let text = |d: &Drain| String::from_utf8_lossy(&d.collect(end)).into_owned();
            RunOutput::Exited {
                code: s.code().unwrap_or(-1),
                stdout: text(&out_rx),
                stderr: text(&err_rx),
            }
        }
        None if cancelled => RunOutput::Cancelled,
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
            RunOutput::Exited {
                code: 0,
                stdout: "a\nb\nc\n".into(),
                stderr: String::new()
            }
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
        assert_eq!(
            SystemRunner.run_merged(&v(&["definitely-not-a-binary-muthur"]), Duration::from_secs(1)),
            RunOutput::NotFound
        );
    }

    #[test]
    fn system_runner_real_commands() {
        let r = SystemRunner;
        assert_eq!(
            r.run(&v(&["sh", "-c", "echo out; echo err >&2; exit 3"]), Duration::from_secs(2)),
            RunOutput::Exited {
                code: 3,
                stdout: "out\n".into(),
                stderr: "err\n".into()
            }
        );
        assert_eq!(r.run(&v(&["definitely-not-a-binary-muthur"]), Duration::from_secs(1)), RunOutput::NotFound);
        let t = Instant::now();
        assert_eq!(r.run(&v(&["sleep", "5"]), Duration::from_millis(200)), RunOutput::TimedOut);
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
            RunOutput::Exited {
                code: 0,
                stdout: "C\n".into(),
                stderr: String::new()
            }
        );
    }

    #[test]
    fn stdin_bytes_reach_child_then_eof() {
        let a = v(&["sh", "-c", "read x; echo got:$x; cat; echo end"]);
        let mut s = RunSpec::new(&a, Duration::from_secs(2));
        s.stdin = Some(b"pw\n");
        s.merged = true;
        assert_eq!(
            SystemRunner.run_spec(&s),
            RunOutput::Exited {
                code: 0,
                stdout: "got:pw\nend\n".into(),
                stderr: String::new()
            }
        );
    }

    #[test]
    fn cancel_flag_stops_and_reports_cancelled() {
        let a = v(&["sleep", "5"]);
        let flag = AtomicBool::new(false);
        let mut s = RunSpec::new(&a, Duration::from_secs(10));
        s.cancel = Some(&flag);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                flag.store(true, Ordering::SeqCst)
            });
            let t = Instant::now();
            assert_eq!(SystemRunner.run_spec(&s), RunOutput::Cancelled);
            assert!(t.elapsed() < Duration::from_secs(2));
        });
    }

    #[test]
    fn graceful_kill_sends_term_first() {
        // TERM-ignoring child: SIGKILL only after timeout + grace.
        let a = v(&["sh", "-c", "trap '' TERM; sleep 30"]);
        let mut s = RunSpec::new(&a, Duration::from_millis(100));
        s.kill = Kill::Graceful(Duration::from_millis(500));
        let t = Instant::now();
        assert_eq!(SystemRunner.run_spec(&s), RunOutput::TimedOut);
        assert!(t.elapsed() >= Duration::from_millis(550) && t.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn graceful_kill_ends_early_when_child_obeys_term() {
        let a = v(&["sleep", "30"]);
        let mut s = RunSpec::new(&a, Duration::from_millis(100));
        s.kill = Kill::Graceful(Duration::from_secs(5));
        let t = Instant::now();
        assert_eq!(SystemRunner.run_spec(&s), RunOutput::TimedOut);
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn keep_bounds_memory() {
        let a = v(&["sh", "-c", "head -c 3000000 /dev/zero | tr '\\0' a; echo; echo END"]);
        let mut s = RunSpec::new(&a, Duration::from_secs(10));
        s.merged = true;
        s.keep = Some(65536);
        let RunOutput::Exited { stdout, .. } = SystemRunner.run_spec(&s) else {
            panic!()
        };
        assert!(stdout.len() <= 65536 + 64 && stdout.ends_with("END\n") && stdout.contains("BYTES OMITTED"));
    }

    #[test]
    fn keep_counts_dropped_bytes_and_respects_utf8() {
        let mut data = "€".repeat(100).into_bytes(); // 300 bytes
        let out = drain_bounded(&mut data.as_slice(), 50);
        let s = String::from_utf8(out).expect("valid utf-8");
        let kept: usize = s.chars().filter(|c| *c == '€').count() * 3;
        let n: usize = s.split("[… ").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
        assert_eq!(kept + n, 300, "{s}");
        data.truncate(10);
        assert_eq!(drain_bounded(&mut data.as_slice(), 50), data);
    }

    #[test]
    fn stdin_write_cannot_block_timeout() {
        let a = v(&["sh", "-c", "sleep 5"]);
        let big = vec![b'x'; 200_000];
        let mut s = RunSpec::new(&a, Duration::from_millis(300));
        s.stdin = Some(&big);
        let t = Instant::now();
        assert_eq!(SystemRunner.run_spec(&s), RunOutput::TimedOut);
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn child_has_no_controlling_terminal() {
        // /proc/PID/stat fields 6 and 7: session id and tty_nr.
        let a = v(&["sh", "-c", "read -r p c st pp pg sid tty rest < /proc/$$/stat; echo $sid $tty; echo $$"]);
        let RunOutput::Exited { stdout, .. } = SystemRunner.run(&a, Duration::from_secs(2)) else {
            panic!()
        };
        let l: Vec<&str> = stdout.lines().collect();
        assert_eq!(l[0], format!("{} 0", l[1]), "child must lead its own session with no tty");
    }

    #[test]
    fn pipe_held_by_unkillable_process_keeps_output() {
        // A setsid'd grandchild leaves our group, so killpg misses it and it
        // keeps the pipe open: the exit must still come back promptly with
        // what was written.
        let a = v(&["sh", "-c", "echo hi; setsid sleep 5 & exit 0"]);
        let mut s = RunSpec::new(&a, Duration::from_secs(20));
        s.merged = true;
        s.keep = Some(65536);
        let t = Instant::now();
        assert_eq!(
            SystemRunner.run_spec(&s),
            RunOutput::Exited {
                code: 0,
                stdout: "hi\n".into(),
                stderr: String::new()
            }
        );
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    }

    #[test]
    fn keep_odd_limit_loses_no_byte() {
        let data = b"abcdefghi".to_vec();
        assert_eq!(drain_bounded(&mut data.as_slice(), 9), data);
    }
}
