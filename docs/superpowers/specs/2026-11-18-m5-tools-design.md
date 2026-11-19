# M5 — MU/TH/UR Tools: Design

**Status:** approved in brainstorming on 2026-11-18. Implements "Tools & safety" in `docs/ARCHITECTURE.md`. Builds on M4 (`docs/superpowers/specs/2026-11-18-m4-muthur-chat-design.md`).

## Goal

MU/TH/UR can inspect the ship (the server) through read-only tools that run automatically. It can also ask to start, stop or restart allowlisted services. Those actions run only after the operator confirms them on screen with `OVERRIDE REQUIRED — CONFIRM Y/N`.

- Nothing ever runs through a shell.
- Every call is written to an audit log.
- Both providers work: Anthropic Messages, and OpenAI-compatible (vLLM).

## Decisions

| Topic | Decision |
|---|---|
| Privilege for tier 2 | `sudo -n` (or `doas -n`) with a narrow sudoers rule that the operator installs. MUTHUR's allowlist is a second, independent filter. |
| Tier-2 actions | `start_service`, `stop_service` and `restart_service`. Each has its own allowlist of units. |
| Tier-1 tools | `ship_status`, `service_status`, `journal`, `list_units`, `listening_ports`. |
| Transcript | One in-character line per call (`ACCESSING: …` / `ACCESS DENIED: …`), with no raw output. Full output goes to the audit log. |
| Loop placement | A pure `agent::Turn` state machine that returns effects. `MuthurTab` carries them out. |

## Modules

```
src/tools/
  mod.rs      Registry: ToolSpec {name, tier, description, schema}; definitions(cfg) → Vec<ToolSpec> (fixed order);
              validate(name, input, cfg) → Ok(Invocation) | Err(reason). Runner trait. Result text cap.
  tier1.rs    ship_status, service_status, journal, list_units, listening_ports
  tier2.rs    start/stop/restart_service: argv build with elevation, allowlist check, error mapping
  audit.rs    AuditLog: append-only JSONL writer
src/agent.rs  Turn state machine (pure)
```

Changes elsewhere:
- **`llm::anthropic` / `llm::openai`:** the request carries `tools` and `tool_choice`. Assistant replays keep tool calls. Tool results are serialised.
- **`chatlog`:** adds the `tool` record kind, exchange-based windows, and the tools hash in the epoch.
- **`muthur`:** carries out effects, adds the `Override` state and the `ACCESSING` lines, and updates the persona.
- **`run`:** adds `Event::ToolDone`, a shared latest-snapshot slot, and wiring.
- **`config`:** adds a `[tools]` section.

## Configuration

```toml
[tools]
enabled = true          # false → no `tools` sent; behaves exactly like M4
max_rounds = 8          # tool rounds per inquiry; then one forced final request with tool_choice none
elevate = "sudo"        # "sudo" | "doas"
journal_max_lines = 50

[tools.services]        # tier 2; an empty list means that tool is not offered
restart = []
start = []
stop = []
```

- **Unit names** in the config, and arguments from the model, are normalised: a name gets `.service` appended unless it already ends in a known unit suffix (`.service`, `.socket`, `.timer`, `.mount`, `.target`, `.path`, `.slice`, `.scope`, `.swap`, `.automount`, `.device`). A valid name:
  - matches `^[A-Za-z0-9@._:-]+$`;
  - is 1–128 characters;
  - does not start with `-` or `.`.
- **Allowlist matching** compares normalised names.
- **Docs.** `docs/config.example.toml` gains the section, and its sync test keeps passing. `docs/sudoers.example` shows the matching rule, e.g. `jake ALL=(root) NOPASSWD: /usr/bin/systemctl restart nginx.service`.

## Tools

All commands are run as an argument vector (no shell), with stdin set to null. Output is stdout followed by stderr, capped at **4096 bytes**; when cut, a newline and `[TRUNCATED]` are appended.

### Tier 1 (runs automatically)

