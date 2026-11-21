# M10 — Self-Destruct Countdown: Design

**Status:** approved in brainstorming on 2026-11-20.

## Goal

Eye candy. From the Utility tab the operator starts a timed **shutdown** or
**reboot** of the host. The whole terminal turns into a countdown modelled on
the Nostromo's self-destruct screens (`Inspiration/destruct.mp4`): a wide
octagon, a pulsing double-lined X, and a boxed five-digit counter. At zero the
host really powers off or reboots.

As an homage to *Alien* and *Galaxy Quest*, the screen offers a comically long
abort questionnaire. Answering any of its questions secretly saves the ship:
the clock stops at `00001`. Esc aborts silently at any time.

## The reference

Taken from the video's frames:

- A wide octagon outline (about 4:3), centred.
- A large X inside it. Each arm is a pair of parallel lines. The pairs
  converge into single lines, flash bright (glow), fade out to an empty
  octagon, then reappear wide apart. The cycle repeats every couple of seconds.
- A boxed counter `1 0 0 8 6` on the octagon's left edge at the X's height,
  one digit per box: `MM SS t` (minutes, seconds, tenths), minutes and tenths
  orange, seconds white. It counts down in real time.

## Decisions

| Topic | Decision |
|---|---|
| Privilege | `CLEARANCE REQUIRED` password prompt, as the agent's `as_root`. Checked at once with `sudo -S -k true`; held zeroized during the countdown; used at zero for `sudo -S systemctl poweroff\|reboot`. No new system configuration. |
| Duration | `[utility] destruct_seconds = 60` default, valid 10–5999. Adjusted at arming with ←/→ through 10 s, 30 s, 1 m, 2 m, 5 m, 10 m (the configured value is the start point and is included in the steps if it is not one of them). |
| Counter | Real time remaining as `MMSSt`, shown as `ceil(remaining / 100 ms)`. |
| Esc | Silent, immediate abort at any time once armed. No message. |
| Abort questionnaire | Any submitted answer, right or wrong, counts. The first submitted answer really aborts the operation: the password is zeroized on the spot and nothing can execute any more. The countdown carries on for the bit and freezes at `00001`. |
| Exclusivity | Once armed: no F1–F4, no prefix key, no standby, no bell. |
| Break | Silent abort, then the usual Break re-init. |
| Structure | A `destruct` module shaped like `standby` and `splash`: `App` holds `Option<Destruct>` and renders it in place of the chrome and tabs. |
| Preview | `muthur destruct [SECONDS]` plays the screen with no password and no `systemctl`. |

## Behaviour

### Arming (Utility tab)

All arming prompts use the Utility tab's bottom row, in reverse video like
Cargo's prompts.

1. `S` (shutdown) or `B` (reboot), either case, shows
   `SHUTDOWN IN 01:00  <-/-> ADJUST  ENTER CONFIRM  ESC CANCEL`
   (`REBOOT IN …` for reboot). ←/→ step the duration; Enter confirms;
   Esc cancels.
2. Enter shows `CLEARANCE REQUIRED -- SHUTDOWN IN 01:00 -- PASSWORD:`. Keys
   are ignored until it has been on screen, uncovered, for `ARMING` (1 s), as
   with the existing prompts. Typed characters are not echoed; Backspace
   edits, Esc cancels and zeroizes, Enter submits. An empty Enter does nothing.
3. If the audit log is offline the request is refused with `AUDIT LOG OFFLINE`
   before the password prompt, as root agent calls and Cargo restart/stop are.
4. On submit the row shows `VERIFYING` and `sudo -S -k true` runs with the
   password on stdin (10 s timeout). On failure: `CLEARANCE DENIED` on the row
   (cleared by the next key) and the password is zeroized. On success the tab
   hands a `DestructRequest { kind, duration, password }` to `App`, and the
   countdown starts from that moment.

Arming keys only reach the Utility tab when it is focused, so S and B mean
nothing on other tabs. While a prompt is open, F1–F4 still switch tabs
(cancelling and zeroizing the prompt).

### The countdown

- `App` forces a full clear (`app.redraw`) and from then on draws only the
  destruct screen. The prefix and any overlay are closed.
- Locked out: F1–F4, the prefix key, standby (`poll_standby` does nothing and
  the idle countdown restarts when the destruct screen ends), the bell. Every
  tab is obscured, as behind the splash. Background work carries on.
- Keys go to the abort questionnaire (below), except Esc and Break.
- The run loop wakes at least every 100 ms while the countdown runs
  (`App::tick_interval`).

