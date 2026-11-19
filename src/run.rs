//! The TUI run loop: logging, input thread, redraw scheduling, teardown.

use crate::app::{App, ChromeView};
use crate::backend::Wy60Backend;
use crate::config::Config;
use crate::dialect::{Dialect, Vt100Dialect};
use crate::input::{Key, KeyParser, Keymap};
use crate::link::{Link, ReadTimeout, install_panic_hook};
use crate::metrics::local::LocalSource;
use crate::metrics::{MetricsSource, Snapshot};
use crate::scroll::HintSlot;
use crate::utility::UtilityTab;
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
    Metrics(Box<Snapshot>),
    MetricsOffline(String),
    Hangup,
    Tick,
    Llm { req_id: u64, ev: crate::llm::StreamEvent },
}

/// The chrome is laid out for 80×43; smaller screens cannot hold it.
pub fn check_geometry(cols: u16, rows: u16) -> anyhow::Result<()> {
    if cols < 80 || rows < 43 {
        anyhow::bail!("MU/TH/UR needs at least 80x43 (got {cols}x{rows})");
    }
    Ok(())
}

/// Take a snapshot every `interval` and send it; errors are logged and the
/// loop carries on. Ends when the app is gone.
/// Consecutive failed snapshots before the Utility tab says TELEMETRY OFFLINE.
pub const OFFLINE_AFTER: u32 = 3;

pub fn spawn_metrics(
    mut source: Box<dyn MetricsSource + Send>,
    interval: Duration,
    tx: UnboundedSender<Event>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut errors = 0;
        loop {
            let sent = match source.snapshot() {
                Ok(s) => {
                    errors = 0;
                    tx.send(Event::Metrics(Box::new(s)))
                }
                Err(e) => {
                    errors += 1;
                    tracing::warn!("metrics: {e:#}");
                    if errors == OFFLINE_AFTER {
                        tx.send(Event::MetricsOffline(format!("{e:#}").to_uppercase()))
                    } else {
                        Ok(())
                    }
                }
            };
            if sent.is_err() {
                return; // the app is gone
            }
            std::thread::sleep(interval);
        }
    })
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
                Some(Event::Metrics(s)) => {
                    app.on_metrics(*s);
                    dirty = true;
                }
                Some(Event::MetricsOffline(why)) => {
                    app.on_metrics_offline(&why);
                    dirty = true;
                }
                Some(Event::Tick) => dirty = true,
                Some(Event::Llm { req_id, ev }) => {
                    app.on_llm(req_id, ev);
                    dirty = true;
                }
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
            _ = tokio::time::sleep(app.tick_interval()), if app.tick_interval() < Duration::from_secs(1) => dirty = true,
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

/// Chat log at `$XDG_STATE_HOME/muthur/chat.jsonl`, or a temp file (no
/// persistence across runs) when that cannot be opened.
/// Create a fresh, empty 0600 chat log in `dir`. Uses O_CREAT|O_EXCL, so a
/// pre-created file or symlink is never opened or removed; a taken name is
/// skipped in favour of `muthur-chat-<pid>-<n>.jsonl`.
fn create_temp_chat_log(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let pid = std::process::id();
    for n in 0..1000u32 {
        let name = if n == 0 { format!("muthur-chat-{pid}.jsonl") } else { format!("muthur-chat-{pid}-{n}.jsonl") };
        let p = dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p) {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free temporary chat log name"))
}

fn open_temp_chat_log() -> anyhow::Result<crate::chatlog::ChatLog> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    crate::chatlog::ChatLog::open(create_temp_chat_log(&dir)?)
}

fn open_chat_log() -> anyhow::Result<crate::chatlog::ChatLog> {
    let Some(dir) = dirs::state_dir() else {
        tracing::error!("no state directory, chat history will not persist");
        return open_temp_chat_log();
    };
    match crate::chatlog::ChatLog::open(dir.join("muthur/chat.jsonl")) {
        Ok(l) => Ok(l),
        Err(e) => {
            tracing::error!("chat log unavailable, history will not persist: {e:#}");
            open_temp_chat_log()
        }
    }
}

