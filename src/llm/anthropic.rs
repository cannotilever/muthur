//! Anthropic Messages API protocol: request JSON, SSE stream parser, replay rule.
use super::sse::SseRecord;
use super::{ChatRequest, LlmError, MessageContent, Role, Stop, StreamEvent, StreamParser};
use crate::config::MuthurSection;
use serde_json::{json, Value};
use std::collections::HashMap;

pub const PATH: &str = "/v1/messages";

pub fn headers(cfg: &MuthurSection) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("x-api-key", cfg.api_key().unwrap_or_default().to_string()),
        ("anthropic-version", "2023-06-01".to_string()),
        ("content-type", "application/json".to_string()),
    ];
    if cfg.fallbacks {
        h.push(("anthropic-beta", "server-side-fallback-2026-07-01".to_string()));
    }
    h
}

pub fn request_body(cfg: &MuthurSection, req: &ChatRequest) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            match &m.content {
                MessageContent::Text(s) => json!({"role": role, "content": s}),
                MessageContent::Blocks(v) => json!({"role": role, "content": v}),
                MessageContent::ToolResults(rs) => {
                    let blocks: Vec<Value> = rs
                        .iter()
                        .map(|r| json!({"type": "tool_result", "tool_use_id": r.id, "content": r.content, "is_error": r.is_error}))
                        .collect();
                    json!({"role": "user", "content": blocks})
                }
                MessageContent::ToolCalls { text, .. } => json!({"role": role, "content": text}),
            }
        })
        .collect();
    let mut cache = json!({"type": "ephemeral"});
    if cfg.cache_ttl == "1h" {
        cache["ttl"] = json!("1h");
    }
    let mut body = json!({
        "model": cfg.model().unwrap_or_default(),
        "max_tokens": cfg.max_tokens,
        "stream": true,
        "system": req.system,
        "messages": messages,
        "output_config": {"effort": cfg.effort},
        "cache_control": cache,
    });
    if cfg.fallbacks {
        body["fallbacks"] = json!("default");
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.schema, "strict": true}))
            .collect();
        if req.tool_choice_none {
            body["tool_choice"] = json!({"type": "none"});
        }
    }
    body
}

/// Blocks to send when replaying a stored assistant turn.
pub fn replay_blocks(raw: &[Value], strip_thinking: bool) -> Vec<Value> {
    let ty = |b: &Value| b.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    let last_fallback = raw.iter().rposition(|b| ty(b) == "fallback");
    raw.iter()
        .enumerate()
        .filter(|(i, b)| {
            let t = ty(b);
            let thinking = t == "thinking" || t == "redacted_thinking";
            if t == "tool_use" && last_fallback.is_some_and(|f| *i < f) {
                return false;
            }
            if thinking && (strip_thinking || last_fallback.is_some_and(|f| *i < f)) {
                return false;
            }
            // An empty or whitespace-only text block (e.g. a refusal) would be rejected on replay.
            !(t == "text" && b.get("text").and_then(Value::as_str).is_some_and(|s| s.trim().is_empty()))
        })
        .map(|(_, b)| b.clone())
        .collect()
}

#[derive(Default)]
pub struct Parser {
    blocks: Vec<Value>,
    partial: HashMap<usize, String>,
    model: Option<String>,
    stop: Option<String>,
    usage: HashMap<&'static str, u64>,
    done: bool,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    fn block(&mut self, idx: usize) -> &mut Value {
        while self.blocks.len() <= idx {
            self.blocks.push(Value::Null);
        }
        &mut self.blocks[idx]
    }

