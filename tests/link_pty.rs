use muthur::config::FlowControl;
use muthur::link::{Link, ReadTimeout, install_panic_hook};
use nix::sys::termios::{LocalFlags, tcgetattr};
use std::os::fd::OwnedFd;
use std::time::Duration;

#[test]
fn drop_restores_termios() {
    let pty = nix::pty::openpty(None, None).unwrap();
    let before = tcgetattr(&pty.slave).unwrap();
    let slave = pty.slave.try_clone().unwrap();
    {
        let _l = Link::from_fd(slave, Some(38400), FlowControl::XonXoff).unwrap();
        assert!(!tcgetattr(&pty.slave).unwrap().local_flags.contains(LocalFlags::ICANON));
    }
    assert_eq!(tcgetattr(&pty.slave).unwrap().local_flags, before.local_flags);
}

#[test]
fn read_timeout_returns_zero_when_idle() {
    let pty = nix::pty::openpty(None, None).unwrap();
    let _master = pty.master;
    let mut l = Link::from_fd(pty.slave, None, FlowControl::XonXoff).unwrap();
    let mut buf = [0u8; 8];
    assert_eq!(l.read_timeout(&mut buf, Duration::from_millis(20)).unwrap(), 0);
}

const CHILD_ENV: &str = "MUTHUR_PANIC_CHILD_TTY";

/// Helper run in a child process by `panic_hook_restores_termios`; a no-op otherwise.
#[test]
fn panic_hook_child() {
    let Ok(tty) = std::env::var(CHILD_ENV) else { return };
    let f = std::fs::OpenOptions::new().read(true).write(true).open(tty).unwrap();
    install_panic_hook();
    let _l = Link::from_fd(OwnedFd::from(f), None, FlowControl::XonXoff).unwrap();
    // Leak the link so Drop cannot be what restores termios — only the hook can.
    std::mem::forget(_l);
    panic!("deliberate panic while in raw mode");
}

#[test]
fn panic_hook_restores_termios() {
    let pty = nix::pty::openpty(None, None).unwrap();
    let tty = nix::unistd::ttyname(&pty.slave).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["panic_hook_child", "--exact", "--test-threads=1"])
        .env(CHILD_ENV, &tty)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "child should have panicked");
    let t = tcgetattr(&pty.slave).unwrap();
    assert!(t.local_flags.contains(LocalFlags::ICANON | LocalFlags::ECHO), "termios not restored");
}
