//! MU/TH/UR without the `agent` feature: M4's chat. Each item stands in for
//! its namesake in `turn` or `mod.rs`; there are no tools, prompts or direct
//! link, and a stream always ends the inquiry.

use super::{Entry, MuthurTab, State, push_reply, strip_ship_time};
use crate::config::MuthurSection;
use crate::input::Key;
use crate::llm::{Stop, ToolSpec};
use crate::ops::AgentId;
use std::time::Instant;

impl State {
    pub(super) fn arming(&mut self) -> Option<(&mut bool, &mut Option<Instant>)> {
        None
    }

    pub(super) fn prompting(&self) -> bool {
        false
    }
}

impl MuthurTab {
    /// The persona's exchanges only; a tool round's results show as nothing.
    pub(super) fn redisplay(&self) -> Vec<Entry> {
        let mut transcript = Vec::new();
        for ex in self.log.history().into_iter().filter(|ex| ex.agent == AgentId::Muthur) {
            transcript.push(Entry::User(strip_ship_time(&ex.user).to_string()));
            push_reply(&mut transcript, ex.reply, self.cfg.uppercase);
        }
        transcript
    }

    pub(super) fn direct(&self) -> bool {
        false
    }

    pub(super) fn llm_cfg(&self) -> &MuthurSection {
        &self.cfg
    }

    /// `/DIRECT` and `/MUTHUR` are unknown commands.
    pub(super) fn link_command(&mut self, _cmd: &str) -> bool {
        false
    }

    pub(super) fn begin_turn(&mut self, _agent: AgentId, _line: &str) -> bool {
        false
    }

    /// No tools; past exchanges with tool rounds (logged by an `agent` build)
    /// go report-only.
    pub(super) fn request_parts(&mut self) -> (String, String, Vec<ToolSpec>, bool) {
        (self.system.clone(), String::new(), Vec::new(), true)
    }

    pub(super) fn drop_turn(&mut self) {}

    pub(super) fn abort_turn(&mut self) {}

    pub(super) fn pending_block(&self) -> Vec<String> {
        Vec::new()
    }

    pub(super) fn prompt_status(&self, _width: usize) -> Option<String> {
        None
    }

    pub(super) fn tool_phase(&self) -> Option<&'static str> {
        None
    }

    pub(super) fn prompt_key(&mut self, _key: &Key) {}

    /// Tool calls are never run.
    pub(super) fn tool_call(&mut self, _id: String, _name: String, _input: serde_json::Value) {}

    /// The stream ended: log the reply and finish, whatever the stop.
    pub(super) fn stream_done(&mut self, stop: Stop, model: Option<String>, raw: Option<Vec<serde_json::Value>>) {
        self.flush_cleaner();
        let a = self.assistant(model, stop.log_str(), raw);
        self.log_assistant(a);
        self.partial_raw.clear();
        self.finish(Some(&stop), false);
    }

    /// Ctrl-C mid-inquiry: cancel the request and mark it interrupted.
    pub(super) fn interrupt(&mut self) {
        if let Some(abort) = self.abort.take() {
            abort();
        }
        self.end_early(None);
    }
}
