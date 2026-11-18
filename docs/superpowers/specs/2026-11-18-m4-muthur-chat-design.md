# M4 — MU/TH/UR Chat: Design

**Status:** approved in brainstorming 2026-11-18. Implements the "MU/TH/UR tab" section of `docs/ARCHITECTURE.md`.

## Goal

The MU/TH/UR folder becomes a chat interface to an LLM in the voice of the MU/TH/UR 6000 mainframe:
- replies stream in and are revealed at teletype speed;
- one conversation persists across restarts;
- failures are reported in character.

It works against two endpoints, picked in config:
- a local **vLLM** server (OpenAI-compatible `/v1/chat/completions`);
- **Anthropic's native Messages API** (`/v1/messages`).

The tool-calling loop is not in M4 (it is M5). M4 does parse tool calls so that M5 has a place to hook in.

## Decisions

| Topic | Decision |
|---|---|
| Endpoints | Two wire protocols, `openai` and `anthropic`, behind one internal stream type. Anthropic uses the native Messages API, never the OpenAI shim. |
| Choosing | One active provider in config (`provider = ...`). To switch, edit the config and restart. |
| HTTP | `reqwest` 0.13 with `default-features = false`, `rustls-no-provider`. `rustls` is a direct dependency with `ring` (no aws-lc-rs, no cmake), and `ring::default_provider().install_default()` runs at startup. Certificates are checked by `rustls-platform-verifier` against the system CA store. |
| Concurrency | The request runs as a `tokio::spawn` task on the existing current-thread runtime, which gains `enable_io` and tokio's `net` feature. The body is read with `Response::chunk()`. Events go out on the existing `Event` channel. |
| Context overflow | Drop the oldest exchanges from what is sent; the full log stays on disk. A reserved `summary` record type leaves room to add summarisation later without migrating the log. |
| While replying | Type-ahead: the input line stays live, and Enter flashes `PROCESSING — STAND BY`. Any non-editing key skips the reveal. Ctrl-C aborts the request and keeps the partial reply, marked `[INTERRUPTED]`. |
| Commands | In-line commands that are never sent to the model: `/PURGE` (asks `CONFIRM Y/N`) and `/HELP`. |

## Modules

```
src/llm/
  mod.rs        ChatRequest, Message, StreamEvent {Text, Thinking, ToolCall, Done(Stop), Error}, Stop
  sse.rs        SSE framing: bytes → {event, data} records. Pure; partial chunks are buffered.
  openai.rs     Request JSON and chunk parser for /v1/chat/completions. Pure.
  anthropic.rs  Request JSON and event parser for /v1/messages. Keeps the raw content blocks. Pure.
  client.rs     reqwest client; spawn_request(req) → JoinHandle; sends Event::Llm{req_id, ev}.
src/chatlog.rs  JSONL store: append, load, build the message window (epochs and stripping), purge.
src/muthur/
  mod.rs        MuthurTab: state machine, key handling, Tab impl.
  reveal.rs     Teletype buffer. Time is passed in; pure.
  text.rs       Markdown strip → uppercase (configurable) → map_glyph transliteration.
  view.rs       Word-wrapped transcript, scrollback, status line, input line.
```

### Data flow

1. Enter with text.
2. `chatlog` appends the user record and builds the message window.
3. `llm::client::spawn_request` starts the request.
4. Each SSE record is parsed into `StreamEvent`s and sent as `Event::Llm { req_id, ev }`.
5. `App::on_llm` passes them to `MuthurTab::on_llm`.
6. Text goes through the output pipeline into the reveal buffer.
7. On `Done`, the assistant record (text, stop reason, raw blocks) is appended to the log.

- **Tick rate.** A new `Tab::tick_interval() -> Option<Duration>` lets the event loop tick at ~100 ms while any tab is revealing or animating. Otherwise it keeps the 1 s tick.
- **Activity marker.** The MU/TH/UR folder's `*` lights when a reply completes while another tab is active.
- **Tool calls.** `ToolCall` events are parsed and logged. With zero tools registered, M4 treats them as `Done`.

## Configuration `[muthur]`

```toml
provider       = "anthropic"                  # "anthropic" | "openai"
base_url       = "https://api.anthropic.com"  # default per provider; openai has no default and must be set
model          = "claude-sonnet-5-5"          # default for anthropic; required for openai
api_key        = ""                           # required for anthropic; optional for openai (sent as Bearer if set)
max_tokens     = 4096
effort         = "low"                        # anthropic only: output_config.effort
fallbacks      = true                         # anthropic only: server-side refusal fallback
cache_ttl      = "5m"                         # anthropic only: prompt-cache TTL, "5m" | "1h"
context_tokens = 200000                       # trim budget, estimated as chars/4
reveal_cps     = 50
uppercase      = true
system_prompt  = ""                           # empty → built-in persona
```

