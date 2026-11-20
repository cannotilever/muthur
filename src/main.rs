use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use muthur::config;
use muthur::link::{Link, install_panic_hook};
use muthur::testcard::{card, flood, keys};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "muthur", about = "MU/TH/UR 6000 serial console for the Wyse WY-60")]
struct Cli {
    /// Config file (default: ~/.config/muthur/config.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Serial device; overrides the config. Without one, stdio is used.
    #[arg(long, global = true)]
    device: Option<PathBuf>,
    /// Baud rate for --device; overrides the config.
    #[arg(long, global = true)]
    baud: Option<u32>,
    /// Screen columns; overrides the config.
    #[arg(long, global = true)]
    cols: Option<u16>,
    /// Screen rows; overrides the config.
    #[arg(long, global = true)]
    rows: Option<u16>,
    /// Check every byte sent against the VT100 allowlist; log violations.
    #[arg(long, global = true)]
    strict: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Play the boot splash once and exit (for tuning the logo).
    Splash,
    /// Hardware probes: attribute card (default), flow-control flood, key echo.
    Testcard(TestcardArgs),
}

#[derive(Args)]
struct TestcardArgs {
    #[command(subcommand)]
    mode: Option<TestcardMode>,
}

#[derive(Subcommand)]
enum TestcardMode {
    /// Stream numbered pattern lines; a shifted alphabet means bytes were dropped.
    Flood {
        #[arg(long, default_value_t = 2000)]
        lines: u32,
    },
    /// Show and record the bytes each key sends. ^] ^] exits.
    Keys {
        #[arg(long, default_value = "muthur-keys.txt")]
        out: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(config::default_path);
    let mut cfg = config::load(&config_path)?;
    if let Some(d) = cli.device {
        cfg.link.device = Some(d);
    }
    if let Some(b) = cli.baud {
        cfg.link.baud = b;
    }
    if let Some(c) = cli.cols {
        cfg.terminal.cols = c;
    }
    if let Some(r) = cli.rows {
        cfg.terminal.rows = r;
    }
    let (cols, rows) = (cfg.terminal.cols, cfg.terminal.rows);
    let tc = match cli.command {
        None => return muthur::run::run(&cfg, &config_path, cli.strict),
        Some(Command::Splash) => return muthur::run::run_splash(&cfg, cli.strict),
        Some(Command::Testcard(tc)) => tc,
    };
    if rows < card::MIN_ROWS {
        bail!("--rows must be at least {}", card::MIN_ROWS);
    }

    install_panic_hook();
    let mut link = match cfg.link.device {
        Some(_) => Link::open_device(&cfg.link)?,
        None => Link::stdio(cfg.link.flow)?,
    };

    match tc.mode {
        None => {
            link.write_all(&card::card(cols, rows))?;
            link.flush()?;
            keys::read_key(&mut link, keys::KEY_GAP)?;
            link.write_all(b"\x1b[0m\x1b(B\x1b[r\x1b[2J\x1b[H")?;
        }
        Some(TestcardMode::Flood { lines }) => {
            link.write_all(b"\x1b[0m\x1b(B\x1b[r\x1b[2J\x1b[H")?;
            link.write_all(&flood::flood(cols, lines))?;
        }
        Some(TestcardMode::Keys { out }) => {
            let mut log = std::fs::File::create(&out).with_context(|| format!("creating {}", out.display()))?;
            keys::echo_loop(&mut link, &mut log)?;
            link.write_all(b"\r\nKEYS WRITTEN TO ")?;
            link.write_all(out.display().to_string().as_bytes())?;
            link.write_all(b"\r\n")?;
        }
    }
    link.flush()?;
    Ok(())
}