| Tool | Input schema | Runs | Timeout |
|---|---|---|---|
| `ship_status` | `{}` | nothing; formats the latest `Snapshot` | — |
| `service_status` | `{unit: string}` | `systemctl status --no-pager --lines=0 <unit>` | 5 s |
| `journal` | `{unit: string, lines: integer 1..journal_max_lines}` | `journalctl -u <unit> -n <lines> --no-pager -o short-iso` | 5 s |
| `list_units` | `{state: "running"\|"failed"\|"inactive"}` | `systemctl list-units --type=service --state=<state> --no-pager --plain --no-legend` | 5 s |
| `listening_ports` | `{}` | `ss -tlnH` | 5 s |

- **`ship_status` output.** A plain-text block, one fact per line:
  - host, uptime, load;
  - CPU total;
  - memory used/total, swap;
  - each disk mount and its percentage;
  - network rx/tx;
  - the top processes;
  - failed units (or `N/A`);
  - containers (or `N/A`).
  - Before the first snapshot arrives, it returns `TELEMETRY NOT YET AVAILABLE` (`is_error`).
- **Exit status.** `systemctl status` exits non-zero for inactive or failed units. That is still a successful result (`is_error: false`) as long as there is output.

### Tier 2 (operator override required)

| Tool | Input schema | Runs | Timeout |
|---|---|---|---|
| `start_service` | `{unit: enum of tools.services.start}` | `<elevate> -n /usr/bin/systemctl start <unit>` | 30 s |
| `stop_service` | `{unit: enum of tools.services.stop}` | `<elevate> -n /usr/bin/systemctl stop <unit>` | 30 s |
| `restart_service` | `{unit: enum of tools.services.restart}` | `<elevate> -n /usr/bin/systemctl restart <unit>` | 30 s |

On success the result is `COMPLETED: <VERB> <UNIT>`. The model checks the outcome with `service_status` if it needs to.

### Definitions

- **Schemas.** Every schema has `"additionalProperties": false` and lists every property in `required`. Anthropic tools are sent with `strict: true`.
- **Strict-safe keywords only.** Anthropic strict tool use rejects numeric and string constraints (`minimum`, `maximum`, `multipleOf`, …). Schemas use only `type`, `properties`, `enum`, `const`, `anyOf`, `allOf`, `$ref`, `required`, `description` and `"additionalProperties": false`. So `journal.lines` is `{"type": "integer", "description": "Number of lines, 1 to N"}` (N = `journal_max_lines`); the range is enforced by validation, not by the schema.
- **Order.** The definitions are built once at startup, in the fixed order of the tables above. Tier-2 tools whose allowlist is empty are left out.
- **When tools are disabled.** With `tools.enabled = false`, or when the definition list is empty, requests carry no `tools` field.

### Results and errors

Every result goes back to the model as `{content: text, is_error: bool}`.

| Cause | Text | is_error |
|---|---|---|
| binary not found (`systemctl`, `journalctl`, `ss`, `sudo`/`doas`) | `NOT AVAILABLE ON THIS SHIP` | true |
| elevation refused (exit 1 with "password is required", "not allowed", "not permitted", "not in the sudoers", "authentication required" or "authorization required", case-insensitive) | `INSUFFICIENT CLEARANCE: <first stderr line>` | true |
| exit 0 with empty (whitespace-only) output, tier 1 | `NO MATCHING ENTRIES` | false |
| timeout | `NO RESPONSE` | true |
| other non-zero exit with no stdout | `EXIT <n>: <stderr>` | true |
| unknown tool, invalid or unlisted argument | `NOT AUTHORIZED: <reason>` (never executed) | true |
| operator said no | `DENIED BY OPERATOR` | true |
| Ctrl-C before the call completed | `ABORTED BY OPERATOR` | true |
| Ctrl-C while a confirmed tier-2 command is running | `ABORTED BY OPERATOR — COMMAND ALREADY ISSUED, OUTCOME UNKNOWN` | true |

### Runner

`trait Runner: Send + Sync { fn run(&self, argv: &[String], timeout: Duration) -> RunOutput }`. `RunOutput` is one of:
- `Exited { code, stdout, stderr }`;
- `NotFound`;
- `TimedOut`.

The production runner reuses the process-group and timeout handling from `metrics::probes`. Tests use a fake runner that records each argv.

## Agent loop (`agent::Turn`)

