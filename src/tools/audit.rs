//! Append-only JSONL audit log of every tool call.
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Auto,
    Confirmed,
    Denied,
    Rejected,
    Aborted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AuditEntry {
    pub tool: String,
    pub args: serde_json::Value,
    pub tier: u8,
    pub decision: Decision,
    pub argv: Option<Vec<String>>,
    pub exit: Option<serde_json::Value>,
    pub ms: u64,
    pub output: String,
}

pub struct AuditLog {
    path: Option<PathBuf>,
    needs_newline: bool,
    online: bool,
}

#[derive(serde::Serialize)]
struct Record<'a> {
    ts: &'a str,
    tool: &'a str,
    args: &'a serde_json::Value,
    tier: u8,
    decision: Decision,
    argv: &'a Option<Vec<String>>,
    exit: &'a Option<serde_json::Value>,
    ms: u64,
    output: &'a str,
}

impl AuditLog {
    /// Creates the parent directory and opens (creating 0600) the file for
    /// appending; fails if it cannot, so the caller falls back to `offline()`.
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&path)?;
        let needs_newline = match std::fs::read(&path) {
            Ok(bytes) => bytes.last().is_some_and(|b| *b != b'\n'),
            Err(_) => false,
        };
        Ok(Self { path: Some(path), needs_newline, online: true })
    }

    pub fn offline() -> Self {
        Self { path: None, needs_newline: false, online: false }
    }

    pub fn is_online(&self) -> bool {
        self.online
    }

    pub fn append(&mut self, e: &AuditEntry, ts: &str) -> std::io::Result<()> {
        let r = self.write(e, ts);
        self.online = r.is_ok();
        r
    }

    fn write(&mut self, e: &AuditEntry, ts: &str) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Err(std::io::Error::other("audit log offline"));
        };
        let rec = Record {
            ts,
            tool: &e.tool,
            args: &e.args,
            tier: e.tier,
            decision: e.decision,
            argv: &e.argv,
            exit: &e.exit,
            ms: e.ms,
            output: &e.output,
        };
        let mut line = String::new();
        if self.needs_newline {
            line.push('\n');
        }
        line.push_str(&serde_json::to_string(&rec).map_err(std::io::Error::other)?);
        line.push('\n');
        let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
        if let Err(e) = f.write_all(line.as_bytes()) {
            // A partial line may be on disk; start the next record on a fresh line.
            self.needs_newline = true;
            return Err(e);
        }
        self.needs_newline = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn entry(tool: &str) -> AuditEntry {
        AuditEntry {
            tool: tool.into(),
            args: serde_json::json!({"unit": "a.service"}),
            tier: 1,
            decision: Decision::Auto,
            argv: Some(vec!["systemctl".into(), "status".into()]),
            exit: None,
            ms: 12,
            output: "ok".into(),
        }
    }

    #[test]
    fn writes_records_0600() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("audit.jsonl");
        let mut l = AuditLog::open(p.clone()).unwrap();
        assert!(l.is_online());
        l.append(&entry("a"), "t1").unwrap();
        l.append(&entry("b"), "t2").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        let mut want = vec!["ts", "tool", "args", "tier", "decision", "argv", "exit", "ms", "output"];
        want.sort();
        assert_eq!(sorted, want);
        assert_eq!(v["decision"], "auto");
        assert!(v["exit"].is_null());
        assert!(lines[0].starts_with("{\"ts\":\"t1\",\"tool\":\"a\""));
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn torn_tail_starts_fresh_line() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audit.jsonl");
        std::fs::write(&p, "{\"ts\":\"t\",\"to").unwrap();
        let mut l = AuditLog::open(p.clone()).unwrap();
        l.append(&entry("a"), "t").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(serde_json::from_str::<serde_json::Value>(lines[1]).is_ok());
    }

    #[test]
    fn open_fails_when_file_unwritable() {
        let d = tempfile::tempdir().unwrap();
        // The path is a directory: it cannot be opened for appending.
        let p = d.path().join("audit.jsonl");
        std::fs::create_dir(&p).unwrap();
        assert!(AuditLog::open(p).is_err());
        // A read-only existing file.
        let p = d.path().join("ro.jsonl");
        std::fs::write(&p, "").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o400)).unwrap();
        if std::fs::OpenOptions::new().append(true).open(&p).is_err() {
            // Not root: the permission bits are enforced.
            assert!(AuditLog::open(p).is_err());
        }
    }

    #[test]
    fn open_creates_file_0600() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audit.jsonl");
        AuditLog::open(p.clone()).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn offline_fails_and_reports() {
        let mut l = AuditLog::offline();
        assert!(!l.is_online());
        assert!(l.append(&entry("a"), "t").is_err());
        assert!(!l.is_online());
    }

    #[test]
    fn failed_write_marks_offline() {
        let d = tempfile::tempdir().unwrap();
        let mut l = AuditLog::open(d.path().join("audit.jsonl")).unwrap();
        l.append(&entry("a"), "t").unwrap();
        assert!(l.is_online());
        l.path = Some(PathBuf::from("/dev/full"));
        assert!(l.append(&entry("b"), "t").is_err());
        assert!(!l.is_online());
        assert!(l.needs_newline);
    }
}
