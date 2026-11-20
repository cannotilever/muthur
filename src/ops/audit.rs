//! Append-only JSONL audit log of every tool call.
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use super::AgentId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Auto,
    Confirmed,
    Denied,
    Rejected,
    Aborted,
    Overridden,
}

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct ReviewRecord {
    pub verdict: String,
    pub risk: String,
    pub note: String,
    pub model: String,
    pub ms: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AuditEntry {
    pub tool: String,
    pub args: serde_json::Value,
    pub agent: Option<AgentId>,
    pub as_root: bool,
    pub reason: Option<String>,
    pub deny_rule: Option<String>,
    pub review: Option<ReviewRecord>,
    pub decision: Decision,
    pub argv: Option<Vec<String>>,
    pub exit: Option<serde_json::Value>,
    pub ms: u64,
    pub output: String,
}

impl AuditEntry {
    pub fn new(tool: &str, args: serde_json::Value, decision: Decision) -> Self {
        Self {
            tool: tool.into(),
            args,
            agent: None,
            as_root: false,
            reason: None,
            deny_rule: None,
            review: None,
            decision,
            argv: None,
            exit: None,
            ms: 0,
            output: String::new(),
        }
    }
}

/// One log for every writer (MU/TH/UR and CARGO); the app is single-threaded.
pub type SharedAudit = std::rc::Rc<std::cell::RefCell<AuditLog>>;

pub fn shared(log: AuditLog) -> SharedAudit {
    std::rc::Rc::new(std::cell::RefCell::new(log))
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
    agent: Option<AgentId>,
    as_root: bool,
    reason: &'a Option<String>,
    deny_rule: &'a Option<String>,
    review: &'a Option<ReviewRecord>,
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
            agent: e.agent,
            as_root: e.as_root,
            reason: &e.reason,
            deny_rule: &e.deny_rule,
            review: &e.review,
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
            args: serde_json::json!({"unit": "a.service"}),
            argv: Some(vec!["systemctl".into(), "status".into()]),
            ms: 12,
            output: "ok".into(),
            ..AuditEntry::new(tool, serde_json::json!(null), Decision::Auto)
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
        let mut want = vec!["ts", "tool", "args", "agent", "as_root", "reason", "deny_rule", "review", "decision", "argv", "exit", "ms", "output"];
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

    #[test]
    fn record_has_m7_fields_and_no_tier() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audit.jsonl");
        let mut l = AuditLog::open(p.clone()).unwrap();
        let e = AuditEntry {
            as_root: true,
            reason: Some("r".into()),
            deny_rule: Some("LOCKOUT".into()),
            review: Some(ReviewRecord { verdict: "deny".into(), risk: "high".into(), note: "n".into(), model: "m".into(), ms: 1, error: None }),
            agent: Some(AgentId::Tech),
            ..AuditEntry::new("run", serde_json::json!({}), Decision::Overridden)
        };
        l.append(&e, "t").unwrap();
        let v: serde_json::Value = serde_json::from_str(std::fs::read_to_string(&p).unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(v["agent"], "tech");
        assert_eq!(v["as_root"], true);
        assert_eq!(v["reason"], "r");
        assert_eq!(v["deny_rule"], "LOCKOUT");
        assert_eq!(v["review"]["verdict"], "deny");
        assert_eq!(v["decision"], "overridden");
        assert!(v.get("tier").is_none());
    }
}
