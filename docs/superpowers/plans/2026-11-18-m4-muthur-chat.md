# M4 — MU/TH/UR Chat Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The MU/TH/UR folder is a streaming chat with an LLM in the MU/TH/UR 6000 persona:
- replies are revealed at teletype speed;
- one JSONL conversation log persists across restarts;
- errors are reported in character.

It talks to either a vLLM server (OpenAI chat-completions) or Anthropic's native Messages API.

**Architecture:**
- **Pure core.** Two pure protocol modules (`llm::openai`, `llm::anthropic`) turn SSE records (`llm::sse`) into one `StreamEvent` type.
- **Client.** `llm::client` runs each request as a tokio task on the existing current-thread runtime, using reqwest with rustls+ring. It forwards `Event::Llm` onto the app channel.
- **Log.** `chatlog` owns the append-only log, plus the "epoch" rules that decide what gets sent: trimming, and stripping thinking blocks.
- **Tab.** `muthur::MuthurTab` ties together:
  - input, with type-ahead;
  - the output text pipeline (markdown strip → uppercase → ASCII);
  - a time-injected teletype `Reveal`;
  - the view.

**Tech Stack:** existing crates, plus:
- `reqwest = { version = "0.13", default-features = false, features = ["rustls-no-provider"] }`;
- `rustls = { version = "0.23", default-features = false, features = ["ring", "std", "tls12", "logging"] }`;
- `serde_json = "1"`;
- tokio gains the `net` and `io-util` features.

**Spec:** `docs/superpowers/specs/2026-11-18-m4-muthur-chat-design.md` (read it first; this plan does not repeat its tables).

## Global Constraints
- **TLS backend.** No `aws-lc-rs` / `aws-lc-sys` anywhere in the dependency tree: `cargo tree -i aws-lc-sys --target x86_64-unknown-linux-musl` must report that the package is not found. The release musl build must succeed without cmake.
- **No secrets in logs.** Nothing writes the API key, request bodies, response bodies or headers to `muthur.log`.
- **Screen text.** Everything the tab renders is printable ASCII, or a glyph `dialect::map_glyph` maps to something other than `Ascii(b'?')`. The golden test runs under `StrictWriter` with zero violations.
- **Frozen system prompt.** It is built once at startup and contains no clock. The time goes in each user turn as `[SHIP TIME DD MON YYYY HH:MM] `, using `app::chrome::format_date`'s date (with `year_offset`) plus `HH:MM`.
- **Log file.** `<dirs::state_dir()>/muthur/chat.jsonl`, created with mode `0600`, append-only except for `/PURGE`.
- **Anthropic headers.**
  - `anthropic-version: 2023-06-01`;
  - `x-api-key`;
  - `anthropic-beta: server-side-fallback-2026-07-01` only when `fallbacks = true`.
- **Anthropic body.** `output_config.effort`, plus `"fallbacks": "default"` when enabled. There is no `thinking` field.
- **Timeouts.** Connect timeout 15 s. Idle timeout 60 s between body chunks. No total timeout and no retries.
- **Defaults.** `reveal_cps = 50`, `max_tokens = 4096`, `effort = "low"`, `context_tokens = 200000`. The token estimate is `chars / 4`. A trim cuts down to ≤ 50 % of `context_tokens`.

## Review Focus
1. **The connection dies mid-reply (some text received, then an error).** The partial text stays on screen and is logged as `stop: "interrupted"`, and the status line shows the error. Test: Task 8 `error_after_partial_keeps_text`.
2. **The model replies with non-ASCII** (em dash, curly quotes, `…`, emoji, CJK). It is transliterated, with no strict violations and no `?` for common punctuation. Test: Task 7 `transliterates_punctuation_and_emoji`.
3. **A word or URL wider than the pane.** It is hard-broken and never overflows the border. Test: Task 8 `wrap_breaks_long_words`.
4. **The last line of `chat.jsonl` is half-written** (power loss mid-append). The line is skipped and the app starts. The next record continues `seq` and starts on a fresh line. Test: Task 5 `truncated_last_line_is_skipped_and_append_starts_fresh_line`.
5. **Enter on empty or whitespace-only input.** Nothing is sent or logged. Test: Task 8 `blank_enter_is_ignored`.

---

## File Structure

| File | Responsibility |
|---|---|
| `src/llm/mod.rs` | `Message`, `MessageContent`, `ChatRequest`, `StreamEvent`, `Stop`, `LlmError`, `StreamParser` trait. |
| `src/llm/sse.rs` | `SseDecoder`: bytes → `SseRecord { event, data }`. |
| `src/llm/anthropic.rs` | `request_body`, `Parser`, `replay_blocks`. |
| `src/llm/openai.rs` | `request_body`, `Parser`. |
| `src/llm/client.rs` | `install_crypto_provider`, `LlmClient` (reqwest), status → `LlmError` mapping. |
| `src/chatlog.rs` | `ChatLog`: records, load, append, `window`, `history`, `purge`, `fnv64`. |
| `src/muthur/text.rs` | `Cleaner` (streaming markdown strip), `ascii_safe`, `display_text`. |
| `src/muthur/reveal.rs` | `Reveal`. |
| `src/muthur/view.rs` | `wrap`, transcript → lines, status/input rendering. |
| `src/muthur/mod.rs` | `MuthurTab`, `MuthurDeps`, `PERSONA`, `system_prompt`, state machine, `Tab` impl. |
| `src/config.rs` | `[muthur]` section. |
| `src/app/tabs.rs`, `src/app/mod.rs`, `src/run.rs` | `Tab::on_llm`/`tick_interval`, `Event::Llm`, fast tick, wiring. |
| `tests/fixtures/llm/*.sse` | Recorded/hand-written stream fixtures. |
| `tests/golden.rs`, `tests/golden/muthur_windowed.txt` | Golden test. |
| `tests/llm_live.rs` | `#[ignore]` live smoke tests (run by the user). |

---

### Task 1: Dependencies, TLS provider, core LLM types, SSE framing

**Files:**
- Modify `Cargo.toml`.
- Create `src/llm/mod.rs` and `src/llm/sse.rs`.
- Add `pub mod llm;` to `src/lib.rs`.