Pure: no I/O, no clocks.

```rust
pub enum Effect {
    Spawn { forced_final: bool },              // tab builds window (chatlog) and spawns a request
    Run { call_id: String, invocation: Invocation },
    Confirm { call_id: String, prompt: String }, // e.g. "RESTART NGINX.SERVICE"
    Note(String),                              // transcript line: "ACCESSING: …" / "ACCESS DENIED: …"
    LogTool(Vec<ToolResult>),                  // append the `tool` record (all results of the round)
    Audit(AuditEntry),
    Orphan(Orphan),                            // a running call was aborted; its outcome is still to be audited
    Finished,
}
```

- **States:** `Streaming`, then `Resolving` (per-call: queued / running / awaiting confirmation / done), then `Streaming`, and so on until `Finished`.
- **Inputs:**
  - `on_done(stop, calls)`: the stream ended; `calls` are the ToolCalls received in it.
  - `on_tool_done(call_id, result)`;
  - `on_confirm(call_id, bool, audit_online)`: a `yes` while the audit log is offline resolves the call `NOT AUTHORIZED: AUDIT LOG OFFLINE` (decision `rejected`) and never runs it;
  - `abort(elevate)`.
- **When a stream ends with `Stop::ToolUse` and at least one call:**
  1. Each call is validated in order.
     - Invalid calls get an immediate `NOT AUTHORIZED` result and an `ACCESS DENIED` note.
     - Tier-1 calls emit `Note("ACCESSING: …")` and `Run`. They may run concurrently.
     - Tier-2 calls are queued for confirmation, one at a time and in order. A `Confirm` effect is emitted for the first one, and the next follows only after the previous is resolved.
  2. Confirmed: `Note("ACCESSING: …")`, then `Run`. Denied: a `DENIED BY OPERATOR` result and `Note("ACCESS DENIED: …")`.
  3. When every call in the round has a result:
     - emit `LogTool(results)` in call order;
     - emit one `Audit` per call;
     - increment the round counter;
     - emit `Spawn { forced_final: rounds == max_rounds }`.
- **A stream ending with any other stop**, or with `ToolUse` and no calls, means `Finished`.
- **A `ToolUse` stop in a forced-final response:**
  - its calls are answered `NOT AUTHORIZED: ROUND LIMIT`;
  - the round is logged;
  - one more forced request is spawned.
  - This can happen at most once; after that the turn finishes with `[TRANSMISSION TRUNCATED]` semantics.
- **`abort()`:**
  - every call without a result gets `ABORTED BY OPERATOR`;
  - except a confirmed tier-2 call whose command is running: it cannot be cancelled (it runs as root, on a blocking thread), so it gets `ABORTED BY OPERATOR — COMMAND ALREADY ISSUED, OUTCOME UNKNOWN`, a `Note("ACCESS INTERRUPTED: …")`, and its `Audit` (decision `aborted`) carries the `argv` that was issued;
  - every call aborted while running also emits `Orphan`: the tab remembers (request id, call id, invocation, tool, args), and when that call's late outcome arrives it appends a second audit record (decision `confirmed` for tier 2, `auto` for tier 1) with the real `argv`, `exit`, `ms` and `output`. Nothing else happens: no transcript line, no chat-log record;
  - if any call was open, emit `LogTool` and `Audit`;
  - then `Finished`.
  - The tab writes the `interrupted` assistant record exactly as in M4.
- **Notes text.** `ACCESSING: <TOOL WORDS> <ARGS>`, for example `ACCESSING: SERVICE STATUS NGINX.SERVICE`, `ACCESSING: JOURNAL NGINX.SERVICE 20`, `ACCESSING: SHIP STATUS`.

## Wire protocols

### Anthropic

- **Request:** add `tools: [{name, description, input_schema, strict: true}]` when there are definitions, and `tool_choice: {"type": "none"}` when `forced_final`.
- **Assistant replay:** sends `raw` unchanged, including `tool_use` blocks. `replay_blocks` no longer drops `tool_use`. It still applies the fallback rule, strips thinking before an epoch boundary, and drops empty or whitespace-only text blocks.
- **Tool results:** one `user` message whose content is every `{type: "tool_result", tool_use_id, content, is_error}` for the round, in call order.

