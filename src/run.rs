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
use crate::shell::{PtyEvent, ShellConfig, ShellTab};
use crate::splash::{Splash, SplashInfo};
use crate::strict::StrictWriter;
use crate::testcard::keys::KEY_GAP;
use crate::utility::UtilityTab;
use chrono::NaiveDateTime;
use ratatui::Terminal;
use std::io::Write;
use std::time::{Duration, Instant};
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
    Llm {
        req_id: u64,
        ev: crate::llm::StreamEvent,
    },
    #[cfg(feature = "agent")]
    ToolDone {
        req_id: u64,
        call_id: String,
        outcome: crate::tools::ToolOutcome,
    },
    #[cfg(feature = "agent")]
    Review {
        req_id: u64,
        call_id: String,
        review: crate::review::Review,
    },
    CargoDone {
        seq: u64,
        output: crate::ops::runner::RunOutput,
        ms: u64,
    },
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
    slot: crate::metrics::SnapshotSlot,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut errors = 0;
        loop {
            let sent = match source.snapshot() {
                Ok(s) => {
                    errors = 0;
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(s.clone());
                    }
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
/// read error, or quietly when the receiver is gone. A NUL byte (Break) calls
/// `on_break` first, here rather than in the event loop, which may be stuck in
/// a write to a terminal that was powered off after sending XOFF.
pub fn spawn_input<R: ReadTimeout + Send + 'static>(
    mut reader: R,
    mut parser: KeyParser,
    tx: UnboundedSender<Event>,
    on_break: impl Fn() + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        loop {
            let keys = match reader.read_timeout(&mut buf, KEY_GAP) {
                Ok(0) => parser.timeout(),
                Ok(n) => {
                    if buf[..n].contains(&0) {
                        // Length only: the same read may hold password characters.
                        tracing::info!(len = n, "break: NUL received");
                        on_break();
                    }
                    parser.feed(&buf[..n])
                }
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
        if (entering || !app.passthrough) && !app.settling() && should_draw(dirty, outq()) {
            let size = terminal.size()?;
            let lines = app.line_attrs(ratatui::layout::Rect::new(0, 0, size.width, size.height));
            terminal.backend_mut().set_line_attrs(&lines);
            terminal.draw(|f| app.render(f, clock()))?;
            dirty = false;
            was_passthrough = app.passthrough;
            if app.take_bell() {
                let w = terminal.backend_mut().writer_mut();
                w.write_all(b"\x07")?;
                w.flush()?;
            }
            // The splash can end inside render: quit or clear now, not after
            // the next wake-up.
            if app.quit {
                return Ok(());
            }
            if app.redraw {
                app.redraw = false;
                terminal.clear()?;
                dirty = true;
                continue;
            }
        }
        tokio::select! {
            // Keys first: ^C and ^] must never wait behind queued output.
            biased;
            ev = rx.recv() => match ev {
                Some(Event::Keys(keys)) => {
                    app.handle_keys(keys);
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
                #[cfg(feature = "agent")]
                Some(Event::ToolDone { req_id, call_id, outcome }) => {
                    app.on_tool_done(req_id, call_id, outcome);
                    dirty = true;
                }
                #[cfg(feature = "agent")]
                Some(Event::Review { req_id, call_id, review }) => {
                    app.on_review(req_id, call_id, review);
                    dirty = true;
                }
                Some(Event::CargoDone { seq, output, ms }) => {
                    app.on_cargo_done(seq, output, ms);
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
        // After Break, nothing is drawn until the line settles (see
        // `App::settling`); the re-init then clears below before drawing.
        app.poll_settle(Instant::now());
        app.poll_standby(Instant::now(), clock());
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
    let level = if std::env::var("MUTHUR_DEBUG").is_ok() {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    tracing_subscriber::fmt().with_writer(writer).with_ansi(false).with_max_level(level).init();
    Ok(guard)
}

fn ship_name(cfg: &Config) -> String {
    cfg.chrome.ship_name.clone().unwrap_or_else(|| {
        let host = nix::unistd::gethostname().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        format!("USCSS {}", host.to_uppercase())
    })
}

/// What startup settles for the technical agent before the TUI starts.
#[cfg(feature = "agent")]
struct TechStartup {
    policy: crate::agent::Policy,
    probe: crate::tools::shell::Probe,
    reviewer_ready: bool,
}

/// Without the `agent` feature there is no technical agent to check.
#[cfg(not(feature = "agent"))]
struct TechStartup;

#[cfg(not(feature = "agent"))]
fn tech_startup(_cfg: &Config, _config_path: &std::path::Path) -> anyhow::Result<TechStartup> {
    Ok(TechStartup)
}

/// Startup checks for the technical agent, on the calling thread: the deny
/// rules and `net_allow` (a bad entry is a startup error), the reviewer's
/// readiness and the agent-account probe.
#[cfg(feature = "agent")]
fn tech_startup(cfg: &Config, config_path: &std::path::Path) -> anyhow::Result<TechStartup> {
    let ctx = deny_ctx(config_path, dirs::state_dir().as_deref());
    let policy = tech_policy(&cfg.agent, &ctx)?;
    let reviewer_ready = match cfg.muthur.reviewer_cfg().readiness() {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("reviewer not ready, reviewed calls will ask: {e:?}");
            false
        }
    };
    let start = Instant::now();
    let probe = crate::tools::shell::probe(&crate::ops::runner::SystemRunner);
    tracing::info!(?probe, ms = start.elapsed().as_millis() as u64, "agent account probe");
    Ok(TechStartup { policy, probe, reviewer_ready })
}

/// The runtime values the CONTROL PLANE rule protects. An unknown value is
/// left empty, and `DenyRules` skips empty patterns.
#[cfg(feature = "agent")]
fn deny_ctx(config_path: &std::path::Path, state_base: Option<&std::path::Path>) -> crate::tools::deny::DenyCtx {
    crate::tools::deny::DenyCtx {
        // `--config` may be relative; commands name it by its full path.
        config_path: std::fs::canonicalize(config_path)
            .unwrap_or_else(|_| config_path.into())
            .to_string_lossy()
            .into_owned(),
        state_dir: state_base.map(|d| d.join("muthur").to_string_lossy().into_owned()).unwrap_or_default(),
        exe: std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        operator: crate::tools::facts::operator(),
    }
}

/// The technical agent's policy, with its deny rules always built. A bad
/// `[agent] deny` regex or `net_allow` host is an error naming the entry.
#[cfg(feature = "agent")]
fn tech_policy(agent: &crate::config::AgentSection, ctx: &crate::tools::deny::DenyCtx) -> anyhow::Result<crate::agent::Policy> {
    if let Some((i, h)) = agent.net_allow.iter().enumerate().find(|(_, h)| !crate::tools::netgate::valid_host(h)) {
        anyhow::bail!("bad [agent] net_allow entry {} ({h:?}): not a valid host", i + 1);
    }
    let deny = crate::tools::deny::DenyRules::new(ctx, &agent.deny).map_err(|e| anyhow::anyhow!(e))?;
    Ok(crate::agent::Policy {
        deny: Some(deny),
        net_allow: agent.net_allow.clone(),
    })
}

#[cfg(feature = "agent")]
type ReviewFn = Box<dyn FnMut(u64, String, crate::review::ReviewInput) -> crate::muthur::Abort>;

/// Reviews run as tasks on the reviewer's client and come back as
/// `Event::Review`; aborting cancels the request. A reviewer that is not
/// ready answers REVIEWER OFFLINE at once.
#[cfg(feature = "agent")]
fn review_dispatch(cfg: &crate::config::MuthurSection, ready: bool, tx: UnboundedSender<Event>) -> ReviewFn {
    use crate::review::Review;
    let model = cfg.model().unwrap_or_default().to_string();
    let client = if ready { llm_client(cfg.clone(), "reviewer") } else { None };
    match client {
        Some(client) => Box::new(move |req_id, call_id, input| {
            let (client, tx) = (client.clone(), tx.clone());
            let h = tokio::spawn(async move {
                let review = crate::review::run(&client, input).await;
                let _ = tx.send(Event::Review { req_id, call_id, review });
            });
            Box::new(move || h.abort())
        }),
        None => Box::new(move |req_id, call_id, _| {
            let review = Review::offline("not configured".into(), model.clone(), 0);
            let _ = tx.send(Event::Review { req_id, call_id, review });
            Box::new(|| {})
        }),
    }
}

/// A streaming client, or None (logged) when it cannot be built.
fn llm_client(cfg: crate::config::MuthurSection, who: &str) -> Option<std::sync::Arc<crate::llm::client::LlmClient>> {
    crate::llm::client::LlmClient::new(cfg)
        .map_err(|e| tracing::error!("{who} llm client unavailable: {e:#}"))
        .ok()
        .map(std::sync::Arc::new)
}

type Client = Option<std::sync::Arc<crate::llm::client::LlmClient>>;

/// Starts each agent's requests on its client; an agent without one gets NO CARRIER.
fn spawner(
    persona: Client,
    technical: Client,
    tx: UnboundedSender<Event>,
) -> Box<dyn FnMut(u64, crate::ops::AgentId, crate::llm::ChatRequest) -> crate::muthur::Abort> {
    use crate::llm::{LlmError, StreamEvent};
    use crate::ops::AgentId;
    Box::new(move |req_id, agent, req| {
        let client = match agent {
            AgentId::Muthur => &persona,
            AgentId::Tech => &technical,
        };
        match client {
            Some(c) => {
                let h = c.spawn(req_id, req, tx.clone());
                Box::new(move || h.abort())
            }
            None => {
                let _ = tx.send(Event::Llm {
                    req_id,
                    ev: StreamEvent::Error(LlmError::NoCarrier),
                });
                Box::new(|| {})
            }
        }
    })
}

/// The ship's time for logged inquiries, e.g. "18 NOV 2137 14:02".
fn ship_time(cfg: &Config) -> Box<dyn Fn() -> String> {
    let year_offset = cfg.chrome.year_offset;
    Box::new(move || crate::app::chrome::format_date(chrono::Local::now().naive_local(), year_offset))
}

#[cfg(not(feature = "agent"))]
fn build_muthur(
    cfg: &Config,
    ship: &str,
    tx: UnboundedSender<Event>,
    _slot: crate::metrics::SnapshotSlot,
    audit: crate::ops::audit::SharedAudit,
    _startup: TechStartup,
) -> anyhow::Result<crate::muthur::MuthurTab> {
    // Each launch starts a clean session; nothing is kept on disk.
    let log = crate::chatlog::ChatLog::memory();
    let deps = crate::muthur::MuthurDeps {
        spawn: spawner(llm_client(cfg.muthur.clone(), "persona"), None, tx),
        now: Box::new(std::time::Instant::now),
        ship_time: ship_time(cfg),
        timestamp: Box::new(|| chrono::Local::now().to_rfc3339()),
    };
    Ok(crate::muthur::MuthurTab::new(cfg.muthur.clone(), ship, log, audit, deps))
}

#[cfg(feature = "agent")]
fn build_muthur(
    cfg: &Config,
    ship: &str,
    tx: UnboundedSender<Event>,
    slot: crate::metrics::SnapshotSlot,
    audit: crate::ops::audit::SharedAudit,
    startup: TechStartup,
) -> anyhow::Result<crate::muthur::MuthurTab> {
    use crate::muthur::{AgentDeps, MuthurDeps, MuthurTab};
    // Each launch starts a clean session; nothing is kept on disk.
    let log = crate::chatlog::ChatLog::memory();
    let tool_tx = tx.clone();
    let tech_cfg = cfg.muthur.technical_cfg();
    let persona = llm_client(cfg.muthur.clone(), "persona");
    let technical = llm_client(tech_cfg.clone(), "technical");
    let review = review_dispatch(&cfg.muthur.reviewer_cfg(), startup.reviewer_ready, tx.clone());
    let tools = crate::tools::ToolDefs::persona(&cfg.tools);
    let ctx = crate::tools::fixed::FixedCtx {
        runner: std::sync::Arc::new(crate::ops::runner::SystemRunner),
        snapshot: slot.clone(),
    };
    let deps = MuthurDeps {
        spawn: spawner(persona, technical, tx),
        now: Box::new(std::time::Instant::now),
        ship_time: ship_time(cfg),
        timestamp: Box::new(|| chrono::Local::now().to_rfc3339()),
        run_tool: Box::new(move |req_id, call_id, inv, secret, cancel| {
            let ctx = ctx.clone();
            let tx = tool_tx.clone();
            tokio::task::spawn_blocking(move || {
                let outcome = execute_tool(&inv, &ctx, secret, &cancel);
                let _ = tx.send(Event::ToolDone { req_id, call_id, outcome });
            });
        }),
        review,
    };
    // `run` is offered only when the agent account is present and isolated.
    let run_available = startup.probe == crate::tools::shell::Probe::Ready;
    let tech = AgentDeps {
        defs: crate::tools::ToolDefs::technical(&cfg.agent, run_available),
        cfg: tech_cfg,
        prompt: crate::muthur::tech_prompt(&cfg.agent),
        facts: Box::new(move || {
            let runtime = slot.lock().ok().and_then(|g| g.as_ref().and_then(|s| s.container_runtime.clone()));
            crate::tools::facts::host_facts(runtime.as_deref())
        }),
        policy: startup.policy,
        label: cfg.agent.label.clone(),
        max_rounds: cfg.agent.max_rounds,
        delegate_report: cfg.tools.delegate_report,
        probe: startup.probe,
        reviewer_ready: startup.reviewer_ready,
    };
    Ok(MuthurTab::new(cfg.muthur.clone(), tools, ship, log, audit, deps, tech))
}

/// Runs one tool call: `run` through the shell runner (the password, if any,
/// is dropped and wiped when it returns), the others as fixed tools.
#[cfg(feature = "agent")]
fn execute_tool(
    inv: &crate::tools::Invocation,
    ctx: &crate::tools::fixed::FixedCtx,
    secret: Option<crate::secret::Secret>,
    cancel: &std::sync::atomic::AtomicBool,
) -> crate::tools::ToolOutcome {
    match inv {
        crate::tools::Invocation::Run {
            command,
            as_root,
            timeout_secs,
            ..
        } => {
            let timeout = Duration::from_secs(u64::from(*timeout_secs));
            crate::tools::shell::run(&crate::ops::runner::SystemRunner, command, *as_root, timeout, secret.as_ref(), cancel)
        }
        // The tab never runs a delegate; the persona's call goes to the officer instead.
        crate::tools::Invocation::Delegate { .. } => crate::tools::ToolOutcome {
            content: "NOT AVAILABLE".into(),
            is_error: true,
            argv: None,
            exit: None,
            ms: 0,
            audit_output: None,
        },
        _ => crate::tools::fixed::execute(inv, ctx),
    }
}

/// The CARGO tab, running container commands on blocking tasks.
fn build_cargo(tx: UnboundedSender<Event>, audit: crate::ops::audit::SharedAudit, hints: HintSlot) -> crate::cargo::CargoTab {
    use crate::ops::runner::{Runner, SystemRunner};
    let deps = crate::cargo::CargoDeps {
        run: Box::new(move |seq, argv, timeout, merged| {
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let output = if merged {
                    SystemRunner.run_merged(&argv, timeout)
                } else {
                    SystemRunner.run(&argv, timeout)
                };
                let ms = start.elapsed().as_millis() as u64;
                let _ = tx.send(Event::CargoDone { seq, output, ms });
            });
        }),
        now: Box::new(Instant::now),
        timestamp: Box::new(|| chrono::Local::now().to_rfc3339()),
        hints,
    };
    crate::cargo::CargoTab::new(deps, audit)
}

fn open_audit_log() -> crate::ops::audit::AuditLog {
    use crate::ops::audit::AuditLog;
    let Some(dir) = dirs::state_dir() else {
        tracing::error!("no state directory; audit log offline, root calls and overrides refused");
        return AuditLog::offline();
    };
    match AuditLog::open(dir.join("muthur/audit.jsonl")) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("audit log unavailable: {e:#}");
            AuditLog::offline()
        }
    }
}

type Term = Terminal<Wy60Backend<Box<dyn Write>, Vt100Dialect>>;

struct Session {
    link: Link,
    tx: UnboundedSender<Event>,
    rx: UnboundedReceiver<Event>,
    terminal: Term,
    hints: HintSlot,
}

/// Open the link, start the input thread and build the cleared terminal.
fn open_session(cfg: &Config, strict: bool) -> anyhow::Result<Session> {
    let link = match cfg.link.device {
        Some(_) => Link::open_device(&cfg.link)?,
        None => Link::stdio(cfg.link.flow)?,
    };
    let keymap = Keymap::wy60_default().with_overrides(&cfg.keys)?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let unsticker = link.unsticker()?;
    spawn_input(link.reader()?, KeyParser::new(keymap), tx.clone(), move || unsticker.unstick());

    let out = link.writer()?;
    let writer: Box<dyn Write> = if strict {
        Box::new(StrictWriter::new(out, Vt100Dialect))
    } else {
        Box::new(out)
    };
    let (cols, rows) = (cfg.terminal.cols, cfg.terminal.rows);
    let hints = HintSlot::default();
    let mut terminal = Terminal::new(Wy60Backend::new(writer, Vt100Dialect, cols, rows).with_hints(hints.clone()))?;
    terminal.clear()?;
    Ok(Session { link, tx, rx, terminal, hints })
}

/// Start the local metrics poller, or report telemetry offline.
fn start_metrics(cfg: &Config, tx: &UnboundedSender<Event>, slot: crate::metrics::SnapshotSlot) {
    match LocalSource::new(cfg.utility.containers, cfg.utility.exclude_fs.clone()) {
        Ok(src) => {
            spawn_metrics(Box::new(src), Duration::from_secs(cfg.utility.interval_secs.max(1)), tx.clone(), slot);
        }
        Err(e) => {
            tracing::error!("metrics unavailable: {e:#}");
            let _ = tx.send(Event::MetricsOffline(format!("{e:#}").to_uppercase()));
        }
    }
}

/// Clear the screen and restore termios (dropping the link).
fn teardown(mut terminal: Term, link: Link) -> anyhow::Result<()> {
    let mut bye = Vec::new();
    Vt100Dialect.clear(&mut bye);
    let w = terminal.backend_mut().writer_mut();
    w.write_all(&bye)?;
    w.flush()?;
    drop(terminal);
    drop(link); // restores termios
    Ok(())
}

fn splash_for(cfg: &Config, ship: &str) -> Splash {
    let info = SplashInfo::from_config(cfg, ship, chrono::Local::now().naive_local());
    Splash::new(info, Instant::now())
}

/// `config::load` runs before logging exists, so its warnings wait for here.
fn log_config_warnings(cfg: &Config) {
    for w in &cfg.warnings {
        tracing::warn!("{w}");
    }
}

/// Play the boot splash once on the configured link, then exit.
pub fn run_splash(cfg: &Config, strict: bool) -> anyhow::Result<()> {
    check_geometry(cfg.terminal.cols, cfg.terminal.rows)?;
    let _log = init_logging()?;
    log_config_warnings(cfg);
    install_panic_hook();
    let Session {
        link,
        tx,
        mut rx,
        mut terminal,
        ..
    } = open_session(cfg, strict)?;
    let ship = ship_name(cfg);
    start_metrics(cfg, &tx, crate::metrics::SnapshotSlot::default());
    let chrome = ChromeView {
        ship_name: ship.clone(),
        year_offset: cfg.chrome.year_offset,
    };
    let mut app = App::new(chrome).with_splash(splash_for(cfg, &ship));
    app.quit_after_splash = true;
    let (_pty_tx, mut pty_rx) = tokio::sync::mpsc::channel(PTY_QUEUE);
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().enable_io().build()?;
    let outq = || link.output_queue().unwrap_or(0);
    let result = rt.block_on(event_loop(&mut app, &mut terminal, &mut rx, &mut pty_rx, outq, || {
        chrono::Local::now().naive_local()
    }));
    teardown(terminal, link)?;
    result
}

/// Run the TUI on the configured link until quit or hangup. `config_path` is
/// where `cfg` was loaded from (the deny rules protect it).
pub fn run(cfg: &Config, config_path: &std::path::Path, strict: bool) -> anyhow::Result<()> {
    check_geometry(cfg.terminal.cols, cfg.terminal.rows)?;
    let _log = init_logging()?;
    log_config_warnings(cfg);
    install_panic_hook();
    // Before the link is taken over, so a bad config entry reaches stderr.
    let tech = tech_startup(cfg, config_path)?;
    let Session {
        link,
        tx,
        mut rx,
        mut terminal,
        hints,
    } = open_session(cfg, strict)?;
    let (cols, rows) = (cfg.terminal.cols, cfg.terminal.rows);
    let shell_cfg = ShellConfig {
        command: cfg.shell.command.clone(),
        args: cfg.shell.args.clone(),
        scrollback: cfg.shell.scrollback,
    };
    // Bounded: when the loop falls behind (passthrough at 38400 baud), the
    // reader thread blocks, the PTY fills, and the program writing blocks too.
    let (pty_tx, mut pty_rx) = tokio::sync::mpsc::channel(PTY_QUEUE);
    let shell = ShellTab::new(shell_cfg, hints.clone(), move |e| {
        let _ = pty_tx.blocking_send(e);
    });
    let ship = ship_name(cfg);
    let chrome = ChromeView {
        ship_name: ship.clone(),
        year_offset: cfg.chrome.year_offset,
    };
    let slot = crate::metrics::SnapshotSlot::default();
    start_metrics(cfg, &tx, slot.clone());
    // One audit log for every writer: MU/TH/UR's tools and CARGO's commands.
    let audit = crate::ops::audit::shared(open_audit_log());
    let muthur = build_muthur(cfg, &chrome.ship_name, tx.clone(), slot, audit.clone(), tech)?;
    let cargo = build_cargo(tx.clone(), audit, hints);
    let mut app = App::assemble(chrome, muthur, UtilityTab::new(), cargo, shell);
    if cfg.chrome.splash {
        app = app.with_splash(splash_for(cfg, &ship));
        // Break replays the splash with fresh facts (year, interface readiness).
        let (cfg, ship) = (cfg.clone(), ship.clone());
        app = app.with_splash_factory(Box::new(move |now| {
            Splash::new(SplashInfo::from_config(&cfg, &ship, chrono::Local::now().naive_local()), now)
        }));
    }
    if cfg.chrome.standby_minutes > 0 {
        app = app.with_standby(Duration::from_secs(u64::from(cfg.chrome.standby_minutes) * 60));
    }
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

    teardown(terminal, link)?;
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
        let app = App::new(ChromeView {
            ship_name: "USCSS TEST".into(),
            year_offset: 111,
        });
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

    fn stale_splash() -> crate::splash::Splash {
        use crate::splash::{Interface, Splash, SplashInfo};
        let info = SplashInfo {
            ship_name: "USCSS TEST".into(),
            year: 2137,
            link: "38400 BAUD  XON/XOFF".into(),
            interface: Interface::Online,
        };
        let mut s = Splash::new(info, std::time::Instant::now() - Duration::from_secs(60));
        s.on_metrics(&Default::default());
        s
    }

    #[tokio::test]
    async fn natural_splash_finish_clears_once_then_paints_tui() {
        let (app, mut t) = setup();
        let mut app = app.with_splash(stale_splash());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let quit = async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('q')])).unwrap();
        };
        let mut pty = no_pty();
        let (r, _) = tokio::join!(event_loop(&mut app, &mut t, &mut rx, &mut pty, || 0, now), quit);
        r.unwrap();
        let out = String::from_utf8(t.backend().writer().clone()).unwrap();
        assert_eq!(out.matches("\x1b[2J").count(), 2, "initial clear + one handover clear");
        let (clear, chrome) = (out.rfind("\x1b[2J").unwrap(), out.find("17 NOV 2137 14:02").expect("tui painted"));
        assert!(chrome > clear, "TUI must be painted only after the clear");
        assert_eq!(out.matches('\x07').count(), 1, "one bell at READY.");
    }

    #[tokio::test]
    async fn quit_after_splash_returns_without_drawing_tui() {
        let (app, mut t) = setup();
        let mut app = app.with_splash(stale_splash());
        app.quit_after_splash = true;
        let (_tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let r = tokio::time::timeout(Duration::from_millis(500), event_loop(&mut app, &mut t, &mut rx, &mut no_pty(), || 0, now)).await;
        assert!(r.expect("loop did not exit").is_ok());
        let out = String::from_utf8(t.backend().writer().clone()).unwrap();
        assert!(!out.contains("17 NOV 2137 14:02"), "placeholder TUI must not be drawn");
        assert_eq!(out.matches('\x07').count(), 1, "muthur splash still rings before exiting");
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
            Ok(crate::metrics::Snapshot {
                host: format!("HOST{}", self.calls),
                ..Default::default()
            })
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
        spawn_metrics(
            Box::new(FakeSource {
                calls: 0,
                fail_every_other: false,
            }),
            Duration::from_millis(10),
            tx,
            Default::default(),
        );
        assert_eq!(next_metrics(&mut rx).unwrap().host, "HOST1");
        assert_eq!(next_metrics(&mut rx).unwrap().host, "HOST2");
    }

    #[test]
    fn collector_survives_errors() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_metrics(
            Box::new(FakeSource {
                calls: 0,
                fail_every_other: true,
            }),
            Duration::from_millis(10),
            tx,
            Default::default(),
        );
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
        spawn_metrics(Box::new(BrokenSource), Duration::from_millis(5), tx, Default::default());
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
        let mut app = crate::app::App::new(ChromeView {
            ship_name: "USCSS TEST".into(),
            year_offset: 111,
        })
        .with_tab(crate::app::TabId::Utility, Box::new(crate::utility::UtilityTab::new()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let snap = crate::metrics::Snapshot {
            host: "NOSTROMO".into(),
            cpu_cores: vec![1.0],
            ..Default::default()
        };
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
            crate::shell::ShellConfig {
                command: "sh".into(),
                args: vec!["-c".into(), "sleep 2".into()],
                scrollback: 10,
            },
            crate::scroll::HintSlot::default(),
            |_| {},
        );
        let mut app = App::assemble(
            ChromeView {
                ship_name: "USCSS TEST".into(),
                year_offset: 111,
            },
            m,
            crate::utility::UtilityTab::new(),
            crate::cargo::CargoTab::detached(),
            shell,
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ks = vec![Key::F(1)];
        ks.extend("HI".chars().map(Key::Char));
        ks.push(Key::Enter);
        tx.send(Event::Keys(ks)).unwrap();
        tx.send(Event::Llm {
            req_id: 1,
            ev: StreamEvent::Text("NOMINAL".into()),
        })
        .unwrap();
        tx.send(Event::Llm {
            req_id: 1,
            ev: StreamEvent::Done {
                stop: Stop::End,
                model: None,
                raw: None,
            },
        })
        .unwrap();
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

    #[test]
    fn collector_fills_snapshot_slot() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let slot = crate::metrics::SnapshotSlot::default();
        spawn_metrics(
            Box::new(FakeSource {
                calls: 0,
                fail_every_other: false,
            }),
            Duration::from_millis(10),
            tx,
            slot.clone(),
        );
        next_metrics(&mut rx).unwrap();
        assert_eq!(slot.lock().unwrap().as_ref().unwrap().host, "HOST1");
    }

    #[cfg(feature = "agent")]
    #[test]
    fn execute_tool_runs_fixed_tools() {
        let ctx = crate::tools::fixed::FixedCtx {
            runner: std::sync::Arc::new(crate::ops::runner::SystemRunner),
            snapshot: crate::metrics::SnapshotSlot::default(),
        };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let o = execute_tool(&crate::tools::Invocation::ShipStatus, &ctx, None, &cancel);
        assert_eq!(o.content, "TELEMETRY NOT YET AVAILABLE");
    }

    #[cfg(feature = "agent")]
    #[tokio::test]
    async fn tool_done_event_reaches_muthur() {
        use crate::llm::{Stop, StreamEvent};
        use crate::muthur::{MuthurDeps, MuthurTab};
        use crate::tools::ToolOutcome;
        let (_, mut t) = setup();
        let dir = tempfile::tempdir().unwrap();
        let log = crate::chatlog::ChatLog::open(dir.path().join("chat.jsonl")).unwrap();
        let cfg = crate::config::MuthurSection {
            api_key: Some("sk-test".into()),
            ..Default::default()
        };
        let spawned = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let sp = spawned.clone();
        let deps = MuthurDeps {
            spawn: Box::new(move |_, _, _| {
                sp.set(sp.get() + 1);
                Box::new(|| {})
            }),
            now: Box::new(std::time::Instant::now),
            ship_time: Box::new(|| "18 NOV 2137 14:02".into()),
            timestamp: Box::new(|| "2026-11-18T14:02:00Z".into()),
            run_tool: Box::new(|_, _, _, _, _| {}),
            review: Box::new(|_, _, _| Box::new(|| {})),
        };
        let tools = crate::tools::ToolDefs::persona(&Default::default());
        let audit = crate::ops::audit::AuditLog::open(dir.path().join("audit.jsonl")).unwrap();
        let tech = crate::app::tests::fake_tech(&cfg);
        let m = MuthurTab::new(cfg, tools, "USCSS TEST", log, crate::ops::audit::shared(audit), deps, tech);
        let shell = crate::shell::ShellTab::new(
            crate::shell::ShellConfig {
                command: "sh".into(),
                args: vec!["-c".into(), "sleep 2".into()],
                scrollback: 10,
            },
            crate::scroll::HintSlot::default(),
            |_| {},
        );
        let mut app = App::assemble(
            ChromeView {
                ship_name: "USCSS TEST".into(),
                year_offset: 111,
            },
            m,
            crate::utility::UtilityTab::new(),
            crate::cargo::CargoTab::detached(),
            shell,
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ks = vec![Key::F(1)];
        ks.extend("HI".chars().map(Key::Char));
        ks.push(Key::Enter);
        tx.send(Event::Keys(ks)).unwrap();
        let input = serde_json::json!({});
        tx.send(Event::Llm {
            req_id: 1,
            ev: StreamEvent::ToolCall {
                id: "c1".into(),
                name: "ship_status".into(),
                input: input.clone(),
            },
        })
        .unwrap();
        let raw = vec![serde_json::json!({"type":"tool_use","id":"c1","name":"ship_status","input":input})];
        tx.send(Event::Llm {
            req_id: 1,
            ev: StreamEvent::Done {
                stop: Stop::ToolUse,
                model: None,
                raw: Some(raw),
            },
        })
        .unwrap();
        let outcome = ToolOutcome {
            content: "UP 3D".into(),
            is_error: false,
            argv: None,
            exit: None,
            ms: 1,
            audit_output: None,
        };
        tx.send(Event::ToolDone {
            req_id: 1,
            call_id: "c1".into(),
            outcome,
        })
        .unwrap();
        tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('q')])).unwrap();
        event_loop(&mut app, &mut t, &mut rx, &mut no_pty(), || 0, now).await.unwrap();
        assert_eq!(spawned.get(), 2, "tool result did not trigger a second request");
    }

    #[cfg(feature = "agent")]
    #[tokio::test]
    async fn review_event_reaches_muthur() {
        use crate::llm::{Stop, StreamEvent};
        use crate::muthur::{MuthurDeps, MuthurTab};
        use crate::review::{Review, Risk, Verdict};
        let (_, mut t) = setup();
        let dir = tempfile::tempdir().unwrap();
        let log = crate::chatlog::ChatLog::open(dir.path().join("chat.jsonl")).unwrap();
        let cfg = crate::config::MuthurSection {
            api_key: Some("sk-test".into()),
            ..Default::default()
        };
        let reviewed = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let ran = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let (rv, rn) = (reviewed.clone(), ran.clone());
        let deps = MuthurDeps {
            spawn: Box::new(|_, _, _| Box::new(|| {})),
            now: Box::new(std::time::Instant::now),
            ship_time: Box::new(|| "18 NOV 2137 14:02".into()),
            timestamp: Box::new(|| "2026-11-18T14:02:00Z".into()),
            run_tool: Box::new(move |_, id, _, _, _| rn.borrow_mut().push(id)),
            review: Box::new(move |_, id, _| {
                rv.borrow_mut().push(id);
                Box::new(|| {})
            }),
        };
        let tools = crate::tools::ToolDefs::persona(&Default::default());
        let audit = crate::ops::audit::AuditLog::open(dir.path().join("audit.jsonl")).unwrap();
        let tech = crate::app::tests::fake_tech(&cfg);
        let m = MuthurTab::new(cfg, tools, "USCSS TEST", log, crate::ops::audit::shared(audit), deps, tech);
        let shell = crate::shell::ShellTab::new(
            crate::shell::ShellConfig {
                command: "sh".into(),
                args: vec!["-c".into(), "sleep 2".into()],
                scrollback: 10,
            },
            crate::scroll::HintSlot::default(),
            |_| {},
        );
        let mut app = App::assemble(
            ChromeView {
                ship_name: "USCSS TEST".into(),
                year_offset: 111,
            },
            m,
            crate::utility::UtilityTab::new(),
            crate::cargo::CargoTab::detached(),
            shell,
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(Event::Keys(vec![Key::F(1)])).unwrap();
        for line in ["/direct", "list the files"] {
            let mut ks: Vec<Key> = line.chars().map(Key::Char).collect();
            ks.push(Key::Enter);
            tx.send(Event::Keys(ks)).unwrap();
        }
        let input = serde_json::json!({"command": "ls", "reason": "asked", "as_root": false, "timeout_secs": 0});
        tx.send(Event::Llm {
            req_id: 1,
            ev: StreamEvent::ToolCall {
                id: "c1".into(),
                name: "run".into(),
                input: input.clone(),
            },
        })
        .unwrap();
        let raw = vec![serde_json::json!({"type":"tool_use","id":"c1","name":"run","input":input})];
        tx.send(Event::Llm {
            req_id: 1,
            ev: StreamEvent::Done {
                stop: Stop::ToolUse,
                model: None,
                raw: Some(raw),
            },
        })
        .unwrap();
        let review = Review {
            verdict: Verdict::Allow,
            risk: Risk::Low,
            note: "read-only".into(),
            model: "r".into(),
            ms: 1,
            error: None,
        };
        tx.send(Event::Review {
            req_id: 1,
            call_id: "c1".into(),
            review,
        })
        .unwrap();
        tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('q')])).unwrap();
        event_loop(&mut app, &mut t, &mut rx, &mut no_pty(), || 0, now).await.unwrap();
        assert_eq!(*reviewed.borrow(), ["c1"], "the call was not sent for review");
        assert_eq!(*ran.borrow(), ["c1"], "an allow verdict did not start the run");
    }

    #[cfg(feature = "agent")]
    fn deny_ctx_for_tests() -> crate::tools::deny::DenyCtx {
        crate::tools::deny::DenyCtx {
            config_path: "/home/op/.config/muthur/config.toml".into(),
            state_dir: "/home/op/.local/state/muthur".into(),
            exe: "/home/op/.local/bin/muthur".into(),
            operator: "op".into(),
        }
    }

    #[cfg(feature = "agent")]
    #[test]
    fn tech_policy_builds_deny_rules() {
        let agent = crate::config::AgentSection {
            deny: vec!["^shutdown".into()],
            net_allow: vec!["example.com".into()],
            ..Default::default()
        };
        let p = tech_policy(&agent, &deny_ctx_for_tests()).unwrap();
        assert_eq!(p.deny.as_ref().unwrap().check("shutdown -h now", true).as_deref(), Some("CONFIG RULE 1"));
        assert_eq!(p.net_allow, ["example.com"]);
    }

    #[cfg(feature = "agent")]
    #[test]
    fn tech_policy_rejects_bad_entries() {
        let agent = crate::config::AgentSection {
            deny: vec!["ok".into(), "(".into()],
            ..Default::default()
        };
        let e = format!("{:#}", tech_policy(&agent, &deny_ctx_for_tests()).err().unwrap());
        assert!(e.contains("deny entry 2"), "{e}");
        let agent = crate::config::AgentSection {
            net_allow: vec!["example.com".into(), "bad host!".into()],
            ..Default::default()
        };
        let e = format!("{:#}", tech_policy(&agent, &deny_ctx_for_tests()).err().unwrap());
        assert!(e.contains("net_allow") && e.contains("bad host!"), "{e}");
    }

    #[cfg(feature = "agent")]
    #[test]
    fn deny_ctx_uses_the_state_dir_and_config_path() {
        let c = deny_ctx(std::path::Path::new("/etc/muthur.toml"), Some(std::path::Path::new("/var/state")));
        assert_eq!(c.config_path, "/etc/muthur.toml");
        assert_eq!(c.state_dir, "/var/state/muthur");
        assert!(!c.operator.is_empty());
        let c = deny_ctx(std::path::Path::new("/etc/muthur.toml"), None);
        assert_eq!(c.state_dir, "", "no state dir: the pattern is skipped");
    }

    #[cfg(feature = "agent")]
    #[tokio::test]
    async fn review_dispatch_offline_when_not_ready() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let cfg = crate::config::MuthurSection {
            api_key: None,
            ..Default::default()
        };
        let mut review = review_dispatch(&cfg, false, tx);
        let input = crate::review::ReviewInput {
            operator_message: "x".into(),
            tool: "run".into(),
            args: serde_json::json!({}),
            unknown_target: false,
            prior: vec![],
            delegated_task: None,
        };
        let _abort = review(7, "c9".into(), input);
        match rx.try_recv() {
            Ok(Event::Review { req_id: 7, call_id, review }) => {
                assert_eq!(call_id, "c9");
                assert_eq!(review.note, "REVIEWER OFFLINE");
                assert_eq!(review.error.as_deref(), Some("not configured"));
            }
            _ => panic!("no immediate offline review"),
        }
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

    #[tokio::test]
    async fn nothing_is_drawn_while_the_line_settles() {
        let (app, mut t) = setup();
        let mut app = app.with_splash_factory(Box::new(|_| stale_splash()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let script = async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(Event::Keys(vec![Key::Ctrl('@')])).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            tx.send(Event::Keys(vec![Key::Char('x')])).unwrap(); // echo
            tokio::time::sleep(Duration::from_millis(100)).await;
            tx.send(Event::Hangup).unwrap(); // before the line has settled
        };
        let mut pty = no_pty();
        let (r, _) = tokio::join!(event_loop(&mut app, &mut t, &mut rx, &mut pty, || 0, now), script);
        r.unwrap();
        let out = String::from_utf8(t.backend().writer().clone()).unwrap();
        assert_eq!(out.matches("\x1b[2J").count(), 1, "only the initial clear: {out:?}");
        assert!(!out.contains("VESSEL"), "splash must wait for the line to settle");
    }

    #[tokio::test]
    async fn break_replays_splash_after_a_clear() {
        let (app, mut t) = setup();
        let mut app = app.with_splash_factory(Box::new(|_| stale_splash()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let script = async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(Event::Keys(vec![Key::Ctrl('@')])).unwrap();
            tokio::time::sleep(crate::app::BREAK_SETTLE + Duration::from_millis(200)).await;
            tx.send(Event::Keys(vec![Key::Ctrl(']'), Key::Char('q')])).unwrap();
        };
        let mut pty = no_pty();
        let (r, _) = tokio::join!(event_loop(&mut app, &mut t, &mut rx, &mut pty, || 0, now), script);
        r.unwrap();
        let out = String::from_utf8(t.backend().writer().clone()).unwrap();
        let first_tui = out.find("17 NOV 2137 14:02").expect("tui painted before Break");
        let splash = out.find("VESSEL").expect("splash replayed");
        assert!(splash > first_tui);
        let clear_before_splash = out[..splash].rfind("\x1b[2J").unwrap();
        assert!(clear_before_splash > first_tui, "Break clears before the splash");
        assert!(out[splash..].contains("17 NOV 2137 14:02"), "TUI repainted after the replay");
        let mut strict = crate::strict::StrictWriter::new(Vec::new(), Vt100Dialect);
        strict.write_all(out.as_bytes()).unwrap();
        assert!(strict.violations.is_empty(), "{:?}", strict.violations);
    }

    struct Scripted(std::collections::VecDeque<Vec<u8>>);
    impl ReadTimeout for Scripted {
        fn read_timeout(&mut self, buf: &mut [u8], _t: Duration) -> std::io::Result<usize> {
            match self.0.pop_front() {
                Some(b) => {
                    buf[..b.len()].copy_from_slice(&b);
                    Ok(b.len())
                }
                None => Err(std::io::Error::other("done")),
            }
        }
    }

    #[test]
    fn input_thread_calls_break_hook_and_forwards_key() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = calls.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let reader = Scripted(vec![b"a".to_vec(), vec![0x00]].into());
        spawn_input(reader, KeyParser::new(Keymap::wy60_default()), tx, move || {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
        .join()
        .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let mut keys = Vec::new();
        while let Ok(Event::Keys(k)) = rx.try_recv() {
            keys.extend(k);
        }
        assert_eq!(keys, vec![Key::Char('a'), Key::Ctrl('@')]);
    }
}