The config file is already required to be `0600`, so the API key is protected.

- **Missing `api_key` with `anthropic`.** The app starts normally. Submitting shows `UNABLE TO COMPLY — NO ACCESS CODE`.
- **`openai` without `base_url` or `model`.** The same applies, and the status line shows `NOT CONFIGURED`.

## Wire protocols

### Anthropic (`POST {base_url}/v1/messages`)

Headers:
- `x-api-key`;
- `anthropic-version: 2023-06-01`;
- `content-type: application/json`;
- `anthropic-beta: server-side-fallback-2026-07-01`, when `fallbacks`.

Body:
- `model`, `max_tokens`, `stream: true`, `system`;
- `messages`, from the log;
- `output_config: {effort}`;
- `fallbacks: "default"`, when `fallbacks` is on;
- `cache_control: {"type": "ephemeral"}`, plus `"ttl": "1h"` when `cache_ttl = "1h"`. This is top-level automatic prompt caching: the API puts the breakpoint on the last cacheable block and moves it forward as the conversation grows, so each turn reads the earlier history from cache. It depends on the frozen system prompt and on user turns being replayed exactly as stored. Sonnet 5.5's minimum cacheable prefix is 512 tokens, so the first turn or two may not cache.

There is no `thinking` field. Adaptive thinking is the default, and `display` stays at its default of `"omitted"`; thinking is never shown.

Stream events handled:
- `message_start`: model name and usage (`input_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`);
- `content_block_start`: the block type — `text`, `thinking`, `redacted_thinking`, `tool_use`, `fallback`, or an unknown type that is kept raw;
- `content_block_delta`: `text_delta` → `Text`; `thinking_delta` and `signature_delta` are accumulated into the raw block; `input_json_delta` is accumulated for `tool_use`;
- `content_block_stop`;
- `message_delta`: `stop_reason`;
- `message_stop`: `muthur.log` gets one `info` line with the turn's token usage: input, cache write, cache read, output (`output_tokens` comes from `message_delta.usage`). This is how cache hits are checked.
- `ping`: ignored;
- `error`: becomes `StreamEvent::Error`.

The parser reassembles every content block exactly as it would appear in a non-streaming `content` array. Those blocks are the `raw` that gets replayed.

**Fallback replay rule.** When replaying an assistant message that contains a `fallback` block, leave out the `thinking`, `redacted_thinking` and `tool_use` blocks that come *before* the last `fallback` block. Everything else is echoed as it is.

### OpenAI / vLLM (`POST {base_url}/v1/chat/completions`)

- **Headers:** `Authorization: Bearer` when `api_key` is set.
- **Body:** `model`, `max_tokens`, `stream: true`, and `messages`, where the system prompt goes first as a `system` message.
- **Stream:** `choices[0].delta.content` → `Text`. `delta.tool_calls[]` are accumulated by index into `ToolCall`. `finish_reason` → `Stop`. `data: [DONE]` ends the stream. Any `reasoning_content` is ignored.

### Stop mapping

| Anthropic | OpenAI | `Stop` |
|---|---|---|
| `end_turn`, `stop_sequence` | `stop` | `End` |
| `max_tokens` | `length` | `MaxTokens` |
| `tool_use` | `tool_calls` | `ToolUse` |
| `refusal` | `content_filter` | `Refusal` |
| other | other | `Other(String)` |

## Persona and prompt assembly

- **System prompt.** It is built once at startup and frozen: the built-in persona text (or `system_prompt`) with the ship name filled in. It contains no clock or other changing values, which is required for Anthropic's thinking-block binding and for prompt caching. Its SHA-256 (or a stable hash) is compared against the last value recorded in the log.
- **Built-in persona, in substance.** You are MU/TH/UR 6000, the mainframe of the `<SHIP>`; "the ship" is the server you run on.
  - Answer in short, declarative, uppercase-friendly statements.
  - Plain text only: no markdown, no headings, no code fences. Lists are short lines.
  - No pleasantries, no apologies.
  - Decline with `UNABLE TO COMPLY.` and a short reason.
  - State uncertainty plainly (`DATA INSUFFICIENT.`).
  - You have no live sensor access yet: never invent telemetry.
