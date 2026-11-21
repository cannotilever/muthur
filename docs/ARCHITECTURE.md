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
      └─ fish  (conf.d/muthur.fish: if on /dev/wy60 → run `muthur`, NOT exec)
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
  tool-call audit log.

## Modes

1. **Boot splash** — Weyland-Yutani / "MU/TH/UR 6000 — INTERFACE 2037" boot
   text at teletype speed, interleaving real facts (hostname, CPU, RAM, link
   speed). ~3–5 s, skippable with any key.
2. **TUI** — four tabs: MU/TH/UR, Utility, Cargo, Shell.
3. **Emergency passthrough** (`prefix p`) — the Shell tab's PTY output goes to
   the wire **untranslated**. The virtual terminal keeps being fed in the
   background so leaving passthrough repaints the Shell tab correctly. Input
   is still scanned for the prefix key so passthrough can be exited. Doubles
   as a debugging aid for the translator.
4. **Standby** — after `[chrome] standby_minutes` (default 10, `0` = off)
   without a key, a static screen (logo, `MU/TH/UR 6000 ── INTERFACE STANDBY`,
   `STANDBY SINCE <date>`, `PRESS ANY KEY`) replaces the TUI and nothing more is
   written, so the WY-60's own screen saver can blank the CRT (it must be on in
   the terminal's setup). Background work carries on and activity markers
   collect. A key wakes it and is swallowed; a MU/TH/UR tool prompt (Y/N,
   password, OVERRIDE) wakes it with a bell, and keeps it from starting; Break
   wakes it and re-initialises. Never entered during the splash or passthrough.
   See [M9 spec](superpowers/specs/2026-11-20-m9-standby-design.md).
5. **Self-destruct** — armed from the Utility tab: `S` (shutdown) or `B`
   (reboot), then the sudo password. A countdown of `[utility] destruct_seconds`
   (default 60, 10..=5999) takes over the whole screen (no F-keys, no prefix key,
   no standby, no bell) with a five-digit counter and an abort questionnaire;
   answering a question freezes the clock briefly. Esc or Break aborts. At zero
   the host is powered off or rebooted through `sudo -S -k`; a failure shows
   `UNABLE TO COMPLY`. Every outcome is written to the audit log (arming is
   refused with `AUDIT LOG OFFLINE` when it is unavailable). `muthur destruct
   [SECONDS] [--reboot]` plays the countdown without running anything (for
   tuning). See [M10 spec](superpowers/specs/2026-11-20-m10-destruct-design.md).

## Screen layout ("chrome")

Modelled on the Alien: Isolation personal terminal
(`Inspiration/alien-console/img/real-main.jpg`):