### Endings

- **Esc:** silent abort. The password is zeroized, the destruct screen ends,
  the Utility tab repaints. Audit outcome `aborted`. Esc works in every phase,
  including the freeze and the failure screen.
- **Break:** as Esc, then the usual Break handling (settle, re-init, splash
  replay if enabled).
- **Freeze:** if at least one answer has been submitted, the first tick with
  ≤ 100 ms remaining freezes the countdown: the counter holds at `00001`, the X
  stays in its current phase. (The password was already zeroized when the first
  answer was submitted.) After 1.5 s
  the prompt row shows `SELF-DESTRUCT ABORTED`; 4 s after that the Utility tab
  returns. Any key during the freeze is ignored except Esc, which returns at
  once. Audit outcome `frozen`.
- **Execute:** if no answer has been submitted, the tick at zero shows `00000`
  and runs `sudo -S systemctl poweroff` (or `reboot`) with the password on
  stdin (30 s timeout); the password is zeroized once it is written. The screen
  holds on `00000` while the host goes down; keys are ignored. Audit outcome
  `executed`.
- **Failure:** if that command fails or times out, the prompt row shows
  `UNABLE TO COMPLY -- <reason>` (reason cut to fit). Any key returns to the
  Utility tab. Audit outcome `failed`.
- **Quit mid-countdown** (MUTHUR exits): audit outcome `aborted`.

### Audit

One `audit.jsonl` entry per run, tool `destruct_shutdown` or
`destruct_reboot`, args `{ "seconds": N }`, written when the run ends with the
outcome above (`executed` is written before the command runs). The password
never appears in argv or the log.

## The screen

80×43, no chrome. Attributes only (the WY-60 has no colour): the octagon and
frame normal, the X normal / bold / dim by phase.

```
row 0    EMERGENCY DESTRUCT SYSTEM ── SHUTDOWN
rows ~3–37   the octagon (about 64 columns × 34 rows), centred
row 41/42    the questionnaire strip (reverse video)
```

- **Octagon:** horizontal edges DEC line drawing `q`, vertical edges `x`,
  the four cut corners `/` and `\`.
- **The X:** two crossing diagonals through the octagon's centre, each arm
  drawn as two parallel staircases of `\` or `/` at about 1.5 columns per row
  (close to 45° on the WY-60's tall cells), clipped to the octagon's inside.
- **Counter:** a DECDHL double-height pair of rows at the X's centre, docked
  on the octagon's left edge. Double-height rows are also double width; the
  digit row is `x1x0x0x8x6x` in double-size characters, with normal-size frame
  rows above (`lqwqwqwqwqk`-style, `w` on the even columns where the
  double-width dividers fall) and below (`mqvqvq…j`). Minutes and tenths are
  normal, seconds are **bold**. The octagon edge and the X segments on the two
  double-height rows are drawn at double-width resolution. Uses the existing
  line-attribute path (`App::line_attrs`, as the splash and standby do).
- **Questionnaire strip:** the question (or the current retort) on the left,
  the answer being typed on the right with the cursor after it.

### Animation

The phase is a pure function of time since arming. The cycle is 3 s:

| Time in cycle | X |
|---|---|
| 0.0–0.5 s | arms wide apart (gap about 6 columns), normal |
| 0.5–1.0 s | gap about 4 |
| 1.0–1.5 s | gap about 2 |
| 1.5–2.0 s | merged into single lines, **bold** (the glow) |
| 2.0–2.5 s | single lines, dim |
| 2.5–3.0 s | blank (empty octagon) |

Both the phase and the counter come from the clock, never from a frame count,
so a slowed line (XON/XOFF) skips frames without the clock drifting.

**Byte budget** (38400 baud, about 3.8 KB/s): a convergence step is about
1.2 KB, a bold, dim or blank change about 0.7 KB, reappearing wide about
1.4 KB, the tenths digit about 30 B ten times a second. That averages about
2.5 KB/s. A golden test holds the largest single step under a fixed budget.

## The abort questionnaire

An ordered list of steps; each has a question, a check (right or wrong) and a
wrong-answer retort. A right answer moves to the next step. A wrong answer
shows its retort for 1.5 s, then the same question again; typing carries on
meanwhile. Only a non-empty Enter submits; every submission sets `answered`. The first
submission zeroizes the password (`password` becomes `None`), so from then on
the run cannot reach `Execute`.
Answers are capped at 30 characters, shown as typed (not uppercased);
Backspace edits, Ctrl-C clears the answer.

| # | Question | Right | Wrong retort |
|---|---|---|---|
| 1 | `TO ABORT, TYPE ABORT` | `abort`, any case | `INPUT NOT RECOGNIZED` |
| 2 | `CONFIRM: TYPE troba` | exactly `troba` | `LOWERCASE.` for `troba` in another case, otherwise `INCORRECT.` |
| 3 | `SOLVE: <a> x <b> - <c> =` (a, b 11–29, c 1–9, random per run; tests inject the seed) | the integer result | `INCORRECT. RECALCULATING.`, then a new problem |
| 4 | `WHAT IS THE AIRSPEED VELOCITY OF AN UNLADEN SWALLOW?` | contains `african` or `european`, or the number `24` or `11` (any unit); right answers get `HUH? I... I DON'T KNOW THAT.` for 1.5 s before step 5 | `INCORRECT. AFRICAN OR EUROPEAN?` |
| 5 | `ENTER SPECIAL ORDER 937 AUTHORIZATION:` | never | cycles `CREW EXPENDABLE.`, `PRIORITY ONE.`, `ALL OTHER CONSIDERATIONS SECONDARY.`; after the third, moves to step 6 |
| 6 | `ACTIVATE OMEGA 13? Y/N` | any answer: `OMEGA 13 OFFLINE.` for 1.5 s, then step 7 | — |
| 7 | `PLEASE HOLD. YOUR ABORT IS IMPORTANT TO US.` | never | `PLEASE CONTINUE TO HOLD.`, forever |

