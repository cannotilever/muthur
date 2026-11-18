//! The TUI run loop: logging, input thread, redraw scheduling, teardown.

use crate::app::{App, ChromeView};
use crate::backend::Wy60Backend;
use crate::config::Config;
use crate::dialect::{Dialect, Vt100Dialect};
use crate::input::{Key, KeyParser, Keymap};
use crate::link::{Link, ReadTimeout, install_panic_hook};
use crate::scroll::HintSlot;
use crate::shell::{PtyEvent, ShellConfig, ShellTab};
use crate::strict::StrictWriter;
use crate::testcard::keys::KEY_GAP;
use chrono::NaiveDateTime;
use ratatui::Terminal;
use std::io::Write;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, UnboundedReceiver, UnboundedSender};

/// Draw only when fewer than this many bytes are still waiting to be sent,
/// so a slow or XOFF'd link never accumulates stale frames.
pub const OUTQ_LOW: usize = 256;
/// PTY output chunks (≤ 4 KB each) buffered between the reader and the loop.
pub const PTY_QUEUE: usize = 16;
/// How soon to re-check the queue when a draw was deferred.
const RETRY: Duration = Duration::from_millis(20);

pub enum Event {
    Keys(Vec<Key>),
    Hangup,
    Tick,
}

/// The chrome is laid out for 80×43; smaller screens cannot hold it.
pub fn check_geometry(cols: u16, rows: u16) -> anyhow::Result<()> {
    if cols < 80 || rows < 43 {
        anyhow::bail!("MU/TH/UR needs at least 80x43 (got {cols}x{rows})");
    }
    Ok(())
}

pub fn should_draw(dirty: bool, outq: usize) -> bool {
    dirty && outq < OUTQ_LOW
}

/// Read the link, parse keys, and forward them. Exits (sending `Hangup`) on a
/// read error, or quietly when the receiver is gone.
pub fn spawn_input<R: ReadTimeout + Send + 'static>(
    mut reader: R,
    mut parser: KeyParser,
    tx: UnboundedSender<Event>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        loop {
            let keys = match reader.read_timeout(&mut buf, KEY_GAP) {
                Ok(0) => parser.timeout(),
                Ok(n) => parser.feed(&buf[..n]),
                Err(e) => {
                    tracing::warn!("link read failed: {e}");
                    let _ = tx.send(Event::Hangup);
                    return;
                }
            };
            let keys: Vec<Key> = keys
                .into_iter()
                .filter(|k| match k {
                    Key::Unknown(seq) => {
                        tracing::debug!(seq = ?String::from_utf8_lossy(seq), "unknown key dropped");
                        false
                    }
                    _ => true,
                })
                .collect();
            if !keys.is_empty() && tx.send(Event::Keys(keys)).is_err() {
                return;
            }
        }
    })
}

pub async fn event_loop<W: Write>(
    app: &mut App,
    terminal: &mut Terminal<Wy60Backend<W, Vt100Dialect>>,
    rx: &mut UnboundedReceiver<Event>,
    pty_rx: &mut Receiver<PtyEvent>,
    outq: impl Fn() -> usize,
    clock: impl Fn() -> NaiveDateTime,
) -> anyhow::Result<()> {
    let mut dirty = true;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    // Passthrough is on when entered from outside (tests) or by `^] p`.
    let mut was_passthrough = false;
    loop {
        // Entering passthrough: draw once more (the shell, now fullscreen,
        // resizes its PTY to the whole screen), then stop drawing.
        let entering = app.passthrough && !was_passthrough && dirty;
        if (entering || !app.passthrough) && should_draw(dirty, outq()) {
            terminal.draw(|f| app.render(f, clock()))?;
            dirty = false;
            was_passthrough = app.passthrough;
        }
        tokio::select! {
            // Keys first: ^C and ^] must never wait behind queued output.
            biased;
            ev = rx.recv() => match ev {
                Some(Event::Keys(keys)) => {
                    for k in keys {
                        app.handle_key(k);
                    }
                    dirty = true;
                }
                Some(Event::Tick) => dirty = true,
                Some(Event::Hangup) | None => return Ok(()),
            },
            Some(ev) = pty_rx.recv() => {
                if app.passthrough && let PtyEvent::Output(bytes) = &ev {
                    let w = terminal.backend_mut().writer_mut();
                    w.write_all(bytes)?;
                    w.flush()?;
                }
                app.on_pty(ev);
                dirty = true;
            }
            _ = tick.tick() => dirty = true,
            _ = tokio::time::sleep(RETRY), if dirty => {}
        }
        if app.quit {
            return Ok(());
        }
        if !app.passthrough {
            was_passthrough = false;
        }
        if app.redraw {
            app.redraw = false;
            terminal.clear()?;
            dirty = true;
        }
    }
}

