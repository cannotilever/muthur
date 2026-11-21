//! The audit entry for one countdown: written when it ends, or (for
//! `executed`) just before the command runs.

use std::rc::Rc;

use serde_json::json;

use super::{Kind, Outcome};
use crate::ops::audit::{AuditEntry, Decision, SharedAudit};

pub struct Recorder {
    kind: Kind,
    seconds: u64,
    audit: SharedAudit,
    timestamp: Rc<dyn Fn() -> String>,
    recorded: bool,
}

impl Recorder {
    pub fn new(kind: Kind, seconds: u64, audit: SharedAudit, timestamp: Rc<dyn Fn() -> String>) -> Recorder {
        Recorder {
            kind,
            seconds,
            audit,
            timestamp,
            recorded: false,
        }
    }

    /// Append one entry; `detail` is the failure reason (empty otherwise).
    pub fn record(&mut self, o: Outcome, argv: Option<Vec<String>>, detail: &str) {
        self.recorded = true;
        let tool = match self.kind {
            Kind::Shutdown => "destruct_shutdown",
            Kind::Reboot => "destruct_reboot",
        };
        let decision = match o {
            Outcome::Executed | Outcome::Failed => Decision::Confirmed,
            Outcome::Aborted | Outcome::Frozen => Decision::Aborted,
        };
        let entry = AuditEntry {
            as_root: true,
            argv,
            exit: Some(json!(o)),
            output: detail.to_string(),
            ..AuditEntry::new(tool, json!({ "seconds": self.seconds }), decision)
        };
        let ts = (self.timestamp)();
        if let Err(e) = self.audit.borrow_mut().append(&entry, &ts) {
            tracing::error!("audit log: cannot record self-destruct: {e}");
        }
    }

    /// The run ended with `o`: record it unless an entry was already written
    /// (`executed`, and then perhaps `failed`).
    pub fn finish(mut self, o: Outcome) {
        if !self.recorded {
            self.record(o, None, "");
        }
    }
}

impl Drop for Recorder {
    /// MUTHUR went away mid-countdown.
    fn drop(&mut self) {
        if !self.recorded {
            self.record(Outcome::Aborted, None, "");
        }
    }
}