### OpenAI / vLLM

- **Request:** add `tools: [{type: "function", function: {name, description, parameters}}]`, and `tool_choice: "none"` when `forced_final`.
- **Assistant replay:** an assistant turn with calls is sent as `{role: "assistant", content: <text or null>, tool_calls: [{id, type: "function", function: {name, arguments: <JSON string>}}]}`. The assistant record stores that `tool_calls` array as `raw` (the provider is openai).
- **Stop:** if the stream carried any tool calls, the stop is `ToolUse` whatever the `finish_reason` (some servers report `stop` or nothing after tool calls).
- **Tool results:** one `{role: "tool", tool_call_id, content}` message per result, in call order. `is_error` results are prefixed `ERROR: `.
- **Docs.** `docs/ARCHITECTURE.md` states that vLLM needs `--enable-auto-tool-choice --tool-call-parser <parser>`.

### Messages

`llm::Message` / `MessageContent` gain a `ToolResults(Vec<ToolResult>)` variant (role user), so chatlog can hand either protocol a neutral message list.

## Chat log

- **New record kind:** `{"v":1,"seq":N,"kind":"tool","ts":…,"results":[{"id","name","content","is_error"}]}`.
- **Assistant records:**
  - an assistant record with `stop: "tool_use"` keeps `raw`, for both providers;
  - for openai, `raw` is the `tool_calls` array.
- **Exchange:** a user record plus every following assistant/tool record up to the next user record.

### Window rules

These replace M4's pair rules.
1. A **past** exchange is sent only if its last record is an assistant record that is final, meaning:
   - its stop is not `tool_use`, `error` or `refusal`;
   - its text is not blank.
   Otherwise the whole exchange is excluded, consistently, on every build.
2. The **current** exchange (the one containing the last user record) is always sent in full, ending with the last record. Failed or aborted rounds never stay current, because the tab finishes the turn first.
3. **Converting records to messages:**
   - user → `Text`;
   - assistant → `Blocks(replay…)` (Anthropic with raw) / openai assistant-with-tool_calls / `Text`;
   - tool → `ToolResults`.
4. **Epoch hash:** `fnv64(provider \n model \n system \n tools_json)`, where `tools_json` is the serialised definitions. An allowlist change therefore writes a `system_changed` epoch and strips thinking once.
5. **Trimming** still cuts only at exchange boundaries. The current exchange is never cut.

### Redisplay

`history()` becomes exchange-aware. `tool` records add a note line per result:
- `ACCESSING: …` for normal results;
- `ACCESS INTERRUPTED: …` for results whose content starts with `ABORTED BY OPERATOR — COMMAND ALREADY ISSUED`;
- `ACCESS DENIED: …` for other results whose content starts with `NOT AUTHORIZED`, `DENIED` or `ABORTED`.

## Audit log

