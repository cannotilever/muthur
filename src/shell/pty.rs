//! A child process on a pseudo-terminal.

use anyhow::Context;
use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::pty::{Winsize, openpty};
use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{SetArg, SpecialCharacterIndices, tcgetattr, tcsetattr};
use nix::unistd::Pid;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Exited,
}

nix::ioctl_write_ptr_bad!(tiocswinsz, nix::libc::TIOCSWINSZ, nix::libc::winsize);

pub struct Pty {
    master: File,
    child: Child,
}

fn winsize(cols: u16, rows: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn cloexec(fd: impl AsFd) -> nix::Result<()> {
    fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map(|_| ())
}

impl Pty {
    /// Start `command` on a new PTY of `cols`×`rows`. `events` receives output
    /// from a reader thread, then `Exited` once the child side is closed.
    pub fn spawn(command: &str, args: &[String], cols: u16, rows: u16, events: impl Fn(PtyEvent) + Send + 'static) -> anyhow::Result<Pty> {
        let pty = openpty(Some(&winsize(cols, rows)), None).context("openpty")?;
        cloexec(&pty.master)?;
        cloexec(&pty.slave)?;
        // Backspace is ^H on the WY-60 and in vt100 terminfo (kbs).
        let mut t = tcgetattr(&pty.slave)?;
        t.control_chars[SpecialCharacterIndices::VERASE as usize] = 0x08;
        tcsetattr(&pty.slave, SetArg::TCSANOW, &t)?;

        let mut cmd = Command::new(command);
        cmd.args(args)
            .env("TERM", "vt100")
            .env("MUTHUR", "1")
            .stdin(Stdio::from(pty.slave.try_clone()?))
            .stdout(Stdio::from(pty.slave.try_clone()?))
            .stderr(Stdio::from(pty.slave.try_clone()?));
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid()?;
                if nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().with_context(|| format!("starting {command}"))?;
        drop(pty.slave); // only the child holds the slave, so its exit reads as EIO

        let master = File::from(pty.master);
        let mut reader = master.try_clone()?;
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => events(PtyEvent::Output(buf[..n].to_vec())),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break, // EIO: child side closed
                }
            }
            events(PtyEvent::Exited);
        });
        Ok(Pty { master, child })
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.master.write_all(bytes)
    }

    /// Change the window size; the kernel sends SIGWINCH to the foreground job.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let ws = winsize(cols, rows);
        // SAFETY: TIOCSWINSZ reads one winsize through the pointer.
        unsafe { tiocswinsz(self.master.as_raw_fd(), &ws) }?;
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let pid = Pid::from_raw(self.child.id() as i32);
        let _ = kill(pid, Signal::SIGHUP);
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn start(script: &str, cols: u16, rows: u16) -> (Pty, mpsc::Receiver<PtyEvent>) {
        let (tx, rx) = mpsc::channel();
        let pty = Pty::spawn("sh", &["-c".into(), script.into()], cols, rows, move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
        (pty, rx)
    }

    /// Output until the child exits (or 2 s pass), and whether Exited arrived.
    fn collect(rx: &mpsc::Receiver<PtyEvent>) -> (String, bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut out = Vec::new();
        while let Ok(ev) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            match ev {
                PtyEvent::Output(b) => out.extend(b),
                PtyEvent::Exited => return (String::from_utf8_lossy(&out).into_owned(), true),
            }
        }
        (String::from_utf8_lossy(&out).into_owned(), false)
    }

    #[test]
    fn runs_command_and_reports_output() {
        let (_p, rx) = start("printf hello", 80, 24);
        let (out, exited) = collect(&rx);
        assert!(out.contains("hello") && exited, "{out:?} {exited}");
    }

    #[test]
    fn exit_is_reported() {
        let (_p, rx) = start("exit 3", 80, 24);
        assert!(collect(&rx).1);
    }

    #[test]
    fn child_sees_term_and_size() {
        let (_p, rx) = start("echo $TERM $MUTHUR; stty size", 59, 38);
        let out = collect(&rx).0;
        assert!(out.contains("vt100 1") && out.contains("38 59"), "{out:?}");
    }

    #[test]
    fn erase_is_ctrl_h() {
        let (_p, rx) = start("stty -a", 80, 24);
        let out = collect(&rx).0;
        assert!(out.contains("erase = ^H"), "{out:?}");
    }

    #[test]
    fn resize_reaches_child() {
        let (p, rx) = start("sleep 0.3; stty size", 59, 38);
        p.resize(80, 43).unwrap();
        let out = collect(&rx).0;
        assert!(out.contains("43 80"), "{out:?}");
    }

    #[test]
    fn input_reaches_child() {
        let (mut p, rx) = start("read x; echo got:$x", 80, 24);
        p.write(b"abc\r").unwrap();
        let out = collect(&rx).0;
        assert!(out.contains("got:abc"), "{out:?}");
    }

    #[test]
    fn drop_kills_child() {
        let (p, rx) = start("sleep 30", 80, 24);
        let start = Instant::now();
        drop(p);
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(collect(&rx).1, "reader saw no exit");
    }
}
