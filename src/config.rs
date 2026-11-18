//! Configuration loading (`~/.config/muthur/config.toml`).

use crate::llm::LlmError;
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlowControl {
    #[default]
    XonXoff,
    RtsCts,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LinkConfig {
    /// Serial device; `None` means use stdio (e.g. when launched under agetty).
    pub device: Option<PathBuf>,
    pub baud: u32,
    pub flow: FlowControl,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self { device: None, baud: 38400, flow: FlowControl::XonXoff }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self { cols: 80, rows: 43 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ChromeConfig {
    /// Title-bar ship name; `None` means `"USCSS <HOSTNAME>"`.
    pub ship_name: Option<String>,
    /// Added to the real year for the in-universe date (2026 + 111 = 2137).
    pub year_offset: i32,
}

impl Default for ChromeConfig {
    fn default() -> Self {
        Self { ship_name: None, year_offset: 111 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ShellSection {
    pub command: String,
    pub args: Vec<String>,
    /// Lines of history kept for `^] [`.
    pub scrollback: usize,
}

impl Default for ShellSection {
    fn default() -> Self {
        Self { command: "fish".into(), args: Vec::new(), scrollback: 1000 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct UtilitySection {
    pub interval_secs: u64,
    pub containers: crate::metrics::local::ContainerRuntime,
    /// Filesystem types left out of the DISKS panel.
    pub exclude_fs: Vec<String>,
}

impl Default for UtilitySection {
    fn default() -> Self {
        Self {
            interval_secs: 2,
            containers: crate::metrics::local::ContainerRuntime::Auto,
            exclude_fs: crate::metrics::local::default_exclude(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Anthropic,
    Openai,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct MuthurSection {
    pub provider: Provider,
    /// Endpoint root; `None` picks the provider default (Anthropic only).
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub max_tokens: u32,
    pub effort: String,
    pub fallbacks: bool,
    pub cache_ttl: String,
    pub context_tokens: usize,
    pub reveal_cps: u32,
    pub uppercase: bool,
    pub system_prompt: Option<String>,
}

impl Default for MuthurSection {
    fn default() -> Self {
        Self {
            provider: Provider::Anthropic,
            base_url: None,
            model: None,
            api_key: None,
            max_tokens: 4096,
            effort: "low".into(),
            fallbacks: true,
            cache_ttl: "5m".into(),
            context_tokens: 200_000,
            reveal_cps: 50,
            uppercase: true,
            system_prompt: None,
        }
    }
}

impl MuthurSection {
    /// Explicit `base_url` (trimmed, trailing '/' removed), else the Anthropic default;
    /// a blank explicit value counts as unset. `None` for Openai.
    pub fn base_url(&self) -> Option<&str> {
        match self.base_url.as_deref().map(|u| u.trim().trim_end_matches('/')) {
            Some(url) if !url.is_empty() => Some(url),
            _ => match self.provider {
                Provider::Anthropic => Some("https://api.anthropic.com"),
                Provider::Openai => None,
            },
        }
    }

    /// Explicit `model` (trimmed), else the Anthropic default; a blank explicit value
    /// counts as unset. `None` for Openai.
    pub fn model(&self) -> Option<&str> {
        match self.model.as_deref().map(str::trim) {
            Some(m) if !m.is_empty() => Some(m),
            _ => match self.provider {
                Provider::Anthropic => Some("claude-sonnet-5-5"),
                Provider::Openai => None,
            },
        }
    }

    /// The API key (trimmed), or `None` when unset or blank.
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref().map(str::trim).filter(|k| !k.is_empty())
    }

    /// The system prompt, or `None` when unset or blank.
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref().filter(|s| !s.trim().is_empty())
    }

    /// Whether the configured provider has what it needs to make a request.
    pub fn readiness(&self) -> Result<(), LlmError> {
        match self.provider {
            Provider::Anthropic => match self.api_key() {
                Some(_) => Ok(()),
                None => Err(LlmError::NoAccessCode),
            },
            Provider::Openai => match (self.base_url(), self.model()) {
                (Some(_), Some(_)) => Ok(()),
                _ => Err(LlmError::NotConfigured),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub shell: ShellSection,
    pub utility: UtilitySection,
    pub muthur: MuthurSection,
    pub link: LinkConfig,
    pub terminal: TerminalConfig,
    pub chrome: ChromeConfig,
    /// Key-name → byte-sequence overrides for the WY-60 keymap.
    pub keys: BTreeMap<String, String>,
}

pub fn default_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".config")).join("muthur/config.toml")
}

/// Load config from `path`. A missing file yields defaults. The file will hold
/// secrets (API key), so it must not be group/world accessible.
pub fn load(path: &Path) -> anyhow::Result<Config> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mode = meta.permissions().mode();
    if mode & 0o077 != 0 {
        bail!(
            "{} is accessible by other users (mode {:o}); run: chmod 600 {}",
            path.display(),
            mode & 0o777,
            path.display()
        );
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn write_cfg(contents: &str, mode: u32) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, contents).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        (dir, p)
    }

    /// docs/config.example.toml must parse and, apart from the keys it spells
    /// out explicitly, show exactly the defaults.
    #[test]
    fn example_config_shows_defaults() {
        let text = include_str!("../docs/config.example.toml");
        let got: Config = toml::from_str(text).unwrap();
        let mut want = Config::default();
        want.muthur.base_url = Some("https://api.anthropic.com".into());
        want.muthur.model = Some("claude-sonnet-5-5".into());
        want.muthur.api_key = Some("sk-ant-REPLACE-ME".into());
        assert_eq!(got, want);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = load(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(c.link.baud, 38400);
        assert_eq!(c.link.flow, FlowControl::XonXoff);
        assert_eq!(c.link.device, None);
    }

    #[test]
    fn partial_file_parses_with_defaults() {
        let (_d, p) = write_cfg("[link]\ndevice = \"/dev/wy60\"\nflow = \"rts-cts\"\n", 0o600);
        let c = load(&p).unwrap();
        assert_eq!(c.link.device.as_deref(), Some(Path::new("/dev/wy60")));
        assert_eq!(c.link.flow, FlowControl::RtsCts);
        assert_eq!(c.link.baud, 38400);
    }

    #[test]
    fn terminal_and_chrome_defaults() {
        let c = Config::default();
        assert_eq!((c.terminal.cols, c.terminal.rows), (80, 43));
        assert_eq!(c.chrome.year_offset, 111);
        assert!(c.chrome.ship_name.is_none() && c.keys.is_empty());
        assert_eq!((c.shell.command.as_str(), c.shell.scrollback), ("fish", 1000));
    }

    #[test]
    fn keys_section_parses_escapes() {
        let (_d, p) = write_cfg("[keys]\nF1 = \"\\u001b[11~\"\n", 0o600);
        assert_eq!(load(&p).unwrap().keys["F1"], "\x1b[11~");
    }

    #[test]
    fn utility_defaults() {
        let c = Config::default();
        assert_eq!(c.utility.interval_secs, 2);
        assert_eq!(c.utility.containers, crate::metrics::local::ContainerRuntime::Auto);
        assert!(c.utility.exclude_fs.iter().any(|f| f == "tmpfs"));
    }

    #[test]
    fn utility_section_parses() {
        let (_d, p) = write_cfg("[utility]\ninterval_secs = 5\ncontainers = \"podman\"\n", 0o600);
        let c = load(&p).unwrap();
        assert_eq!(c.utility.interval_secs, 5);
        assert_eq!(c.utility.containers, crate::metrics::local::ContainerRuntime::Podman);
    }

    #[test]
    fn muthur_defaults() {
        let m = MuthurSection::default();
        assert_eq!(m.provider, Provider::Anthropic);
        assert_eq!(m.base_url(), Some("https://api.anthropic.com"));
        assert_eq!(m.model(), Some("claude-sonnet-5-5"));
        assert_eq!(
            (m.max_tokens, m.effort.as_str(), m.fallbacks, m.cache_ttl.as_str(), m.context_tokens, m.reveal_cps, m.uppercase),
            (4096, "low", true, "5m", 200000, 50, true)
        );
        assert_eq!(m.readiness(), Err(LlmError::NoAccessCode));
    }

    #[test]
    fn whitespace_api_key_is_missing() {
        let m = MuthurSection { api_key: Some(" \t\n".into()), ..Default::default() };
        assert_eq!(m.readiness(), Err(LlmError::NoAccessCode));
    }

    #[test]
    fn muthur_openai_section_parses() {
        let (_d, p) = write_cfg("[muthur]\nprovider = \"openai\"\nbase_url = \"http://10.0.0.5:8000/\"\nmodel = \"qwen\"\n", 0o600);
        let c = load(&p).unwrap();
        assert_eq!(c.muthur.base_url(), Some("http://10.0.0.5:8000"));
        assert_eq!(c.muthur.readiness(), Ok(()));
    }

    #[test]
    fn openai_without_url_is_not_configured() {
        let m = MuthurSection { provider: Provider::Openai, ..Default::default() };
        assert_eq!(m.readiness(), Err(LlmError::NotConfigured));
    }

    #[test]
    fn blank_openai_base_url_is_not_configured() {
        for url in ["", "   ", "/"] {
            let m = MuthurSection {
                provider: Provider::Openai,
                base_url: Some(url.into()),
                model: Some("qwen".into()),
                ..Default::default()
            };
            assert_eq!(m.readiness(), Err(LlmError::NotConfigured), "base_url {url:?}");
        }
    }

    #[test]
    fn blank_openai_model_is_not_configured() {
        let m = MuthurSection {
            provider: Provider::Openai,
            base_url: Some("http://10.0.0.5:8000".into()),
            model: Some("  ".into()),
            ..Default::default()
        };
        assert_eq!(m.readiness(), Err(LlmError::NotConfigured));
    }

    #[test]
    fn blank_anthropic_model_falls_back_to_default() {
        let m = MuthurSection { model: Some("".into()), ..Default::default() };
        assert_eq!(m.model(), Some("claude-sonnet-5-5"));
    }

    #[test]
    fn system_prompt_blank_counts_as_none() {
        let mut m = MuthurSection::default();
        assert_eq!(m.system_prompt(), None);
        m.system_prompt = Some("".into());
        assert_eq!(m.system_prompt(), None);
        m.system_prompt = Some("   ".into());
        assert_eq!(m.system_prompt(), None);
        m.system_prompt = Some("Be terse.".into());
        assert_eq!(m.system_prompt(), Some("Be terse."));
    }

    #[test]
    fn world_readable_file_is_rejected() {
        let (_d, p) = write_cfg("", 0o644);
        let err = load(&p).unwrap_err().to_string();
        assert!(err.contains("chmod 600"), "{err}");
    }
}
