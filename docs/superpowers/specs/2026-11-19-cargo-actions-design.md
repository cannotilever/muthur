# CARGO — Container Actions and Log View: Design

**Status:** approved in brainstorming on 2026-11-19. Builds on the CARGO folder on the `cargo-tab` branch: F3, a read-only container list from `ps --all`, with anything not `UP` sorted first and shown in reverse video. That part is implemented and deployed to omicron, but not yet committed.

## Goal

From the CARGO folder the operator can select a container with the arrow keys and start, restart or stop it, or read its recent logs, without leaving the WY-60 or asking MU/TH/UR. The account muthur runs under is in the `docker` group, so no command needs elevation.

## Decisions

| Topic | Decision |
|---|---|
| Selecting | Arrow keys move a selection; Shift-F15/F16 move it a page; Home goes to the top. The list scrolls to keep the selection visible. |
| Triggering | Direct letter keys, either case: **U** start, **R** restart, **D** stop, **L** logs. A hint line lists them. |
| Confirmation | Restart and stop ask `<VERB> <NAME>? Y/N`, armed like MU/TH/UR's override prompt. Start and logs run at once. |
| Log view | The last 200 lines, fetched once per **L**. Full pane, wrapped, scrollable, opens at the bottom. **L** fetches again, Esc goes back. No live follow. |
| How commands run | Approach A: an injected `run` dependency, like `MuthurDeps::run_tool`. A blocking task runs `SystemRunner` and the result comes back as `Event::CargoDone`. |
| Command target | The container ID, never the display name. |
| Runtime | Whichever runtime produced the snapshot (`docker` or `podman`). |
| Audit | Every CARGO command goes into the existing `audit.jsonl`, sharing MU/TH/UR's `AuditLog`. |
| Concurrency | One CARGO command at a time. |

## Probe and data changes (`metrics`)

- **Format:** `ps --all --format '{{.ID}}\t{{.Names}}\t{{.Status}}'`.
- **`Container`** gains:
  - `id: String`, the short ID;
  - `detail: String`, the full status text, e.g. `Exited (137) 2 hours ago`.
- **`name`** keeps the runtime's case; it is no longer upper-cased in `parse_containers`. Display sites upper-case it: CARGO, UTILITY (count only, so no change) and the `ship_status` tool text. What MU/TH/UR and the screen show stays the same.
- **`status`** stays the upper-cased first word of the status (`UP`, `EXITED`, `CREATED`, `RESTARTING`, …). Sorting and highlighting use it.
- **`Snapshot`** gains `container_runtime: Option<String>`: the command that answered (`"docker"` / `"podman"`), or `None` when `containers` is `None`.
- A line with fewer than three tab-separated fields is skipped. It cannot be acted on.

## The CARGO list screen

```
┌ CARGO - 50 CONTAINERS (1-35) ─────────────────────────────┐
│ NAME                                                STATUS│
│ REFINERY-UNIT-03                                    EXITED│   trouble: reverse video
│>REFINERY-UNIT-00                                    UP    │   selected: '>' + underline
│ REFINERY-UNIT-01                                    UP    │
│ ...                                                       │
│ REFINERY-UNIT-00: UP 3 HOURS                              │   detail / message line
│ U START  R RESTART  D STOP  L LOGS                        │   hint line
└───────────────────────────────────────────────────────────┘
```

- **Gutter:** one column for the `>` marker. The name column shrinks by one.
- **Selected row:** `>` in the gutter and the whole row underlined. Reverse video stays reserved for trouble statuses. The WY-60 renders underline (`docs/hardware.md`).
- **Rows:** the two bottom lines take two rows from the list, leaving 35 rows windowed and 38 fullscreen.
- **Selection identity:** the selection is a container ID. After a snapshot re-sorts the list, it stays on the same container. If that container is gone, the selection keeps its row index, clamped to the list.
- **Detail / message line:** by default it shows `<NAME>: <DETAIL>` for the selected container, upper-cased. A message replaces it while one is active:
  - **Prompt:** `STOP REFINERY-UNIT-00? Y/N`.
  - **Progress:** `RESTARTING REFINERY-UNIT-00 ...`.
  - **Result:** `REFINERY-UNIT-00 RESTARTED` / `STARTED` / `STOPPED`.
  - **Error** (reverse video): see Errors.

  Results and errors stay until the next key.