## Components

- **`src/destruct/`** (always built):
  - `mod.rs` — `Destruct { kind, started, total, quiz, answered, phase, password }`
    with `phase: Running | Frozen { since } | Executing | Failed(String)`.
    `tick(now) -> Option<Effect>` and `key(&Key, now) -> Option<Effect>`;
    `Effect::Execute` (the run loop runs the command) and
    `Effect::Done(Outcome)` (`App` ends the screen and audits).
    `counter(now) -> [u8; 5]`. Preview mode never emits `Execute`.
  - `quiz.rs` — the steps, checks, retorts and the seeded arithmetic.
  - `screen.rs` — octagon, X phases, counter rows, strip; `render` and
    `line_attrs`.
- **`src/secret.rs`** — no longer behind the `agent` feature.
- **Utility tab** — the arming prompts; `UtilityDeps` (injected like
  `CargoDeps`): `run(seq, argv, timeout, stdin: Option<Secret>)`, `now`,
  `audit_online`. `Tab::take_destruct() -> Option<DestructRequest>` (default
  `None`), polled by `App` after key handling.
- **`App`** — `Option<Destruct>`, entering on a request, routing keys
  (Esc and Break first), lockouts, `tick_interval` ≤ 100 ms while active,
  `line_attrs`, the audit entry on `Done`.
- **Run loop** — the verify and execute commands through `ops::Runner` with
  stdin, results as events back to the Utility tab (verify) or `App`
  (execute).
- **Config** — `[utility] destruct_seconds`, validated 10–5999.
- **`muthur destruct [SECONDS]`** — preview subcommand (default 60).
- **Docs** — ARCHITECTURE.md (modes, keys, Utility tab, modules, milestones),
  `docs/config.example.toml`.

## Testing

- **Unit:** counter formatting and the ceiling; freeze vs. execute at the
  100 ms boundary (including an answer submitted inside the last 100 ms); Esc
  and Break in every phase; the password zeroized on every ending and on the
  first submitted answer (no `Execute` possible afterwards); each quiz
  check and retort; retort timing; the arithmetic seed.
- **Golden (strict allowlist):** one frame per animation phase, the frozen
  screen, the failure screen, the counter's double-height rows; the largest
  frame-to-frame step under the byte budget.
- **App:** F1–F4, the prefix and standby locked out while active; Break aborts
  then re-inits; the audit entry for each outcome.
- **Utility:** arming flow with a fake runner (right and wrong password,
  `AUDIT LOG OFFLINE`, the arming delay, Esc cancelling at each stage, F-key
  cancelling).
- **By hand (operator):** a real poweroff and a real reboot; `muthur destruct`
  on the WY-60 to tune the look.

## Out of scope

- Voice or sound beyond the existing bell (and the bell is silenced).
- Scheduling for a wall-clock time, or a countdown that survives a MUTHUR
  restart.
- Starting the countdown from MU/TH/UR or the agents.
