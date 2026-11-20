//! Deterministic deny rules for agent shell commands, checked before review.

use regex::{Regex, RegexBuilder, escape};

pub struct DenyCtx {
    pub config_path: String,
    pub state_dir: String,
    pub exe: String,
    pub operator: String,
}

pub struct DenyRules {
    as_root: Regex,
    builtin: Vec<(&'static str, Vec<Regex>)>,
    extra: Vec<Regex>,
}

/// Command-word boundaries (the regex crate has no lookaround).
const B: &str = r#"(?:^|[\s;&|(`'"$])"#;
const A: &str = r"(?:$|[\s;&|)])";

fn w(word: &str) -> String {
    format!("{B}(?:{word}){A}")
}

fn build(p: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(p).case_insensitive(true).build()
}

impl DenyRules {
    pub fn new(ctx: &DenyCtx, extra: &[String]) -> Result<DenyRules, String> {
        let op = escape(&ctx.operator);
        let control = vec![
            "/etc/sudoers".to_string(),
            escape(&ctx.config_path),
            escape(&ctx.state_dir),
            escape(&ctx.exe),
            r"(sysusers|tmpfiles)\.d/muthur-agent".to_string(),
            r"nftables\.d/muthur-agent".to_string(),
            format!("{}.*(muthur-agent|{op})", w("usermod|gpasswd|passwd|userdel|chsh")),
            format!(r"{B}nft\W.*(flush|delete|muthur_agent)"),
        ];
        let lockout = vec![
            format!("serial-getty@wy60.*{}", w("stop|mask|disable|kill")),
            format!("{}.*serial-getty@wy60", w("stop|mask|disable|kill")),
            r"99-wy60\.rules".to_string(),
            "/dev/wy60".to_string(),
        ];
        let destructive = vec![
            format!(r"{B}rm\s(.*\s)?-[a-z]*[rf][a-z]*\s(.*\s)?(/\s*$|/\*|/[\s;&|])"),
            "--no-preserve-root".to_string(),
            format!(r"{B}mkfs(\.\w+)?{A}"),
            format!("{}.*of=/dev/(sd|nvme|vd|mmcblk)", w("dd")),
            format!(r"{B}wipefs\s"),
            r":\(\)\s*\{".to_string(),
        ];
        let compile = |v: Vec<String>| -> Result<Vec<Regex>, String> {
            v.iter().map(|p| build(p).map_err(|e| e.to_string())).collect()
        };
        let mut extra_re = Vec::new();
        for (i, p) in extra.iter().enumerate() {
            extra_re.push(build(p).map_err(|e| format!("bad [agent] deny entry {} ({p:?}): {e}", i + 1))?);
        }
        Ok(DenyRules {
            as_root: build(&w("sudo|su|doas|pkexec|runuser")).map_err(|e| e.to_string())?,
            builtin: vec![
                ("CONTROL PLANE", compile(control)?),
                ("LOCKOUT", compile(lockout)?),
                ("DESTRUCTIVE", compile(destructive)?),
            ],
            extra: extra_re,
        })
    }

    /// Name of the first rule the command trips, if any.
    pub fn check(&self, command: &str, as_root: bool) -> Option<String> {
        if !as_root && self.as_root.is_match(command) {
            return Some("USE AS_ROOT".into());
        }
        for (name, res) in &self.builtin {
            if res.iter().any(|r| r.is_match(command)) {
                return Some((*name).into());
            }
        }
        self.extra.iter().position(|r| r.is_match(command)).map(|i| format!("CONFIG RULE {}", i + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> DenyCtx {
        DenyCtx {
            config_path: "/home/op/.config/muthur/config.toml".into(),
            state_dir: "/home/op/.local/state/muthur".into(),
            exe: "/home/op/.local/bin/muthur".into(),
            operator: "op".into(),
        }
    }

    #[test]
    fn deny_table() {
        let rules = DenyRules::new(&ctx(), &["^shutdown".to_string()]).unwrap();
        let rows: &[(&str, bool, Option<&str>)] = &[
            ("sudo ls", false, Some("USE AS_ROOT")),
            ("SUDO ls", false, Some("USE AS_ROOT")),
            ("sudo ls", true, None),
            ("ls | su -", false, Some("USE AS_ROOT")),
            ("echo pseudocode", false, None),
            ("cat sudoku.txt", false, None),
            ("cat /etc/sudoers.d/x", true, Some("CONTROL PLANE")),
            ("tail /home/op/.local/state/muthur/audit.jsonl", false, Some("CONTROL PLANE")),
            ("usermod -aG docker muthur-agent", true, Some("CONTROL PLANE")),
            ("nft flush ruleset", true, Some("CONTROL PLANE")),
            ("systemctl stop serial-getty@wy60", true, Some("LOCKOUT")),
            ("systemctl status serial-getty@wy60", false, None),
            ("rm /etc/udev/rules.d/99-wy60.rules", true, Some("LOCKOUT")),
            ("rm -rf /", true, Some("DESTRUCTIVE")),
            ("rm -rf / ", true, Some("DESTRUCTIVE")),
            ("rm -rf /*", true, Some("DESTRUCTIVE")),
            ("rm -rf /tmp/x", true, None),
            ("mkfs.ext4 /dev/sdb1", true, Some("DESTRUCTIVE")),
            ("dd if=x of=/dev/nvme0n1", true, Some("DESTRUCTIVE")),
            (":(){ :|:& };:", false, Some("DESTRUCTIVE")),
            ("shutdown -h now", true, Some("CONFIG RULE 1")),
            ("ls -la", false, None),
        ];
        for (cmd, root, want) in rows {
            assert_eq!(rules.check(cmd, *root).as_deref(), *want, "{cmd:?} as_root={root}");
        }
    }

    #[test]
    fn bad_extra_regex_is_error() {
        let e = DenyRules::new(&ctx(), &["(".into()]).err().unwrap();
        assert!(e.contains("deny"), "{e}");
    }
}
