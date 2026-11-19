# MUTHUR — Architecture

A bespoke serial front-end for a Wyse WY-60 terminal, styled after the
computers of the Alien franchise (Alien: Isolation's personal terminals and
the original film's MU/TH/UR 6000 interface).

It exists for two reasons:

- **Functional** — the WY-60 renders escape sequences it does not understand
  instead of ignoring them. MUTHUR sits between modern software and the
  terminal and guarantees that only sequences the terminal understands ever
  reach the wire.
- **Aesthetic** — it should look and feel like a Weyland-Yutani console.

## Hardware & link

| Item | Value |
|---|---|
| Terminal | Wyse WY-60, **VT100 personality, enhanced mode** |
| Geometry | **80×43**, WY-60 status line disabled |
| Cable | USB FTDI → DB9, DB9 → DB25 null modem |
| Speed | **38400 baud** |
| Flow control | **XON/XOFF** (RTS/CTS tested: does not work with this cabling) |
| Host | Dedicated server, Arch or Alpine, x86_64 |

Consequences:

- At 38400 baud the budget is ~3.8 KB/s. A full 80×43 repaint with
  attributes is ~4–5 KB (≈1.2 s). The renderer must diff, use scroll regions,
  and pace output. Full repaints (tab switch, fullscreen toggle) are painted
  top-to-bottom so they read as a deliberate wipe.
- With XON/XOFF, Ctrl-S / Ctrl-Q never reach applications. MUTHUR's raw mode
  must leave `IXON`/`IXOFF` enabled on the serial tty.

## Deployment & process model

```
udev rule ─► /dev/wy60 (stable symlink to the FTDI adapter)
serial-getty@wy60 (38400, XON/XOFF, TERM=vt100)
  └─ login
      └─ fish  (config.fish: if on /dev/wy60 → run `muthur`, NOT exec)
          └─ muthur
              └─ fish (Shell tab, on a PTY, TERM=vt100)
```

- Authentication is delegated to agetty/login.
- If MUTHUR exits or crashes, the user lands in plain fish — an out-of-process
  emergency fallback for free.
- MUTHUR always restores termios on exit, including via a panic hook.
- Build: static `x86_64-unknown-linux-musl` binary built on the dev machine;
  `make deploy` copies it to the server. TLS via rustls (no OpenSSL).
- Config: `~/.config/muthur/config.toml`. Contains the LLM API key; MUTHUR
  refuses to start if the file is group/world readable (must be `0600`).
  Annotated example with every key and its default: `docs/config.example.toml`.
- Logs: `~/.local/state/muthur/muthur.log` (tracing, rotated) plus a separate
  LLM tool-call audit log.

## Modes

1. **Boot splash** — Weyland-Yutani / "MU/TH/UR 6000 — INTERFACE 2037" boot
   text at teletype speed, interleaving real facts (hostname, CPU, RAM, link
   speed). ~3–5 s, skippable with any key.
2. **TUI** — three tabs: Utility, MU/TH/UR, Shell.
3. **Emergency passthrough** (`prefix p`) — the Shell tab's PTY output goes to
   the wire **untranslated**. The virtual terminal keeps being fed in the
   background so leaving passthrough repaints the Shell tab correctly. Input
   is still scanned for the prefix key so passthrough can be exited. Doubles
   as a debugging aid for the translator.

## Screen layout ("chrome")

Modelled on the Alien: Isolation personal terminal
(`Inspiration/alien-console/img/real-main.jpg`):

```
┌──────────────────────────────────────────────────────────────────────────────┐
│▓USCSS <SHIP> ── MU/TH/UR 6000 ─────────────────────── 17 NOV 2137 14:02▓▓▓▓▓│  title bar (reverse video)
│                                                                              │
│ FOLDERS        ┌───────────────────────────────────────────────────────────┐ │
│┌────────────┐  │                                                           │ │
││ UTILITY    │──│                content pane (~60 cols)                    │ │
│└────────────┘  │                                                           │ │
│┌────────────┐  │                                                           │ │
││ MU/TH/UR  *│  │                                                           │ │  * = activity marker
│└────────────┘  │                                                           │ │
│┌────────────┐  │                                                           │ │
││ SHELL      │  │                                                           │ │
│└────────────┘  └───────────────────────────────────────────────────────────┘ │
│ F1/F2/F3 SELECT ..... ^] f FULLSCREEN ..... PGUP/PGDN SCROLL ..... ^] ? HELP │  footer
└──────────────────────────────────────────────────────────────────────────────┘
```

- Title bar: ship name (config, e.g. `USCSS <hostname>`), in-universe date
  (real date + configurable year offset; real date available on a key).
- **Fullscreen** (`prefix f`) hides title bar, sidebar and footer. State is
  remembered **per tab**. All tabs — including Shell — start windowed.
  In fullscreen, pressing the prefix shows a transient one-line reverse-video
  strip (`PREFIX: …`).
- Background tabs keep running; the sidebar shows an activity marker when a
  background tab has new output (e.g. a finished MU/TH/UR reply).

## Keys

- **F1 / F2 / F3** — Utility / MU/TH/UR / Shell.
- **Prefix `Ctrl-]`** then:
  - `f` fullscreen toggle · `p` passthrough toggle · `[` scrollback (Shell)
  - `r` force full redraw · `]` send a literal `Ctrl-]` · `?` help
- Measured key sequences and output capabilities: see `docs/hardware.md`.
  PgUp/PgDn are Shift-F15/Shift-F16 (the Page key only sends `^J`); delete-char
  is Line Del/Char (PF2) because Del is a hold-screen key.
- The keymap lives in config; its defaults come from those measurements.

## Modules

Single Rust crate, tokio runtime.

| Module | Responsibility |
|---|---|
| `link` | Owns the terminal fd — serial device or plain stdio (dev). termios raw mode that preserves IXON/IXOFF; baud setup; guaranteed restore on drop/panic. |
| `dialect` | `Dialect` trait describing the output language: cursor addressing, attributes, charset switching, scroll regions, and the **allowlist** of sequences. `Vt100Dialect` for v1; `Wyse60Dialect` (native personality: dim attribute, graphics set, pages) later without touching other modules. |
| `backend` | `Wy60Backend: ratatui::backend::Backend`. **The only path to the wire.** Keeps two full grids — `want` (built from ratatui's diffs) and `shown` (what the terminal displays) — and emits exactly the cells where they differ. Tabs may leave a **scroll hint** (shared `HintSlot`); the backend then scrolls that band of full-width lines with DECSTBM + LF/RI and the compare repairs anything the scroll disturbed (sidebar, borders), so a wrong hint costs bytes, never correctness. Cursor/attribute/charset changes are coalesced; redraws wait for the kernel output queue to drain. Generic over `Dialect`. |
| `input` | Byte stream → key events via configurable keymap; prefix-key state machine. |
| `app` | Event bus (`Input`, `Tick`, `PtyOutput`, `Llm`, `Metrics`, later `Alert`), chrome rendering, tab focus, per-tab fullscreen, mode transitions. |
| `tabs::utility` | System monitor (see below). |
| `tabs::muthur` | Chat UI (see below). |
| `tabs::shell` | PTY + virtual terminal + translation (see below). |
| `llm` | Streaming LLM client (reqwest + rustls/ring, no aws-lc): Anthropic Messages and OpenAI-compatible protocols, SSE parsing, typed `LlmError`s, idle timeout. Each request runs as a tokio task and sends `Event::Llm` to the loop. Deploy note: Alpine needs the `ca-certificates` package for TLS. |
| `chatlog` | Append-only JSONL chat history at `$XDG_STATE_HOME/muthur/chat.jsonl` (epochs, context window/trim, purge). |
| `tools` | Tool registry and safety tiers (`mod`), fixed-argv execution with timeouts (`exec`, `runner`), JSONL audit log (`audit`). |
| `agent` | Pure tool-turn state machine: round cap, tier-2 confirmation queue, aborts; emits effects the MU/TH/UR tab carries out. |
| `metrics` | `MetricsSource` trait; `LocalSource` for v1. |
| `splash` | Boot sequence. |
| `testcard` | `muthur testcard` subcommand: attribute / line-drawing probe and key-echo mode. |
| `config` | TOML load, defaults, `0600` permission check. |

### Shell tab

- fish spawned on a PTY with `TERM=vt100`.
- PTY output feeds a `vt100::Parser` (virtual screen + scrollback). This is
  what makes tab switching possible: returning to Shell repaints from the
  virtual screen.
- Each frame the virtual screen is translated into the ratatui buffer:
  colours → bold / reverse / underline; Unicode → DEC Special Graphics where
  possible, ASCII fallback otherwise, `?` as a last resort.
- Fullscreen toggle resizes the PTY (`SIGWINCH`) so apps redraw at the new size.
- `prefix [` enters scrollback over the virtual terminal's history.

### MU/TH/UR tab

- Configurable `base_url`, `model`, `api_key`; streaming responses.
- **Teletype reveal**: tokens go into a buffer; a fixed-rate (~40–60 cps,
  configurable) reveal drains it, decoupled from token arrival. Any key skips
  the reveal.
- Output **uppercased** (configurable). User input echoed uppercase.
- Prompt `READY FOR INQUIRY`; single-line horizontally-scrolling input;
  PgUp/PgDn scrollback.
- System prompt: terse MU/TH/UR 6000 persona; the server is "the ship";
  plain text only. A light post-processor strips residual markdown.
- Memory: one persistent conversation log on disk (JSONL), oldest turns
  trimmed/summarised to fit the context window; a purge command clears it.
  Format leaves room for multiple sessions later.
- Errors (endpoint down etc.) shown in-character (`UNABLE TO COMPLY — …`).

#### MU/TH/UR configuration

All keys live in the `[muthur]` table of the config file (which must be
`0600`, protecting `api_key`). Every key is optional.

| key | default | meaning |
|---|---|---|
| `provider` | `"anthropic"` | `"anthropic"` (Messages API) or `"openai"` (OpenAI-compatible chat completions, e.g. vLLM) |
| `base_url` | `"https://api.anthropic.com"` for anthropic; none for openai | endpoint root; **required** for openai |
| `model` | `"claude-sonnet-5-5"` for anthropic; none for openai | model name; **required** for openai |
| `api_key` | none | required for anthropic (`x-api-key`); optional for openai (sent as `Bearer`); surrounding whitespace is trimmed, blank counts as unset |
| `max_tokens` | `4096` | per-reply output cap |
| `effort` | `"low"` | anthropic only: `output_config.effort` |
| `fallbacks` | `true` | anthropic only: server-side fallback (`fallbacks: "default"` + beta header) |
| `cache_ttl` | `"5m"` | anthropic only: prompt-cache TTL, `"5m"` or `"1h"` |
| `context_tokens` | `200000` | history trim budget, estimated as chars/4 |
| `reveal_cps` | `50` | teletype reveal rate, characters per second |
| `uppercase` | `true` | uppercase replies on screen |
| `system_prompt` | none | empty or unset uses the built-in MU/TH/UR persona |

- **vLLM:** set `context_tokens` to about 0.75 × the server's
  `max_model_len` (the chars/4 estimate is rough and the reply needs room).
- **vLLM tools:** tool calling needs the server started with
  `--enable-auto-tool-choice --tool-call-parser <parser>` (parser depends on the model).
- **Chat log:** `~/.local/state/muthur/chat.jsonl`, mode `0600`, append-only
  JSONL. If it cannot be opened, a fresh temporary log is created in
  `$XDG_RUNTIME_DIR` (or the temp dir) and history does not persist.
  Changing the system prompt, provider or model starts a new epoch, so older
  thinking blocks are not replayed. `/PURGE` deletes it.
- **Token usage** for each turn (input, cache write/read, output) is logged to
  `muthur.log` (anthropic; the openai stream reports none). Request/response
  bodies and headers are never logged.
- **Alpine:** install `ca-certificates`, or TLS to the endpoint fails
  (`NO CARRIER`).

### Tools & safety

MU/TH/UR can call tools in a bounded loop (`[tools].max_rounds`, default 8; at
the cap the model is asked for a final answer with tools disabled). Tools are
fixed argv commands; arbitrary shell execution never exists, and unit names are
validated before they reach an argument list.

| Tool | Tier | Does |
|---|---|---|
| `ship_status` | 1 | latest metrics snapshot (uptime, load, CPU, memory, disks, network, top processes, failed units, containers) |
| `service_status` | 1 | `systemctl status` for one unit |
| `journal` | 1 | last N journal lines for one unit (N capped by `journal_max_lines`) |
| `list_units` | 1 | units in state running / failed / inactive |
| `listening_ports` | 1 | listening sockets and their processes |
| `restart_service`, `start_service`, `stop_service` | 2 | the verb on a unit from the `[tools.services]` allowlist; offered to the model only when its list is non-empty |

- **Tier 1** is read-only and runs automatically (5 s timeout).
- **Tier 2** runs only after the operator answers `OVERRIDE REQUIRED -- <ACTION> -- CONFIRM Y/N`, shown in
  reverse video on the status row. Y/N/Esc are ignored until one second after
  the prompt is known to be on screen (the arming delay): the clock starts on
  the second uncovered render showing it (draws wait for the output queue to
  drain, so the first frame has then reached the wire), and it is reset while
  the tab is hidden or covered by the help overlay, a message overlay or the
  prefix strip (`Tab::set_obscured`). A keystroke typed before the prompt
  appeared cannot approve it; Ctrl-C always aborts. Tier-2 commands run via
  `sudo -n` or `doas -n` (`elevate`) with a 30 s timeout. `docs/sudoers.example`
  has the matching sudoers / doas rules, one per verb and unit.
- **Audit log:** every call, run or refused, is appended to
  `$XDG_STATE_HOME/muthur/audit.jsonl` (`~/.local/state/muthur/audit.jsonl`,
  mode 0600) when it is resolved, before its result goes back to the model.
  A running command cannot be cancelled, so Ctrl-C during one writes an
  `aborted` record at once (for a confirmed tier-2 call: with the issued
  `argv` and `OUTCOME UNKNOWN`, shown as `ACCESS INTERRUPTED`), then a second
  record (`confirmed` / `auto`) with the real exit and output when the command
  finishes. If the log cannot be opened at startup or a write fails, tier-2
  calls are refused (`AUDIT LOG OFFLINE`), including at the moment the
  operator confirms.
- **`[tools]` keys:** `enabled` (true), `max_rounds` (8), `elevate`
  (`"sudo"` or `"doas"`), `journal_max_lines` (50), and `[tools.services]`
  `restart` / `start` / `stop` lists. See `docs/config.example.toml`.
- Tool turns are recorded in the chat log, so history redisplays them as
  `ACCESSING: ...` notes and later requests replay them to the model.

### Utility tab

Refresh every 2 s, diffs only, game-style boxed panels:

- CPU overall gauge + per-core mini-bars; rolling CPU/network sparklines
  (VT100-safe glyphs).
- Memory / swap gauges; per-mount disk usage gauges.
- Load average, uptime.
- Failed systemd units (reverse video).
- Top 5 processes.
- Containers, only if a runtime (Docker/Podman) is present.

### Metrics

`MetricsSource` trait consumed by both the Utility tab and MU/TH/UR tools.
v1: `LocalSource` (`sysinfo`, `/proc`, `systemctl`). Later: `InfluxSource`
for multi-server monitoring — tools then get a "which host" parameter.

## Testing & development

- **Stdio link** — runs in any desktop terminal for development.
- **`--strict`** — every byte written is checked against the dialect
  allowlist; violations are logged (or panic in tests).
- **Golden tests** — render a screen state and snapshot the exact bytes sent.
- **`muthur testcard`** on the real WY-60 — confirms which attributes and
  charset features actually work, and records key sequences.

## Milestones

| | Scope |
|---|---|
| **M0** | `link`, `config` skeleton, `muthur testcard` (attribute probe + key echo). Validates hardware first. |
| **M1** | `dialect`, `Wy60Backend`, chrome, tab switching, `--strict`, golden tests. |
| **M2** | Shell tab: PTY, `vt100` translation, scrollback, fullscreen/SIGWINCH, passthrough. |
| **M3** | Utility tab + `LocalSource`. |
| **M4** | MU/TH/UR chat: streaming, persona, reveal, persistent log. |
| **M5** | Tools: tier 1, then tier 2 with confirmation. |
| **M6** | Splash, deploy tooling (udev rule, getty override, fish snippet, `make deploy`), polish. |

## Deferred (post-v1)

- Native `Wyse60Dialect`.
- Hardware (RTS/CTS) flow control.
- Proactive alerts from MU/TH/UR (event bus already accommodates them).
- InfluxDB metrics source / multi-server.
- Multiple chat sessions ("folders").
- Idle lock (v1 relies on the terminal's own screen blanking).