    fn take_usage(&mut self, u: &Value, keys: &[&'static str]) {
        for k in keys {
            if let Some(n) = u.get(*k).and_then(Value::as_u64) {
                self.usage.insert(*k, n);
            }
        }
    }
}

fn map_stop(s: &str) -> Stop {
    match s {
        "end_turn" | "stop_sequence" => Stop::End,
        "max_tokens" => Stop::MaxTokens,
        "tool_use" => Stop::ToolUse,
        "refusal" => Stop::Refusal,
        o => Stop::Other(o.to_string()),
    }
}

impl StreamParser for Parser {
    fn feed(&mut self, rec: &SseRecord) -> Vec<StreamEvent> {
        let Ok(v) = serde_json::from_str::<Value>(&rec.data) else { return vec![] };
        let kind = v.get("type").and_then(Value::as_str).or(rec.event.as_deref()).unwrap_or("");
        let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let mut out = vec![];
        match kind {
            "message_start" => {
                self.model = v["message"]["model"].as_str().map(str::to_string);
                let u = v["message"]["usage"].clone();
                self.take_usage(&u, &["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens"]);
            }
            "content_block_start" => {
                *self.block(idx) = v["content_block"].clone();
            }
            "content_block_delta" => {
                let d = &v["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or("");
                        if !t.is_empty() {
                            let b = self.block(idx);
                            let cur = b["text"].as_str().unwrap_or("").to_string();
                            b["text"] = json!(cur + t);
                            out.push(StreamEvent::Text(t.to_string()));
                        }
                    }
                    "thinking_delta" => {
                        let t = d["thinking"].as_str().unwrap_or("");
                        let b = self.block(idx);
                        let cur = b["thinking"].as_str().unwrap_or("").to_string();
                        b["thinking"] = json!(cur + t);
                    }
                    "signature_delta" => {
                        let s = d["signature"].as_str().unwrap_or("").to_string();
                        self.block(idx)["signature"] = json!(s);
                    }
                    "input_json_delta" => {
                        let p = d["partial_json"].as_str().unwrap_or("");
                        self.partial.entry(idx).or_default().push_str(p);
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(s) = self.partial.remove(&idx) {
                    let input = if s.trim().is_empty() { json!({}) } else { serde_json::from_str(&s).unwrap_or_else(|_| json!({})) };
                    let b = self.block(idx);
                    b["input"] = input.clone();
                    out.push(StreamEvent::ToolCall {
                        id: b["id"].as_str().unwrap_or("").to_string(),
                        name: b["name"].as_str().unwrap_or("").to_string(),
                        input,
                    });
                }
            }
            "message_delta" => {
                if let Some(s) = v["delta"]["stop_reason"].as_str() {
                    self.stop = Some(s.to_string());
                }
                let u = v["usage"].clone();
                self.take_usage(&u, &["output_tokens", "input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]);
            }
            "message_stop" => {
                self.done = true;
                let blocks = self.blocks.iter().filter(|b| !b.is_null()).cloned().collect();
                out.push(StreamEvent::Done {
                    stop: map_stop(self.stop.as_deref().unwrap_or("end_turn")),
                    model: self.model.clone(),
                    raw: Some(blocks),
                });
            }
            "error" => {
                self.done = true;
                let e = if v["error"]["type"] == "overloaded_error" { LlmError::Overloaded } else { LlmError::Transmission };
                out.push(StreamEvent::Error(e));
            }
            _ => {}
        }
        out
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        if self.done {
            vec![]
        } else {
            self.done = true;
            vec![StreamEvent::Error(LlmError::Transmission)]
        }
    }

    fn usage_summary(&self) -> Option<String> {
        if self.usage.is_empty() {
            return None;
        }
        let g = |k: &str| self.usage.get(k).copied().unwrap_or(0);
        Some(format!(
            "in={} cache_write={} cache_read={} out={}",
            g("input_tokens"),
            g("cache_creation_input_tokens"),
            g("cache_read_input_tokens"),
            g("output_tokens")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MuthurSection;
    use crate::llm::sse::SseDecoder;
    use crate::llm::{ChatRequest, LlmError, Message, MessageContent, Role, Stop, StreamEvent, StreamParser, ToolResult, ToolSpec};
    use serde_json::json;

    fn feed_file(p: &mut Parser, name: &str) -> Vec<StreamEvent> {
        let bytes = std::fs::read(format!("{}/tests/fixtures/llm/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut dec = SseDecoder::default();
        let mut out = Vec::new();
        for rec in dec.push(&bytes) {
            out.extend(p.feed(&rec));
        }
        out
    }
    fn run(name: &str) -> Vec<StreamEvent> {
        let mut p = Parser::new();
        let mut ev = feed_file(&mut p, name);
        ev.extend(p.finish());
        ev
    }
    fn texts(ev: &[StreamEvent]) -> String {
        ev.iter().filter_map(|e| if let StreamEvent::Text(t) = e { Some(t.as_str()) } else { None }).collect()
    }
    fn user(s: &str) -> Message { Message { role: Role::User, content: MessageContent::Text(s.into()) } }
    fn req_with(messages: Vec<Message>) -> ChatRequest { ChatRequest { system: "SYS".into(), messages, tools: vec![], tool_choice_none: false } }

    #[test] fn text_stream() { let ev = run("anthropic_text.sse"); assert_eq!(texts(&ev), "ALL SYSTEMS NOMINAL."); assert!(matches!(ev.last(), Some(StreamEvent::Done{stop: Stop::End, model: Some(_), ..}))); }
    #[test] fn thinking_is_kept_raw_not_emitted() { let ev = run("anthropic_thinking.sse"); let StreamEvent::Done{raw: Some(raw), ..} = ev.last().unwrap() else { panic!() }; assert_eq!(raw[0]["type"], "thinking"); assert!(!raw[0]["signature"].as_str().unwrap().is_empty()); assert_eq!(raw[1], json!({"type":"text","text": texts(&ev)})); assert!(!ev.iter().any(|e| matches!(e, StreamEvent::Text(t) if t.is_empty()))); }
    #[test] fn tool_input_is_reassembled() { let ev = run("anthropic_tool.sse"); assert!(ev.iter().any(|e| matches!(e, StreamEvent::ToolCall{name, input, ..} if name == "metrics" && input == &json!({"host":"local"})))); assert!(matches!(ev.last(), Some(StreamEvent::Done{stop: Stop::ToolUse, ..}))); }
    #[test] fn error_event() { assert_eq!(run("anthropic_error.sse").last(), Some(&StreamEvent::Error(LlmError::Overloaded))); }
    #[test] fn refusal_stop() { assert!(matches!(run("anthropic_refusal.sse").last(), Some(StreamEvent::Done{stop: Stop::Refusal, ..}))); }
    #[test] fn truncated_stream_is_transmission_error() {
        let mut p = Parser::new();
        let mut dec = SseDecoder::default();
        let s = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"AL\"}}\n\n";
        for rec in dec.push(s.as_bytes()) { p.feed(&rec); }
        assert_eq!(p.finish(), vec![StreamEvent::Error(LlmError::Transmission)]);
    }
    #[test] fn replay_fallback_rule() { let raw = vec![json!({"type":"thinking","thinking":"","signature":"s"}), json!({"type":"fallback"}), json!({"type":"thinking","thinking":"","signature":"t"}), json!({"type":"text","text":"X"})]; assert_eq!(replay_blocks(&raw, false), raw[1..].to_vec()); assert_eq!(replay_blocks(&raw, true), vec![raw[1].clone(), raw[3].clone()]); }
    #[test] fn replay_drops_whitespace_text() { let raw = vec![json!({"type":"text","text":" \n "}), json!({"type":"text","text":"X"})]; assert_eq!(replay_blocks(&raw, false), raw[1..].to_vec()); }
    #[test] fn fallback_fixture_keeps_order_and_replay_drops_pre_fallback_thinking() {
        let ev = run("anthropic_fallback.sse");
        assert_eq!(texts(&ev), "PROCEEDING.");
        let Some(StreamEvent::Done{stop: Stop::End, raw: Some(raw), ..}) = ev.last() else { panic!("{ev:?}") };
        let types: Vec<&str> = raw.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(types, ["thinking", "fallback", "text"]);
        assert_eq!(replay_blocks(raw, false), raw[1..].to_vec());
    }
    #[test] fn api_key_is_trimmed() { let cfg = MuthurSection{ api_key: Some("  k\n".into()), ..Default::default() }; assert!(headers(&cfg).contains(&("x-api-key", "k".into()))); }
    #[test] fn body_shape() { let cfg = MuthurSection{ api_key: Some("k".into()), ..Default::default() }; let b = request_body(&cfg, &req_with(vec![user("HI")])); assert_eq!(b["model"], "claude-sonnet-5-5"); assert_eq!(b["cache_control"], json!({"type":"ephemeral"})); assert_eq!(b["stream"], true); assert_eq!(b["output_config"]["effort"], "low"); assert_eq!(b["fallbacks"], "default"); assert!(b.get("thinking").is_none()); assert_eq!(b["messages"][0], json!({"role":"user","content":"HI"})); }
    #[test] fn no_fallbacks_no_beta() { let cfg = MuthurSection{ fallbacks: false, api_key: Some("k".into()), ..Default::default() }; assert!(request_body(&cfg, &req_with(vec![])).get("fallbacks").is_none()); assert!(!headers(&cfg).iter().any(|(k,_)| *k == "anthropic-beta")); }
    #[test] fn one_hour_cache_ttl() { let cfg = MuthurSection{ cache_ttl: "1h".into(), api_key: Some("k".into()), ..Default::default() }; assert_eq!(request_body(&cfg, &req_with(vec![]))["cache_control"], json!({"type":"ephemeral","ttl":"1h"})); }
    #[test] fn usage_is_collected() { let mut p = Parser::new(); feed_file(&mut p, "anthropic_text.sse"); assert_eq!(p.usage_summary().as_deref(), Some("in=12 cache_write=0 cache_read=480 out=9")); }

    fn key_cfg() -> MuthurSection { MuthurSection{ api_key: Some("k".into()), ..Default::default() } }
    fn spec(name: &str) -> ToolSpec { ToolSpec{ name: name.into(), description: "d".into(), schema: json!({"type":"object","properties":{},"required":[],"additionalProperties":false}) } }
    #[test] fn tools_and_choice_in_body() { let cfg = key_cfg(); let mut r = req_with(vec![user("HI")]); r.tools = vec![spec("ship_status")]; let b = request_body(&cfg, &r); assert_eq!(b["tools"][0], json!({"name":"ship_status","description":"d","input_schema":{"type":"object","properties":{},"required":[],"additionalProperties":false},"strict":true})); assert!(b.get("tool_choice").is_none()); r.tool_choice_none = true; assert_eq!(request_body(&cfg, &r)["tool_choice"], json!({"type":"none"})); }
    #[test] fn no_tools_no_fields() { let b = request_body(&key_cfg(), &req_with(vec![user("HI")])); assert!(b.get("tools").is_none() && b.get("tool_choice").is_none()); }
    #[test] fn tool_results_message() { let m = Message{ role: Role::User, content: MessageContent::ToolResults(vec![ToolResult{ id:"t1".into(), name:"journal".into(), content:"X".into(), is_error:true, label:"L".into() }]) }; let b = request_body(&key_cfg(), &req_with(vec![user("HI"), m])); assert_eq!(b["messages"][1], json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"X","is_error":true}]})); }
    #[test] fn replay_keeps_tool_use() { let raw = vec![json!({"type":"thinking","thinking":"","signature":"s"}), json!({"type":"text","text":"CHECKING."}), json!({"type":"tool_use","id":"t1","name":"ship_status","input":{}})]; assert_eq!(replay_blocks(&raw, false), raw); assert_eq!(replay_blocks(&raw, true), raw[1..].to_vec()); }
    #[test] fn replay_drops_tool_use_before_fallback() { let raw = vec![json!({"type":"tool_use","id":"1","name":"m","input":{}}), json!({"type":"fallback"}), json!({"type":"text","text":"X"})]; assert_eq!(replay_blocks(&raw, false), raw[1..].to_vec()); }
    #[test] fn tool_thinking_fixture_raw_round_trips() {
        let ev = run("anthropic_tool_thinking.sse");
        assert!(ev.iter().any(|e| matches!(e, StreamEvent::ToolCall{name, input, ..} if name == "service_status" && input == &json!({"unit":"nginx"}))));
        let Some(StreamEvent::Done{stop: Stop::ToolUse, raw: Some(raw), ..}) = ev.last() else { panic!("{ev:?}") };
        let types: Vec<&str> = raw.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(types, ["thinking", "text", "tool_use"]);
        assert_eq!(replay_blocks(raw, false), *raw);
    }
}
