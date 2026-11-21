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
        Self {
            device: None,
            baud: 38400,
            flow: FlowControl::XonXoff,
        }
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
    /// Play the boot splash on launch.
    pub splash: bool,
}

impl Default for ChromeConfig {
    fn default() -> Self {
        Self {
            ship_name: None,
            year_offset: 111,
            splash: true,
        }
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
        Self {
            command: "fish".into(),
            args: Vec::new(),
            scrollback: 1000,
        }
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
    /// Overrides for the technical agent; unset keys inherit from `[muthur]`.
    pub technical: LlmOverrides,
    /// Overrides for the command reviewer; unset keys inherit from `[muthur]`.
    pub reviewer: LlmOverrides,
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
            technical: LlmOverrides::default(),
            reviewer: LlmOverrides::default(),
        }
    }
}

/// Per-agent overrides of the connection and budget keys of `[muthur]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct LlmOverrides {
    pub provider: Option<Provider>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub effort: Option<String>,
    pub cache_ttl: Option<String>,
    pub max_tokens: Option<u32>,
    pub fallbacks: Option<bool>,
    pub context_tokens: Option<usize>,
}

impl MuthurSection {
    fn with_overrides(&self, o: &LlmOverrides) -> MuthurSection {
        fn set(v: &Option<String>) -> Option<&str> {
            v.as_deref().filter(|s| !s.trim().is_empty())
        }
        let mut c = self.clone();
        if let Some(v) = o.provider {
            c.provider = v;
        }
        if let Some(v) = set(&o.base_url) {
            c.base_url = Some(v.to_string());
        }
        if let Some(v) = set(&o.model) {
            c.model = Some(v.to_string());
        }
        if let Some(v) = set(&o.api_key) {
            c.api_key = Some(v.to_string());
        }
        if let Some(v) = set(&o.effort) {
            c.effort = v.to_string();
        }
        if let Some(v) = set(&o.cache_ttl) {
            c.cache_ttl = v.to_string();
        }
        if let Some(v) = o.max_tokens {
            c.max_tokens = v;
        }
        if let Some(v) = o.fallbacks {
            c.fallbacks = v;
        }
        if let Some(v) = o.context_tokens {
            c.context_tokens = v;
        }
        c.system_prompt = None;
        c.technical = LlmOverrides::default();
        c.reviewer = LlmOverrides::default();
        c
    }

    /// The effective settings for the technical agent.
    pub fn technical_cfg(&self) -> MuthurSection {
        self.with_overrides(&self.technical)
    }

