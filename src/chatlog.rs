//! Append-only JSONL conversation log and message-window rules.
use crate::config::Provider;
use crate::llm::anthropic::replay_blocks;
use crate::llm::{Message, MessageContent, Role};
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
}

/// One past exchange, for redisplay.
#[derive(Clone, Debug, PartialEq)]
pub struct Exchange {
    pub user: String,
    pub reply: Option<(String, String)>,
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

    pub fn append_user(&mut self, text: &str, ts: &str) -> std::io::Result<u64> {
        self.write(json!({"kind":"user","ts":ts,"text":text}))
    }

    pub fn append_assistant(&mut self, a: Assistant, ts: &str) -> std::io::Result<()> {
        let mut r = json!({"kind":"assistant","ts":ts,"provider":a.provider,"model":a.model,"text":a.text,"stop":a.stop});
        if let Some(raw) = a.raw {
            r["raw"] = Value::Array(raw);
        }
        self.write(r).map(|_| ())
    }

    fn append_epoch(&mut self, from: u64, reason: &str, hash: &str) -> std::io::Result<()> {
        self.write(json!({"kind":"epoch","from":from,"reason":reason,"system_hash":hash})).map(|_| ())
    }

    fn latest_epoch(&self) -> Option<&Value> {
        self.records.iter().rev().find(|r| kind(r) == "epoch")
    }

    /// Seq below which assistant turns lose thinking. The very first epoch
    /// (initial system hash, no predecessor) changes no prefix, so it counts as 0.
    fn strip_boundary(&self) -> u64 {
        let mut it = self.records.iter().filter(|r| kind(r) == "epoch");
        let first = it.next().map(seq);
        match it.next_back() {
            Some(e) => seq(e),
            None => first.map(|_| 0).unwrap_or(0),
        }
    }

    /// Build the window from user seq `from`. Returns (messages, candidate trim starts).
    fn build(&self, provider: Provider, from: u64, e_seq: u64) -> (Vec<Message>, Vec<u64>) {
        let ua: Vec<&Value> = self.records.iter().filter(|r| matches!(kind(r), "user" | "assistant")).collect();
        let mut msgs = Vec::new();
        let mut starts = Vec::new();
        if ua.last().map(|r| kind(r)) != Some("user") {
            return (msgs, starts);
        }
        let last = ua.len() - 1;
        for (i, u) in ua.iter().enumerate() {
            if kind(u) != "user" || seq(u) < from {
                continue;
            }
            if i == last {
                starts.push(seq(u));
                msgs.push(Message { role: Role::User, content: MessageContent::Text(text(u).into()) });
                continue;
            }
            let a = ua[i + 1];
            if kind(a) != "assistant" {
                continue;
            }
            let stop = a.get("stop").and_then(Value::as_str).unwrap_or("");
            if matches!(stop, "error" | "refusal") || text(a).trim().is_empty() {
                continue;
            }
            starts.push(seq(u));
            msgs.push(Message { role: Role::User, content: MessageContent::Text(text(u).into()) });
            let mut content = MessageContent::Text(text(a).into());
            if provider == Provider::Anthropic
                && stop != "interrupted"
                && let Some(raw) = a.get("raw").and_then(Value::as_array)
            {
                let b = replay_blocks(raw, seq(a) < e_seq);
                if !b.is_empty() {
                    content = MessageContent::Blocks(b);
                }
            }
            msgs.push(Message { role: Role::Assistant, content });
        }
        (msgs, starts)
    }

    fn estimate(system: &str, msgs: &[Message]) -> usize {
        let n: usize = msgs
            .iter()
            .map(|m| match &m.content {
                MessageContent::Text(t) => t.chars().count(),
                MessageContent::Blocks(b) => b.iter().map(|v| v.to_string().chars().count()).sum(),
            })
            .sum();
        (system.chars().count() + n) / 4
    }

