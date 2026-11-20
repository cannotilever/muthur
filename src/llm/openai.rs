//! OpenAI-compatible (vLLM) chat-completions protocol: request JSON and SSE stream parser.
use super::sse::SseRecord;
use super::{ChatRequest, ToolChoice, LlmError, MessageContent, Role, Stop, StreamEvent, StreamParser};
use crate::config::MuthurSection;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const PATH: &str = "/v1/chat/completions";

pub fn headers(cfg: &MuthurSection) -> Vec<(&'static str, String)> {
    let mut h = vec![("content-type", "application/json".to_string())];
    if let Some(k) = cfg.api_key() {
        h.push(("authorization", format!("Bearer {k}")));
    }
    h
}

pub fn request_body(cfg: &MuthurSection, req: &ChatRequest) -> Value {
    let mut messages = vec![json!({"role": "system", "content": req.system})];
    for m in &req.messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        match &m.content {
            MessageContent::Text(t) => messages.push(json!({"role": role, "content": t})),
            MessageContent::Blocks(v) => {
                let t: String = v.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect();
                messages.push(json!({"role": role, "content": t}));
            }
            MessageContent::ToolResults(rs) => {
                for r in rs {
                    let content = if r.is_error { format!("ERROR: {}", r.content) } else { r.content.clone() };
                    messages.push(json!({"role": "tool", "tool_call_id": r.id, "content": content}));
                }
            }
            MessageContent::ToolCalls { text, calls } => {
                let content = if text.trim().is_empty() { Value::Null } else { json!(text) };
                messages.push(json!({"role": "assistant", "content": content, "tool_calls": calls}));
            }
        }
    }
    let mut body = json!({
        "model": cfg.model().unwrap_or_default(),
        "max_tokens": cfg.max_tokens,
        "stream": true,
        "messages": messages,
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}}))
            .collect();
        match &req.tool_choice {
            ToolChoice::Auto => {}
            ToolChoice::None => body["tool_choice"] = json!("none"),
            ToolChoice::Force(n) => body["tool_choice"] = json!({"type": "function", "function": {"name": n}}),
        }
    }
    body
}

#[derive(Default)]
pub struct Parser {
    /// index -> (id, name, arguments)
    calls: BTreeMap<usize, (String, String, String)>,
    model: Option<String>,
    finish: Option<String>,
    done: bool,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }
}

fn map_stop(s: &str) -> Stop {
    match s {
        "stop" => Stop::End,
        "length" => Stop::MaxTokens,
        "tool_calls" | "function_call" => Stop::ToolUse,
        "content_filter" => Stop::Refusal,
        o => Stop::Other(o.to_string()),
    }
}