- **File:** `<state_dir>/muthur/audit.jsonl`, created with mode `0600` when it is opened at startup (an open that fails leaves the log offline), append-only. Each record is a single `write_all`, and a torn tail gets a newline first, as in chatlog.
- **Fields of one record per call:**
  - `ts`
  - `tool`
  - `args` (the model's input)
  - `tier` (1 or 2)
  - `decision`: `auto` | `confirmed` | `denied` | `rejected` | `aborted`
  - `argv` (null if never run)
  - `exit` (code, `"timeout"`, `"not_found"` or null)
  - `ms`
  - `output`: the exact capped text sent to the model
- **When it is written:** before the result is sent back to the model. A call aborted while running gets its `aborted` record at the abort and a second record with the real outcome when the command finishes.
- **If the audit log cannot be written:** the tool result is still delivered, `tracing::error!` is written, and the status line flashes `AUDIT LOG OFFLINE`. Tier-2 calls are **refused** while the audit log is unavailable: they return `NOT AUTHORIZED: AUDIT LOG OFFLINE` without asking the operator. The check is repeated when the operator confirms, so a log that goes offline while the prompt is up still refuses the call.

## Screen

### Override prompt

- **Status row**, in reverse video: `OVERRIDE REQUIRED -- RESTART NGINX.SERVICE -- CONFIRM Y/N`, passed through `ascii_safe`.
- **Arming delay:** keys received less than **1 s** after the prompt first renders are ignored.
- **Keys once armed:**
  - `Y`/`y` confirms;
  - `N`/`n`/`Esc` denies;
  - `Ctrl-C` aborts the inquiry;
  - all other keys are ignored and are not typed into the input.
- **Background:** if the MU/TH/UR tab is not active when a prompt is raised, the activity marker lights. The prompt waits with no timeout.
- **Status while tools run:** `ACCESSING` (the dots animate like `PROCESSING`).

### Transcript

Notes appear in order between the user line and the reply, e.g.:
```
> IS NGINX RUNNING

ACCESSING: SERVICE STATUS NGINX.SERVICE

NGINX.SERVICE ACTIVE. RUNNING SINCE 17 NOV 2137 09:12.
```

## Persona

The last line of `PERSONA` is replaced with:

```
You can query the ship's systems with your tools. Use them instead of guessing.
Actions that change the ship require operator override and may be denied; if denied, say so plainly and do not retry.
```

## Events and wiring

- `run::Event` gains `ToolDone { req_id: u64, call_id: String, result: ToolResult }`. Tools run on `tokio::task::spawn_blocking`.
- The tab ignores `ToolDone` events with a stale `req_id`.
- `Tab::on_tool_done` is added, with a default no-op.
- **Shared snapshot:** `Arc<Mutex<Option<Snapshot>>>`. The metrics collector thread writes each new snapshot into it, and `ship_status` reads it.
- **`MuthurDeps` gains:**
  - `run_tool: Box<dyn FnMut(u64, String, Invocation)>`, which spawns the blocking run and sends `ToolDone`;
  - `tools: ToolDefs`;
  - `audit: AuditLog`.

## Testing

- **`tools` with a fake `Runner`:**
  - the exact argv for every tool and for both elevators;
  - unit normalisation and validation (`-x`, `../`, spaces, 129 characters);
  - allowlist enforcement, including when the schema enum is bypassed;
  - the journal line bounds;
  - the error mappings (not found, sudo refusal text, timeout, non-zero exit);
  - the 4 KB cap;
  - `ship_status` formatting, and its behaviour before the first snapshot;
  - empty allowlists drop the tool;
  - the definition order is stable.
- **`agent::Turn`:**
  - tier-1 auto-run;
  - tier-2 confirm and deny;
  - mixed parallel calls;
  - invalid calls;
  - the round cap leading to a forced final;
  - a `ToolUse` stop during a forced final;
  - abort while running and while confirming;
  - an audit entry per call;
  - `LogTool` in call order.
- **Protocols:**
  - request bodies with `tools` and `tool_choice`;
  - tool-result serialisation, both providers;
  - replay of tool_use blocks, Anthropic raw and openai tool_calls;
  - a fixture with thinking + tool_use + text.
- **Chat log:**
  - an exchange with rounds is replayed in full;
  - a past exchange ending in a tool round or an error is excluded;
  - the current exchange is sent in full;
  - a tools change writes an epoch;
  - trimming at exchange boundaries;
  - redisplay notes.
- **Audit:** 0600, record fields, a torn tail, and tier-2 refusal when the log is offline.
- **Tab:**
  - the override prompt;
  - the arming delay;
  - Y, N and Esc;
  - Ctrl-C during confirmation;
  - `ACCESSING` notes;
  - a stale `ToolDone` is ignored.
- **Golden:** the windowed MU/TH/UR tab showing an `ACCESSING` note and the `OVERRIDE REQUIRED` prompt.
- **Live (`#[ignore]`, run by the user):** an Anthropic and an openai round trip where the question needs `ship_status` (fake snapshot). It asserts that a tool round happened and the final stop is `End`. A follow-up turn replays tool_use + thinking. Tier 2 is never live.

## Out of scope

- Container and power actions.
- Arbitrary file reads and `du`.
- OpenRC equivalents of the systemd tools (they report `NOT AVAILABLE ON THIS SHIP`).
- A multi-host `which host` parameter.
- Proactive alerts.
- Configurable timeouts.