**Interfaces — Produces:**
```rust
// llm/mod.rs
#[derive(Clone, Debug, PartialEq)] pub enum Role { User, Assistant }
#[derive(Clone, Debug, PartialEq)] pub enum MessageContent { Text(String), Blocks(Vec<serde_json::Value>) }
#[derive(Clone, Debug, PartialEq)] pub struct Message { pub role: Role, pub content: MessageContent }
#[derive(Clone, Debug, PartialEq)] pub struct ChatRequest { pub system: String, pub messages: Vec<Message> }
#[derive(Clone, Debug, PartialEq)] pub enum Stop { End, MaxTokens, ToolUse, Refusal, Other(String) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LlmError { NoCarrier, Auth, Overloaded, Malformed, SignalLost, Transmission, Restricted, NoAccessCode, NotConfigured }
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Text(String),
    ToolCall { id: String, name: String, input: serde_json::Value },
    /// `raw`: Anthropic content blocks as they'd appear non-streamed; None for openai.
    Done { stop: Stop, model: Option<String>, raw: Option<Vec<serde_json::Value>> },
    Error(LlmError),
}
pub trait StreamParser: Send {
    fn feed(&mut self, rec: &sse::SseRecord) -> Vec<StreamEvent>;
    /// Body ended. Emits Error(Transmission) if no Done was produced.
    fn finish(&mut self) -> Vec<StreamEvent>;
}
impl Stop { pub fn log_str(&self) -> &str }  // "end_turn" | "max_tokens" | "tool_use" | "refusal" | other
impl LlmError { pub fn reason(&self) -> &'static str }
// sse.rs
#[derive(Clone, Debug, PartialEq, Default)] pub struct SseRecord { pub event: Option<String>, pub data: String }
#[derive(Default)] pub struct SseDecoder { /* line buffer, current record */ }
impl SseDecoder { pub fn push(&mut self, bytes: &[u8]) -> Vec<SseRecord> }
```
`LlmError::reason()` returns, in order:
- `NO CARRIER`
- `AUTHORIZATION REJECTED`
- `SYSTEM OVERLOADED`
- `MALFORMED INQUIRY`
- `SIGNAL LOST`
- `TRANSMISSION ERROR`
- `DIRECTIVE RESTRICTED`
- `NO ACCESS CODE`
- `NOT CONFIGURED`

The SSE rules follow the HTML spec:
- Lines end in `\n` or `\r\n`.
- A blank line dispatches the record.
- Lines starting with `:` are comments.
- `data:` lines are joined with `\n`, with one leading space after the colon stripped.
- A record whose data is empty is not dispatched.
- Bytes may split anywhere, including inside a UTF-8 character: buffer them until a full line arrives.

- [ ] **Step 1: Add the dependencies** (Tech Stack line). Keep tokio's existing features and add `net` and `io-util`.
- [ ] **Step 2: Verify the dependency tree**:
  - Run `cargo tree -i aws-lc-sys --target x86_64-unknown-linux-musl`. Expected: an error saying the package ID specification matched no packages.
  - Run `cargo tree -i ring`. Expected: reached via `rustls`.
  - If aws-lc appears, find the feature that enables it (`cargo tree -e features -i aws-lc-rs`) and switch it off. Do not proceed until it is gone.
- [ ] **Step 3: Write failing tests in `sse.rs`**:
```rust
#[test] fn splits_records_on_blank_line() { let mut d = SseDecoder::default(); let r = d.push(b"event: a\ndata: {\"x\":1}\n\ndata: two\n\n"); assert_eq!(r, vec![SseRecord{event:Some("a".into()),data:"{\"x\":1}".into()}, SseRecord{event:None,data:"two".into()}]); }
#[test] fn handles_crlf_comments_and_multiline() { let mut d = SseDecoder::default(); assert_eq!(d.push(b": ping\r\ndata: a\r\ndata: b\r\n\r\n"), vec![SseRecord{event:None,data:"a\nb".into()}]); }
#[test] fn arbitrary_splits_including_utf8() { let all = "data: caf\u{e9} \u{2014}\n\n".as_bytes(); for cut in 0..all.len() { let mut d = SseDecoder::default(); let mut r = d.push(&all[..cut]); r.extend(d.push(&all[cut..])); assert_eq!(r, vec![SseRecord{event:None,data:"caf\u{e9} \u{2014}".into()}], "cut {cut}"); } }
#[test] fn done_sentinel_is_plain_data() { let mut d = SseDecoder::default(); assert_eq!(d.push(b"data: [DONE]\n\n")[0].data, "[DONE]"); }
```
plus, in `mod.rs`, `#[test] fn reasons_are_uppercase_ascii()`, which loops over every `LlmError` variant.
- [ ] **Step 4: Run** `cargo test llm::` → FAIL (types missing).
- [ ] **Step 5: Implement** the types and `SseDecoder`, plus `pub fn install_crypto_provider()` in `llm/mod.rs`. It calls `rustls::crypto::ring::default_provider().install_default()` and ignores the `Err` that means it is already installed, so calling it twice is fine. Test: `#[test] fn crypto_provider_install_is_idempotent() { install_crypto_provider(); install_crypto_provider(); }`.
- [ ] **Step 6: Run** `cargo test` → all pass.
- [ ] **Step 7: Commit** `feat: llm core types, SSE decoder, ring crypto provider`

---

### Task 2: `[muthur]` config

**Files:** modify `src/config.rs`.

