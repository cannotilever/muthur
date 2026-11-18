//! Configuration loading (`~/.config/muthur/config.toml`).

use anyhow::{Context, bail};
use serde::Deserialize;
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

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub link: LinkConfig,
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
    fn world_readable_file_is_rejected() {
        let (_d, p) = write_cfg("", 0o644);
        let err = load(&p).unwrap_err().to_string();
        assert!(err.contains("chmod 600"), "{err}");
    }
}