- **In-flight row:** the selected container's STATUS column shows `STARTING…` / `RESTARTING…` / `STOPPING…`, written with ASCII `...` to stay inside the dialect.
- **Hint line:** always `U START  R RESTART  D STOP  L LOGS`, or `NO CONTAINER RUNTIME` when `container_runtime` is `None`.
- **No containers:** when the list is empty (`HOLD EMPTY`), the action keys do nothing.

## Commands

| Key | argv | Timeout | Confirm | Merged output |
|---|---|---|---|---|
| U | `<rt> start <id>` | 30 s | no | no |
| R | `<rt> restart <id>` | 30 s | yes | no |
| D | `<rt> stop <id>` | 30 s | yes | no |
| L | `<rt> logs --tail 200 <id>` | 10 s | no | yes |
| (after U/R/D) | `<rt> ps --all --format <CONTAINER_FORMAT>` | 2 s | no | no |

- `<rt>` is `Snapshot::container_runtime` from the latest snapshot. The `ps` format string is the probe's constant, shared rather than copied.
- After a start, restart or stop completes, successfully or not, CARGO runs the `ps` refresh. It shows that list (parsed and sorted like a snapshot) until the next snapshot replaces it. A failed refresh is ignored silently, because the next snapshot comes within 10 s anyway.

## Confirmation

- **Mirrors `MuthurTab`'s override prompt.** The prompt arms only after a render that shows it unobscured, plus the arming delay. The 1 s `ARMING` constant moves from `muthur::turn` to somewhere both tabs can use, keeping its value.
- **Keys:**
  - Y before arming is ignored.
  - N or Esc cancels at any time.
  - Any other key is ignored while the prompt is up.
- **Obscured:** `set_obscured(true)` (another folder, an overlay, the prefix strip) clears the arming. It re-arms after the next unobscured render plus the delay. Leaving the folder does not cancel the prompt.
- **Pinned target:** the prompt stores the target's ID and display name when it is raised. If a snapshot arrives without that ID, the prompt is dropped and the message line shows `<NAME> GONE`.

## Running commands (approach A)

```rust
pub struct CargoDeps {
    /// Start `argv` in the background; its result comes back as
    /// `Tab::on_cargo_done(seq, output, ms)`.
    pub run: Box<dyn FnMut(u64, Vec<String>, Duration, bool)>,
    pub now: Box<dyn Fn() -> Instant>,
    pub timestamp: Box<dyn Fn() -> String>,
}
```

- **Production `run`:** wrapped in `tokio::task::spawn_blocking`. It calls `SystemRunner` (merged or not, per the flag) and sends `Event::CargoDone { seq, output: RunOutput, ms }`.
- **Routing:** the run loop calls `App::on_cargo_done`, which calls `Tab::on_cargo_done` on the CARGO tab. The new trait method defaults to doing nothing, like the other per-tab hooks.
- **`seq`:** increases with every command CARGO starts. At most two can be outstanding: one action (U/R/D/L) and one `ps` refresh. CARGO keeps a pending `seq` for each and drops any result matching neither. Esc out of the log view clears the pending log `seq`. A snapshot that arrives clears the pending refresh `seq`, because it is at least as fresh.
- **Busy:** while an action (U/R/D/L) is in flight, the action keys show `BUSY` on the message line. Arrows, paging and Esc still work. The `ps` refresh does not count as busy.
- **Activity:** if a U/R/D/L result arrives while the tab is obscured, `take_activity` returns true once, and the sidebar shows `*` on CARGO.

### `SystemRunner` merged mode

`docker logs` sends the container's stderr to its own stderr, so separate pipes lose the order. `Runner` gains a merged variant: stdout and stderr go to one pipe, and the result comes back as `Exited { stdout: <both>, stderr: "" }`. Timeout and process-group kill behave as in the existing path. MU/TH/UR's tools keep using the separate-pipe path.

## Log view

- **Layout:** replaces the list in the same area. The title is `LOGS - <NAME> (<N> LINES)`, the name upper-cased; the text keeps its original case.
- **Cleaning:** before wrapping, the text is cleaned:
  - ANSI CSI and OSC sequences are stripped;
  - `\r\n` becomes `\n`, and a lone `\r` is dropped;
  - tabs expand to the next multiple of 8;
  - other C0/C1 control characters are dropped.

  Non-ASCII characters are left for the backend, which maps anything the dialect lacks to `?`.