```
┌──────────────────────────────────────────────────────────────────────────────┐
│▓USCSS <SHIP> ── MU/TH/UR 6000 ─────────────────────── 17 NOV 2137 14:02▓▓▓▓▓│  title bar (reverse video)
│                                                                              │
│ FOLDERS        ┌───────────────────────────────────────────────────────────┐ │
│┌────────────┐  │                                                           │ │
││ MU/TH/UR  *│  │                                                           │ │  * = activity marker
│└────────────┘  │                                                           │ │
│┌────────────┐  │                                                           │ │
││ UTILITY    │──│                content pane (~60 cols)                    │ │
│└────────────┘  │                                                           │ │
│┌────────────┐  │                                                           │ │
││ CARGO      │  │                                                           │ │
│└────────────┘  │                                                           │ │
│┌────────────┐  │                                                           │ │
││ SHELL      │  │                                                           │ │
│└────────────┘  └───────────────────────────────────────────────────────────┘ │
│ F1-F4 SELECT ..... ^] f FULLSCREEN ..... PGUP/PGDN SCROLL ..... ^] ? HELP    │  footer
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

- **F1 / F2 / F3 / F4** — MU/TH/UR / Utility / Cargo / Shell (sidebar order;
  Utility is shown at startup). F4 is therefore not passed to the shell.
- **Cargo folder:** arrows select, `U`/`R`/`D` start / restart / stop
  (restart and stop ask Y/N), `L` logs, Esc back.
- **Prefix `Ctrl-]`** then:
  - `f` fullscreen toggle · `p` passthrough toggle · `[` scrollback (Shell)
  - `r` force full redraw · `]` send a literal `Ctrl-]` · `?` help
- **Break** — re-initialise after the WY-60 was power-cycled: clear and reset
  the terminal, replay the boot splash (if `[chrome] splash` is on; activity
  markers survive it), then a full repaint. Also leaves passthrough, closes the
  prefix and any overlay, and unsticks output the terminal paused with XOFF
  before it lost power (the input thread flushes and resumes the link's output
  queue, since the event loop may be blocked in a write).
  The re-init waits until the line has been quiet for 500 ms (`BREAK_SETTLE`),
  and nothing is drawn in the meantime: the WY-60 loses what it receives while
  it is still sending the break, and while it powers up it sends line noise and
  echoes our own output back. Input received while waiting is dropped.
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
| `tabs::cargo` | Container manifest and actions: selection, start/restart/stop with Y/N, log view (see below). |
| `tabs::muthur` | Chat UI (see below). |
| `tabs::shell` | PTY + virtual terminal + translation (see below). |
| `llm` | Streaming LLM client (reqwest + rustls/ring, no aws-lc): Anthropic Messages and OpenAI-compatible protocols, SSE parsing, typed `LlmError`s, idle timeout. Each request runs as a tokio task and sends `Event::Llm` to the loop. Deploy note: Alpine needs the `ca-certificates` package for TLS. |
| `chatlog` | Append-only chat history, in memory for the session (epochs, context window/trim, purge); can mirror to JSONL, unused at runtime. |
| `ops` | Always built: `Runner` (fixed-argv execution, timeouts, stdin, kill), JSONL audit log, output `cap`. Shared with the Cargo tab. |
| `tools` | Feature `agent`: per-agent tool registry, fixed tools (`exec`), `run` (`shell`), deny rules (`deny`), network target gate (`netgate`). |
| `review` | Feature `agent`: the reviewer LLM request and verdict. |
| `secret` | Zeroize-on-drop password buffer. |
| `agent` | Feature `agent`: pure tool-turn state machine: round cap, review / confirm / password / override stages, aborts; emits effects the MU/TH/UR tab carries out. |
| `metrics` | `MetricsSource` trait; `LocalSource` for v1. |
| `destruct` | Self-destruct state machine, abort quiz and full-screen renderer; `App` runs the countdown and the sudo command. Also `muthur destruct` preview. See [M10 spec](superpowers/specs/2026-11-20-m10-destruct-design.md). |
| `standby` | The static standby screen (shared logo drawing from `splash::logo`, caption with the date standby began); `App` enters and leaves it. |
| `splash` | Boot splash state machine and renderer (logo, status lines, skip on key) that `App` plays before the TUI; also `muthur splash` preview. See [M6 spec](superpowers/specs/2026-11-19-m6-splash-deploy-design.md). |
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
- Memory: one conversation log per launch, held in memory only, oldest
  turns trimmed to fit the context window; a purge command clears it.
- What the operator typed (`> ` and `$ ` lines) is underlined, the marker excepted.
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
- **vLLM tools:** tool calling (both agents and the reviewer) needs the server started with
  `--enable-auto-tool-choice --tool-call-parser <parser>` (parser depends on the model).
- **Chat log:** in memory only; each launch starts a clean session and
  nothing is written to disk (an old `~/.local/state/muthur/chat.jsonl` from
  earlier versions is ignored). Changing the system prompt, provider or model
  starts a new epoch, so older thinking blocks are not replayed. `/PURGE`
  clears it.
- **Token usage** for each turn (input, cache write/read, output) is logged to
  `muthur.log` (anthropic; the openai stream reports none). Request/response
  bodies and headers are never logged.
- **Alpine:** install `ca-certificates`, or TLS to the endpoint fails
  (`NO CARRIER`).

### Tools & safety

Spec: docs/superpowers/specs/2026-11-19-m7-technical-agent-design.md

Two agents share the MU/TH/UR tab. The **persona** (default) has read-only
tools: `ship_status`, `containers`, `container_inspect`, `container_logs`
(`[tools].max_rounds`, default 8). `/DIRECT` switches to the **technical
agent**, which adds `run` (arbitrary shell command), `ping`, `traceroute`,
`dns_lookup`, `port_check` and `http_check` (`[agent].max_rounds`, default 24);
`/MUTHUR` switches back. Fixed tools use fixed argv with no shell; `run` is
the only path to a shell. Settings live in `[tools]`, `[agent]`,
`[muthur.technical]` and `[muthur.reviewer]` (see `docs/config.example.toml`).

**The agent account.** `run` executes as `muthur-agent`, an unprivileged system
user reached with `sudo -n -u muthur-agent` (no sudo rights, not in `docker` or
`wheel`). nftables blocks all of its non-loopback egress and its DNS to loopback
resolvers (TCP rejected with a reset, the rest dropped, so both fail at once), so a hijacked command cannot exfiltrate over the network; agent
commands also run in their own session, with no controlling terminal, so they
cannot reach the WY-60. With `as_root` the command runs through `sudo -S` as the
operator, who types their password on the WY-60 for every call; MUTHUR never
caches it and it only ever reaches sudo's stdin. Network diagnostics are fixed
tools that run as the operator, behind a target gate: a host must come from the
operator's message or `[agent] net_allow`, or the call always ends at Y/N.
A startup probe (each step limited to 5 s, result fixed for the session)
checks the account and the isolation. Isolated means a UDP datagram to TEST-NET-1
fails with `Operation not permitted`; success, a timeout or `Network is
unreachable` count as not isolated. If either fails, `run` is withheld and the `/DIRECT` banner says why. Files and install steps:
`docs/deploy.md`.

**The gates**, in order; the first refusal ends the call:

1. **Deny rules** (code, final): `USE AS_ROOT`, `CONTROL PLANE`, `LOCKOUT`,
   `DESTRUCTIVE`, plus `[agent] deny` regexes. Not overridable; the reviewer
   never sees the command.
2. **Reviewer:** a second LLM (`[muthur.reviewer]`) returns allow / ask / deny
   from the operator's message, the command, the reason and earlier calls.
   It never sees tool output. If it is unreachable the call goes to `ask`
   (`REVIEWER OFFLINE`), never `allow`.
3. **Operator:** `ask` shows `CONFIRM -- <cmd> -- Y/N`; a root call shows
   `CLEARANCE REQUIRED -- <cmd> -- PASSWORD:` instead; a reviewer `deny` can
   be forced only by typing `OVERRIDE` (audited as `overridden`). All prompts
   are reverse video on the status row. Keys are ignored until one second
   after the prompt is known to be on screen (the arming delay): the clock
   starts on the second uncovered render showing it and resets while the tab
   is hidden or covered (`Tab::set_obscured`), so type-ahead cannot approve
   anything. Ctrl-C always aborts.
4. **Audit log:** every call, run or refused, is appended to
   `~/.local/state/muthur/audit.jsonl` (mode 0600) with the agent, `as_root`,
   reason, deny rule, review verdict and decision; root argv never holds the
   password, and `run` output is kept up to 64 KB. If the log cannot be
   written, root calls and overrides are refused (`AUDIT LOG OFFLINE`).
   A running command cannot be cancelled cleanly, so Ctrl-C kills the sudo
   process; for a root call the audit says `OUTCOME UNKNOWN`.

The model sees at most 8 KB of `run` output (first 4 KB and last 4 KB); fixed
tools are capped at 4 KB. Tool turns are recorded in the chat log, tagged by
agent, and redisplayed as `RUN` lines.

**Delegation.** In persona mode MU/TH/UR also has `delegate(task)`, which hands
a self-contained task to the technical agent as a fresh child turn: the child
sees only the task (an untrusted `DELEGATED TASK` section for the reviewer;
authority still comes only from the operator's own message) and runs through
the same gates, prompts and audit as `/DIRECT`. The screen shows
`DELEGATING TO <LABEL>` and one condensed `<LABEL>: <command>  [<verdict>]` line
per command; with `[tools] delegate_report = "verbatim"` the report is printed
after `<LABEL> REPORTS:`. The persona gets `COMMANDS:` and `REPORT:` as the tool
result and answers in its own voice. The child's records live in their own chat
log lane, and redisplay folds each delegation back into the persona exchange.
Ctrl-C aborts the child and the persona turn. Spec:
docs/superpowers/specs/2026-11-20-m8-delegate-design.md

**The `agent` feature.** All LLM tool use, for both agents, sits behind the
Cargo feature `agent`, on by default. `make AGENT=0 build` (or
`cargo build --no-default-features`) builds MUTHUR as a plain chat: requests
carry no tools, `/DIRECT` is an unknown command, and no code path can run a
command for a model. CARGO's key-driven container actions are not agentic and
stay. `make AGENT=0 install-system` skips the agent-account files. `make check`
runs clippy and tests in both modes.

**Known gap.** Everything the technical agent reads goes to the LLM provider.
For sensitive hosts, point `[muthur.technical]` at a local vLLM. The reviewer
sees commands but no output, so it can stay remote. Loopback services remain
reachable from `muthur-agent`: glibc's nss-resolve (systemd-resolved over
varlink) and any local proxy can carry data out. `docs/deploy.md` says how to
close them.

### Utility tab

Refresh every 2 s, diffs only, game-style boxed panels:

- CPU overall gauge + per-core mini-bars; rolling CPU/network sparklines
  (VT100-safe glyphs).
- Memory / swap gauges; per-mount disk usage gauges.
- Load average, uptime.
- Failed systemd units (reverse video).
- Top 5 processes.
- Keys: `S` arms a self-destruct shutdown, `B` a reboot (see Self-destruct above).
- Container count (running and stopped), one line, only if a runtime (Docker/Podman) is present;
  the list itself is in the Cargo tab, so a busy host cannot push the other
  panels off the screen.

### Cargo tab

The container manifest: `CARGO - N CONTAINERS`, a NAME / STATUS row per
container, stopped and crashed ones included (`ps --all`; names cut to fit;
any status other than `UP` in reverse video and sorted to the top, so a
dead container is never scrolled out of sight).
Fed by the same snapshots as Utility. Says `NO CONTAINER RUNTIME` or
`HOLD EMPTY` when there is nothing to list.

- **Selection** — the arrows move a `>` + underline cursor (Shift-F15/F16 a
  page, Home to the top); the list scrolls to keep it visible and the title
  shows the visible range. The cursor follows the container ID across
  re-sorts. Below the list: the selected container's full status
  (`DB: EXITED (137) 2 HOURS AGO`) or a message, then the key hint.
- **Actions** — `U` start, `R` restart, `D` stop, either case, on the
  selected container by ID (`<docker|podman> <verb> <id>`, argv only, 30 s
  timeout, no elevation: the account is in the `docker` group). Restart and
  stop ask `<VERB> <NAME> (<ID>)? Y/N` (a long name is cut in the middle so
  the question and ID always show), armed like MU/TH/UR's override prompt (on
  screen uncovered for `ARMING`, 1 s); N or Esc cancels. One command at a
  time (`BUSY`); progress, result and errors (reverse video) show on the
  message line, and a `ps --all` refresh follows each action. STATUS is a
  fixed 10 columns, so an in-flight `RESTARTING` never shifts the rows. If a
  probe comes back empty-handed (daemon busy), the last list stays and the
  message line says `RUNTIME NOT ANSWERING`.
- **Logs** — `L` fetches `logs --tail 200` (stdout and stderr merged, 10 s)
  into a full-pane view: escape sequences and control characters stripped,
  lines wrapped (newest 64 KiB kept), opened at the newest line, scrollable
  with scroll hints so a line scroll does not repaint the pane; `L`
  refetches, Esc returns to the list.
- **Audit** — every command goes into `audit.jsonl` (shared with MU/TH/UR's
  tools) as `cargo_start`/`cargo_restart`/`cargo_stop`/`cargo_logs`; restart
  and stop are `confirmed`, start and logs `auto`. As with
  root agent calls, restart and stop are refused while the audit log
  is offline (`AUDIT LOG OFFLINE`); a failed write is shown on the message line.
  A log fetch closed with Esc is still audited when it finishes, and commands
  still running when muthur quits are recorded as `aborted`.

Spec: docs/superpowers/specs/2026-11-19-cargo-actions-design.md

### Metrics

`MetricsSource` trait consumed by the Utility and Cargo tabs and MU/TH/UR tools.
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
| **M5** | Tools: tier 1, then tier 2 with confirmation (replaced in M7). |
| **M6** | Splash, deploy tooling (udev rule, getty override, fish snippet, make deploy). Spec: docs/superpowers/specs/2026-11-19-m6-splash-deploy-design.md |
| **M7** | Technical agent: `/DIRECT`, `run` as `muthur-agent` or root, reviewer, network isolation, `agent` feature. Spec: docs/superpowers/specs/2026-11-19-m7-technical-agent-design.md |
| **M8** | Persona `delegate(task)`: MU/TH/UR hands self-contained tasks to the technical agent; relayed or verbatim report. Spec: docs/superpowers/specs/2026-11-20-m8-delegate-design.md |
| **M9** | Chat UI: operator lines underlined; chat history in memory only, clean per launch. |
| **M10** | Self-destruct: armed from Utility (`S`/`B`), full-screen countdown with abort quiz, sudo shutdown/reboot, audit; `muthur destruct` preview. Spec: docs/superpowers/specs/2026-11-20-m10-destruct-design.md |

## Deferred (post-v1)

- Native `Wyse60Dialect`.
- Hardware (RTS/CTS) flow control.
- Proactive alerts from MU/TH/UR (event bus already accommodates them).
- InfluxDB metrics source / multi-server.
- Persistent chat history, multiple chat sessions ("folders").
- Idle lock (v1 relies on the terminal's own screen blanking).
- Cargo: dedicated agent tools that act on containers (M7 agents only read them, or use `run`); live log following;
  remove / pull / exec; acting on several containers at once; a sidebar
  marker when a container dies on its own.
