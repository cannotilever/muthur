//! The terminal link: a raw-mode fd (serial device or stdio) with guaranteed
//! termios restore on drop and on panic.

pub use crate::config::FlowControl;
use crate::config::LinkConfig;
use anyhow::{Context, bail};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{
    BaudRate, ControlFlags, FlowArg, FlushArg, InputFlags, LocalFlags, OutputFlags, SetArg, SpecialCharacterIndices, Termios, cfsetspeed, tcflow, tcflush,
    tcgetattr, tcsetattr,
};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Mutex, Once};
use std::time::Duration;

/// Map a numeric baud rate to a termios `BaudRate`.
pub fn baud_rate(baud: u32) -> anyhow::Result<BaudRate> {
    Ok(match baud {
        1200 => BaudRate::B1200,
        2400 => BaudRate::B2400,
        4800 => BaudRate::B4800,
        9600 => BaudRate::B9600,
        19200 => BaudRate::B19200,
        38400 => BaudRate::B38400,
        57600 => BaudRate::B57600,
        115200 => BaudRate::B115200,
        other => bail!("unsupported baud rate {other}"),
    })
}

/// Raw-mode transform of `t`, preserving the requested flow control.
pub fn make_raw(t: &Termios, flow: FlowControl) -> Termios {
    let mut r = t.clone();
    r.local_flags
        .remove(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ECHONL | LocalFlags::ISIG | LocalFlags::IEXTEN);
    r.input_flags
        .remove(InputFlags::ICRNL | InputFlags::INLCR | InputFlags::IGNCR | InputFlags::ISTRIP | InputFlags::BRKINT | InputFlags::PARMRK);
    r.output_flags.remove(OutputFlags::OPOST);
    r.control_flags.remove(ControlFlags::CSIZE | ControlFlags::PARENB);
    // CLOCAL: ignore modem lines (no carrier on a null-modem link); CREAD: enable receiver.
    r.control_flags.insert(ControlFlags::CS8 | ControlFlags::CLOCAL | ControlFlags::CREAD);
    // IXANY would let any keypress resume output the terminal paused with XOFF.
    r.input_flags.remove(InputFlags::IXANY);
    r.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
    r.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
    let soft = InputFlags::IXON | InputFlags::IXOFF;
    match flow {
        FlowControl::XonXoff => {
            r.input_flags.insert(soft);
            r.control_flags.remove(ControlFlags::CRTSCTS);
        }
        FlowControl::RtsCts => {
            r.input_flags.remove(soft);
            r.control_flags.insert(ControlFlags::CRTSCTS);
        }
        FlowControl::None => {
            r.input_flags.remove(soft);
            r.control_flags.remove(ControlFlags::CRTSCTS);
        }
    }
    r
}

/// Read with a timeout. `Ok(0)` means the timeout elapsed with no data;
/// hangup/EOF is an error, never `Ok(0)`.
pub trait ReadTimeout {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

/// The fd + original termios of the live link, for the panic hook.
static RESTORE: Mutex<Option<(RawFd, Termios)>> = Mutex::new(None);

/// Install a panic hook that restores the live link's termios and resets
/// attributes/charset before the default hook prints the panic message.
pub fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Ok(guard) = RESTORE.lock()
                && let Some((fd, t)) = guard.as_ref()
            {
                // SAFETY: the fd is registered only while its Link is alive.
                let bfd = unsafe { BorrowedFd::borrow_raw(*fd) };
                // TCSANOW: never block behind a terminal stuck in XOFF.
                let _ = tcsetattr(bfd, SetArg::TCSANOW, t);
                let _ = nix::unistd::write(bfd, b"\x1b[0m\x1b(B\r\n");
            }
            prev(info);
        }));
    });
}

/// A terminal in raw mode. Restores the original termios on drop.
pub struct Link {
    file: File,
    original: Termios,
}

impl Link {
    /// Open the configured serial device and set baud + flow control.
    pub fn open_device(cfg: &LinkConfig) -> anyhow::Result<Link> {
        let path = cfg.device.as_ref().context("no serial device configured")?;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            // O_NONBLOCK so open() never waits for carrier; cleared below.
            .custom_flags(nix::libc::O_NOCTTY | nix::libc::O_NONBLOCK)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        fcntl(&f, FcntlArg::F_SETFL(OFlag::empty())).context("clearing O_NONBLOCK")?;
        Link::from_fd(OwnedFd::from(f), Some(cfg.baud), cfg.flow)
    }

    /// Use the controlling terminal on stdin. Baud is left as agetty set it.
    pub fn stdio(flow: FlowControl) -> anyhow::Result<Link> {
        let fd = io::stdin().as_fd().try_clone_to_owned().context("dup stdin")?;
        Link::from_fd(fd, None, flow)
    }

    pub fn from_fd(fd: OwnedFd, baud: Option<u32>, flow: FlowControl) -> anyhow::Result<Link> {
        let original = tcgetattr(&fd).context("not a terminal")?;
        let mut raw = make_raw(&original, flow);
        if let Some(b) = baud {
            cfsetspeed(&mut raw, baud_rate(b)?).context("setting baud")?;
        }
        // TCSAFLUSH also discards stale input (line noise, keys typed earlier).
        tcsetattr(&fd, SetArg::TCSAFLUSH, &raw).context("setting raw mode")?;
        let file = File::from(fd);
        *RESTORE.lock().unwrap_or_else(|e| e.into_inner()) = Some((file.as_raw_fd(), original.clone()));
        Ok(Link { file, original })
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let mut guard = RESTORE.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(guard.as_ref(), Some((fd, _)) if *fd == self.file.as_raw_fd()) {
            *guard = None;
        }
        let _ = self.file.flush();
        let _ = tcsetattr(&self.file, SetArg::TCSADRAIN, &self.original);
    }
}

