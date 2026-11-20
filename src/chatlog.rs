//! Append-only JSONL conversation log and message-window rules.
use crate::config::Provider;
use crate::llm::anthropic::replay_blocks;
use crate::ops::AgentId;
use crate::llm::{Message, MessageContent, Role, ToolResult};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// FNV-1a 64, lowercase hex.
pub fn fnv64(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Assistant {
    pub provider: Provider,
    pub model: String,
    pub text: String,
    pub stop: String,
    pub raw: Option<Vec<Value>>,
    pub lane: Lane,
}

/// Which stretch of the log a record belongs to. `Delegated` carries the
/// delegation id (`<persona call id>#<seq of its user record>`, unique even
/// when a server repeats call ids); its records are stored apart from `/DIRECT`'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lane {
    Muthur,
    Tech,
    Delegated(String),
}

impl Lane {
    pub fn agent(&self) -> AgentId {
        match self {
            Lane::Muthur => AgentId::Muthur,
            Lane::Tech | Lane::Delegated(_) => AgentId::Tech,
        }
    }
}

impl From<AgentId> for Lane {
    fn from(a: AgentId) -> Self {
        match a {
            AgentId::Muthur => Lane::Muthur,
            AgentId::Tech => Lane::Tech,
        }
    }
}

/// One delegated technical exchange, folded into the persona exchange it sits in.
#[derive(Clone, Debug, PartialEq)]
pub struct Delegation {
    /// The persona's `delegate` call id (not unique: servers may repeat ids).
    pub call_id: String,
    pub verbatim: bool,
    pub tools: Vec<ToolResult>,
    pub report: Option<String>,
}

/// One past exchange, for redisplay.
#[derive(Clone, Debug, PartialEq)]
pub struct Exchange {
    pub agent: AgentId,
    pub user: String,
    pub tools: Vec<ToolResult>,
    pub reply: Option<(String, String)>,
    pub delegations: Vec<Delegation>,
}

pub struct ChatLog {
    path: PathBuf,
    records: Vec<Value>,
    next_seq: u64,
    needs_newline: bool,
}

fn kind(r: &Value) -> &str {
    r.get("kind").and_then(Value::as_str).unwrap_or("")
}
fn seq(r: &Value) -> u64 {
    r.get("seq").and_then(Value::as_u64).unwrap_or(0)
}
fn text(r: &Value) -> &str {
    r.get("text").and_then(Value::as_str).unwrap_or("")
}
/// A record's agent: its `agent` field, or Muthur when absent (legacy records).
fn agent_of(r: &Value) -> AgentId {
    r.get("agent").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or(AgentId::Muthur)
}
/// A record's lane: `Delegated` when it has a `delegated` id (whatever its agent), else by agent.
fn lane_of(r: &Value) -> Lane {
    match r.get("delegated").and_then(Value::as_str) {
        Some(id) => Lane::Delegated(id.to_string()),
        None => agent_of(r).into(),
    }
}
/// Stamp a record with its lane's `agent` (and `delegated`) fields.
fn with_lane(mut rec: Value, lane: &Lane) -> Value {
    rec["agent"] = json!(lane.agent());
    if let Lane::Delegated(id) = lane {
        rec["delegated"] = json!(id);
    }
    rec
}
fn stop(r: &Value) -> &str {
    r.get("stop").and_then(Value::as_str).unwrap_or("")
}