    /// The effective settings for the reviewer: short replies, and Haiku on Anthropic.
    pub fn reviewer_cfg(&self) -> MuthurSection {
        let mut c = self.with_overrides(&self.reviewer);
        if self.reviewer.max_tokens.is_none() {
            c.max_tokens = 512;
        }
        if c.provider == Provider::Anthropic && self.reviewer.model.as_deref().is_none_or(|m| m.trim().is_empty()) {
            c.model = Some("claude-haiku-5-5".into());
        }
        c
    }

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

/// How the persona presents the technical agent's report after a delegation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DelegateReport {
    #[default]
    Relayed,
    Verbatim,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ToolsSection {
    pub enabled: bool,
    pub max_rounds: u32,
    pub delegate_report: DelegateReport,
}

impl Default for ToolsSection {
    fn default() -> Self {
        Self {
            enabled: true,
            max_rounds: 8,
            delegate_report: DelegateReport::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AgentSection {
    pub label: String,
    pub max_rounds: u32,
    pub default_timeout_secs: u32,
    pub max_timeout_secs: u32,
    pub deny: Vec<String>,
    pub net_allow: Vec<String>,
    pub system_prompt: Option<String>,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            label: "SCIENCE OFFICER".into(),
            max_rounds: 24,
            default_timeout_secs: 30,
            max_timeout_secs: 300,
            deny: Vec::new(),
            net_allow: Vec::new(),
            system_prompt: None,
        }
    }
}

impl AgentSection {
    /// The system prompt, or `None` when unset or blank.
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref().filter(|s| !s.trim().is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub agent: AgentSection,
    pub shell: ShellSection,
    pub utility: UtilitySection,
    pub muthur: MuthurSection,
    pub tools: ToolsSection,
    pub link: LinkConfig,
    pub terminal: TerminalConfig,
    pub chrome: ChromeConfig,
    /// Key-name → byte-sequence overrides for the WY-60 keymap.
    pub keys: BTreeMap<String, String>,
    /// Startup warnings found by `load`. They are logged once logging is up,
    /// which is after the config is read.
    #[serde(skip)]
    pub warnings: Vec<String>,
}

impl Config {
    pub fn validate(&self) -> anyhow::Result<()> {
        // Without the agent build `[agent]` is ignored, so it cannot be wrong.
        if !cfg!(feature = "agent") {
            return Ok(());
        }
        let a = &self.agent;
        if !(1..=3600).contains(&a.max_timeout_secs) {
            bail!("[agent] max_timeout_secs must be 1..=3600 (got {})", a.max_timeout_secs);
        }
        if !(1..=a.max_timeout_secs).contains(&a.default_timeout_secs) {
            bail!(
                "[agent] default_timeout_secs must be 1..=max_timeout_secs (got {} with max {})",
                a.default_timeout_secs,
                a.max_timeout_secs
            );
        }
        Ok(())
    }
}

/// Startup warnings for the raw config text: removed keys, and sections ignored
/// because the `agent` feature is not built.
pub fn warnings(text: &str, agent_built: bool) -> Vec<String> {
    let Ok(v) = toml::from_str::<toml::Value>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let tools = v.get("tools");
    for key in ["services", "elevate", "journal_max_lines"] {
        if tools.and_then(|t| t.get(key)).is_some() {
            out.push(format!("removed config key [tools.{key}] ignored"));
        }
    }
    if !agent_built {
        let muthur = v.get("muthur");
        let present = tools.and_then(|t| t.get("enabled")).and_then(|e| e.as_bool()) == Some(true)
            || tools.and_then(|t| t.get("delegate_report")).is_some()
            || v.get("agent").is_some()
            || muthur.and_then(|m| m.get("technical")).is_some()
            || muthur.and_then(|m| m.get("reviewer")).is_some();
        if present {
            out.push("agent feature not built; [tools] [agent] [muthur.technical] [muthur.reviewer] ignored".into());
        }
    }
    out
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
    let mut cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    cfg.warnings = warnings(&text, cfg!(feature = "agent"));
    cfg.validate().with_context(|| format!("validating {}", path.display()))?;
    Ok(cfg)
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
    fn agent_defaults() {
        let a = Config::default().agent;
        assert_eq!(a.label, "SCIENCE OFFICER");
        assert_eq!((a.max_rounds, a.default_timeout_secs, a.max_timeout_secs), (24, 30, 300));
        assert!(a.deny.is_empty() && a.net_allow.is_empty() && a.system_prompt().is_none());
        let b = AgentSection {
            system_prompt: Some("  ".into()),
            ..AgentSection::default()
        };
        assert_eq!(b.system_prompt(), None);
    }

    #[test]
    fn technical_inherits_and_overrides() {
        let c: Config = toml::from_str(
            "[muthur]\nmodel = \"claude-sonnet-5-5\"\napi_key = \"k\"\nsystem_prompt = \"p\"\n[muthur.technical]\nmodel = \"local\"\nprovider = \"openai\"\nbase_url = \"http://h\"\n",
        )
        .unwrap();
        let t = c.muthur.technical_cfg();
        assert_eq!(t.model.as_deref(), Some("local"));
        assert_eq!(t.provider, Provider::Openai);
        assert_eq!(t.base_url.as_deref(), Some("http://h"));
        assert_eq!(t.api_key.as_deref(), Some("k"));
        assert_eq!(t.system_prompt, None);
        assert_eq!(t.technical, LlmOverrides::default());
    }

    #[test]
    fn reviewer_defaults_to_haiku_on_anthropic() {
        let r = Config::default().muthur.reviewer_cfg();
        assert_eq!(r.model.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(r.max_tokens, 512);
        let c: Config = toml::from_str("[muthur]\nprovider = \"openai\"\nmodel = \"q\"\nbase_url = \"http://h\"\n").unwrap();
        assert_eq!(c.muthur.reviewer_cfg().model.as_deref(), Some("q"));
    }

    #[test]
    fn blank_overrides_inherit() {
        let c: Config = toml::from_str(
            "[muthur]\nmodel = \"m\"\napi_key = \"k\"\nsystem_prompt = \"p\"\n[muthur.technical]\nmodel = \"\"\napi_key = \"  \"\n[muthur.reviewer]\nmodel = \" \"\nmax_tokens = 1000\n",
        )
        .unwrap();
        let t = c.muthur.technical_cfg();
        assert_eq!(t.model.as_deref(), Some("m"));
        assert_eq!(t.api_key.as_deref(), Some("k"));
        assert_eq!(t.system_prompt, None);
        let r = c.muthur.reviewer_cfg();
        assert_eq!(r.model.as_deref(), Some("claude-haiku-5-5"));
        assert_eq!(r.max_tokens, 1000);
    }

    #[test]
    fn warnings_ignore_invalid_toml() {
        assert!(warnings("not [toml", true).is_empty());
    }

    #[test]
    fn validate_rejects_bad_timeouts() {
        for (d, m) in [(30, 0), (30, 3601), (301, 300), (0, 300)] {
            let mut c = Config::default();
            c.agent.default_timeout_secs = d;
            c.agent.max_timeout_secs = m;
            // Only the agent build checks the section it uses.
            assert_eq!(c.validate().is_err(), cfg!(feature = "agent"), "{d} {m}");
        }
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn warnings_for_removed_and_unbuilt() {
        let w = warnings("[tools.services]\nrestart=[]\n[tools]\nelevate=\"sudo\"\n", true);
        assert_eq!(w.len(), 2);
        assert!(w.iter().any(|l| l.contains("tools.services")));
        assert!(w.iter().any(|l| l.contains("tools.elevate")));
        let w = warnings("[agent]\nlabel=\"X\"\n", false);
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("agent feature not built"));
        assert!(warnings("[agent]\nlabel=\"X\"\n", true).is_empty());
    }

    /// The sections of the agent build warn only in a build without it, and
    /// load either way: one config file serves both builds.
    #[test]
    fn agent_sections_warn_when_not_built() {
        let text = "[tools]\nenabled = true\n[agent]\nlabel = \"X\"\n[muthur.technical]\nmodel = \"m\"\n[muthur.reviewer]\nmodel = \"r\"\n";
        let w = warnings(text, cfg!(feature = "agent"));
        if cfg!(feature = "agent") {
            assert!(w.is_empty(), "{w:?}");
        } else {
            assert_eq!(w, ["agent feature not built; [tools] [agent] [muthur.technical] [muthur.reviewer] ignored"]);
        }
        let (_d, p) = write_cfg(text, 0o600);
        let c = load(&p).unwrap();
        assert_eq!(c.agent.label, "X");
        assert_eq!(c.warnings, w, "load keeps the warnings for logging");
    }

    #[test]
    fn tools_section_parses_and_defaults() {
        let d = Config::default().tools;
        assert!(d.enabled && d.max_rounds == 8);
        // Removed M5 keys are ignored (and reported by `warnings`).
        let (_d, p) = write_cfg(
            "[tools]\nenabled = false\nmax_rounds = 3\nelevate = \"doas\"\njournal_max_lines = 10\n[tools.services]\nrestart = [\"nginx\"]\n",
            0o600,
        );
        let t = load(&p).unwrap().tools;
        assert_eq!(
            t,
            ToolsSection {
                enabled: false,
                max_rounds: 3,
                ..Default::default()
            }
        );
    }

    #[test]
    fn delegate_report_default_relayed() {
        assert_eq!(ToolsSection::default().delegate_report, DelegateReport::Relayed);
    }

    #[test]
    fn delegate_report_parses_verbatim() {
        let (_d, p) = write_cfg("[tools]\ndelegate_report = \"verbatim\"\n", 0o600);
        assert_eq!(load(&p).unwrap().tools.delegate_report, DelegateReport::Verbatim);
    }

    #[test]
    fn delegate_report_bad_value_is_error() {
        let (_d, p) = write_cfg("[tools]\ndelegate_report = \"loud\"\n", 0o600);
        assert!(load(&p).is_err());
    }

    #[test]
    fn warnings_cover_delegate_report() {
        let text = "[tools]\ndelegate_report = \"verbatim\"\n";
        let w = warnings(text, false);
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("agent feature not built"));
        assert!(warnings(text, true).is_empty());
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
            (
                m.max_tokens,
                m.effort.as_str(),
                m.fallbacks,
                m.cache_ttl.as_str(),
                m.context_tokens,
                m.reveal_cps,
                m.uppercase
            ),
            (4096, "low", true, "5m", 200000, 50, true)
        );
        assert_eq!(m.readiness(), Err(LlmError::NoAccessCode));
    }

    #[test]
    fn whitespace_api_key_is_missing() {
        let m = MuthurSection {
            api_key: Some(" \t\n".into()),
            ..Default::default()
        };
        assert_eq!(m.readiness(), Err(LlmError::NoAccessCode));
    }

    #[test]
    fn muthur_openai_section_parses() {
        let (_d, p) = write_cfg(
            "[muthur]\nprovider = \"openai\"\nbase_url = \"http://10.0.0.5:8000/\"\nmodel = \"qwen\"\n",
            0o600,
        );
        let c = load(&p).unwrap();
        assert_eq!(c.muthur.base_url(), Some("http://10.0.0.5:8000"));
        assert_eq!(c.muthur.readiness(), Ok(()));
    }

    #[test]
    fn openai_without_url_is_not_configured() {
        let m = MuthurSection {
            provider: Provider::Openai,
            ..Default::default()
        };
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
        let m = MuthurSection {
            model: Some("".into()),
            ..Default::default()
        };
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

    #[test]
    fn splash_defaults_on_and_can_be_disabled() {
        assert!(ChromeConfig::default().splash);
        let (_d, p) = write_cfg("[chrome]\nsplash = false\n", 0o600);
        assert!(!load(&p).unwrap().chrome.splash);
    }
}