**Interfaces:**
- Consumes `llm::LlmError`.
- Produces:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)] #[serde(rename_all = "lowercase")]
pub enum Provider { #[default] Anthropic, Openai }
#[derive(Debug, Clone, PartialEq, Deserialize)] #[serde(default)]
pub struct MuthurSection {
    pub provider: Provider, pub base_url: Option<String>, pub model: Option<String>, pub api_key: Option<String>,
    pub max_tokens: u32, pub effort: String, pub fallbacks: bool, pub context_tokens: usize,
    pub reveal_cps: u32, pub uppercase: bool, pub system_prompt: Option<String>,
}
impl MuthurSection {
    pub fn base_url(&self) -> Option<&str>;   // explicit, else "https://api.anthropic.com" for Anthropic, None for Openai; trailing '/' trimmed
    pub fn model(&self) -> Option<&str>;      // explicit, else "claude-opus-5-5" for Anthropic, None for Openai
    pub fn readiness(&self) -> Result<(), LlmError>; // Anthropic w/o non-empty api_key → NoAccessCode; Openai w/o base_url or model → NotConfigured
}
```
Add `pub muthur: MuthurSection` to `Config`. Defaults are given in Global Constraints, with `fallbacks = true`, `uppercase = true`, and `system_prompt = None` (an empty string also counts as `None` when it is used).

- [ ] **Step 1: Failing tests**:
```rust
#[test] fn muthur_defaults() { let m = MuthurSection::default(); assert_eq!(m.provider, Provider::Anthropic); assert_eq!(m.base_url(), Some("https://api.anthropic.com")); assert_eq!(m.model(), Some("claude-opus-5-5")); assert_eq!((m.max_tokens, m.effort.as_str(), m.fallbacks, m.context_tokens, m.reveal_cps, m.uppercase), (4096, "low", true, 200000, 50, true)); assert_eq!(m.readiness(), Err(LlmError::NoAccessCode)); }
#[test] fn muthur_openai_section_parses() { let (_d, p) = write_cfg("[muthur]\nprovider = \"openai\"\nbase_url = \"http://10.0.0.5:8000/\"\nmodel = \"qwen\"\n", 0o600); let c = load(&p).unwrap(); assert_eq!(c.muthur.base_url(), Some("http://10.0.0.5:8000")); assert_eq!(c.muthur.readiness(), Ok(())); }
#[test] fn openai_without_url_is_not_configured() { let m = MuthurSection { provider: Provider::Openai, ..Default::default() }; assert_eq!(m.readiness(), Err(LlmError::NotConfigured)); }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: [muthur] config section`

---

### Task 3: Anthropic protocol

**Files:**
- Create `src/llm/anthropic.rs`.
- Create the fixtures `tests/fixtures/llm/anthropic_text.sse`, `anthropic_thinking.sse`, `anthropic_tool.sse`, `anthropic_fallback.sse`, `anthropic_error.sse` and `anthropic_refusal.sse`.

**Interfaces:**
- Consumes the Task 1 types and the Task 2 `MuthurSection`.
- Produces:
```rust
pub fn request_body(cfg: &MuthurSection, req: &ChatRequest) -> serde_json::Value;
pub fn headers(cfg: &MuthurSection) -> Vec<(&'static str, String)>;   // x-api-key, anthropic-version, content-type, [anthropic-beta]
pub const PATH: &str = "/v1/messages";
pub struct Parser { /* blocks: Vec<Value>, partial json per index, model, stop */ }
impl Parser { pub fn new() -> Self } impl StreamParser for Parser
/// Blocks to send when replaying a stored assistant turn.
pub fn replay_blocks(raw: &[serde_json::Value], strip_thinking: bool) -> Vec<serde_json::Value>;
```

**`request_body`:**
- The fields are `model`, `max_tokens`, `stream: true`, `system` (a string), `messages` and `output_config: {"effort": cfg.effort}`. When `cfg.fallbacks` is on, add `"fallbacks": "default"`.
- How each message is sent:
  - `Text(s)` → `{"role", "content": s}`;
  - `Blocks(v)` → `{"role", "content": v}`.

**`Parser`:**
- It rebuilds each content block by index:
  - `content_block_start` stores `content_block`;
  - `text_delta` appends to `text` and emits `Text`;
  - `thinking_delta` appends to `thinking`;
  - `signature_delta` sets `signature`;
  - `input_json_delta` accumulates a string, which is parsed into `input` at `content_block_stop`; it emits `ToolCall` there.
- `message_start` records `message.model`. `message_delta` records `delta.stop_reason`. `message_stop` emits `Done { stop, model, raw: Some(blocks) }`.
- `error` emits `Error(Overloaded)` when `error.type == "overloaded_error"`, and otherwise `Error(Transmission)`.
- `ping` and unknown event types are ignored. Unknown block types are kept in `raw` without changes.
- The stop mapping follows the spec table.

**`replay_blocks`:**
1. If there is a `fallback` block, drop the `thinking`, `redacted_thinking` and `tool_use` blocks that come before the last one.
2. If `strip_thinking` is set, drop all `thinking` and `redacted_thinking` blocks.
3. Always drop `tool_use` blocks. M4 never sends tool results, so an unpaired `tool_use` would be rejected.
4. Return what is left. The caller falls back to plain text if the result is empty.

**Fixtures:** each one is a realistic `event: …\ndata: {…}\n\n` stream following the documented Messages streaming format, including `ping`.
- `anthropic_thinking.sse`: one thinking block (empty `thinking`, with a `signature_delta`), then one text block in two deltas.
- `anthropic_fallback.sse`: a thinking block, then a `fallback` block, then text, ending with `stop_reason: "end_turn"`.

- [ ] **Step 1: Failing tests** (`fn run(fixture) -> Vec<StreamEvent>` reads the file, decodes it with `SseDecoder`, and feeds it through `Parser`):
```rust
#[test] fn text_stream() { let ev = run("anthropic_text.sse"); assert_eq!(texts(&ev), "ALL SYSTEMS NOMINAL."); assert!(matches!(ev.last(), Some(StreamEvent::Done{stop: Stop::End, model: Some(_), ..}))); }
#[test] fn thinking_is_kept_raw_not_emitted() { let ev = run("anthropic_thinking.sse"); let StreamEvent::Done{raw: Some(raw), ..} = ev.last().unwrap() else { panic!() }; assert_eq!(raw[0]["type"], "thinking"); assert!(raw[0]["signature"].as_str().unwrap().len() > 0); assert_eq!(raw[1], json!({"type":"text","text": texts(&ev)})); assert!(!ev.iter().any(|e| matches!(e, StreamEvent::Text(t) if t.is_empty()))); }
#[test] fn tool_input_is_reassembled() { let ev = run("anthropic_tool.sse"); assert!(ev.iter().any(|e| matches!(e, StreamEvent::ToolCall{name, input, ..} if name == "metrics" && input == &json!({"host":"local"})))); assert!(matches!(ev.last(), Some(StreamEvent::Done{stop: Stop::ToolUse, ..}))); }
#[test] fn error_event() { assert_eq!(run("anthropic_error.sse").last(), Some(&StreamEvent::Error(LlmError::Overloaded))); }
#[test] fn refusal_stop() { assert!(matches!(run("anthropic_refusal.sse").last(), Some(StreamEvent::Done{stop: Stop::Refusal, ..}))); }
#[test] fn truncated_stream_is_transmission_error() { let mut p = Parser::new(); /* feed message_start + one text delta only */ assert_eq!(p.finish(), vec![StreamEvent::Error(LlmError::Transmission)]); }
#[test] fn replay_fallback_rule() { let raw = vec![json!({"type":"thinking","thinking":"","signature":"s"}), json!({"type":"fallback"}), json!({"type":"thinking","thinking":"","signature":"t"}), json!({"type":"text","text":"X"})]; assert_eq!(replay_blocks(&raw, false), raw[1..].to_vec()); assert_eq!(replay_blocks(&raw, true), vec![raw[1].clone(), raw[3].clone()]); }
#[test] fn replay_drops_tool_use() { let raw = vec![json!({"type":"text","text":"A"}), json!({"type":"tool_use","id":"1","name":"m","input":{}})]; assert_eq!(replay_blocks(&raw, false), raw[..1].to_vec()); }
#[test] fn body_shape() { let cfg = MuthurSection{ api_key: Some("k".into()), ..Default::default() }; let b = request_body(&cfg, &req_with(vec![user("HI")])); assert_eq!(b["model"], "claude-opus-5-5"); assert_eq!(b["stream"], true); assert_eq!(b["output_config"]["effort"], "low"); assert_eq!(b["fallbacks"], "default"); assert!(b.get("thinking").is_none()); assert_eq!(b["messages"][0], json!({"role":"user","content":"HI"})); }
#[test] fn no_fallbacks_no_beta() { let cfg = MuthurSection{ fallbacks: false, api_key: Some("k".into()), ..Default::default() }; assert!(request_body(&cfg, &req_with(vec![])).get("fallbacks").is_none()); assert!(!headers(&cfg).iter().any(|(k,_)| *k == "anthropic-beta")); }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: anthropic messages protocol`

---

### Task 4: OpenAI / vLLM protocol

**Files:**
- Create `src/llm/openai.rs`.
- Create the fixtures `tests/fixtures/llm/openai_text.sse`, `openai_tool.sse` and `openai_length.sse`, in the vLLM chat-completions chunk format with a final `data: [DONE]`.

**Interfaces — Produces:** `request_body`, `headers`, `PATH = "/v1/chat/completions"`, and `Parser`, with the same shapes as Task 3. There is no `replay_blocks`.

**`request_body`:**
- The fields are `model`, `max_tokens`, `stream: true` and `messages`.
- The system prompt goes first, as a `system` message.
- `Blocks(v)` messages are sent as the text of their `text` blocks joined together. In practice it is unreachable, because chatlog sends `Text` for openai.

**`headers`:** `content-type`, plus `authorization: Bearer <key>` only when `api_key` is non-empty.

**`Parser`:**
- `choices[0].delta.content` (non-empty) → `Text`. `reasoning_content` is ignored.
- `delta.tool_calls[i]` is accumulated by `index`: `id`, `function.name`, and the `function.arguments` string.
- `finish_reason` is recorded when it is non-null.
- On `[DONE]`:
  - emit a `ToolCall` for each accumulated call, with arguments parsed and `{}` when parsing fails;
  - then emit `Done { stop, model: first chunk's "model", raw: None }`.
- A chunk containing `"error"` → `Error(Transmission)`.

- [ ] **Step 1: Failing tests**:
```rust
#[test] fn text_stream() { let ev = run("openai_text.sse"); assert_eq!(texts(&ev), "ALL SYSTEMS NOMINAL."); assert!(matches!(ev.last(), Some(StreamEvent::Done{stop: Stop::End, raw: None, ..}))); }
#[test] fn tool_calls_accumulate_by_index() { let ev = run("openai_tool.sse"); assert!(ev.iter().any(|e| matches!(e, StreamEvent::ToolCall{name, input, ..} if name == "metrics" && input == &json!({"host":"local"})))); assert!(matches!(ev.last(), Some(StreamEvent::Done{stop: Stop::ToolUse, ..}))); }
#[test] fn length_is_max_tokens() { assert!(matches!(run("openai_length.sse").last(), Some(StreamEvent::Done{stop: Stop::MaxTokens, ..}))); }
#[test] fn system_goes_first() { let b = request_body(&cfg(), &ChatRequest{ system: "SYS".into(), messages: vec![user("HI")] }); assert_eq!(b["messages"][0], json!({"role":"system","content":"SYS"})); assert_eq!(b["messages"][1], json!({"role":"user","content":"HI"})); }
#[test] fn bearer_only_with_key() { assert!(!headers(&cfg()).iter().any(|(k,_)| *k == "authorization")); assert!(headers(&MuthurSection{ api_key: Some("k".into()), ..cfg() }).contains(&("authorization", "Bearer k".into()))); }
#[test] fn missing_done_is_transmission_error() { /* feed one content chunk, then finish() */ }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: openai-compatible chat protocol`

---

### Task 5: Chat log

**Files:**
- Create `src/chatlog.rs`.
- Add `pub mod chatlog;` to `lib.rs`.

**Interfaces:**
- Consumes `llm::{Message, MessageContent, Role}`, `llm::anthropic::replay_blocks` and `config::Provider`.
- Produces:
```rust
pub fn fnv64(s: &str) -> String;   // FNV-1a 64, lowercase hex (stable across Rust versions; never use DefaultHasher)
#[derive(Clone, Debug, PartialEq)]
pub struct Assistant { pub provider: Provider, pub model: String, pub text: String, pub stop: String, pub raw: Option<Vec<serde_json::Value>> }
/// One past exchange, for redisplay.
#[derive(Clone, Debug, PartialEq)]
pub struct Exchange { pub user: String, pub reply: Option<(String, String)> }  // (text, stop)
pub struct ChatLog { /* path, records: Vec<serde_json::Value> or typed, next_seq */ }
impl ChatLog {
    pub fn open(path: PathBuf) -> anyhow::Result<Self>;       // creates parent dir; missing file = empty
    pub fn append_user(&mut self, text: &str, ts: &str) -> std::io::Result<u64>;
    pub fn append_assistant(&mut self, a: Assistant, ts: &str) -> std::io::Result<()>;
    /// Messages to send, ending with the latest user record. Writes epoch records as needed.
    pub fn window(&mut self, provider: Provider, system: &str, budget_tokens: usize) -> std::io::Result<Vec<Message>>;
    pub fn history(&self) -> Vec<Exchange>;                    // all exchanges, oldest first (ignores epochs)
    pub fn purge(&mut self) -> std::io::Result<()>;            // delete file, clear memory, seq restarts at 0
}
```
The record JSON follows the spec's Log table exactly (`v`, `seq`, `kind`, and so on). `Provider` is serialized lowercase. `ts` is passed in by the caller as RFC 3339 local time.

**Writing.** The file is opened with `OpenOptions::append` and created with `.mode(0o600)`. Each record is written as one `write_all` of `json + "\n"`. If the existing file does not end in `\n` (a torn tail), write a `\n` first.

**Loading.** Read line by line. A line that fails to parse, or has no `seq`, is skipped with `tracing::warn!`. `next_seq` is the highest valid `seq` + 1.

**`window` algorithm:**
1. **Epoch check.** Let `h = fnv64(system)`. If there is no epoch yet, or the last epoch's `system_hash != h`, append `epoch { from: last_from_or_0, reason: "system_changed", system_hash: h }`.
2. **Collect.** `E` is the latest epoch. Collect the user/assistant pairs with user `seq ≥ E.from`. A pair is a user record immediately followed by an assistant record. Skip:
   - pairs whose assistant has `stop == "error"` or empty text;
   - user records not followed by an assistant, except the very last record.
   The final message is the last user record; return an empty `Vec` if the last record is not a user record.
3. **Convert pairs to messages:**
   - User → `Text(text)`.
   - Assistant with `Provider::Anthropic`, a `raw` value, and `stop != "interrupted"` → `Blocks(replay_blocks(raw, strip = assistant.seq < E.seq))`, or `Text(text)` if that is empty.
   - Everything else → `Text(text)`.
   - With the `Openai` provider, always `Text`.
4. **Estimate** = `(system.len() + Σ message chars) / 4`. Blocks count as their serialized JSON length.
5. **Trim** if the estimate is over `budget_tokens`:
   - Pick the smallest new `from` (a user `seq` at a pair boundary) such that the window built from it is ≤ `budget_tokens / 2`. The newest user record is always kept.
   - Append `epoch { from, reason: "trim", system_hash: h }`.
   - Rebuild once. This is not recursive: after a trim, return the result even if it is still over budget.

- [ ] **Step 1: Failing tests** (helper `log()` opens a log in a `tempdir`; `asst(text, raw)` builds an Anthropic `Assistant` with stop `end_turn`):
```rust
#[test] fn round_trip_and_mode() { let (d, mut l) = log(); l.append_user("[SHIP TIME X] HI", "t").unwrap(); l.append_assistant(asst("HELLO", None), "t").unwrap(); let l2 = ChatLog::open(d.path().join("chat.jsonl")).unwrap(); assert_eq!(l2.history(), vec![Exchange{ user: "[SHIP TIME X] HI".into(), reply: Some(("HELLO".into(), "end_turn".into())) }]); assert_eq!(std::fs::metadata(d.path().join("chat.jsonl")).unwrap().permissions().mode() & 0o777, 0o600); }
#[test] fn window_replays_raw_and_ends_with_inquiry() { let (_d, mut l) = log(); l.append_user("A", "t").unwrap(); let raw = vec![json!({"type":"thinking","thinking":"","signature":"s"}), json!({"type":"text","text":"B"})]; l.append_assistant(asst("B", Some(raw.clone())), "t").unwrap(); l.append_user("C", "t").unwrap(); let w = l.window(Provider::Anthropic, "SYS", 200000).unwrap(); assert_eq!(w.len(), 3); assert_eq!(w[1].content, MessageContent::Blocks(raw)); assert_eq!(w[2].content, MessageContent::Text("C".into())); }
#[test] fn failed_pairs_and_orphans_excluded() { /* A + error reply, B (no reply), C(current) → window == [C] */ }
#[test] fn openai_gets_text_only() { /* same as above with Provider::Openai → w[1].content == Text("B") */ }
#[test] fn system_change_strips_thinking_once() { /* window with "SYS1", append reply with thinking raw, user; window with "SYS2" → that reply's Blocks has no "thinking"; a reply written AFTER the new epoch keeps its thinking on the next window */ }
#[test] fn trim_cuts_to_half_budget_with_hysteresis() { /* 20 pairs of 400-char text, budget 1000 tokens → after window(): estimate ≤ 500, an epoch{reason:"trim"} exists; append one more small pair → window() writes no new epoch */ }
#[test] fn truncated_last_line_is_skipped_and_append_starts_fresh_line() { /* write a valid record line + "{\"v\":1,\"seq\":1,\"ki" (no newline); open → history has 1 user; append_user returns seq 1; reopen → both records parse */ }
#[test] fn purge_deletes() { /* append, purge → file gone, history empty, next append_user returns 0 */ }
#[test] fn fnv_is_stable() { assert_eq!(fnv64(""), "cbf29ce484222325"); assert_eq!(fnv64("a"), "af63dc4c8601ec8c"); }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: append-only chat log with epochs`

---

### Task 6: HTTP client

**Files:** create `src/llm/client.rs`.

**Interfaces:**
- Consumes the Tasks 1–4 protocol modules and `MuthurSection`.
- Produces:
```rust
pub fn map_status(code: u16) -> LlmError;  // 401|403 → Auth; 429|529|500..=599 → Overloaded; other → Malformed
pub struct LlmClient { /* reqwest::Client, Arc<MuthurSection> */ }
impl LlmClient {
    pub fn new(cfg: MuthurSection) -> anyhow::Result<Self>;   // calls install_crypto_provider(); connect_timeout 15s; no total timeout
    /// Stream one request; every event goes through `emit`. Must be awaited inside a tokio runtime.
    pub async fn stream(&self, req: ChatRequest, emit: impl FnMut(StreamEvent)) ;
    /// tokio::spawn(stream(..)) forwarding into the app channel as Event::Llm { req_id, ev }; returns the handle (abort = handle.abort()).
    pub fn spawn(self: &Arc<Self>, req_id: u64, req: ChatRequest, tx: UnboundedSender<crate::run::Event>) -> tokio::task::JoinHandle<()>;
}
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
```
`stream`:
1. Check `cfg.readiness()`; on failure emit `Error(that)` and return.
2. Pick the protocol by `provider`, POST to `base_url + PATH` with that protocol's headers and the serialized body.
3. A connect, DNS or TLS error → `Error(NoCarrier)`. A non-2xx status → `Error(map_status)`, after logging status and error type with `tracing::warn!` (the body's `error.type` and `error.message` only).
4. Loop `tokio::time::timeout(IDLE_TIMEOUT, resp.chunk())`:
   - timeout → `Error(SignalLost)`;
   - read error → `Error(Transmission)`;
   - `None` → `parser.finish()`.
   Every event goes through `emit`. Stop after the first `Done` or `Error`.

`Event::Llm` is added to `run::Event` in this task: `Llm { req_id: u64, ev: StreamEvent }`. The event loop arm for it is a no-op until Task 9.

The idle timeout is a constructor parameter for tests: `LlmClient::with_idle(cfg, Duration)`.

- [ ] **Step 1: Failing tests** (`#[tokio::test]`; helper `serve(response_bytes, delay_between_chunks) -> base_url`, which binds `127.0.0.1:0`, accepts one connection, reads the request headers, and writes a `HTTP/1.1` response with `Transfer-Encoding: chunked` from the fixture):
```rust
#[test] fn status_mapping() { assert_eq!(map_status(401), LlmError::Auth); assert_eq!(map_status(403), LlmError::Auth); assert_eq!(map_status(429), LlmError::Overloaded); assert_eq!(map_status(529), LlmError::Overloaded); assert_eq!(map_status(502), LlmError::Overloaded); assert_eq!(map_status(400), LlmError::Malformed); }
#[tokio::test] async fn streams_openai_fixture() { /* serve openai_text.sse; cfg openai with base_url=served; collect events; texts == "ALL SYSTEMS NOMINAL.", last is Done */ }
#[tokio::test] async fn streams_anthropic_fixture_and_sends_headers() { /* server captures request; assert path "/v1/messages", header x-api-key: k, anthropic-version: 2023-06-01; body JSON has stream:true */ }
#[tokio::test] async fn http_401_is_auth() { /* serve "HTTP/1.1 401 ..." with {"type":"error","error":{"type":"authentication_error","message":"x"}} → [Error(Auth)] */ }
#[tokio::test] async fn refused_connection_is_no_carrier() { /* base_url = http://127.0.0.1:<port of a dropped listener> → [Error(NoCarrier)] */ }
#[tokio::test] async fn idle_timeout_is_signal_lost() { /* with_idle(200ms); server sends headers + one chunk then sleeps 2s → last event Error(SignalLost), elapsed < 1s */ }
#[tokio::test] async fn not_ready_emits_without_network() { /* default cfg (no key) → [Error(NoAccessCode)] */ }
#[tokio::test] async fn spawn_forwards_and_abort_stops() { /* spawn against a slow server; receive first Event::Llm with req_id 7; handle.abort(); then no further events within 300ms */ }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: streaming LLM client (reqwest + rustls/ring)`

---

### Task 7: Output text pipeline and reveal

**Files:**
- Create `src/muthur/text.rs` and `src/muthur/reveal.rs`.
- Create `src/muthur/mod.rs` with `pub mod text; pub mod reveal;` for now.
- Add `pub mod muthur;` to `lib.rs`.

**Interfaces — Produces:**
```rust
// text.rs
pub struct Cleaner { /* line-start buffer, link buffer, in_fence */ }
impl Cleaner { pub fn new() -> Self; pub fn push(&mut self, chunk: &str) -> String; pub fn finish(&mut self) -> String; }
pub fn ascii_safe(s: &str) -> String;
/// Cleaner(all) → uppercase (if `upper`) → ascii_safe. Used for history redisplay.
pub fn display_text(raw: &str, upper: bool) -> String;
// reveal.rs
pub struct Reveal { /* text: String, shown: usize (chars), cps, last: Option<Instant>, credit: f64, skipping: bool */ }
impl Reveal {
    pub fn new(cps: u32) -> Self;
    pub fn push(&mut self, s: &str, now: Instant);   // if fully shown before push, restart the clock at `now`
    pub fn advance(&mut self, now: Instant) -> bool; // releases floor(elapsed*cps + credit) chars; true if `shown` changed
    pub fn skip(&mut self);                          // show all; later pushes show immediately until reset()
    pub fn reset(&mut self);                         // new reply: clear text, skipping=false
    pub fn visible(&self) -> &str;
    pub fn caught_up(&self) -> bool;
}
```

**`Cleaner`, processing line by line as it streams:**
- **Line start.** At the start of a line, buffer characters until the prefix can be decided: up to 4 characters, a newline, or a non-marker character.
  - `#`…`# ` prefixes are dropped.
  - `* `, `+ ` and `- ` become `- `.
  - A line that starts with ```` ``` ```` toggles `in_fence`, and the whole line (up to its `\n`) is dropped.
- **Inside a line:**
  - `*` and `` ` `` are dropped. So is `__`, but a single `_` is kept.
  - `[` starts link buffering, up to 200 characters or a newline. `[t](u)` → `t (u)`. If that pattern does not complete, the buffer is flushed as it is.
- **`finish`** flushes every buffer.

**`ascii_safe`:**
- Printable ASCII and `\n` are kept.
- Other characters whose `dialect::map_glyph` is `Graphics(_)` are kept.
- Otherwise, use `deunicode::deunicode_char(c)` filtered to printable ASCII, so `—` becomes `--` and `…` becomes `...`.
- If that is empty, use `?`.

- [ ] **Step 1: Failing tests**:
```rust
#[test] fn strips_markdown() { assert_eq!(display_text("## Status\n**All** `systems` nominal.\n* one\n```\ncode\n```\nsee [docs](http://x)\n", false), "Status\nAll systems nominal.\n- one\nsee docs (http://x)\n"); }
#[test] fn markers_split_across_chunks() { let src = "## Head\n**bo"; let rest = "ld** [l](u) x_y __z__\n"; for cut in 0..src.len() { let mut c = Cleaner::new(); let mut out = c.push(&src[..cut]); out += &c.push(&src[cut..]); out += &c.push(rest); out += &c.finish(); assert_eq!(out, "Head\nbold l (u) x_y z\n", "cut {cut}"); } }
#[test] fn unterminated_link_is_flushed() { assert_eq!(display_text("a [b c", false), "a [b c"); }
#[test] fn transliterates_punctuation_and_emoji() { assert_eq!(ascii_safe("A \u{2014} \u{201c}B\u{201d} \u{2026}"), "A -- \"B\" ..."); let s = ascii_safe("\u{1f680} \u{65e5}"); assert!(s.chars().all(|c| c == '\n' || (' '..='~').contains(&c))); }
#[test] fn uppercases() { assert_eq!(display_text("ok", true), "OK"); }
#[test] fn reveal_paces_at_cps() { let t0 = Instant::now(); let mut r = Reveal::new(50); r.push("0123456789", t0); r.advance(t0 + Duration::from_millis(100)); assert_eq!(r.visible(), "01234"); r.advance(t0 + Duration::from_millis(130)); assert_eq!(r.visible(), "012345"); }
#[test] fn reveal_does_not_burst_after_stall() { let t0 = Instant::now(); let mut r = Reveal::new(50); r.push("AB", t0); r.advance(t0 + Duration::from_secs(1)); r.push("CDEFG", t0 + Duration::from_secs(5)); r.advance(t0 + Duration::from_millis(5020)); assert_eq!(r.visible(), "ABC"); }
#[test] fn skip_shows_all_and_later_pushes() { let t0 = Instant::now(); let mut r = Reveal::new(50); r.push("ABC", t0); r.skip(); assert_eq!(r.visible(), "ABC"); r.push("D", t0); assert_eq!(r.visible(), "ABCD"); r.reset(); r.push("E", t0); assert_eq!(r.visible(), ""); }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: chat text pipeline and teletype reveal`

---

### Task 8: MuthurTab (view and state machine)

**Files:**
- Create `src/muthur/view.rs`.
- Fill in `src/muthur/mod.rs`.
- Create `src/muthur/tests.rs`.
- Modify `src/app/tabs.rs`: add `fn on_llm(&mut self, _req_id: u64, _ev: crate::llm::StreamEvent) {}` and `fn tick_interval(&self) -> Option<Duration> { None }` to `Tab`.

**Interfaces:**
- Consumes `ChatLog`, `Cleaner`, `ascii_safe`, `display_text`, `Reveal`, `StreamEvent`, `LlmError`, `MuthurSection` and `Key`.
- Produces:
```rust
pub const PERSONA: &str = /* below, with "{SHIP}" placeholder */;
pub fn system_prompt(cfg: &MuthurSection, ship: &str) -> String;  // override (non-empty) or PERSONA, "{SHIP}" replaced
pub type Abort = Box<dyn FnOnce()>;
pub struct MuthurDeps {
    pub spawn: Box<dyn FnMut(u64, ChatRequest) -> Abort>,
    pub now: Box<dyn Fn() -> Instant>,
    pub ship_time: Box<dyn Fn() -> String>,     // e.g. "18 NOV 2137 14:02"
    pub timestamp: Box<dyn Fn() -> String>,     // RFC 3339 for the log
}
pub struct MuthurTab { /* cfg, system, log, deps, transcript: Vec<Entry>, input: String, cursor: usize, state, reveal, cleaner, status_flash: Option<String>, scroll: usize, req_id: u64, abort: Option<Abort>, partial_raw: String, activity: bool */ }
impl MuthurTab { pub fn new(cfg: MuthurSection, ship: &str, log: ChatLog, deps: MuthurDeps) -> Self }
impl Tab for MuthurTab
// view.rs
pub fn wrap(text: &str, width: u16) -> Vec<String>;   // word wrap; words longer than width hard-broken; '\n' forces a break; empty line preserved
```

**`PERSONA`** (exact copy):
```
You are MU/TH/UR 6000, the mainframe computer of the {SHIP}. "The ship" means the server you run on.
Answer in short, declarative statements, like a 1970s ship computer printing to a terminal.
Use plain text only: no markdown, no headings, no bold, no code blocks. If a list is needed, put each item on its own short line.
No greetings, pleasantries, apologies or filler.
If you cannot or will not do something, reply "UNABLE TO COMPLY." followed by a short reason.
If you do not know, reply "DATA INSUFFICIENT." Never invent readings.
You do not yet have access to the ship's sensors or systems. Do not claim to have checked anything on the ship.
```

**States:**
- `Ready`;
- `Waiting` (request sent, no text yet);
- `Streaming`;
- `Confirm` (`/PURGE` asked).

**Transcript entries:** `User(String)`, `Reply(String)`, and `Note(String)`. Replies hold the cleaned, uppercased, ASCII text. Notes are `[INTERRUPTED]` and `[TRANSMISSION TRUNCATED]`.
- `new()` builds the transcript from `log.history()`:
  - user text is shown without its `[SHIP TIME …] ` prefix;
  - replies go through `display_text`;
  - stop `interrupted` adds the `[INTERRUPTED]` note, and `max_tokens` adds `[TRANSMISSION TRUNCATED]`;
  - an `error` reply shows only the user line.

**Keys:**
- **`Confirm` state.** `Y` or `y` → `log.purge()`, clear the transcript, back to `Ready`. Any other key → `Ready`.
- **Editing keys, in any state other than `Confirm`:**
  - a printable `Char` is inserted at the cursor, uppercased when `cfg.uppercase`;
  - `Backspace` deletes before the cursor;
  - `DeleteChar` and `Pf(2)` delete at the cursor;
  - `Left` and `Right` move the cursor;
  - `Ctrl('u')` clears the line.
  - `Char` and the editing keys never skip the reveal.
- **`Ctrl('c')`** while `Waiting` or `Streaming` runs `abort()`, then finalizes the reply as interrupted. When idle it does nothing.
- **`PageUp` / `PageDown`** scroll by `inner_height - 4` lines. `scroll` is the number of lines up from the bottom, clamped.
- **`Enter`:**
  - with blank input (after trim): ignored;
  - while busy: `status_flash = "PROCESSING — STAND BY"`;
  - `/PURGE` (case-insensitive, trimmed): go to `Confirm`;
  - `/HELP`: add a `Note` listing `/PURGE  CLEAR MEMORY BANKS`, `/HELP   THIS LIST`, `^C     ABORT TRANSMISSION`, `PGUP/PGDN SCROLL`;
  - another `/word`: `status_flash = "UNKNOWN COMMAND"`;
  - otherwise, submit (below).
  - In every case Enter clears `scroll`.
- **Any other key** (`Esc`, `Tab`, `F` keys that reach the tab, etc.) calls `reveal.skip()` when a reply is revealing.
- Any key clears `status_flash`, except the keys that set it.

**Submit:**
1. If `cfg.readiness()` returns `Err(e)`: `status_flash = "UNABLE TO COMPLY — " + reason`. Nothing is logged.
2. Log `"[SHIP TIME {ship_time}] {input}"` with `append_user`.
3. Push `User(input)` to the transcript and clear the input.
4. `req_id += 1`.
5. `msgs = log.window(cfg.provider, &system, cfg.context_tokens)`.
6. `abort = Some((deps.spawn)(req_id, ChatRequest{system, messages: msgs}))`.
7. Set state `Waiting`, `reveal.reset()`, a new `Cleaner`, and clear `partial_raw`.

**`on_llm(req_id, ev)`:** ignored if `req_id != self.req_id` or the state is `Ready`.
- `Text(t)`:
  - `partial_raw += t`;
  - `s = ascii_safe(upper(cleaner.push(t)))`;
  - `reveal.push(s, now)`;
  - state becomes `Streaming`.
- `ToolCall`: ignored (M5).
- `Done { stop, model, raw }`:
  - flush `cleaner.finish()` into the reveal;
  - append an `Assistant` record: provider, model or the configured model, `partial_raw`, `stop.log_str()`, raw;
  - `Refusal` → flash `UNABLE TO COMPLY — DIRECTIVE RESTRICTED`;
  - `MaxTokens` → add the `[TRANSMISSION TRUNCATED]` note;
  - then `Ready` and set `activity = true`.
- `Error(e)`:
  - if `partial_raw` is non-empty, log the assistant with stop `interrupted` and `raw: None`, and add the `[INTERRUPTED]` note;
  - otherwise log it with stop `error` and empty text;
  - flash `UNABLE TO COMPLY — {e.reason()}`; set `Ready` and `activity`.
- Abort with Ctrl-C: like an error with partial text, but no flash.

The reply being revealed is shown from `reveal.visible()` until `caught_up()`; after that it is moved into a `Reply` entry.

**`tick_interval()`:** `Some(100ms)` while `!reveal.caught_up()` or the state is `Waiting`, otherwise `None`.

`render` calls `reveal.advance(now)` first.

**Render** (into `area`):
- A bordered block titled ` MU/TH/UR 6000 `.
- The inner area: the transcript region on top; then a `├─…─┤` separator drawn over the border row, using `─` across with `├`/`┤` at the edges; then a status row; then the input row.
- **Transcript region.** Lines are: `INTERFACE 2037 READY.`, a blank line, then for each entry its wrapped lines (`User` prefixed `> `), with a blank line between entries. The reply being revealed comes last, with `█` appended. The region shows the window `[len - height - scroll, len - scroll)`.
- **Status row:**
  - if a flash is set, the flash in reverse video;
  - otherwise by state: `READY FOR INQUIRY` / `PROCESSING` + `.`×(1 + secs % 3) / `RECEIVING`;
  - `CONFIRM Y/N` in reverse while purging;
  - `[SCROLL -N]` right-aligned when `scroll > 0`.
- **Input row.** `> ` followed by the input, scrolled horizontally so that the cursor stays visible. `cursor()` returns that cell.

- [ ] **Step 1: Failing tests** in `src/muthur/tests.rs` (helper `tab()` uses a temp log, an `api_key`, a spawner that pushes `(id, ChatRequest)` into an `Rc<RefCell<Vec<_>>>` and returns a closure that sets an `aborted` flag, a fake `now` from a `Cell<Instant>`, and `ship_time` = `"18 NOV 2137 14:02"`; `screen(tab, w, h) -> Vec<String>` renders into a `Buffer`):
```rust
#[test] fn wrap_breaks_long_words() { assert_eq!(wrap("AB CDEFGHIJ K", 4), vec!["AB", "CDEF", "GHIJ", "K"]); assert_eq!(wrap("A\n\nB", 10), vec!["A", "", "B"]); }
#[test] fn submit_logs_with_ship_time_and_spawns() { let (mut t, sent) = tab(); type_str(&mut t, "status"); t.handle_key(&Key::Enter); let s = sent.borrow(); assert_eq!(s.len(), 1); let MessageContent::Text(u) = &s[0].1.messages.last().unwrap().content else { panic!() }; assert_eq!(u, "[SHIP TIME 18 NOV 2137 14:02] STATUS"); assert!(s[0].1.system.contains("MU/TH/UR 6000")); }
#[test] fn blank_enter_is_ignored() { let (mut t, sent) = tab(); type_str(&mut t, "   "); t.handle_key(&Key::Enter); assert!(sent.borrow().is_empty()); }
#[test] fn reply_reveals_over_time_and_logs() { /* submit; on_llm(1, Text("All **good**.")); advance clock 100ms; screen contains "ALL G" but not "ALL GOOD."; on_llm(1, Done{End..}); advance 1s; screen contains "ALL GOOD."; reopen log → history reply text "All **good**." */ }
#[test] fn typing_during_reply_does_not_skip_but_esc_does() { /* submit; Text("ABCDEFGHIJ"); type "X" → visible still ""; Esc → visible "ABCDEFGHIJ"; input line shows "> X" */ }
#[test] fn enter_while_busy_flashes() { /* submit; type "Y"; Enter → 1 spawn only; status row contains "PROCESSING — STAND BY" rendered as ascii-safe "PROCESSING -- STAND BY" */ }
#[test] fn ctrl_c_aborts_and_marks_interrupted() { /* submit; Text("PART"); Ctrl('c') → aborted flag set; screen has "[INTERRUPTED]"; log reply stop "interrupted"; later on_llm(1, Text("MORE")) ignored */ }
#[test] fn error_after_partial_keeps_text() { /* submit; Text("HALF"); Error(SignalLost) → screen has "HALF" and "UNABLE TO COMPLY -- SIGNAL LOST"; log stop "interrupted" */ }
#[test] fn error_without_text_logs_error_pair() { /* Error(Auth) → log stop "error"; next submit's window excludes the failed pair */ }
#[test] fn stale_req_id_ignored() { /* submit twice (second after Done); on_llm(1, Text("OLD")) ignored */ }
#[test] fn purge_requires_confirmation() { /* history exists; "/purge" Enter → screen "CONFIRM Y/N"; 'n' → history intact; "/PURGE" Enter 'Y' → transcript empty, log file gone */ }
#[test] fn not_ready_flashes_no_access_code() { /* cfg without key; submit → no spawn, no log record, "NO ACCESS CODE" on screen */ }
#[test] fn history_redisplays_on_open() { /* pre-populate log with "[SHIP TIME X] HI" / "**Hello**" → new tab shows "> HI" and "HELLO" with no reveal */ }
#[test] fn scrollback_and_marker() { /* 30 exchanges; PageUp → "[SCROLL -" visible and the bottom line is not the input's latest reply; Enter clears */ }
#[test] fn many_lines_fit_and_border_intact() { /* 61x40 area, long unbroken 200-char reply → every rendered row is exactly 61 chars and starts with '│' or a corner */ }
#[test] fn tick_interval_only_while_busy() { /* idle → None; after submit → Some(100ms); after Done + skip → None */ }
#[test] fn activity_on_completion() { /* Done → take_activity() true once */ }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: MU/TH/UR chat tab`

---

### Task 9: Wiring, golden, live smoke test, verification

**Files:**
- Modify `src/app/mod.rs`, `src/run.rs`.
- Modify `tests/golden.rs`; create `tests/golden/muthur_windowed.txt`.
- Create `tests/llm_live.rs`.
- Modify `docs/ARCHITECTURE.md` (Modules table: `llm` and `chatlog` rows match reality, plus a deploy note: Alpine needs `ca-certificates`).

**Interfaces:**
- Consumes everything above.
- Produces:
  - `App::assemble(chrome, utility, muthur: MuthurTab, shell)`;
  - `App::on_llm(req_id, ev)`, which routes to the MU/TH/UR tab;
  - `App::tick_interval() -> Duration`: the minimum of the tabs' `tick_interval()`, defaulting to 1 s.

**Event loop.** Keep the 1 s `tick`. Add the branch `_ = tokio::time::sleep(app.tick_interval()), if app.tick_interval() < Duration::from_secs(1) => dirty = true`, and handle `Event::Llm { req_id, ev } => { app.on_llm(req_id, ev); dirty = true }`.

**Runtime.** Use `new_current_thread().enable_time().enable_io()`.

**`run()`:**
1. Build `LlmClient::new(cfg.muthur.clone())` into an `Arc`.
2. Open the chat log at `dirs::state_dir()…/muthur/chat.jsonl`.
3. Build `MuthurDeps`:
   - `spawn` = `move |id, req| { let h = client.spawn(id, req, tx.clone()); Box::new(move || h.abort()) }`;
   - `now` = `Instant::now`;
   - `ship_time` = `chrome::format_date(local_now, year_offset)` + `" HH:MM"`, matching the title bar's date format;
   - `timestamp` = `chrono::Local::now().to_rfc3339()`.
4. If the log fails to open, log the error and continue with a log in a temp path, so the tab still works without persistence. Add a `tracing::error!` for it.

`spawn` is only ever called from `handle_key` inside `rt.block_on`, so `tokio::spawn` has a runtime.

- [ ] **Step 1: Failing tests**:
  - **`run::tests::llm_event_reaches_muthur`**: an App whose MU/TH/UR tab has a fake spawner. Send `Keys` to type and Enter on F2, then `Event::Llm{req_id:1, ev: Text("NOMINAL")}` and `Done`, then `^] q`. Assert the output bytes contain `NOMINAL`.
  - **`app::tests::activity_marker_for_background_reply`**: submit on F2, switch with F1, deliver `Done`, render. Assert the sidebar MU/TH/UR folder shows `*`.
  - **`golden::muthur_windowed`**:
    - Build the windowed screen with a fake-spawner `MuthurTab` and a temp log already holding one exchange (`HI` → `"GREETINGS."`).
    - Type `REACTOR STATUS`, press Enter, deliver `Text("NO REACTOR TELEMETRY AVAILABLE — SENSOR ACCESS NOT INSTALLED.")`, then advance the fake clock 400 ms so that 20 characters are revealed.
    - Run under `StrictWriter`, assert no violations, and compare with `tests/golden/muthur_windowed.txt` (written with `UPDATE_GOLDEN=1`, then read and checked by eye against the spec's Screen sketch).
- [ ] **Step 2: Run them** → FAIL.
- [ ] **Step 3: Implement the wiring** described above.
- [ ] **Step 4: Run** `cargo test` → all pass. `UPDATE_GOLDEN=1 cargo test --test golden muthur_windowed`, then read the golden file and confirm:
  - the title ` MU/TH/UR 6000 `;
  - the banner;
  - `> HI` / `GREETINGS.` / `> REACTOR STATUS`;
  - a partial reply ending in `█`, which the backend renders as a reverse-video space;
  - the separator;
  - `RECEIVING`;
  - `> ` with an empty input.
- [ ] **Step 5: Create `tests/llm_live.rs`**: two `#[ignore]` tokio tests, `anthropic_round_trip` (needs `MUTHUR_LIVE_ANTHROPIC_KEY`) and `openai_round_trip` (needs `MUTHUR_LIVE_OPENAI_URL` and `MUTHUR_LIVE_OPENAI_MODEL`). If a variable is unset, print a skip message and return. Each test:
  1. Opens a temp `ChatLog`, appends a user turn, and gets `window()`.
  2. Runs `LlmClient::stream` and asserts some non-empty text and a `Done { stop: End }`.
  3. Appends the assistant record and a second user turn, `"REPEAT YOUR LAST ANSWER IN THREE WORDS."`.
  4. Streams again and asserts `Done { stop: End }`. On Anthropic, this proves that replaying the thinking blocks is accepted.
  5. Prints the received text.

  Make sure it compiles: `cargo test --test llm_live` → 0 run, 2 ignored. **Do not run it with credentials**; the user runs it.
- [ ] **Step 6: Final verification**:
  - `cargo clippy --all-targets -- -D warnings` → clean.
  - `cargo test` → all pass.
  - `cargo build --release --target x86_64-unknown-linux-musl` → success.
  - `cargo tree -i aws-lc-sys --target x86_64-unknown-linux-musl` → no match.
  - `grep -rn "api_key\|x-api-key" src | grep -i "trace\|info!\|warn!\|debug!"` → nothing.
  - Smoke test: run the release binary on a PTY with `--strict` and a config holding `provider = "openai"` and `base_url = "http://127.0.0.1:9"`. Press F2, type `HELLO`, press Enter, wait 1 s, then `^] q`. Expect exit 0, `NO CARRIER` on screen, and 0 strict violations.
- [ ] **Step 7: Commit** `feat: wire MU/TH/UR tab; golden and live smoke tests`

---

## Self-review notes

- **Spec coverage:**
  - protocols → Tasks 3 and 4;
  - client, errors and timeouts → Task 6;
  - log, epochs, trim and purge → Task 5;
  - persona, prompt, pipeline, reveal, screen and keys → Tasks 7 and 8;
  - events, tick and activity → Task 9;
  - testing and build checks → Tasks 1 and 9.
- **Deviation from the spec, deliberately.** `StreamEvent::Thinking` was dropped. Thinking is never shown, and the raw blocks travel in `Done.raw`, so the variant would carry nothing anyone uses.
- **Transliteration.** It uses `map_glyph` for graphics characters plus a multi-character `deunicode` fallback (`—` → `--`), which is better than `map_glyph`'s first-byte-only fallback for prose. This is consistent with the spec's "only VT100-safe characters ever reach the buffer".