fn init_logging() -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    let dir = dirs::state_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join("muthur");
    std::fs::create_dir_all(&dir)?;
    let (writer, guard) = tracing_appender::non_blocking(tracing_appender::rolling::daily(&dir, "muthur.log"));
    let level = if std::env::var("MUTHUR_DEBUG").is_ok() { tracing::Level::DEBUG } else { tracing::Level::INFO };
    tracing_subscriber::fmt().with_writer(writer).with_ansi(false).with_max_level(level).init();
    Ok(guard)
}

fn ship_name(cfg: &Config) -> String {
    cfg.chrome.ship_name.clone().unwrap_or_else(|| {
        let host = nix::unistd::gethostname().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        format!("USCSS {}", host.to_uppercase())
    })
}

/// Run the TUI on the configured link until quit or hangup.
pub fn run(cfg: &Config, strict: bool) -> anyhow::Result<()> {
    check_geometry(cfg.terminal.cols, cfg.terminal.rows)?;
    let _log = init_logging()?;
    install_panic_hook();
    let link = match cfg.link.device {
        Some(_) => Link::open_device(&cfg.link)?,
        None => Link::stdio(cfg.link.flow)?,
    };
    let keymap = Keymap::wy60_default().with_overrides(&cfg.keys)?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    spawn_input(link.reader()?, KeyParser::new(keymap), tx.clone());

    let out = link.writer()?;
    let writer: Box<dyn Write> =
        if strict { Box::new(StrictWriter::new(out, Vt100Dialect)) } else { Box::new(out) };
    let (cols, rows) = (cfg.terminal.cols, cfg.terminal.rows);
    let hints = HintSlot::default();
    let mut terminal = Terminal::new(Wy60Backend::new(writer, Vt100Dialect, cols, rows).with_hints(hints.clone()))?;
    terminal.clear()?;
    let shell_cfg = ShellConfig {
        command: cfg.shell.command.clone(),
        args: cfg.shell.args.clone(),
        scrollback: cfg.shell.scrollback,
    };
    // Bounded: when the loop falls behind (passthrough at 38400 baud), the
    // reader thread blocks, the PTY fills, and the program writing blocks too.
    let (pty_tx, mut pty_rx) = tokio::sync::mpsc::channel(PTY_QUEUE);
    let shell = ShellTab::new(shell_cfg, hints, move |e| {
        let _ = pty_tx.blocking_send(e);
    });
    let chrome = ChromeView { ship_name: ship_name(cfg), year_offset: cfg.chrome.year_offset };
    let mut app = App::with_shell(chrome, shell);
    tracing::info!(cols, rows, strict, "MU/TH/UR online");

    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build()?;
    let outq = || {
        let q = link.output_queue().unwrap_or(0);
        if q > 0 {
            tracing::debug!(outq = q, "output queue");
        }
        q
    };
    let result = rt.block_on(event_loop(&mut app, &mut terminal, &mut rx, &mut pty_rx, outq, || {
        chrono::Local::now().naive_local()
    }));

    let mut bye = Vec::new();
    Vt100Dialect.clear(&mut bye);
    let w = terminal.backend_mut().writer_mut();
    w.write_all(&bye)?;
    w.flush()?;
    drop(terminal);
    drop(link); // restores termios
    tracing::info!("MU/TH/UR offline");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, ChromeView};
    use crate::backend::Wy60Backend;
    use crate::dialect::Vt100Dialect;
    use crate::input::Key;
    use ratatui::Terminal;
    use std::time::Duration;

    fn now() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap().and_hms_opt(14, 2, 0).unwrap()
    }

    fn setup() -> (App, Terminal<Wy60Backend<Vec<u8>, Vt100Dialect>>) {
        let app = App::new(ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 });
        let mut t = Terminal::new(Wy60Backend::new(Vec::new(), Vt100Dialect, 80, 43)).unwrap();
        t.clear().unwrap();
        (app, t)
    }

    #[test]
    fn geometry_must_fit_the_chrome() {
        assert!(check_geometry(80, 43).is_ok());
        assert!(check_geometry(132, 50).is_ok());
        assert!(check_geometry(80, 24).unwrap_err().to_string().contains("80x43"));
        assert!(check_geometry(40, 43).is_err());
    }

    fn no_pty() -> tokio::sync::mpsc::Receiver<PtyEvent> {
        tokio::sync::mpsc::channel(1).1
    }

    #[test]
    fn should_draw_waits_for_queue() {
        assert!(should_draw(true, 0));
        assert!(should_draw(true, OUTQ_LOW - 1));
        assert!(!should_draw(true, OUTQ_LOW));
        assert!(!should_draw(false, 0));
    }

    #[tokio::test]
    async fn hangup_event_quits() {
        let (mut app, mut t) = setup();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(Event::Hangup).unwrap();
        let r = tokio::time::timeout(Duration::from_millis(100), event_loop(&mut app, &mut t, &mut rx, &mut no_pty(), || 0, now)).await;
        assert!(r.expect("loop did not exit").is_ok());
    }

    #[tokio::test]
    async fn quit_key_ends_loop_and_redraw_clears() {
        let (mut app, mut t) = setup();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('r')])).unwrap();
        tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('q')])).unwrap();
        let r = tokio::time::timeout(Duration::from_millis(500), event_loop(&mut app, &mut t, &mut rx, &mut no_pty(), || 0, now)).await;
        assert!(r.expect("loop did not exit").is_ok());
        let out = String::from_utf8(t.backend().writer().clone()).unwrap();
        assert_eq!(out.matches("\x1b[2J").count(), 2, "initial clear + redraw");
        assert!(out.contains("17 NOV 2137 14:02"));
    }

    #[tokio::test]
    async fn passthrough_writes_raw_bytes() {
        let (mut app, mut t) = setup();
        app.passthrough = true;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (ptx, mut prx) = tokio::sync::mpsc::channel(4);
        ptx.send(PtyEvent::Output(b"\x1b[31mX".to_vec())).await.unwrap();
        let quit = async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('q')])).unwrap();
        };
        let (r, _) = tokio::join!(event_loop(&mut app, &mut t, &mut rx, &mut prx, || 0, now), quit);
        r.unwrap();
        let out = t.backend().writer().clone();
        assert!(out.windows(6).any(|w| w == b"\x1b[31mX"), "{:?}", String::from_utf8_lossy(&out));
    }

    #[tokio::test]
    async fn keys_jump_ahead_of_queued_output() {
        let (mut app, mut t) = setup();
        app.passthrough = true;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (ptx, mut prx) = tokio::sync::mpsc::channel(64);
        for _ in 0..64 {
            ptx.send(PtyEvent::Output(b"FLOOD".to_vec())).await.unwrap();
        }
        tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('q')])).unwrap();
        event_loop(&mut app, &mut t, &mut rx, &mut prx, || 0, now).await.unwrap();
        let out = String::from_utf8_lossy(t.backend().writer()).into_owned();
        assert_eq!(out.matches("FLOOD").count(), 0, "quit waited behind chunks");
    }

    #[tokio::test]
    async fn full_queue_defers_drawing() {
        let (mut app, mut t) = setup();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('q')])).unwrap();
        event_loop(&mut app, &mut t, &mut rx, &mut no_pty(), || OUTQ_LOW, now).await.unwrap();
        let out = String::from_utf8(t.backend().writer().clone()).unwrap();
        assert!(!out.contains("MU/TH/UR"), "drew while the queue was full");
    }
}