- **Wrapping and scrolling:** lines wrap at the inner width by characters. The view opens scrolled to the bottom. Up/Down move one row, Shift-F15/F16 a page, Home goes to the top.
- **Keys:** **L** fetches again and keeps the view at the bottom. **Esc** returns to the list with the same selection. U/R/D do nothing in the log view.
- **Bottom line:** the last inner row is the hint `L REFRESH  ESC BACK  SH-F15/16 SCROLL`.
- **Empty logs:** an empty result shows `NO LOG OUTPUT`.

## Audit

- **One writer:** `MuthurTab` and `CargoTab` share one `AuditLog` as `Rc<RefCell<AuditLog>>`. The app runs on a single thread, and a single writer keeps `needs_newline` handling correct.
- **Entries:** `AuditEntry` is written when the result arrives:

  | Field | Value |
  |---|---|
  | `tool` | `cargo_start` / `cargo_restart` / `cargo_stop` / `cargo_logs` |
  | `args` | `{"id": ..., "name": ...}` |
  | `tier` | 2 for restart and stop; 1 for start and logs |
  | `decision` | `confirmed` for restart and stop; `auto` for start and logs |
  | `argv` | the command line run |
  | `exit` | same shape as MU/TH/UR tool calls |
  | `ms` | duration |
  | `output` | trimmed with `tools::cap` |
- **Not logged:** cancelled prompts and the `ps` refresh.

## Errors

All errors appear on the message line in reverse video until the next key.

| Situation | Message |
|---|---|
| Non-zero exit | `<VERB> FAILED: <first non-empty line of stderr (or stdout), upper-cased, cut to fit>` |
| `RunOutput::NotFound` | `RUNTIME NOT FOUND` |
| `RunOutput::TimedOut` | `<VERB> TIMED OUT` |
| Logs fail | Stays on the list; `LOGS FAILED: …` as above |
| No runtime in snapshot | Action keys do nothing; the hint line says `NO CONTAINER RUNTIME` |
| Telemetry offline | The last list stays and actions still work. The message line shows `TELEMETRY OFFLINE` while no message is active. Before any list has arrived, the screen is the existing `TELEMETRY OFFLINE` panel. |

## Wiring (`run.rs`, `app`)

- **`Event::CargoDone { seq, output, ms }`:** the run loop passes it to `App::on_cargo_done`.
- **Audit log:** `build_muthur` takes the shared audit log instead of opening its own. `run()` opens it once and hands clones of the `Rc` to both tabs.
- **`App::assemble`:** its signature is unchanged. `CargoTab::new(deps, audit)` is built in `run()`, and tests build it with `CargoTab::detached()`, which has no-op deps and an offline audit log.

## Testing

- **Probe:** parse ID, name (case kept) and full status; skip short lines; `ship_status` text still shows names upper-cased; `container_runtime` is set by the source that answered.
- **CARGO list (fake `run`):**
  - each key sends the right argv for either letter case, using ID and runtime;
  - U runs at once;
  - R and D raise a prompt, Y before arming is ignored, Y after an unobscured render plus the delay runs it;
  - N and Esc cancel;
  - an obscured render clears the arming;
  - a vanished target gives `GONE`;
  - `BUSY`;
  - each error message;
  - the `ps` refresh after an action and its result replacing the list;
  - a stale `seq` is ignored;
  - the selection follows the ID across re-sort, and clamps when the container is gone;
  - the selection stays visible when paging.
- **Log view:** wrapping, opening at the bottom, Up/Down/page/Home, L refetch, Esc back with the selection kept, `NO LOG OUTPUT`, cleaning (ANSI, CR, tabs, control characters), merged flag set only for logs.
- **`SystemRunner` merged mode:** `sh -c 'echo a; echo b >&2; echo c'` comes back as `a\nb\nc\n` in order; timeout still kills.
- **App:** `CargoDone` reaches CARGO; a background completion lights CARGO's `*`.
- **Audit:** an entry per action with the right tool, tier and decision; nothing for a cancel; MU/TH/UR and CARGO entries land in one file through the shared log.
- **Golden screens:** windowed list with selection, detail and hint lines; a raised stop prompt; the log view with wrapped lines. All are checked through `StrictWriter`.

## Out of scope

- MU/TH/UR tools that act on containers.
- Live log following.
- Remove, pull, exec.
- Acting on several containers at once.
- A sidebar marker when a container dies on its own.