    pub fn window(&mut self, provider: Provider, model: &str, system: &str, budget_tokens: usize) -> std::io::Result<Vec<Message>> {
        let h = fnv64(&format!("{provider:?}\n{model}\n{system}"));
        let last = self.latest_epoch().map(|e| (e["from"].as_u64().unwrap_or(0), e["system_hash"].as_str().unwrap_or("").to_string()));
        if last.as_ref().map(|l| l.1.as_str()) != Some(h.as_str()) {
            self.append_epoch(last.map(|l| l.0).unwrap_or(0), "system_changed", &h)?;
        }
        let e = self.latest_epoch().unwrap();
        let from = e["from"].as_u64().unwrap_or(0);
        let e_seq = self.strip_boundary();
        let (msgs, starts) = self.build(provider, from, e_seq);
        if Self::estimate(system, &msgs) <= budget_tokens {
            return Ok(msgs);
        }
        let Some(&current) = starts.last() else { return Ok(msgs) };
        // Epoch seq the trim will get.
        let new_e = self.next_seq;
        let new_from = starts
            .iter()
            .copied()
            .find(|&c| Self::estimate(system, &self.build(provider, c, new_e).0) <= budget_tokens / 2)
            .unwrap_or(current);
        self.append_epoch(new_from, "trim", &h)?;
        Ok(self.build(provider, new_from, new_e).0)
    }