impl Read for Link {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Write for Link {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Poll-then-read shared by `Link` and `LinkReader`. Hangup/EOF is an error.
fn read_file_timeout(file: &mut File, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
    let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let mut fds = [PollFd::new(file.as_fd(), PollFlags::POLLIN)];
    let ready = poll(&mut fds, PollTimeout::try_from(ms).unwrap_or(PollTimeout::MAX))?;
    if ready == 0 {
        return Ok(0);
    }
    // poll said readable (or hung up) and VMIN=1, so 0 bytes means EOF/hangup.
    match file.read(buf)? {
        0 => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "terminal hung up")),
        n => Ok(n),
    }
}

impl ReadTimeout for Link {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        read_file_timeout(&mut self.file, buf, timeout)
    }
}

/// An independent read handle on the link (for the input thread).
pub struct LinkReader {
    file: File,
}

impl ReadTimeout for LinkReader {
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        read_file_timeout(&mut self.file, buf, timeout)
    }
}

/// See `Link::unsticker`.
pub struct Unsticker {
    file: File,
}

impl Unsticker {
    /// Drop output queued for a screen that is gone and resume output the
    /// terminal suspended with XOFF before it lost power. Never fatal.
    /// (An RTS/CTS stall can't be overridden from here.)
    pub fn unstick(&self) {
        let mut q: nix::libc::c_int = 0;
        // SAFETY: TIOCOUTQ writes one c_int through the pointer.
        let _ = unsafe { tiocoutq(self.file.as_raw_fd(), &mut q) };
        tracing::info!(discarded = q, "break: flushing output queue");
        if let Err(e) = tcflush(&self.file, FlushArg::TCOFLUSH) {
            tracing::warn!("break: flushing output failed: {e}");
        }
        if let Err(e) = tcflow(&self.file, FlowArg::TCOON) {
            tracing::warn!("break: resuming output failed: {e}");
        }
    }
}

nix::ioctl_read_bad!(tiocoutq, nix::libc::TIOCOUTQ, nix::libc::c_int);

impl Link {
    /// An independent write handle (the renderer's), so `self` stays free for
    /// `output_queue` and owns the termios restore.
    pub fn writer(&self) -> io::Result<File> {
        self.file.try_clone()
    }

    pub fn reader(&self) -> io::Result<LinkReader> {
        Ok(LinkReader { file: self.file.try_clone()? })
    }

    /// A handle the input thread uses to unstick output when Break arrives.
    pub fn unsticker(&self) -> io::Result<Unsticker> {
        Ok(Unsticker { file: self.file.try_clone()? })
    }

    /// Bytes written but not yet transmitted (kernel output queue).
    pub fn output_queue(&self) -> io::Result<usize> {
        let mut n: nix::libc::c_int = 0;
        // SAFETY: TIOCOUTQ writes one c_int through the pointer.
        unsafe { tiocoutq(self.file.as_raw_fd(), &mut n) }?;
        Ok(n.max(0) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::termios::{ControlFlags, InputFlags, LocalFlags, OutputFlags, Termios, tcgetattr};

    fn sample_cooked() -> Termios {
        let pty = nix::pty::openpty(None, None).unwrap();
        tcgetattr(&pty.slave).unwrap()
    }

    #[test]
    fn xonxoff_raw_keeps_software_flow() {
        let t = make_raw(&sample_cooked(), FlowControl::XonXoff);
        assert!(t.input_flags.contains(InputFlags::IXON | InputFlags::IXOFF));
        assert!(!t.control_flags.contains(ControlFlags::CRTSCTS));
        assert!(!t.local_flags.intersects(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG));
        assert!(!t.output_flags.contains(OutputFlags::OPOST));
    }

    #[test]
    fn rtscts_raw_disables_software_flow() {
        let t = make_raw(&sample_cooked(), FlowControl::RtsCts);
        assert!(!t.input_flags.intersects(InputFlags::IXON | InputFlags::IXOFF));
        assert!(t.control_flags.contains(ControlFlags::CRTSCTS));
    }

    #[test]
    fn raw_forces_local_and_receiver_and_clears_ixany() {
        let mut cooked = sample_cooked();
        cooked.input_flags.insert(InputFlags::IXANY);
        cooked.control_flags.remove(ControlFlags::CLOCAL | ControlFlags::CREAD);
        let t = make_raw(&cooked, FlowControl::XonXoff);
        assert!(t.control_flags.contains(ControlFlags::CLOCAL | ControlFlags::CREAD));
        assert!(!t.input_flags.contains(InputFlags::IXANY));
    }

    #[test]
    fn unsupported_baud_is_error() {
        assert!(baud_rate(38400).is_ok());
        assert!(baud_rate(38401).unwrap_err().to_string().contains("38401"));
    }

    #[test]
    fn unstick_resumes_output_suspended_by_xoff() {
        let pty = nix::pty::openpty(None, None).unwrap();
        let link = Link::from_fd(pty.slave, None, FlowControl::XonXoff).unwrap();
        tcflow(&link.file, FlowArg::TCOOFF).unwrap();
        let mut w = link.writer().unwrap();
        let unsticker = link.unsticker().unwrap();
        let writer = std::thread::spawn(move || w.write_all(b"HI"));
        std::thread::sleep(Duration::from_millis(50));
        unsticker.unstick();
        let mut master = File::from(pty.master);
        let mut fds = [PollFd::new(master.as_fd(), PollFlags::POLLIN)];
        assert_eq!(poll(&mut fds, PollTimeout::from(2000u16)).unwrap(), 1, "output still suspended");
        let mut buf = [0u8; 8];
        let n = master.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"HI");
        writer.join().unwrap().unwrap();
    }
}