impl ChatLog {
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut records = Vec::new();
        let mut next_seq = 0;
        let mut needs_newline = false;
        if path.exists() {
            let mut bytes = Vec::new();
            std::fs::File::open(&path)?.read_to_end(&mut bytes)?;
            needs_newline = bytes.last().is_some_and(|b| *b != b'\n');
            for (i, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(line) {
                    Ok(v) if v.get("seq").and_then(Value::as_u64).is_some() => {
                        next_seq = next_seq.max(seq(&v) + 1);
                        records.push(v);
                    }
                    _ => tracing::warn!("chat log: skipping bad line {}", i + 1),
                }
            }
        }
        Ok(Self { path, records, next_seq, needs_newline })
    }

    fn write(&mut self, mut rec: Value) -> std::io::Result<u64> {
        let s = self.next_seq;
        rec["v"] = json!(1);
        rec["seq"] = json!(s);
        let mut line = String::new();
        if self.needs_newline {
            line.push('\n');
        }
        line.push_str(&rec.to_string());
        line.push('\n');
        let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&self.path)?;
        if let Err(e) = f.write_all(line.as_bytes()) {
            // A partial line may be on disk; start the next record on a fresh line.
            self.needs_newline = true;
            return Err(e);
        }
        self.needs_newline = false;
        self.next_seq += 1;
        self.records.push(rec);
        Ok(s)
    }

    pub fn append_user(&mut self, text: &str, ts: &str, agent: AgentId) -> std::io::Result<u64> {
        self.write(json!({"kind":"user","ts":ts,"agent":agent,"text":text}))
    }

    pub fn append_assistant(&mut self, a: Assistant, ts: &str) -> std::io::Result<()> {
        let mut r = json!({"kind":"assistant","ts":ts,"provider":a.provider,"model":a.model,"text":a.text,"stop":a.stop});
        if let Some(raw) = a.raw {
            r["raw"] = Value::Array(raw);
        }
        self.write(with_lane(r, &a.lane)).map(|_| ())
    }

    /// The user record that opens a delegated exchange: the persona's task for
    /// its call `call_id`. Returns the delegation id, which keys the lane: the
    /// call id (kept as a prefix, for correlation) and this record's seq.
    pub fn append_delegation(&mut self, call_id: &str, task: &str, verbatim: bool, ts: &str) -> std::io::Result<String> {
        let report = if verbatim { "verbatim" } else { "relayed" };
        let id = format!("{call_id}#{}", self.next_seq);
        self.write(json!({"kind":"user","ts":ts,"agent":"tech","delegated":id,"call":call_id,"report":report,"text":task}))?;
        Ok(id)
    }

    pub fn append_tool(&mut self, results: &[ToolResult], ts: &str, lane: &Lane) -> std::io::Result<()> {
        self.write(with_lane(json!({"kind":"tool","ts":ts,"results":results}), lane)).map(|_| ())
    }

    fn append_epoch(&mut self, lane: &Lane, from: u64, reason: &str, hash: &str) -> std::io::Result<()> {
        self.write(with_lane(json!({"kind":"epoch","from":from,"reason":reason,"system_hash":hash}), lane)).map(|_| ())
    }

    fn lane_records<'a>(&'a self, lane: &'a Lane) -> impl DoubleEndedIterator<Item = &'a Value> {
        self.records.iter().filter(move |r| lane_of(r) == *lane)
    }

    fn latest_epoch<'a>(&'a self, lane: &'a Lane) -> Option<&'a Value> {
        self.lane_records(lane).rev().find(|r| kind(r) == "epoch")
    }

    /// Seq below which assistant turns lose thinking. The very first epoch
    /// (initial system hash, no predecessor) changes no prefix, so it counts as 0.
    fn strip_boundary(&self, lane: &Lane) -> u64 {
        let mut it = self.lane_records(lane).filter(|r| kind(r) == "epoch");
        let first = it.next().map(seq);
        match it.next_back() {
            Some(e) => seq(e),
            None => first.map(|_| 0).unwrap_or(0),
        }
    }

    /// Exchanges: each user record plus the assistant/tool records up to the next user.
    fn exchanges<'a>(&'a self, lane: &'a Lane) -> Vec<Vec<&'a Value>> {
        let mut out: Vec<Vec<&Value>> = Vec::new();
        for r in self.lane_records(lane) {
            match kind(r) {
                "user" => out.push(vec![r]),
                "assistant" | "tool" => {
                    if let Some(ex) = out.last_mut() {
                        ex.push(r);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// A tool record's results, or None if they are missing or malformed.
    fn tool_results(r: &Value) -> Option<Vec<ToolResult>> {
        r.get("results").cloned().and_then(|v| serde_json::from_value(v).ok())
    }

    fn to_message(provider: Provider, r: &Value, e_seq: u64) -> Message {
        match kind(r) {
            "tool" => {
                let results = Self::tool_results(r).unwrap_or_else(|| {
                    tracing::warn!("chat log: malformed tool record {}", seq(r));
                    Vec::new()
                });
                Message { role: Role::User, content: MessageContent::ToolResults(results) }
            }
            "assistant" => {
                let stop = stop(r);
                let raw = r.get("raw").and_then(Value::as_array);
                let mut content = MessageContent::Text(text(r).into());
                match (provider, raw) {
                    (Provider::Anthropic, Some(raw)) if stop != "interrupted" => {
                        let b = replay_blocks(raw, seq(r) < e_seq);
                        if !b.is_empty() {
                            content = MessageContent::Blocks(b);
                        }
                    }
                    (Provider::Openai, Some(raw)) if stop == "tool_use" => {
                        content = MessageContent::ToolCalls { text: text(r).into(), calls: raw.clone() };
                    }
                    _ => {}
                }
                Message { role: Role::Assistant, content }
            }
            _ => Message { role: Role::User, content: MessageContent::Text(text(r).into()) },
        }
    }

    /// A past exchange is sendable only if it ends in a final assistant record
    /// and none of its tool records is malformed.
    fn ends_final(ex: &[&Value]) -> bool {
        if let Some(bad) = ex.iter().find(|r| kind(r) == "tool" && Self::tool_results(r).is_none()) {
            tracing::warn!("chat log: excluding exchange with malformed tool record {}", seq(bad));
            return false;
        }
        ex.last().is_some_and(|r| {
            kind(r) == "assistant" && !matches!(stop(r), "tool_use" | "error" | "refusal") && !text(r).trim().is_empty()
        })
    }

    /// Build the window from user seq `from`. Returns (messages, candidate trim starts).
    /// Records at or after the current exchange's start never lose thinking.
    fn build(&self, lane: &Lane, provider: Provider, from: u64, e_seq: u64, report_only_past: bool) -> (Vec<Message>, Vec<u64>) {
        let exs = self.exchanges(lane);
        let mut msgs = Vec::new();
        let mut starts = Vec::new();
        // Never an assistant-last request.
        if self.lane_records(lane).rev().find(|r| matches!(kind(r), "user" | "assistant" | "tool")).is_none_or(|r| kind(r) == "assistant") {
            return (msgs, starts);
        }
        let Some(last) = exs.len().checked_sub(1) else { return (msgs, starts) };
        let e_seq = e_seq.min(seq(exs[last][0]));
        let pv = serde_json::to_value(provider).unwrap_or(Value::Null);
        for (i, ex) in exs.iter().enumerate() {
            if seq(ex[0]) < from || (i != last && !Self::ends_final(ex)) {
                continue;
            }
            starts.push(seq(ex[0]));
            let foreign = i != last && (report_only_past || ex.iter().any(|r| kind(r) == "assistant" && r.get("provider") != Some(&pv)));
            if foreign {
                // Another provider's (or, in report-only mode, any past) raw/tool rounds aren't replayed: keep only the question and final text.
                msgs.push(Self::to_message(provider, ex[0], e_seq));
                msgs.push(Message { role: Role::Assistant, content: MessageContent::Text(text(ex[ex.len() - 1]).into()) });
            } else {
                msgs.extend(ex.iter().map(|r| Self::to_message(provider, r, e_seq)));
            }
        }
        (msgs, starts)
    }

    fn estimate(system: &str, msgs: &[Message]) -> usize {
        let n: usize = msgs
            .iter()
            .map(|m| match &m.content {
                MessageContent::Text(t) => t.chars().count(),
                MessageContent::Blocks(b) => b.iter().map(|v| v.to_string().chars().count()).sum(),
                MessageContent::ToolResults(rs) => serde_json::to_string(rs).map(|s| s.chars().count()).unwrap_or(0),
                MessageContent::ToolCalls { text, calls } => json!({"text":text,"calls":calls}).to_string().chars().count(),
            })
            .sum();
        (system.chars().count() + n) / 4
    }

    #[allow(clippy::too_many_arguments)]
    pub fn window(&mut self, lane: &Lane, provider: Provider, model: &str, system: &str, tools_json: &str, budget_tokens: usize, report_only_past: bool) -> std::io::Result<Vec<Message>> {
        let h = fnv64(&format!("{provider:?}\n{model}\n{system}\n{tools_json}"));
        let last = self.latest_epoch(lane).map(|e| (e["from"].as_u64().unwrap_or(0), e["system_hash"].as_str().unwrap_or("").to_string()));
        if last.as_ref().map(|l| l.1.as_str()) != Some(h.as_str()) {
            self.append_epoch(lane, last.map(|l| l.0).unwrap_or(0), "system_changed", &h)?;
        }
        let e = self.latest_epoch(lane).unwrap();
        let from = e["from"].as_u64().unwrap_or(0);
        let e_seq = self.strip_boundary(lane);
        let (msgs, starts) = self.build(lane, provider, from, e_seq, report_only_past);
        if Self::estimate(system, &msgs) <= budget_tokens {
            return Ok(msgs);
        }
        let Some(&current) = starts.last() else { return Ok(msgs) };
        // Only trim at the start of an inquiry: mid-chain the prefix must stay stable.
        let exs = self.exchanges(lane);
        if exs.last().is_some_and(|ex| ex.iter().any(|r| kind(r) == "assistant")) {
            return Ok(msgs);
        }
        // Epoch seq the trim will get.
        let new_e = self.next_seq;
        let new_from = starts
            .iter()
            .copied()
            .find(|&c| Self::estimate(system, &self.build(lane, provider, c, new_e, report_only_past).0) <= budget_tokens / 2)
            .unwrap_or(current);
        if new_from == from {
            return Ok(msgs);
        }
        self.append_epoch(lane, new_from, "trim", &h)?;
        Ok(self.build(lane, provider, new_from, new_e, report_only_past).0)
    }

    pub fn history(&self) -> Vec<Exchange> {
        let tools_of = |ex: &[&Value]| -> Vec<ToolResult> {
            ex.iter().filter(|r| kind(r) == "tool").flat_map(|r| Self::tool_results(r).unwrap_or_default()).collect()
        };
        let (muthur, tech) = (Lane::Muthur, Lane::Tech);
        let mut all: Vec<(AgentId, Vec<&Value>)> = [(AgentId::Muthur, &muthur), (AgentId::Tech, &tech)]
            .into_iter()
            .flat_map(|(a, lane)| self.exchanges(lane).into_iter().map(move |ex| (a, ex)))
            .collect();
        all.sort_by_key(|(_, ex)| seq(ex[0]));
        let mut out: Vec<Exchange> = all
            .iter()
            .map(|(agent, ex)| {
                let reply = ex.iter().rev().find(|r| kind(r) == "assistant").map(|a| (text(a).to_string(), stop(a).to_string()));
                Exchange { agent: *agent, user: text(ex[0]).into(), tools: tools_of(ex), reply, delegations: vec![] }
            })
            .collect();
        // Delegated exchanges, in log order, fold into the persona exchange they sit in.
        let mut ids: Vec<&str> = Vec::new();
        for id in self.records.iter().filter_map(|r| r.get("delegated").and_then(Value::as_str)) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        for id in ids {
            for ex in self.exchanges(&Lane::Delegated(id.to_string())) {
                let report = ex
                    .iter()
                    .rev()
                    .find(|r| kind(r) == "assistant" && !matches!(stop(r), "tool_use" | "error"))
                    .map(|r| text(r).to_string());
                let d = Delegation {
                    // Records without `call` key the lane on the call id itself.
                    call_id: ex[0].get("call").and_then(Value::as_str).unwrap_or(id).to_string(),
                    verbatim: ex[0].get("report").and_then(Value::as_str) == Some("verbatim"),
                    tools: tools_of(&ex),
                    report,
                };
                let host = all
                    .iter()
                    .enumerate()
                    .filter(|(_, (a, e))| *a == AgentId::Muthur && seq(e[0]) < seq(ex[0]))
                    .max_by_key(|(_, (_, e))| seq(e[0]))
                    .map(|(i, _)| i);
                if let Some(i) = host {
                    out[i].delegations.push(d);
                }
            }
        }
        out
    }

    pub fn purge(&mut self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        self.records.clear();
        self.next_seq = 0;
        self.needs_newline = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn log() -> (tempfile::TempDir, ChatLog) {
        let d = tempfile::tempdir().unwrap();
        let l = ChatLog::open(d.path().join("chat.jsonl")).unwrap();
        (d, l)
    }
    fn asst(text: &str, raw: Option<Vec<Value>>) -> Assistant {
        Assistant { provider: Provider::Anthropic, model: "m".into(), text: text.into(), stop: "end_turn".into(), raw, lane: Lane::Muthur }
    }
    fn t(s: &str) -> MessageContent {
        MessageContent::Text(s.into())
    }
    fn has_thinking(c: &MessageContent) -> bool {
        matches!(c, MessageContent::Blocks(b) if b.iter().any(|v| v["type"] == "thinking"))
    }
    fn thinking_raw(text: &str) -> Vec<Value> {
        vec![json!({"type":"thinking","thinking":"","signature":"s"}), json!({"type":"text","text":text})]
    }

    #[test]
    fn round_trip_and_mode() {
        let (d, mut l) = log();
        l.append_user("[SHIP TIME X] HI", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst("HELLO", None), "t").unwrap();
        let l2 = ChatLog::open(d.path().join("chat.jsonl")).unwrap();
        assert_eq!(l2.history(), vec![Exchange { agent: AgentId::Muthur, user: "[SHIP TIME X] HI".into(), tools: vec![], reply: Some(("HELLO".into(), "end_turn".into())), delegations: vec![] }]);
        assert_eq!(std::fs::metadata(d.path().join("chat.jsonl")).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn window_replays_raw_and_ends_with_inquiry() {
        let (_d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        let raw = thinking_raw("B");
        l.append_assistant(asst("B", Some(raw.clone())), "t").unwrap();
        l.append_user("C", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(w[1].content, MessageContent::Blocks(raw));
        assert_eq!(w[2].content, t("C"));
    }

    fn failed_log() -> (tempfile::TempDir, ChatLog) {
        let (d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        let mut a = asst("oops", None);
        a.stop = "error".into();
        l.append_assistant(a, "t").unwrap();
        l.append_user("B", "t", AgentId::Muthur).unwrap();
        l.append_user("C", "t", AgentId::Muthur).unwrap();
        (d, l)
    }

    #[test]
    fn failed_pairs_and_orphans_excluded() {
        let (_d, mut l) = failed_log();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w, vec![Message { role: Role::User, content: t("C") }]);
    }

    #[test]
    fn openai_gets_text_only() {
        let (_d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst("B", Some(thinking_raw("B"))), "t").unwrap();
        l.append_user("C", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Openai, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(w[1].content, t("B"));
    }

    #[test]
    fn system_change_strips_thinking_once() {
        let (_d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS1", "", 200000, false).unwrap();
        l.append_assistant(asst("B", Some(thinking_raw("B"))), "t").unwrap();
        l.append_user("C", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS2", "", 200000, false).unwrap();
        assert!(!has_thinking(&w[1].content));
        l.append_assistant(asst("D", Some(thinking_raw("D"))), "t").unwrap();
        l.append_user("E", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS2", "", 200000, false).unwrap();
        assert!(!has_thinking(&w[1].content));
        assert!(has_thinking(&w[3].content));
    }

    fn epochs(l: &ChatLog, reason: &str) -> usize {
        l.records.iter().filter(|r| kind(r) == "epoch" && r["reason"] == reason).count()
    }

    #[test]
    fn trim_cuts_to_half_budget_with_hysteresis() {
        let (_d, mut l) = log();
        let big = "x".repeat(400);
        for _ in 0..20 {
            l.append_user(&big, "t", AgentId::Muthur).unwrap();
            l.append_assistant(asst(&big, None), "t").unwrap();
        }
        l.append_user("now", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 1000, false).unwrap();
        assert!(ChatLog::estimate("SYS", &w) <= 500);
        assert_eq!(epochs(&l, "trim"), 1);
        l.append_assistant(asst("ok", None), "t").unwrap();
        l.append_user("next", "t", AgentId::Muthur).unwrap();
        let n = l.records.iter().filter(|r| kind(r) == "epoch").count();
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 1000, false).unwrap();
        assert_eq!(l.records.iter().filter(|r| kind(r) == "epoch").count(), n);
    }

    #[test]
    fn truncated_last_line_is_skipped_and_append_starts_fresh_line() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("chat.jsonl");
        std::fs::write(&p, "{\"v\":1,\"seq\":0,\"kind\":\"user\",\"ts\":\"t\",\"text\":\"A\"}\n{\"v\":1,\"seq\":1,\"ki").unwrap();
        let mut l = ChatLog::open(p.clone()).unwrap();
        assert_eq!(l.history().len(), 1);
        assert_eq!(l.append_user("B", "t", AgentId::Muthur).unwrap(), 1);
        let l2 = ChatLog::open(p).unwrap();
        assert_eq!(l2.history().len(), 2);
    }

    #[test]
    fn purge_deletes() {
        let (d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        l.purge().unwrap();
        assert!(!d.path().join("chat.jsonl").exists());
        assert!(l.history().is_empty());
        assert_eq!(l.append_user("B", "t", AgentId::Muthur).unwrap(), 0);
    }

    #[test]
    fn refused_pairs_excluded_but_shown() {
        let (_d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        let mut a = asst("PARTIAL", Some(vec![json!({"type":"text","text":"PARTIAL"})]));
        a.stop = "refusal".into();
        l.append_assistant(a, "t").unwrap();
        l.append_user("C", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w, vec![Message { role: Role::User, content: t("C") }]);
        assert_eq!(l.history()[0].reply, Some(("PARTIAL".into(), "refusal".into())));
    }

    #[test]
    fn whitespace_only_reply_excluded() {
        let (_d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst(" \n\t", Some(vec![json!({"type":"text","text":" \n\t"})])), "t").unwrap();
        l.append_user("C", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w, vec![Message { role: Role::User, content: t("C") }]);
    }

    #[test]
    fn model_change_starts_epoch_and_strips_thinking_once() {
        let (_d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        l.window(&Lane::Muthur, Provider::Anthropic, "m1", "SYS", "", 200000, false).unwrap();
        l.append_assistant(asst("B", Some(thinking_raw("B"))), "t").unwrap();
        l.append_user("C", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m2", "SYS", "", 200000, false).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
        assert!(!has_thinking(&w[1].content));
        l.append_assistant(asst("D", Some(thinking_raw("D"))), "t").unwrap();
        l.append_user("E", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m2", "SYS", "", 200000, false).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
        assert!(!has_thinking(&w[1].content));
        assert!(has_thinking(&w[3].content));
    }

    #[test]
    fn provider_change_starts_epoch() {
        let (_d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        l.window(&Lane::Muthur, Provider::Openai, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
    }

    #[test]
    fn failed_write_forces_fresh_line() {
        let (_d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        l.path = PathBuf::from("/dev/full");
        assert!(l.append_user("B", "t", AgentId::Muthur).is_err());
        assert!(l.needs_newline);
    }

    fn asst_stop(text: &str, stop: &str, raw: Option<Vec<Value>>) -> Assistant {
        Assistant { stop: stop.into(), ..asst(text, raw) }
    }
    fn r1() -> ToolResult {
        ToolResult { id: "t1".into(), name: "ship_status".into(), content: "ALL NOMINAL".into(), is_error: false, label: "SHIP STATUS".into() }
    }
    fn tool_use_raw() -> Vec<Value> {
        vec![json!({"type":"text","text":"CHECKING"}), json!({"type":"tool_use","id":"t1","name":"ship_status","input":{}})]
    }
    fn user(s: &str) -> Message {
        Message { role: Role::User, content: t(s) }
    }

    #[test]
    fn exchange_with_rounds_replays_in_full() {
        let (_d, mut l) = log();
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst_stop("CHECKING", "tool_use", Some(tool_use_raw())), "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Muthur).unwrap();
        let done_raw = vec![json!({"type":"text","text":"DONE"})];
        l.append_assistant(asst("DONE", Some(done_raw.clone())), "t").unwrap();
        l.append_user("Q2", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(
            w,
            vec![
                user("Q"),
                Message { role: Role::Assistant, content: MessageContent::Blocks(tool_use_raw()) },
                Message { role: Role::User, content: MessageContent::ToolResults(vec![r1()]) },
                Message { role: Role::Assistant, content: MessageContent::Blocks(done_raw) },
                user("Q2"),
            ]
        );
    }

    #[test]
    fn past_exchange_ending_in_tool_use_is_excluded() {
        let (_d, mut l) = log();
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst_stop("CHECKING", "tool_use", Some(tool_use_raw())), "t").unwrap();
        l.append_user("Q2", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w, vec![user("Q2")]);
    }

    #[test]
    fn past_exchange_ending_in_tool_round_is_excluded() {
        let (_d, mut l) = log();
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst_stop("CHECKING", "tool_use", Some(tool_use_raw())), "t").unwrap();
        let aborted = ToolResult { content: "ABORTED".into(), is_error: true, ..r1() };
        l.append_tool(&[aborted], "t", &Lane::Muthur).unwrap();
        l.append_assistant(asst_stop("", "interrupted", None), "t").unwrap();
        l.append_user("Q2", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w, vec![user("Q2")]);
    }

    #[test]
    fn current_exchange_sent_in_full_ending_with_tool_results() {
        let (_d, mut l) = log();
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst_stop("CHECKING", "tool_use", Some(tool_use_raw())), "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(w[0], user("Q"));
        assert_eq!(w[1].content, MessageContent::Blocks(tool_use_raw()));
        assert_eq!(w.last().unwrap(), &Message { role: Role::User, content: MessageContent::ToolResults(vec![r1()]) });
    }

    #[test]
    fn openai_tool_calls_replay() {
        let (_d, mut l) = log();
        let call = json!({"id":"t1","type":"function","function":{"name":"ship_status","arguments":"{}"}});
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        let a = Assistant { provider: Provider::Openai, ..asst_stop("CHECKING", "tool_use", Some(vec![call.clone()])) };
        l.append_assistant(a, "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Muthur).unwrap();
        l.append_assistant(Assistant { provider: Provider::Openai, ..asst("DONE", None) }, "t").unwrap();
        l.append_user("Q2", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Openai, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w.len(), 5);
        assert_eq!(w[1], Message { role: Role::Assistant, content: MessageContent::ToolCalls { text: "CHECKING".into(), calls: vec![call] } });
        assert_eq!(w[2].content, MessageContent::ToolResults(vec![r1()]));
        assert_eq!(w[3], Message { role: Role::Assistant, content: t("DONE") });
        assert_eq!(w[4], user("Q2"));
    }

    #[test]
    fn tools_change_writes_epoch() {
        let (_d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "T1", 200000, false).unwrap();
        l.append_assistant(asst("B", Some(thinking_raw("B"))), "t").unwrap();
        l.append_user("C", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "T1", 200000, false).unwrap();
        assert!(has_thinking(&w[1].content));
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "T2", 200000, false).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
        assert!(!has_thinking(&w[1].content));
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "T2", 200000, false).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
    }

    #[test]
    fn trim_keeps_current_exchange_whole() {
        let (_d, mut l) = log();
        let big = "x".repeat(400);
        for _ in 0..10 {
            l.append_user(&big, "t", AgentId::Muthur).unwrap();
            l.append_assistant(asst_stop(&big, "tool_use", Some(tool_use_raw())), "t").unwrap();
            l.append_tool(&[ToolResult { content: big.clone(), ..r1() }], "t", &Lane::Muthur).unwrap();
            l.append_assistant(asst(&big, None), "t").unwrap();
        }
        l.append_user("now", "t", AgentId::Muthur).unwrap();
        // Trim happens at the start of the inquiry.
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 1600, false).unwrap();
        assert_eq!(epochs(&l, "trim"), 1);
        assert!(w.len() > 1 && w.len() < 41);
        assert!(matches!(&w[0], Message { role: Role::User, content: MessageContent::Text(_) }));
        assert_eq!((w.len() - 1) % 4, 0);
        // Mid-chain the current exchange grows past budget/2; it is sent whole with no new epoch.
        l.append_assistant(asst_stop("CHECKING", "tool_use", Some(tool_use_raw())), "t").unwrap();
        let huge = ToolResult { content: "y".repeat(8000), ..r1() };
        l.append_tool(std::slice::from_ref(&huge), "t", &Lane::Muthur).unwrap();
        let n_ep = l.records.iter().filter(|r| kind(r) == "epoch").count();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 1600, false).unwrap();
        assert_eq!(l.records.iter().filter(|r| kind(r) == "epoch").count(), n_ep);
        assert!(matches!(&w[0], Message { role: Role::User, content: MessageContent::Text(_) }));
        assert_eq!((w.len() - 3) % 4, 0);
        let n = w.len();
        assert_eq!(w[n - 3], user("now"));
        assert_eq!(w[n - 2].content, MessageContent::Blocks(tool_use_raw()));
        assert_eq!(w[n - 1].content, MessageContent::ToolResults(vec![huge]));
    }

    fn thinking_tool_use_raw() -> Vec<Value> {
        let mut r = vec![json!({"type":"thinking","thinking":"","signature":"s"})];
        r.extend(tool_use_raw());
        r
    }

    #[test]
    fn provider_switch_flattens_foreign_exchanges() {
        let (_d, mut l) = log();
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst_stop("CHECKING", "tool_use", Some(thinking_tool_use_raw())), "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Muthur).unwrap();
        l.append_assistant(asst("FINAL", Some(thinking_raw("FINAL"))), "t").unwrap();
        l.append_user("Q2", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Openai, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w, vec![user("Q"), Message { role: Role::Assistant, content: t("FINAL") }, user("Q2")]);
    }

    #[test]
    fn provider_switch_flattens_foreign_exchanges_reverse() {
        let (_d, mut l) = log();
        let call = json!({"id":"t1","type":"function","function":{"name":"ship_status","arguments":"{}"}});
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.append_assistant(Assistant { provider: Provider::Openai, ..asst_stop("CHECKING", "tool_use", Some(vec![call])) }, "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Muthur).unwrap();
        l.append_assistant(Assistant { provider: Provider::Openai, ..asst("FINAL", None) }, "t").unwrap();
        l.append_user("Q2", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w, vec![user("Q"), Message { role: Role::Assistant, content: t("FINAL") }, user("Q2")]);
    }

    #[test]
    fn mid_chain_tiny_budget_writes_no_epoch_and_keeps_thinking() {
        let (_d, mut l) = log();
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        l.append_assistant(asst_stop("CHECKING", "tool_use", Some(thinking_tool_use_raw())), "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Muthur).unwrap();
        let n_ep = l.records.iter().filter(|r| kind(r) == "epoch").count();
        for _ in 0..2 {
            let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 1, false).unwrap();
            assert_eq!(l.records.iter().filter(|r| kind(r) == "epoch").count(), n_ep);
            assert_eq!(w.len(), 3);
            assert!(has_thinking(&w[1].content));
        }
    }

    #[test]
    fn mid_chain_system_change_keeps_current_thinking() {
        let (_d, mut l) = log();
        l.append_user("A", "t", AgentId::Muthur).unwrap();
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "T1", 200000, false).unwrap();
        l.append_assistant(asst("B", Some(thinking_raw("B"))), "t").unwrap();
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "T1", 200000, false).unwrap();
        l.append_assistant(asst_stop("CHECKING", "tool_use", Some(thinking_tool_use_raw())), "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "T2", 200000, false).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
        assert!(!has_thinking(&w[1].content));
        assert!(has_thinking(&w[3].content));
    }

    #[test]
    fn oversized_inquiry_trims_once() {
        let (_d, mut l) = log();
        let big = "x".repeat(400);
        for _ in 0..5 {
            l.append_user(&big, "t", AgentId::Muthur).unwrap();
            l.append_assistant(asst(&big, None), "t").unwrap();
        }
        l.append_user(&"z".repeat(6000), "t", AgentId::Muthur).unwrap();
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 1000, false).unwrap();
        assert_eq!(epochs(&l, "trim"), 1);
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 1000, false).unwrap();
        assert_eq!(epochs(&l, "trim"), 1);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn past_exchange_with_corrupt_tool_record_is_excluded() {
        let (_d, mut l) = log();
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst_stop("CHECKING", "tool_use", Some(tool_use_raw())), "t").unwrap();
        l.write(json!({"kind":"tool","ts":"t","results":"garbage"})).unwrap();
        l.append_assistant(asst("DONE", None), "t").unwrap();
        l.append_user("Q2", "t", AgentId::Muthur).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w, vec![user("Q2")]);
    }

    #[test]
    fn assistant_last_gives_empty_window() {
        let (_d, mut l) = log();
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst("DONE", None), "t").unwrap();
        assert!(l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap().is_empty());
    }

    #[test]
    fn history_collects_tools() {
        let (_d, mut l) = log();
        l.append_user("Q", "t", AgentId::Muthur).unwrap();
        l.append_assistant(asst_stop("CHECKING", "tool_use", Some(tool_use_raw())), "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Muthur).unwrap();
        l.append_assistant(asst("DONE", None), "t").unwrap();
        let h = l.history();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].tools, vec![r1()]);
        assert_eq!(h[0].reply, Some(("DONE".into(), "end_turn".into())));
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv64(""), "cbf29ce484222325");
        assert_eq!(fnv64("a"), "af63dc4c8601ec8c");
    }

    const M: AgentId = AgentId::Muthur;
    const T: AgentId = AgentId::Tech;
    fn asst_for(agent: AgentId, text: &str) -> Assistant {
        Assistant { lane: agent.into(), ..asst(text, None) }
    }
    fn texts(w: &[Message]) -> Vec<String> {
        w.iter().filter_map(|m| if let MessageContent::Text(s) = &m.content { Some(s.clone()) } else { None }).collect()
    }

    #[test]
    fn agent_filter() {
        let (_d, mut l) = log();
        l.append_user("m1", "t", M).unwrap();
        l.append_assistant(asst_for(M, "ma1"), "t").unwrap();
        l.append_user("t1", "t", T).unwrap();
        l.append_assistant(asst_for(T, "ta1"), "t").unwrap();
        l.append_user("m2", "t", M).unwrap();
        l.append_user("t2", "t", T).unwrap();
        let wm = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(texts(&wm), vec!["m1", "ma1", "m2"]);
        let wt = l.window(&Lane::Tech, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(texts(&wt), vec!["t1", "ta1", "t2"]);
    }

    #[test]
    fn legacy_records_are_muthur() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("chat.jsonl");
        std::fs::write(
            &p,
            "{\"v\":1,\"seq\":0,\"kind\":\"user\",\"ts\":\"t\",\"text\":\"A\"}\n{\"v\":1,\"seq\":1,\"kind\":\"assistant\",\"ts\":\"t\",\"provider\":\"anthropic\",\"model\":\"m\",\"text\":\"B\",\"stop\":\"end_turn\"}\n{\"v\":1,\"seq\":2,\"kind\":\"user\",\"ts\":\"t\",\"text\":\"C\"}\n",
        )
        .unwrap();
        let mut l = ChatLog::open(p).unwrap();
        assert_eq!(texts(&l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap()), vec!["A", "B", "C"]);
        assert!(l.window(&Lane::Tech, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap().is_empty());
    }

    #[test]
    fn separate_epochs() {
        let (_d, mut l) = log();
        l.append_user("t1", "t", T).unwrap();
        l.window(&Lane::Tech, Provider::Anthropic, "m", "TECH SYS", "", 200000, false).unwrap();
        l.append_user("m1", "t", M).unwrap();
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "MUTHUR SYS", "", 200000, false).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
        l.window(&Lane::Muthur, Provider::Anthropic, "m", "MUTHUR SYS", "", 200000, false).unwrap();
        l.window(&Lane::Tech, Provider::Anthropic, "m", "TECH SYS", "", 200000, false).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
        let mine = |a: AgentId| l.records.iter().filter(|r| kind(r) == "epoch" && lane_of(r) == Lane::from(a)).count();
        assert_eq!((mine(M), mine(T)), (1, 1));
    }

    #[test]
    fn report_only_drops_tool_rounds() {
        let (_d, mut l) = log();
        l.append_user("Q", "t", T).unwrap();
        l.append_assistant(Assistant { lane: Lane::Tech, ..asst_stop("CHECKING", "tool_use", Some(tool_use_raw())) }, "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Tech).unwrap();
        l.append_assistant(asst_for(T, "DONE"), "t").unwrap();
        l.append_user("Q2", "t", T).unwrap();
        let w = l.window(&Lane::Tech, Provider::Anthropic, "m", "SYS", "", 200000, true).unwrap();
        assert_eq!(w, vec![user("Q"), Message { role: Role::Assistant, content: t("DONE") }, user("Q2")]);
        l.append_assistant(Assistant { lane: Lane::Tech, ..asst_stop("CHECKING", "tool_use", Some(tool_use_raw())) }, "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Tech).unwrap();
        let w = l.window(&Lane::Tech, Provider::Anthropic, "m", "SYS", "", 200000, true).unwrap();
        assert_eq!(w.len(), 5);
        assert!(matches!(w[4].content, MessageContent::ToolResults(_)));
    }

    #[test]
    fn history_tags_agent() {
        let (_d, mut l) = log();
        l.append_user("m1", "t", M).unwrap();
        l.append_assistant(asst_for(M, "ma"), "t").unwrap();
        l.append_user("t1", "t", T).unwrap();
        l.append_assistant(asst_for(T, "ta"), "t").unwrap();
        l.append_user("m2", "t", M).unwrap();
        let h: Vec<_> = l.history().into_iter().map(|e| (e.agent, e.user)).collect();
        assert_eq!(h, vec![(M, "m1".to_string()), (T, "t1".to_string()), (M, "m2".to_string())]);
    }

    #[test]
    fn purge_clears_both() {
        let (d, mut l) = log();
        l.append_user("m", "t", M).unwrap();
        l.append_user("t", "t", T).unwrap();
        l.purge().unwrap();
        assert!(!d.path().join("chat.jsonl").exists());
        assert!(l.history().is_empty());
        assert_eq!(l.append_user("x", "t", T).unwrap(), 0);
    }

    fn d(id: &str) -> Lane {
        Lane::Delegated(id.into())
    }
    fn tool_use_asst(lane: Lane) -> Assistant {
        Assistant { lane, ..asst_stop("CHECKING", "tool_use", Some(tool_use_raw())) }
    }

    /// tech A, then persona (tool_use) wrapping delegation d1, then tech B.
    /// Returns the delegation's lane with the log.
    fn delegated_log() -> (tempfile::TempDir, ChatLog, Lane) {
        let (dir, mut l) = log();
        l.append_user("A", "t", T).unwrap();
        l.append_assistant(asst_for(T, "ta"), "t").unwrap();
        l.append_user("P", "t", M).unwrap();
        l.append_assistant(tool_use_asst(Lane::Muthur), "t").unwrap();
        let dl = d(&l.append_delegation("d1", "task", false, "t").unwrap());
        l.append_assistant(tool_use_asst(dl.clone()), "t").unwrap();
        l.append_tool(&[r1()], "t", &dl).unwrap();
        (dir, l, dl)
    }

    fn full_log() -> (tempfile::TempDir, ChatLog) {
        let (dir, mut l, dl) = delegated_log();
        l.append_assistant(Assistant { lane: dl, ..asst("FINDINGS", None) }, "t").unwrap();
        l.append_tool(&[r1()], "t", &Lane::Muthur).unwrap();
        l.append_assistant(asst_for(M, "PERSONA DONE"), "t").unwrap();
        l.append_user("B", "t", T).unwrap();
        (dir, l)
    }

    #[test]
    fn lane_of_records() {
        assert_eq!(lane_of(&json!({"agent":"tech","delegated":"d1"})), d("d1"));
        assert_eq!(lane_of(&json!({"agent":"muthur","delegated":"d1"})), d("d1"));
        assert_eq!(lane_of(&json!({"kind":"user"})), Lane::Muthur);
        assert_eq!(lane_of(&json!({"agent":"tech"})), Lane::Tech);
        assert_eq!(d("x").agent(), AgentId::Tech);
    }

    #[test]
    fn direct_window_skips_delegations() {
        let (_d, mut l) = full_log();
        let w = l.window(&Lane::Tech, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(texts(&w), vec!["A", "ta", "B"]);
        assert_eq!(w.len(), 3);
        assert!(!format!("{w:?}").contains("task"));
    }

    #[test]
    fn delegated_window_is_task_only() {
        let (_d, mut l, dl) = delegated_log();
        let w = l.window(&dl, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(w[0], user("task"));
        assert_eq!(w[1].role, Role::Assistant);
        assert!(matches!(w[2].content, MessageContent::ToolResults(_)));
        assert!(!format!("{w:?}").contains("\"ta\""));
        // Its epoch carries the delegated id, so the Tech lane never sees it.
        assert!(l.records.iter().any(|r| kind(r) == "epoch" && lane_of(r) == dl && agent_of(r) == AgentId::Tech));
        assert!(l.latest_epoch(&Lane::Tech).is_none());
    }

    #[test]
    fn persona_window_unaffected_by_delegation() {
        let (_d, mut l) = full_log();
        l.append_user("Q", "t", M).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        let (_d2, mut l2) = log();
        l2.append_user("A", "t", T).unwrap();
        l2.append_assistant(asst_for(T, "ta"), "t").unwrap();
        l2.append_user("P", "t", M).unwrap();
        l2.append_assistant(tool_use_asst(Lane::Muthur), "t").unwrap();
        l2.append_tool(&[r1()], "t", &Lane::Muthur).unwrap();
        l2.append_assistant(asst_for(M, "PERSONA DONE"), "t").unwrap();
        l2.append_user("B", "t", T).unwrap();
        l2.append_user("Q", "t", M).unwrap();
        let w2 = l2.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert!(!w.is_empty());
        assert_eq!(w, w2);
        let dump = format!("{w:?}");
        assert!(!dump.contains("task") && !dump.contains("FINDINGS"));
    }

    #[test]
    fn history_folds_delegations() {
        let (_d, l) = full_log();
        let h = l.history();
        assert_eq!(h.iter().map(|e| (e.agent, e.user.as_str())).collect::<Vec<_>>(), vec![(T, "A"), (M, "P"), (T, "B")]);
        assert_eq!(h[1].delegations, vec![Delegation { call_id: "d1".into(), verbatim: false, tools: vec![r1()], report: Some("FINDINGS".into()) }]);
        assert!(h[0].delegations.is_empty() && h[2].delegations.is_empty());
    }

    #[test]
    fn verbatim_flag_and_no_report() {
        let (_d, mut l) = log();
        l.append_user("P", "t", M).unwrap();
        let id = l.append_delegation("d1", "task", true, "t").unwrap();
        l.append_assistant(tool_use_asst(d(&id)), "t").unwrap();
        let h = l.history();
        assert_eq!(h[0].delegations, vec![Delegation { call_id: "d1".into(), verbatim: true, tools: vec![], report: None }]);
        assert!(l.records.iter().any(|r| r["report"] == "verbatim" && r["delegated"] == id.as_str()));
    }

    #[test]
    fn orphan_delegation_records_are_ignored() {
        let (dir, mut l, dl) = delegated_log();
        l.append_assistant(Assistant { lane: dl, ..asst("FINDINGS", None) }, "t").unwrap();
        let mut l = ChatLog::open(dir.path().join("chat.jsonl")).unwrap();
        // M5 rules: the unfinished persona exchange is excluded from a later persona window.
        l.append_user("Q", "t", M).unwrap();
        let w = l.window(&Lane::Muthur, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w, vec![user("Q")]);
        l.append_user("T2", "t", T).unwrap();
        let wt = l.window(&Lane::Tech, Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(texts(&wt), vec!["A", "ta", "T2"]);
        assert!(!format!("{wt:?}").contains("task") && !format!("{wt:?}").contains("FINDINGS"));
        let h = l.history();
        assert_eq!(h.iter().map(|e| e.user.as_str()).collect::<Vec<_>>(), vec!["A", "P", "Q", "T2"]);
        assert_eq!(h[1].delegations.len(), 1);
        assert!(h[2].delegations.is_empty());
    }

    #[test]
    fn delegation_attaches_past_interleaved_direct_exchange() {
        let (_d, mut l) = log();
        l.append_user("P", "t", M).unwrap();
        l.append_assistant(tool_use_asst(Lane::Muthur), "t").unwrap();
        l.append_user("T1", "t", T).unwrap();
        l.append_assistant(asst_for(T, "ta"), "t").unwrap();
        let id = l.append_delegation("d1", "task", false, "t").unwrap();
        l.append_assistant(Assistant { lane: d(&id), ..asst("FINDINGS", None) }, "t").unwrap();
        let h = l.history();
        assert_eq!(h.iter().map(|e| e.user.as_str()).collect::<Vec<_>>(), vec!["P", "T1"]);
        assert_eq!(h[0].delegations.len(), 1);
        assert!(h[1].delegations.is_empty());
    }

    #[test]
    fn feature_off_history_still_skips_delegations() {
        let (_d, l) = full_log();
        let shown: Vec<_> = l.history().into_iter().filter(|ex| ex.agent == AgentId::Muthur).collect();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].user, "P");
    }

    #[test]
    fn repeated_call_id_gets_unique_lanes() {
        let (_d, mut l) = log();
        l.append_user("P", "t", M).unwrap();
        let a = l.append_delegation("d1", "first", false, "t").unwrap();
        l.append_assistant(Assistant { lane: d(&a), ..asst("ONE", None) }, "t").unwrap();
        let b = l.append_delegation("d1", "second", false, "t").unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with("d1#") && b.starts_with("d1#"), "{a} {b}");
        let w = l.window(&d(&b), Provider::Anthropic, "m", "SYS", "", 200000, false).unwrap();
        assert_eq!(w, vec![user("second")]);
        let user_recs: Vec<_> = l.records.iter().filter(|r| kind(r) == "user" && r.get("delegated").is_some()).collect();
        assert!(user_recs.iter().all(|r| r["call"] == "d1"), "{user_recs:#?}");
        let calls: Vec<_> = l.history()[0].delegations.iter().map(|x| (x.call_id.clone(), x.report.clone())).collect();
        assert_eq!(calls, vec![("d1".to_string(), Some("ONE".to_string())), ("d1".to_string(), None)]);
    }

    #[test]
    fn delegation_without_call_falls_back_to_its_id() {
        let (_d, mut l) = log();
        l.append_user("P", "t", M).unwrap();
        l.write(json!({"kind":"user","ts":"t","agent":"tech","delegated":"d7","report":"relayed","text":"task"})).unwrap();
        assert_eq!(l.history()[0].delegations[0].call_id, "d7");
    }
}