impl StreamParser for Parser {
    fn feed(&mut self, rec: &SseRecord) -> Vec<StreamEvent> {
        let data = rec.data.trim();
        let mut out = vec![];
        if data == "[DONE]" {
            self.done = true;
            let mut raw = vec![];
            for (_, (id, name, args)) in std::mem::take(&mut self.calls) {
                raw.push(json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}}));
                let input = if args.trim().is_empty() { json!({}) } else { serde_json::from_str(&args).unwrap_or_else(|_| json!({})) };
                out.push(StreamEvent::ToolCall { id, name, input });
            }
            // Some servers report "stop" (or nothing) after tool calls; calls mean ToolUse.
            let stop = if raw.is_empty() { map_stop(self.finish.as_deref().unwrap_or("stop")) } else { Stop::ToolUse };
            out.push(StreamEvent::Done {
                stop,
                model: self.model.clone(),
                raw: if raw.is_empty() { None } else { Some(raw) },
            });
            return out;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else { return out };
        if v.get("error").is_some() {
            self.done = true;
            return vec![StreamEvent::Error(LlmError::Transmission)];
        }
        if self.model.is_none() {
            self.model = v["model"].as_str().map(str::to_string);
        }
        let Some(choice) = v["choices"].get(0) else { return out };
        if let Some(t) = choice["delta"]["content"].as_str().filter(|t| !t.is_empty()) {
            out.push(StreamEvent::Text(t.to_string()));
        }
        if let Some(tcs) = choice["delta"]["tool_calls"].as_array() {
            for tc in tcs {
                let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                let e = self.calls.entry(idx).or_default();
                if let Some(id) = tc["id"].as_str() {
                    e.0 = id.to_string();
                }
                if let Some(n) = tc["function"]["name"].as_str() {
                    e.1.push_str(n);
                }
                if let Some(a) = tc["function"]["arguments"].as_str() {
                    e.2.push_str(a);
                }
            }
        }
        if let Some(f) = choice["finish_reason"].as_str() {
            self.finish = Some(f.to_string());
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MuthurSection;
    use crate::llm::sse::SseDecoder;
    use crate::llm::{ChatRequest, LlmError, Message, MessageContent, Role, Stop, StreamEvent, StreamParser};
    use serde_json::json;

    fn run(name: &str) -> Vec<StreamEvent> {
        let bytes = std::fs::read(format!("{}/tests/fixtures/llm/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut p = Parser::new();
        let mut dec = SseDecoder::default();
        let mut ev = Vec::new();
        for rec in dec.push(&bytes) {
            ev.extend(p.feed(&rec));
        }
        ev.extend(p.finish());
        ev
    }
    fn texts(ev: &[StreamEvent]) -> String {
        ev.iter().filter_map(|e| if let StreamEvent::Text(t) = e { Some(t.as_str()) } else { None }).collect()
    }
    fn user(s: &str) -> Message { Message { role: Role::User, content: MessageContent::Text(s.into()) } }
    fn cfg() -> MuthurSection {
        MuthurSection { provider: crate::config::Provider::Openai, base_url: Some("http://10.0.0.5:8000".into()), model: Some("qwen".into()), ..Default::default() }
    }

    #[test] fn text_stream() { let ev = run("openai_text.sse"); assert_eq!(texts(&ev), "ALL SYSTEMS NOMINAL."); assert!(matches!(ev.last(), Some(StreamEvent::Done{stop: Stop::End, raw: None, ..}))); }
    #[test] fn tool_calls_accumulate_by_index() { let ev = run("openai_tool.sse"); assert!(ev.iter().any(|e| matches!(e, StreamEvent::ToolCall{name, input, ..} if name == "metrics" && input == &json!({"host":"local"})))); assert!(matches!(ev.last(), Some(StreamEvent::Done{stop: Stop::ToolUse, ..}))); }
    #[test] fn length_is_max_tokens() { assert!(matches!(run("openai_length.sse").last(), Some(StreamEvent::Done{stop: Stop::MaxTokens, ..}))); }
    #[test] fn system_goes_first() { let b = request_body(&cfg(), &ChatRequest{ system: "SYS".into(), messages: vec![user("HI")], tools: vec![], tool_choice: ToolChoice::Auto }); assert_eq!(b["messages"][0], json!({"role":"system","content":"SYS"})); assert_eq!(b["messages"][1], json!({"role":"user","content":"HI"})); assert_eq!(b["model"], "qwen"); assert_eq!(b["stream"], true); }
    #[test] fn bearer_only_with_key() { assert!(!headers(&cfg()).iter().any(|(k,_)| *k == "authorization")); assert!(headers(&MuthurSection{ api_key: Some("k".into()), ..cfg() }).contains(&("authorization", "Bearer k".into()))); }
    #[test] fn api_key_is_trimmed() { assert!(headers(&MuthurSection{ api_key: Some(" k\n".into()), ..cfg() }).contains(&("authorization", "Bearer k".into()))); assert!(!headers(&MuthurSection{ api_key: Some("  ".into()), ..cfg() }).iter().any(|(k,_)| *k == "authorization")); }
    #[test] fn missing_done_is_transmission_error() {
        let mut p = Parser::new();
        let mut dec = SseDecoder::default();
        let s = "data: {\"model\":\"qwen\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"AL\"},\"finish_reason\":null}]}\n\n";
        for rec in dec.push(s.as_bytes()) { p.feed(&rec); }
        assert_eq!(p.finish(), vec![StreamEvent::Error(LlmError::Transmission)]);
    }
    fn spec(name: &str) -> crate::llm::ToolSpec { crate::llm::ToolSpec{ name: name.into(), description: "d".into(), schema: json!({"type":"object","properties":{}}) } }
    fn req_with(messages: Vec<Message>) -> ChatRequest { ChatRequest { system: "SYS".into(), messages, tools: vec![], tool_choice: ToolChoice::Auto } }
    #[test] fn tools_and_choice_in_body() {
        let mut r = req_with(vec![user("HI")]);
        assert!(request_body(&cfg(), &r).get("tools").is_none());
        r.tools = vec![spec("ship_status")];
        let b = request_body(&cfg(), &r);
        assert_eq!(b["tools"][0], json!({"type":"function","function":{"name":"ship_status","description":"d","parameters":{"type":"object","properties":{}}}}));
        assert!(b.get("tool_choice").is_none());
        r.tool_choice = ToolChoice::None;
        assert_eq!(request_body(&cfg(), &r)["tool_choice"], "none");
    }
    #[test] fn forced_choice_bodies() {
        let mut r = req_with(vec![user("HI")]);
        r.tools = vec![spec("verdict")];
        r.tool_choice = ToolChoice::Force("verdict".into());
        assert_eq!(request_body(&cfg(), &r)["tool_choice"], json!({"type":"function","function":{"name":"verdict"}}));
        assert!(request_body(&cfg(), &r).get("thinking").is_none());
    }
    #[test] fn assistant_tool_calls_and_results() {
        let c = json!({"id":"call_1","type":"function","function":{"name":"metrics","arguments":"{}"}});
        let tr = |id: &str, content: &str, e: bool| crate::llm::ToolResult { id: id.into(), name: "metrics".into(), content: content.into(), is_error: e, label: String::new() };
        let r = req_with(vec![user("HI"),
            Message { role: Role::Assistant, content: MessageContent::ToolCalls { text: " ".into(), calls: vec![c.clone()] } },
            Message { role: Role::User, content: MessageContent::ToolResults(vec![tr("call_1", "X", false), tr("call_2", "Y", true)]) }]);
        let m = &request_body(&cfg(), &r)["messages"];
        assert_eq!(m[2], json!({"role":"assistant","content":null,"tool_calls":[c]}));
        assert_eq!(m[3], json!({"role":"tool","tool_call_id":"call_1","content":"X"}));
        assert_eq!(m[4]["content"], "ERROR: Y");
        assert_eq!(m[4]["tool_call_id"], "call_2");
    }
    #[test] fn done_raw_carries_tool_calls() {
        let ev = run("openai_tool.sse");
        assert!(matches!(ev.last(), Some(StreamEvent::Done{raw: Some(r), ..}) if r == &vec![json!({"id":"call_1","type":"function","function":{"name":"metrics","arguments":"{\"host\":\"local\"}"}})]));
    }
    #[test] fn tool_calls_with_stop_finish_are_tool_use() {
        // Some OpenAI-compatible servers end a tool-calling response with finish_reason "stop" (or none).
        for finish in ["\"stop\"", "null"] {
            let mut p = Parser::new();
            let mut dec = SseDecoder::default();
            let s = format!(
                "data: {{\"model\":\"qwen\",\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"c1\",\"type\":\"function\",\"function\":{{\"name\":\"ship_status\",\"arguments\":\"{{}}\"}}}}]}},\"finish_reason\":null}}]}}\n\n\
                 data: {{\"model\":\"qwen\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":{finish}}}]}}\n\n\
                 data: [DONE]\n\n"
            );
            let mut ev = Vec::new();
            for rec in dec.push(s.as_bytes()) { ev.extend(p.feed(&rec)); }
            assert!(ev.iter().any(|e| matches!(e, StreamEvent::ToolCall{name, ..} if name == "ship_status")), "{ev:?}");
            assert!(matches!(ev.last(), Some(StreamEvent::Done{stop: Stop::ToolUse, ..})), "{finish}: {ev:?}");
        }
    }
    #[test] fn plain_text_done_raw_none() { assert!(matches!(run("openai_text.sse").last(), Some(StreamEvent::Done{raw: None, ..}))); }
}