    pub fn history(&self) -> Vec<Exchange> {
        let ua: Vec<&Value> = self.records.iter().filter(|r| matches!(kind(r), "user" | "assistant")).collect();
        let mut out = Vec::new();
        for (i, u) in ua.iter().enumerate() {
            if kind(u) != "user" {
                continue;
            }
            let reply = ua.get(i + 1).filter(|a| kind(a) == "assistant").map(|a| {
                (text(a).to_string(), a.get("stop").and_then(Value::as_str).unwrap_or("").to_string())
            });
            out.push(Exchange { user: text(u).into(), reply });
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
        Assistant { provider: Provider::Anthropic, model: "m".into(), text: text.into(), stop: "end_turn".into(), raw }
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
        l.append_user("[SHIP TIME X] HI", "t").unwrap();
        l.append_assistant(asst("HELLO", None), "t").unwrap();
        let l2 = ChatLog::open(d.path().join("chat.jsonl")).unwrap();
        assert_eq!(l2.history(), vec![Exchange { user: "[SHIP TIME X] HI".into(), reply: Some(("HELLO".into(), "end_turn".into())) }]);
        assert_eq!(std::fs::metadata(d.path().join("chat.jsonl")).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn window_replays_raw_and_ends_with_inquiry() {
        let (_d, mut l) = log();
        l.append_user("A", "t").unwrap();
        let raw = thinking_raw("B");
        l.append_assistant(asst("B", Some(raw.clone())), "t").unwrap();
        l.append_user("C", "t").unwrap();
        let w = l.window(Provider::Anthropic, "m", "SYS", 200000).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(w[1].content, MessageContent::Blocks(raw));
        assert_eq!(w[2].content, t("C"));
    }

    fn failed_log() -> (tempfile::TempDir, ChatLog) {
        let (d, mut l) = log();
        l.append_user("A", "t").unwrap();
        let mut a = asst("oops", None);
        a.stop = "error".into();
        l.append_assistant(a, "t").unwrap();
        l.append_user("B", "t").unwrap();
        l.append_user("C", "t").unwrap();
        (d, l)
    }

    #[test]
    fn failed_pairs_and_orphans_excluded() {
        let (_d, mut l) = failed_log();
        let w = l.window(Provider::Anthropic, "m", "SYS", 200000).unwrap();
        assert_eq!(w, vec![Message { role: Role::User, content: t("C") }]);
    }

    #[test]
    fn openai_gets_text_only() {
        let (_d, mut l) = log();
        l.append_user("A", "t").unwrap();
        l.append_assistant(asst("B", Some(thinking_raw("B"))), "t").unwrap();
        l.append_user("C", "t").unwrap();
        let w = l.window(Provider::Openai, "m", "SYS", 200000).unwrap();
        assert_eq!(w.len(), 3);
        assert_eq!(w[1].content, t("B"));
    }

    #[test]
    fn system_change_strips_thinking_once() {
        let (_d, mut l) = log();
        l.append_user("A", "t").unwrap();
        l.window(Provider::Anthropic, "m", "SYS1", 200000).unwrap();
        l.append_assistant(asst("B", Some(thinking_raw("B"))), "t").unwrap();
        l.append_user("C", "t").unwrap();
        let w = l.window(Provider::Anthropic, "m", "SYS2", 200000).unwrap();
        assert!(!has_thinking(&w[1].content));
        l.append_assistant(asst("D", Some(thinking_raw("D"))), "t").unwrap();
        l.append_user("E", "t").unwrap();
        let w = l.window(Provider::Anthropic, "m", "SYS2", 200000).unwrap();
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
            l.append_user(&big, "t").unwrap();
            l.append_assistant(asst(&big, None), "t").unwrap();
        }
        l.append_user("now", "t").unwrap();
        let w = l.window(Provider::Anthropic, "m", "SYS", 1000).unwrap();
        assert!(ChatLog::estimate("SYS", &w) <= 500);
        assert_eq!(epochs(&l, "trim"), 1);
        l.append_assistant(asst("ok", None), "t").unwrap();
        l.append_user("next", "t").unwrap();
        let n = l.records.iter().filter(|r| kind(r) == "epoch").count();
        l.window(Provider::Anthropic, "m", "SYS", 1000).unwrap();
        assert_eq!(l.records.iter().filter(|r| kind(r) == "epoch").count(), n);
    }

    #[test]
    fn truncated_last_line_is_skipped_and_append_starts_fresh_line() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("chat.jsonl");
        std::fs::write(&p, "{\"v\":1,\"seq\":0,\"kind\":\"user\",\"ts\":\"t\",\"text\":\"A\"}\n{\"v\":1,\"seq\":1,\"ki").unwrap();
        let mut l = ChatLog::open(p.clone()).unwrap();
        assert_eq!(l.history().len(), 1);
        assert_eq!(l.append_user("B", "t").unwrap(), 1);
        let l2 = ChatLog::open(p).unwrap();
        assert_eq!(l2.history().len(), 2);
    }

    #[test]
    fn purge_deletes() {
        let (d, mut l) = log();
        l.append_user("A", "t").unwrap();
        l.purge().unwrap();
        assert!(!d.path().join("chat.jsonl").exists());
        assert!(l.history().is_empty());
        assert_eq!(l.append_user("B", "t").unwrap(), 0);
    }

    #[test]
    fn refused_pairs_excluded_but_shown() {
        let (_d, mut l) = log();
        l.append_user("A", "t").unwrap();
        let mut a = asst("PARTIAL", Some(vec![json!({"type":"text","text":"PARTIAL"})]));
        a.stop = "refusal".into();
        l.append_assistant(a, "t").unwrap();
        l.append_user("C", "t").unwrap();
        let w = l.window(Provider::Anthropic, "m", "SYS", 200000).unwrap();
        assert_eq!(w, vec![Message { role: Role::User, content: t("C") }]);
        assert_eq!(l.history()[0].reply, Some(("PARTIAL".into(), "refusal".into())));
    }

    #[test]
    fn whitespace_only_reply_excluded() {
        let (_d, mut l) = log();
        l.append_user("A", "t").unwrap();
        l.append_assistant(asst(" \n\t", Some(vec![json!({"type":"text","text":" \n\t"})])), "t").unwrap();
        l.append_user("C", "t").unwrap();
        let w = l.window(Provider::Anthropic, "m", "SYS", 200000).unwrap();
        assert_eq!(w, vec![Message { role: Role::User, content: t("C") }]);
    }

    #[test]
    fn model_change_starts_epoch_and_strips_thinking_once() {
        let (_d, mut l) = log();
        l.append_user("A", "t").unwrap();
        l.window(Provider::Anthropic, "m1", "SYS", 200000).unwrap();
        l.append_assistant(asst("B", Some(thinking_raw("B"))), "t").unwrap();
        l.append_user("C", "t").unwrap();
        let w = l.window(Provider::Anthropic, "m2", "SYS", 200000).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
        assert!(!has_thinking(&w[1].content));
        l.append_assistant(asst("D", Some(thinking_raw("D"))), "t").unwrap();
        l.append_user("E", "t").unwrap();
        let w = l.window(Provider::Anthropic, "m2", "SYS", 200000).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
        assert!(!has_thinking(&w[1].content));
        assert!(has_thinking(&w[3].content));
    }

    #[test]
    fn provider_change_starts_epoch() {
        let (_d, mut l) = log();
        l.append_user("A", "t").unwrap();
        l.window(Provider::Anthropic, "m", "SYS", 200000).unwrap();
        l.window(Provider::Openai, "m", "SYS", 200000).unwrap();
        assert_eq!(epochs(&l, "system_changed"), 2);
    }

    #[test]
    fn failed_write_forces_fresh_line() {
        let (_d, mut l) = log();
        l.append_user("A", "t").unwrap();
        l.path = PathBuf::from("/dev/full");
        assert!(l.append_user("B", "t").is_err());
        assert!(l.needs_newline);
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv64(""), "cbf29ce484222325");
        assert_eq!(fnv64("a"), "af63dc4c8601ec8c");
    }
}