fn build_muthur(cfg: &Config, ship: &str, tx: UnboundedSender<Event>) -> anyhow::Result<crate::muthur::MuthurTab> {
    use crate::llm::{LlmError, StreamEvent, client::LlmClient};
    use crate::muthur::{Abort, MuthurDeps, MuthurTab};
    let log = open_chat_log()?;
    let spawn: Box<dyn FnMut(u64, crate::llm::ChatRequest) -> Abort> = match LlmClient::new(cfg.muthur.clone()) {
        Ok(client) => {
            let client = std::sync::Arc::new(client);
            Box::new(move |id, req| {
                let h = client.spawn(id, req, tx.clone());
                Box::new(move || h.abort())
            })
        }
        Err(e) => {
            tracing::error!("llm client unavailable: {e:#}");
            Box::new(move |req_id, _| {
                let _ = tx.send(Event::Llm { req_id, ev: StreamEvent::Error(LlmError::NoCarrier) });
                Box::new(|| {})
            })
        }
    };
    let year_offset = cfg.chrome.year_offset;
    let deps = MuthurDeps {
        spawn,
        now: Box::new(std::time::Instant::now),
        ship_time: Box::new(move || crate::app::chrome::format_date(chrono::Local::now().naive_local(), year_offset)),
        timestamp: Box::new(|| chrono::Local::now().to_rfc3339()),
        run_tool: Box::new(|_, _, _| {}),
    };
    let tools = crate::tools::ToolDefs::new(&crate::config::ToolsSection { enabled: false, ..cfg.tools.clone() });
    let audit = crate::tools::audit::AuditLog::offline();
    Ok(MuthurTab::new(cfg.muthur.clone(), tools, ship, log, audit, deps))
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
    match LocalSource::new(cfg.utility.containers, cfg.utility.exclude_fs.clone()) {
        Ok(src) => {
            spawn_metrics(Box::new(src), Duration::from_secs(cfg.utility.interval_secs.max(1)), tx.clone());
        }
        Err(e) => {
            tracing::error!("metrics unavailable: {e:#}");
            let _ = tx.send(Event::MetricsOffline(format!("{e:#}").to_uppercase()));
        }
    }
    let muthur = build_muthur(cfg, &chrome.ship_name, tx.clone())?;
    let mut app = App::assemble(chrome, UtilityTab::new(), muthur, shell);
    tracing::info!(cols, rows, strict, "MU/TH/UR online");

    let rt = tokio::runtime::Builder::new_current_thread().enable_time().enable_io().build()?;
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

    struct FakeSource {
        calls: usize,
        fail_every_other: bool,
    }
    impl crate::metrics::MetricsSource for FakeSource {
        fn snapshot(&mut self) -> anyhow::Result<crate::metrics::Snapshot> {
            self.calls += 1;
            if self.fail_every_other && self.calls % 2 == 1 {
                anyhow::bail!("probe exploded");
            }
            Ok(crate::metrics::Snapshot { host: format!("HOST{}", self.calls), ..Default::default() })
        }
    }

    fn next_metrics(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) -> Option<crate::metrics::Snapshot> {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok(Event::Metrics(s)) => return Some(*s),
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        None
    }

    #[test]
    fn collector_sends_snapshots() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_metrics(Box::new(FakeSource { calls: 0, fail_every_other: false }), Duration::from_millis(10), tx);
        assert_eq!(next_metrics(&mut rx).unwrap().host, "HOST1");
        assert_eq!(next_metrics(&mut rx).unwrap().host, "HOST2");
    }

    #[test]
    fn collector_survives_errors() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_metrics(Box::new(FakeSource { calls: 0, fail_every_other: true }), Duration::from_millis(10), tx);
        assert_eq!(next_metrics(&mut rx).unwrap().host, "HOST2");
    }

    struct BrokenSource;
    impl crate::metrics::MetricsSource for BrokenSource {
        fn snapshot(&mut self) -> anyhow::Result<crate::metrics::Snapshot> {
            anyhow::bail!("no /proc")
        }
    }

    #[test]
    fn collector_reports_offline_after_repeated_errors() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_metrics(Box::new(BrokenSource), Duration::from_millis(5), tx);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match rx.try_recv() {
                Ok(Event::MetricsOffline(why)) => {
                    assert!(why.contains("NO /PROC"), "{why}");
                    break;
                }
                _ if std::time::Instant::now() > deadline => panic!("no offline event"),
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }

    #[tokio::test]
    async fn metrics_event_reaches_utility() {
        let (_, mut t) = setup();
        let mut app = crate::app::App::with_tabs(
            ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 },
            [
                Box::new(crate::utility::UtilityTab::new()),
                Box::new(crate::app::PlaceholderTab { id: crate::app::TabId::Muthur }),
                Box::new(crate::app::PlaceholderTab { id: crate::app::TabId::Shell }),
            ],
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let snap = crate::metrics::Snapshot { host: "NOSTROMO".into(), cpu_cores: vec![1.0], ..Default::default() };
        tx.send(Event::Metrics(Box::new(snap))).unwrap();
        tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('q')])).unwrap();
        event_loop(&mut app, &mut t, &mut rx, &mut no_pty(), || 0, now).await.unwrap();
        let out = String::from_utf8_lossy(t.backend().writer()).into_owned();
        assert!(out.contains("NOSTROMO"), "{out:?}");
    }

    #[tokio::test]
    async fn llm_event_reaches_muthur() {
        use crate::llm::{Stop, StreamEvent};
        let (_, mut t) = setup();
        let (m, _dir) = crate::app::tests::fake_muthur();
        let shell = crate::shell::ShellTab::new(
            crate::shell::ShellConfig { command: "sh".into(), args: vec!["-c".into(), "sleep 2".into()], scrollback: 10 },
            crate::scroll::HintSlot::default(),
            |_| {},
        );
        let mut app = App::assemble(
            ChromeView { ship_name: "USCSS TEST".into(), year_offset: 111 },
            crate::utility::UtilityTab::new(),
            m,
            shell,
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ks = vec![Key::F(2)];
        ks.extend("HI".chars().map(Key::Char));
        ks.push(Key::Enter);
        tx.send(Event::Keys(ks)).unwrap();
        tx.send(Event::Llm { req_id: 1, ev: StreamEvent::Text("NOMINAL".into()) }).unwrap();
        tx.send(Event::Llm { req_id: 1, ev: StreamEvent::Done { stop: Stop::End, model: None, raw: None } }).unwrap();
        let quit = async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('q')])).unwrap();
        };
        let mut pty = no_pty();
        let (r, _) = tokio::join!(event_loop(&mut app, &mut t, &mut rx, &mut pty, || 0, now), quit);
        r.unwrap();
        let mut p = vt100::Parser::new(std::num::NonZeroU16::new(43).unwrap(), std::num::NonZeroU16::new(80).unwrap(), 0);
        p.process(t.backend().writer());
        let screen = p.screen().contents();
        assert!(screen.contains("NOMINAL"), "{screen}");
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

    #[test]
    fn temp_chat_log_never_reuses_existing_paths() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let planted = d.path().join(format!("muthur-chat-{pid}.jsonl"));
        std::fs::write(&planted, "planted").unwrap();
        let target = d.path().join("victim");
        std::fs::write(&target, "victim").unwrap();
        std::os::unix::fs::symlink(&target, d.path().join(format!("muthur-chat-{pid}-1.jsonl"))).unwrap();
        let p = create_temp_chat_log(d.path()).unwrap();
        assert_eq!(p, d.path().join(format!("muthur-chat-{pid}-2.jsonl")));
        assert_eq!(std::fs::read_to_string(&planted).unwrap(), "planted");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "victim");
        let mut l = crate::chatlog::ChatLog::open(p.clone()).unwrap();
        l.append_user("A", "t").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