- **User turns.** Each is sent as `[SHIP TIME <DD MON YYYY HH:MM>] <text>`, using the title bar's in-universe date (real date plus `year_offset`). It is stored in the log exactly as sent.

## Output pipeline

Model text is processed incrementally, chunk by chunk:
1. **Markdown strip.** Remove the `#` marks that start headings, `**`/`__`/`*` emphasis markers, single backticks, and the lines of code fences. `[t](u)` becomes `t (u)`. Bullets are normalised to `- `. Markers split across chunk boundaries are handled with a small amount of carry-over state.
2. **Uppercase**, when configured.
3. **Transliteration** with `dialect::map_glyph` (or the shell's equivalent table), so that only VT100-safe characters ever reach the buffer.
4. **Reveal buffer.**

The model's raw text is what gets stored in the log. The pipeline runs again when the transcript is loaded.

## Reveal

- `Reveal { pending: String, shown: usize }`. `advance(now)` releases `floor(elapsed × reveal_cps)` characters, carrying over the fractional remainder.
- `skip()` shows everything received so far, and text that arrives later shows immediately for the rest of that reply.
- The reveal is decoupled from token arrival: if tokens arrive slowly, the reveal just waits.
- Newlines count as one character.

## Screen

The layout is the same in the windowed content pane (61×40) and in fullscreen (80×43):

```
┌ MU/TH/UR 6000 ────────────────────────────────────────────┐
│ INTERFACE 2037 READY.                                     │
│                                                           │
│ > WHAT IS THE STATUS OF THE REACTOR                       │
│                                                           │
│ NO REACTOR TELEMETRY AVAILABLE. SENSOR ACCESS NOT         │
│ INSTALLED.                                                │
│                                                           │
│ UNABLE TO COMPLY. CLEARANCE REQUIRED█                     │
├───────────────────────────────────────────────────────────┤
│ RECEIVING                                    [SCROLL -12] │
│ > WHAT IS_                                                │
└───────────────────────────────────────────────────────────┘
```

- **Transcript.** User lines are prefixed `> `, and exchanges are separated by a blank line. Text is word-wrapped to the inner width (hard-broken if a single word is longer) and re-wrapped on resize. A `█` cursor follows the reveal.
- **Status line:**
  - `READY FOR INQUIRY`;
  - `PROCESSING` (before the first text; dots animate `.`→`..`→`...` about once a second);
  - `RECEIVING`;
  - `UNABLE TO COMPLY — <REASON>` and `PROCESSING — STAND BY`, both in reverse video and both cleared by the next key;
  - `CONFIRM Y/N`, during `/PURGE`.
  - `[SCROLL -N]` sits at the right edge while scrolled back.
- **Input.** One line that scrolls horizontally, with the cursor placed through `Tab::cursor()`. Editing keys: printable characters, Backspace, the Line Del/Char key (PF2, per `hardware.md`), Left/Right, and Ctrl-U to clear the line. Input is echoed uppercase when `uppercase` is set, and sent that way too: the input buffer itself holds uppercase text.
- **Scrollback.** PgUp/PgDn scroll by one page minus one line. The view is pinned to the bottom unless scrolled back; Enter, or new output while pinned, keeps it at the bottom.
- **Startup.** The transcript is rebuilt from the log with no reveal. The banner line `INTERFACE 2037 READY.` is always first.
- **Keys:**
  - Ctrl-C aborts when a request is active and does nothing otherwise.
  - Any non-editing key skips the reveal: `Esc`, or keys that would otherwise be ignored. Typing does *not* skip it, because type-ahead must not cancel the reveal.

## Log

- **File.** `<state_dir>/muthur/chat.jsonl`, created with mode `0600`. It is append-only, one JSON object per line, each with `v: 1` and a monotonic `seq`.

| kind | fields |
|---|---|
| `user` | `ts`, `text` (exactly as sent, including the `[SHIP TIME]` prefix) |
| `assistant` | `ts`, `provider`, `model`, `text` (concatenated text blocks), `stop` (`end_turn`/`max_tokens`/`tool_use`/`refusal`/`interrupted`/`error`/other), `raw` (Anthropic content blocks; absent for openai and for interrupted or error turns) |
| `epoch` | `from` (first `seq` that is still sent), `reason` (`trim` / `system_changed`), `system_hash` |
| `summary` | reserved; M4 ignores it when loading |

### Building the message window

1. Find the latest `epoch` E; if there is none, `from = 0`.
2. Take the user/assistant pairs with `seq ≥ from`.
3. Leave out any pair whose assistant record has `stop: error` or empty text, and any trailing user record that has no answer (it is the current inquiry, which is appended separately).
4. **Anthropic.** An assistant record with `seq < E.seq` is sent with its thinking and redacted-thinking blocks removed from `raw` (falling back to `text` if nothing is left). Records written after E replay `raw` exactly, with the fallback rule applied. Interrupted records are sent as plain text.
5. **OpenAI.** Only `text` is sent.
6. **Trimming.** If the estimated size (system + window + new inquiry, chars/4) is over `context_tokens`, write a new `epoch` with `reason: trim`. Its `from` is chosen so that the remaining window is ≤ 50 % of the budget, cutting only between whole exchanges. Then rebuild the window.
7. **System change.** If the current system hash differs from the last recorded one, write `epoch { from: <unchanged>, reason: system_changed }` before building the window.

### Other rules

- **Corrupt lines.** They are skipped with a warning in `muthur.log`, and `seq` resumes after the highest valid one.
- **Purge.** `/PURGE` then `Y` deletes the file and clears the transcript. Any other key cancels.

## Errors

| Cause | Status line |
|---|---|
| connect / DNS / TLS failure | `UNABLE TO COMPLY — NO CARRIER` |
| HTTP 401 / 403 | `UNABLE TO COMPLY — AUTHORIZATION REJECTED` |
| HTTP 429 / 529 / 5xx | `UNABLE TO COMPLY — SYSTEM OVERLOADED` |
| other HTTP 4xx | `UNABLE TO COMPLY — MALFORMED INQUIRY` |
| no chunk for 60 s (idle timeout) | `UNABLE TO COMPLY — SIGNAL LOST` |
| in-stream `error` event | `UNABLE TO COMPLY — SYSTEM OVERLOADED` (for `overloaded_error`) or `TRANSMISSION ERROR` |
| `Stop::Refusal` | `UNABLE TO COMPLY — DIRECTIVE RESTRICTED` |
| `Stop::MaxTokens` | reply kept; a `[TRANSMISSION TRUNCATED]` line is appended |

- **Connect timeout:** 15 s. There is no total timeout.
- **No retries in M4.**
- **Logging.** `muthur.log` records the status, the error type and the provider message. It never records request or response bodies, or headers.

## Event and trait changes

- `run::Event` gains `Llm { req_id: u64, ev: StreamEvent }`.
- `Tab` gains:
  - `on_llm(&mut self, req_id, ev)`, a no-op by default;
  - `tick_interval(&self) -> Option<Duration>`.
- `App::assemble` takes the real `MuthurTab`.
- `MuthurTab` gets a small "spawner" closure, so that tests can capture requests instead of doing real HTTP.

## Testing

- **Pure unit tests:**
  - **SSE framing:** split chunks, CRLF, comments, multi-line `data`.
  - **Both parsers, against fixture streams in `tests/fixtures/llm/`:**
    - Anthropic: text, thinking with a signature, `tool_use` with `input_json_delta`, a mid-stream `fallback`, `error`, a refusal;
    - vLLM: text, `tool_calls`, `length`.
  - **The request builders.**
  - **The chatlog:** epochs, stripping, trimming hysteresis, excluded failed pairs, corrupt lines, purge, 0600 mode.
  - **The text pipeline**, including markers split across chunk boundaries.
  - **Reveal pacing.**
  - **The `MuthurTab` state machine:** submit, type-ahead refusal, skip, abort, `/PURGE` confirm and cancel, error display.
- **Client:** a loopback `TcpListener` serving canned HTTP covers streaming, abort, idle timeout, and the 401/429/500 → error mapping.
- **Golden test:** the MU/TH/UR tab windowed, mid-reveal, run under `--strict` with no violations.
- **Live smoke test:** `#[ignore]`, run manually by the user with `MUTHUR_LIVE_ANTHROPIC_KEY` or `MUTHUR_LIVE_OPENAI_URL`/`_MODEL`. It does one round-trip, checks that text arrived and the stop is `End`, then a second turn replaying the first. That second turn checks thinking-block replay on Anthropic.
- **Build check:** `cargo tree -i aws-lc-rs` is empty, and a release musl build succeeds without cmake.

## Out of scope (M4)

- Tool execution and the tool loop (M5).
- Summarisation of trimmed turns.
- Profiles and switching providers at runtime.
- Automatic retries.
- Multiple conversations.
- Showing thinking.
