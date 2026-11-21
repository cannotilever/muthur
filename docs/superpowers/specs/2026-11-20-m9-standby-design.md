# M9 — Standby Screen: Design

**Status:** approved in brainstorming on 2026-11-20.

## Goal

The WY-60's built-in screen saver never triggers while MUTHUR runs, because
MUTHUR writes to the line all the time (the title-bar clock, the 2 s Utility
refresh). After a configurable time without a key, MUTHUR shows a static
standby screen and then writes **nothing**, so the terminal's own screen saver
can blank the CRT. A key brings the TUI back.

This replaces the earlier decision to have no screen saver or attract mode.

## Decisions

| Topic | Decision |
|---|---|
| Config | `[chrome] standby_minutes = 10`. `0` disables standby. On by default. |
| What counts as idle | No key for `standby_minutes`. Background activity never resets the countdown. |
| What wakes it | A key; an operator prompt in the MU/TH/UR tab (`Ask`, `Password`, `OverridePrompt`), which also rings the bell; Break. Nothing else. |
| The wake key | Consumed whole (the batch it arrived in), never passed to a tab or the prefix machine. |
| Standby screen | The splash logo, centred vertically, plus a three-line caption that includes the time standby began. Fixed once drawn. |
| Structure | A `standby` module shaped like `splash`: `App` holds `Option<Standby>` and renders it in place of the tabs. The backend diff makes a static frame cost zero bytes. |

## Behaviour

- **Entering standby.** `App::poll_standby` runs on every loop wake-up (the
  1 s tick at the latest). Standby begins when all of these hold:
  - `standby_minutes > 0`, and `standby_minutes` have passed since the last key
    batch (or since startup if no key has been pressed yet);
  - no splash is playing, passthrough is off, and the line is not settling after Break;
  - the MU/TH/UR tab is not showing an operator prompt (a prompt keeps the TUI awake).
- **The screen.** Entering standby forces a full clear (`app.redraw`), then the frame is:
  - the logo from `assets/wy-logo.txt`, including the double-height header,
    centred vertically on the 80×43 screen;
  - one blank row under the logo, then three centred lines:
    ```
    MU/TH/UR 6000 ── INTERFACE STANDBY
    STANDBY SINCE 17 NOV 2137 14:02
    PRESS ANY KEY
    ```
    The date uses `chrome::format_date` with the same year offset as the title
    bar (the real date if `show_real_date` is on), taken once when standby begins.
  - No title bar, sidebar, footer, prefix strip or overlay. The prefix and
    overlay are closed when standby begins.
- **While asleep.**
  - Every tab is obscured, as behind the splash. Background work carries on:
    metrics, Shell output into the virtual terminal, LLM streams, tool calls
    and Cargo actions. Activity markers keep updating and are seen on wake.
  - The frame is identical on every render, so the backend sends nothing.
  - The bell is suppressed (`take_bell` stays false), except when a prompt wakes the screen.
- **Waking up.**
  - **A key.** The whole batch is dropped, standby ends, the last-key time is
    reset, and `app.redraw` forces a full repaint of the current tab
    (about 1.2 s). A `^]` in that batch does not open the prefix.
  - **An operator prompt.** When the MU/TH/UR tab starts prompting
    (`Ask`, `Password` or `OverridePrompt`), standby ends, the bell rings and
    the screen repaints. The current tab is kept; if it is not MU/TH/UR, the
    activity marker points there, as it does today. A prompt arms only one
    second after its first render, so it cannot be answered by accident.
  - **Break.** Ends standby, then the usual Break handling (settle, re-init,
    splash replay if enabled).
- **Passthrough** never enters standby; MUTHUR does not control that output.

## Components

- **`src/splash/logo.rs`:** gains `pub fn draw(logo: &Logo, area: Rect, top: i32, buf: &mut Buffer)`,
  the logo-drawing loop moved out of `Splash::render`, plus the line-attribute
  helper for its double-height rows. `Splash` uses it unchanged in behaviour
  (the splash goldens must still pass byte for byte).
- **`src/standby.rs`:** `Standby { since: String }` (the formatted date),
  `render(&self, area, buf)` and `line_attrs(&self, area)`. No state changes
  after construction.
- **`App`** (`src/app/mod.rs`):
  - fields `last_key: Instant`, `standby: Option<Standby>`, and the timeout
    (`Option<Duration>`, `None` when `standby_minutes = 0`), from `ChromeConfig`;
  - `poll_standby(now: Instant, wall: NaiveDateTime)` enters standby or wakes
    it for a prompt;
  - `handle_keys_at`: Break first (also clears standby), then settling, then
    **standby (swallow batch, wake)**, then splash, then normal keys. Every
    batch that reaches this point updates `last_key`;
  - `render` and `line_attrs` draw the standby frame while it is active;
  - `standby_active()` for tests.
- **MU/TH/UR tab:** `awaiting_operator() -> bool`, true while `State::prompting()`,
  reached from `App` through the `Tab` trait in `src/app/tabs.rs` (default `false`).
- **`run.rs`:** calls `app.poll_standby(Instant::now(), clock())` next to `poll_settle`.
- **Config:** `ChromeConfig.standby_minutes: u32`, default `10`, in `src/config.rs`.

## Docs

- `docs/config.example.toml`: `standby_minutes` under `[chrome]`, with a
  comment that the WY-60's own screen saver must be on.
- `docs/ARCHITECTURE.md`: a **Standby** entry under Modes, and `standby` in the module table.
- `docs/hardware.md` and `docs/deploy.md`: enable the WY-60 screen saver in its
  setup menu; otherwise the standby logo stays lit on the CRT indefinitely.

## Testing

Time is injected, as in the splash and Break tests. No test sleeps.

- **App unit tests:**
  - standby begins at the timeout and not before; a key batch resets the countdown;
  - `standby_minutes = 0` never enters standby;
  - no standby during the splash, passthrough or Break settling, or while a prompt is showing;
  - the waking batch is swallowed (no character reaches the tab, `^]` opens no prefix) and sets `redraw`;
  - a prompt arriving while asleep wakes it and rings the bell;
  - Break while asleep clears standby and starts settling;
  - no bell while asleep otherwise.
- **Render test:** the standby frame (logo position, caption text and date)
  against a golden buffer; the `STANDBY SINCE` date honours the year offset.
- **Wire test** (backend + `StrictWriter`, as the existing goldens): after the
  first standby frame, sixty further renders with a changing clock, new
  metrics and PTY output write **0 bytes**.
- **Splash goldens** still pass after the logo-drawing refactor.

## Out of scope

- Animated attract modes, or anything that writes while asleep.
- Waking on output, replies or Cargo results.
- Switching to the MU/TH/UR tab on a prompt wake.
