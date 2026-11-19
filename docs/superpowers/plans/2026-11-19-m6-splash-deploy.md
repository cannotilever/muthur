# M6 Boot Splash and Deploy Tooling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every login on the WY-60 opens with a skippable Weyland-Yutani boot splash showing real ship facts. `make install-system` and `make deploy` set up and update `omicron`.

**Architecture:** `src/splash/` is a pure, time-injected state machine that `App` holds as `Option<Splash>`. It runs inside the existing event loop while the tabs warm up underneath, and it reads facts from the first metrics snapshot. The logo is a data file (`assets/wy-logo.txt`). Deploy is plain files under `deploy/` plus a `Makefile`.

**Tech Stack:** Rust 2024, ratatui 0.30, tokio, existing `Wy60Backend`/`Vt100Dialect`, GNU make, systemd, udev, fish.

**Spec:** `docs/superpowers/specs/2026-11-19-m6-splash-deploy-design.md`

## Global Constraints

- Screen is 80×43, rows 1-based in the spec (0-based `y` in ratatui = spec row − 1).
- Logo budget: at most 22 rows × 80 columns. `█` = block; every other character is printed as itself. Encoded logo frame ≤ 3072 bytes.
- Logo band rows 2–23, header row 25, boot lines rows 27–41 starting at column 9, `READY.` row 43 column 9.
- Boot line: `LABEL`, one space, dots up to column 46 inclusive, one space, value. All copy uppercase.
- Reveal 400 chars/s; 150 ms pause after each value; fact-line wait 1.5 s (once); `READY.` hold 1 s.
- `[chrome] splash = true` default.
- udev: vendor `0403`, product `6001`, serial `A9E91BA3`, `SYMLINK+="wy60"`, `TAG+="systemd"`.
- `HOST ?= omicron`, `TARGET := x86_64-unknown-linux-musl`.
- All timing constants live in `src/splash/mod.rs`; none are config keys.
- New `Tab`/`App` behaviour must not change existing golden files.

### Clarifications of the spec (decided here)

- **Header timing:** the logo and header appear in the first frame. Boot lines start `LOGO_SETTLE = 800 ms` after the splash starts, so typing doesn't compete with the logo on the wire. Total with a warm snapshot is still about 4 s.
- **`INTERFACE` status** comes from `cfg.muthur.readiness()`, not from `build_muthur`: `Ok` → `MU/TH/UR [ONLINE]`, `Err(e)` → `MU/TH/UR [OFFLINE: <e.reason()>]` (for example `NO ACCESS CODE`, the M4 copy). `build_muthur` is unchanged.
- **Catch-up:** `Splash::advance(now)` makes all progress due by `now` in one call (phases, pauses and reveal), so a `now` far enough past the start reaches `Done` in a single call.

## Review Focus

1. **MU/TH/UR override arming during the splash.** If the M5 override prompt is pending (for example, restored from a previous session), it must not arm while the splash covers it. Every tab is `set_obscured(true)` during the splash. Test in Task 3.
2. **Activity markers after the splash.** PTY output (the fish prompt) and metrics arrive during the splash. When the TUI appears, no sidebar activity marker may be lit. Task 3 drains `take_activity` when the splash ends. Test in Task 3.
3. **A snapshot arriving during a fact-line wait** resumes the reveal at once with real values, rather than waiting out 1.5 s. Test in Task 2.
4. **A snapshot arriving after a `NO DATA` timeout** fills the later fact lines with real values. Only lines with no data skip the wait. Test in Task 2.
5. **A taller or wider screen than 80×43** (`--rows 50`): the splash renders without panicking, the logo stays centred on the width, and nothing is drawn outside the frame area. Test in Task 2.

---

### Task 1: Config key, logo asset and logo parser

**Files:**
- Modify: `src/config.rs` (`ChromeConfig`)
- Create: `assets/wy-logo.txt`, `src/splash/mod.rs` (module skeleton only: `pub mod logo;`), `src/splash/logo.rs`
- Modify: `src/lib.rs` (`pub mod splash;`)
- Modify: `docs/config.example.toml` (`[chrome]` section)

**Interfaces:**
- Produces:
  - `ChromeConfig.splash: bool` (default `true`).
  - `splash::logo::{Logo, Cell, MAX_ROWS: usize = 22, MAX_COLS: usize = 80}`.
  - `pub enum Cell { Block, Char(char) }` (spaces are `Char(' ')`).
  - `Logo::parse(src: &str) -> Logo`. Trailing newline ignored; trailing spaces on a line are not significant.
  - `Logo::shipped() -> &'static Logo` (`include_str!("../../assets/wy-logo.txt")`, parsed once via `std::sync::OnceLock`).
  - `Logo::rows(&self) -> &[Vec<Cell>]`, `Logo::height(&self) -> usize`, `Logo::width(&self) -> usize` (longest row).

- [ ] **Step 1: Write the failing tests**

In `src/config.rs` tests:
```rust
#[test]
fn splash_defaults_on_and_can_be_disabled() {
    assert!(ChromeConfig::default().splash);
    let (_d, p) = write_cfg("[chrome]\nsplash = false\n", 0o600);
    assert!(!load(&p).unwrap().chrome.splash);
}
```
In `src/splash/logo.rs`:
```rust
#[test]
fn parse_maps_blocks_and_keeps_text() {
    let l = Logo::parse("█ A\n  ██\n");
    assert_eq!(l.height(), 2);
    assert_eq!(l.width(), 4);
    assert_eq!(l.rows()[0], vec![Cell::Block, Cell::Char(' '), Cell::Char('A')]);
    assert_eq!(l.rows()[1][2], Cell::Block);
}
#[test]
fn shipped_logo_fits_the_budget() {
    let l = Logo::shipped();
    assert!(l.height() >= 1 && l.height() <= MAX_ROWS, "{} rows", l.height());
    assert!(l.width() <= MAX_COLS, "{} cols", l.width());
    assert!(l.rows().iter().flatten().any(|c| *c == Cell::Block));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib splash_defaults parse_maps shipped_logo`
Expected: compile errors (no `splash` field, no `splash` module).

- [ ] **Step 3: Implement**

- `ChromeConfig.splash` with doc comment `/// Play the boot splash on launch.`; default `true`.
- `logo.rs` as in Interfaces.
- `assets/wy-logo.txt`, a first draft of **style A, block art**. It's the Weyland-Yutani wings mark: a wide "W" whose centre strokes rise into a "Y", with at least one blank column of negative space separating the Y from the W's inner strokes everywhere they run side by side. Strokes are at least 2 columns wide. Below the mark, after one blank row, the wordmark `W E Y L A N D - Y U T A N I`, and on the next row `BUILDING BETTER WORLDS`, each centred within the logo width. Target about 60 columns by 18–20 rows in total. Use only `█`, spaces and the wordmark text. The operator retunes this on hardware, so the draft's job is to respect the budget and the negative-space rule, not to be final.
- `docs/config.example.toml` `[chrome]`: add
  ```toml
  # Play the boot splash (logo and system check) on launch. Any key skips it.
  splash = true
  ```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib splash_defaults parse_maps shipped_logo`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add assets src/splash src/lib.rs src/config.rs docs/config.example.toml
git commit -m "feat: splash config key and logo asset"
```

---

### Task 2: `Splash` state machine and rendering

**Files:**
- Modify: `src/splash/mod.rs`
- Test: `src/splash/tests.rs` (`#[cfg(test)] mod tests;`)

**Interfaces:**
- Consumes: `logo::Logo::shipped()`, `muthur::reveal::Reveal`, `metrics::{Snapshot, human_bytes, human_uptime}`.
- Produces:
```rust
pub enum Interface { Online, Offline(String) }
pub struct SplashInfo { pub ship_name: String, pub year: i32, pub link: String, pub interface: Interface }
pub fn link_label(baud: u32, flow: crate::config::FlowControl) -> String; // "38400 BAUD  XON/XOFF"
impl Splash {
    pub fn new(info: SplashInfo, now: Instant) -> Self;
    pub fn with_logo(info: SplashInfo, logo: Logo, now: Instant) -> Self; // tests
    pub fn on_metrics(&mut self, s: &Snapshot);
    pub fn on_metrics_offline(&mut self);
    pub fn advance(&mut self, now: Instant) -> bool;
    pub fn render(&self, area: Rect, buf: &mut Buffer);
    pub fn tick_interval(&self) -> Duration; // 50 ms while lines type, 100 ms otherwise
    pub fn is_done(&self) -> bool;
}
pub const LOGO_SETTLE: Duration = 800 ms; CPS: u32 = 400; VALUE_PAUSE: Duration = 150 ms;
pub const FACT_WAIT: Duration = 1500 ms; READY_HOLD: Duration = 1000 ms;
```
`render` takes a `Rect` and `Buffer`, like `Tab::render`, so `App` and the golden test can both call it.

Boot lines in order, as in the spec's table: `SYSTEM CHECK`, `VESSEL`, `PROCESSOR`, `MEMORY`, `UPTIME`, `LINK`, `SYSTEM UNITS`, `INTERFACE`. Fact lines: `PROCESSOR`, `MEMORY`, `UPTIME`, `SYSTEM UNITS`. Header: `MU/TH/UR 6000 — INTERFACE {year}`.

Approach: build the script as a sequence of steps, each one either text to type (fed to a `Reveal` at `CPS`), a pause, or "value of fact line i". A fact value step resolves when a snapshot is present (format it), when metrics are offline or a previous fact timed out (`NO DATA`), or after `FACT_WAIT` from when it was reached (`NO DATA` and mark timed-out). `advance` loops until it hits something not yet due at `now`.

- [ ] **Step 1: Write the failing tests** (`src/splash/tests.rs`)

Helpers: `info()` with ship `USCSS OMICRON`, year 2137, link `link_label(38400, FlowControl::XonXoff)`, `Interface::Online`. `snap()` = `Snapshot { cpu_cores: vec![0.0; 16], mem: Usage { used: 0, total: (31.2 * (1u64 << 30) as f64) as u64 }, uptime_secs: 3 * 86400 + 4 * 3600 + 12 * 60, failed_units: Some(vec![]), ..Default::default() }`. `text(&Splash) -> String` renders into an 80×43 buffer and joins rows with `\n`. `t(ms)` = `t0 + Duration::from_millis(ms)`.

```rust
#[test] fn link_label_formats() {
    assert_eq!(link_label(38400, FlowControl::XonXoff), "38400 BAUD  XON/XOFF");
    assert_eq!(link_label(9600, FlowControl::None), "9600 BAUD  NONE");
}
#[test] fn first_frame_is_logo_and_header_only() {
    let s = Splash::new(info(), t0);
    let out = text(&s);
    assert!(out.lines().nth(24).unwrap().trim() == "MU/TH/UR 6000 — INTERFACE 2137");
    assert!(!out.contains("SYSTEM CHECK"));
}
#[test] fn lines_wait_for_logo_settle() {
    let mut s = Splash::new(info(), t0); s.on_metrics(&snap());
    s.advance(t(799)); assert!(!text(&s).contains("SYS"));
    s.advance(t(900)); assert!(text(&s).contains("SYS"));
}
#[test] fn full_run_formats_every_fact() {
    let mut s = Splash::new(info(), t0); s.on_metrics(&snap());
    s.advance(t(60_000));
    let out = text(&s);
    let row = |n: usize| out.lines().nth(n - 1).unwrap().to_string();
    assert_eq!(row(27).trim_end(), format!("        SYSTEM CHECK {} OK", ".".repeat(25)));
    for want in ["USCSS OMICRON", "16 CORES", "31.2G", "3D 04:12", "38400 BAUD  XON/XOFF", "NOMINAL", "MU/TH/UR [ONLINE]"] {
        assert!(out.contains(want), "{want}\n{out}");
    }
    assert_eq!(row(43).trim(), "READY.");
}
```
For the column rule in `full_run_formats_every_fact`: `SYSTEM CHECK` starts at column 9 and ends at column 20, the space is column 21, and the dots run over columns 22–46. Write the expected string from that rule rather than from the code.

```rust
#[test] fn ready_holds_one_second_then_done() // advance until READY. visible at time T (step 10 ms); !is_done at T+999ms; is_done at T+1000ms
#[test] fn total_time_with_warm_snapshot_is_about_four_seconds() // is_done first true between 3_000 and 5_000 ms (10 ms steps)
#[test] fn fact_line_waits_then_no_data_once() {
    // no snapshot: PROCESSOR shows NO DATA, and the whole run takes under (warm total + 1.6 s)
    // assert MEMORY, UPTIME, SYSTEM UNITS also NO DATA
}
#[test] fn metrics_offline_skips_the_wait() // on_metrics_offline() before start; done time within 100 ms of the warm-snapshot done time; all four facts NO DATA
#[test] fn snapshot_during_wait_resumes_immediately() // Review Focus 3: no snapshot; step until "PROCESSOR" dots fully visible at T; on_metrics at T+200ms; advance(T+210ms) shows "16 CORES"
#[test] fn snapshot_after_timeout_fills_later_lines() // Review Focus 4: time out PROCESSOR (NO DATA), then on_metrics; run to end; MEMORY shows "31.2G"
#[test] fn failed_units_variants() // Some(vec!["a","b"]) → "2 FAILED"; None → "NOT MONITORED"
#[test] fn offline_interface() // Interface::Offline("NO ACCESS CODE".into()) → "MU/TH/UR [OFFLINE: NO ACCESS CODE]"
#[test] fn uptime_under_a_day() // uptime_secs 4*3600+12*60 → line ends "04:12"
#[test] fn tick_interval_is_fast_while_typing() // 50 ms at t(900) with snapshot; 100 ms at t(0)
#[test] fn logo_is_centred_and_blocks_render_as_full_block() {
    // with_logo(Logo::parse("██\n"), …): row 2 (y=1) has "█" at x=39 and x=40 (centred: (80-2)/2 = 39);
    // one-row logo is vertically centred in rows 2–23 (y = 1 + (22-1)/2 = 11)
}
#[test] fn renders_inside_a_larger_area_without_panic() {
    // Review Focus 5: render into Rect 100×50 at offset (0,0) of a 100×50 buffer: logo centred on width 100; no panic.
    // render into a Buffer whose area is 80×43 at y offset 0 with a Rect of 80×20: no panic, nothing written below y=19.
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib splash::`
Expected: compile errors (types not defined).

- [ ] **Step 3: Implement `Splash` in `src/splash/mod.rs`** per Interfaces. Logo cells: `Cell::Block` → buffer symbol `"█"` (the dialect turns it into a reverse-video space). Clip every write to `area`. Logo vertical centring: `top = 1 + (22 - height) / 2`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib splash::`
Expected: all splash tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/splash
git commit -m "feat: boot splash state machine and render"
```

---

### Task 3: `App` integration, golden and byte budget

**Files:**
- Modify: `src/app/mod.rs`
- Test: `src/app/tests.rs`, `tests/golden.rs`, `tests/golden/splash.txt` (generated)

**Interfaces:**
- Consumes: `splash::{Splash, SplashInfo, Interface}` from Task 2.
- Produces:
  - `App::with_splash(self, splash: Splash) -> Self` (builder).
  - `pub quit_after_splash: bool` field (default `false`).
  - `App::splash_active(&self) -> bool`.

Behaviour while `self.splash` is `Some`, per the spec's "`App` integration" section:
- `render`: `advance(Instant::now())`. If done → end the splash (below); otherwise `set_obscured(true)` on every tab and draw only the splash into `frame.area()`, with no cursor. Once ended, render the TUI normally in the same call.
- `handle_key`: end the splash; the key is not passed on.
- `on_metrics` / `on_metrics_offline`: also feed the splash.
- `tick_interval`: the splash's.
- **Ending the splash:** `splash = None`; drain and discard `take_activity()` on every tab, and clear `self.activity`; then `quit = true` if `quit_after_splash`, else `redraw = true`.

For tests that need a finished splash, construct it with `Splash::new(info, Instant::now() - Duration::from_secs(60))` and feed a snapshot, so the first `render` reaches `Done`.

- [ ] **Step 1: Write the failing tests** (`src/app/tests.rs`)

```rust
fn splash_info() -> SplashInfo { SplashInfo { ship_name: "USCSS TEST".into(), year: 2137, link: "38400 BAUD  XON/XOFF".into(), interface: Interface::Online } }
fn fresh_splash() -> Splash { Splash::new(splash_info(), Instant::now()) }
fn stale_splash() -> Splash { let mut s = Splash::new(splash_info(), Instant::now() - Duration::from_secs(60)); s.on_metrics(&Snapshot::default()); s }

#[test] fn splash_draws_instead_of_chrome() // App::new(chrome()).with_splash(fresh_splash()); draw → row 24 (y) contains "MU/TH/UR 6000"; no "USCSS TEST" title bar text
#[test] fn key_skips_splash_and_is_not_delivered() // use a recording Tab (existing test helpers if present, else a small one in tests.rs that records keys); Key::Char('x') → !splash_active, redraw, tab saw no key
#[test] fn ctrl_c_skips_too() // Key::Ctrl('c') → !splash_active, !quit
#[test] fn finished_splash_hands_over_with_redraw() // stale_splash; draw → !splash_active, redraw, title bar visible in that same frame
#[test] fn quit_after_splash_quits_on_finish_and_on_skip() // both paths set quit, not redraw
#[test] fn metrics_reach_splash_and_utility() // with a recording tab at Utility: on_metrics → tab got it; splash shows "16 CORES" once run to end (use a snapshot with 16 cores)
#[test] fn tabs_are_obscured_during_splash() // Review Focus 1: recording tab tracks last set_obscured value; after draw with splash, the active tab saw set_obscured(true)
#[test] fn no_activity_markers_after_splash() // Review Focus 2: tab whose take_activity returns true once; skip splash; draw → sidebar has no activity marker for that tab (assert the same way existing activity tests do)
#[test] fn no_splash_is_unchanged() // App::new(chrome()) → !splash_active
```
If `src/app/tests.rs` already has a recording or fake `Tab`, extend it with `obscured: Rc<Cell<bool>>` and key recording rather than adding a second one.

`tests/golden.rs`:
```rust
#[test]
fn splash() {
    // Splash::new(info for "USCSS TEST", Instant::now() - 60 s) + on_metrics(snapshot: 16 cores, 31.2G, 3D 04:12, failed_units Some(vec![]))
    // advance(Instant::now()); render through Terminal<Wy60Backend<StrictWriter<Vec<u8>>, Vt100Dialect>> 80×43,
    // drawing with f.render_widget-free closure: s.render(f.area(), f.buffer_mut()).
    // Assert no strict violations; compare emulator text with tests/golden/splash.txt (same UPDATE_GOLDEN flow as scene()).
}
#[test]
fn splash_logo_frame_fits_the_wire_budget() {
    // Fresh splash at t0 (logo + header only); one draw through Wy60Backend<Vec<u8>> after t.clear();
    // assert bytes written (excluding the clear) <= 3072.
}
```
The golden test uses a `Splash` that has already finished and keeps its last frame (`READY.` visible). `render` draws the final state even after `is_done`. Make sure Task 2's render does that, and add the test there if it's missing.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib app:: && cargo test --test golden splash`
Expected: compile errors (`with_splash`, `quit_after_splash` missing).

- [ ] **Step 3: Implement** per Interfaces and behaviour above.

- [ ] **Step 4: Generate the golden file, read it, then run everything**

Run: `UPDATE_GOLDEN=1 cargo test --test golden splash`, then read `tests/golden/splash.txt`. Check the logo is centred, the header is on row 25, the boot lines are on rows 27–34 with dots ending at column 46, and `READY.` is on row 43.
Run: `cargo test`
Expected: all pass; existing golden files unchanged (`git status tests/golden` shows only `splash.txt`).

- [ ] **Step 5: Commit**

```bash
git add src/app tests/golden.rs tests/golden/splash.txt
git commit -m "feat: App plays the splash before the TUI"
```

---

### Task 4: Wiring — `run` and `muthur splash`

**Files:**
- Modify: `src/run.rs`, `src/main.rs`, `src/splash/mod.rs` (`SplashInfo::from_config`)

**Interfaces:**
- Consumes: Task 2 `SplashInfo`, `Interface`, `link_label`; Task 3 `App::with_splash`, `quit_after_splash`.
- Produces:
  - `SplashInfo::from_config(cfg: &Config, ship_name: &str, now: NaiveDateTime) -> SplashInfo`. `year = now.year() + cfg.chrome.year_offset`; `link = link_label(cfg.link.baud, cfg.link.flow)`; `interface` from `cfg.muthur.readiness()`.
  - `pub fn run_splash(cfg: &Config, strict: bool) -> anyhow::Result<()>` in `src/run.rs`.

- [ ] **Step 1: Write the failing tests**

In `src/splash/tests.rs`:
```rust
#[test] fn info_from_default_config_is_offline_without_key() {
    let cfg = Config::default();
    let i = SplashInfo::from_config(&cfg, "USCSS X", chrono::NaiveDate::from_ymd_opt(2026, 11, 19).unwrap().and_hms_opt(0, 0, 0).unwrap());
    assert_eq!(i.year, 2137);
    assert_eq!(i.link, "38400 BAUD  XON/XOFF");
    assert!(matches!(i.interface, Interface::Offline(ref r) if r == "NO ACCESS CODE"));
}
#[test] fn info_online_with_key() // cfg.muthur.api_key = Some("k") → Interface::Online
```
If `MuthurSection::api_key()` also reads an environment variable, set the key through the config field only and make sure the test doesn't depend on the environment (check `api_key()` first).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib splash::tests::info_`
Expected: compile error (`from_config` missing).

- [ ] **Step 3: Implement**

- `SplashInfo::from_config` per Interfaces.
- `run()`: after `App::assemble(...)`, if `cfg.chrome.splash`, call `.with_splash(Splash::new(SplashInfo::from_config(cfg, &ship, chrono::Local::now().naive_local()), Instant::now()))`. `ship` is the same string the chrome uses (`ship_name(cfg)`).
- `run_splash()`: the same link, writer, strict and terminal setup as `run()`, plus `check_geometry`, logging and the panic hook. It uses `App::new(chrome)` (placeholder tabs), `with_splash(...)` and `quit_after_splash = true`. It starts `LocalSource` metrics exactly as `run()` does (factor the shared setup out of `run()` into a private helper if it would otherwise be duplicated: link + terminal construction and the metrics spawn). No MU/TH/UR, no PTY: pass a `pty_rx` from a channel whose sender is dropped, as the existing tests' `no_pty()` does. Same final clear and termios restore.
- `main.rs`: add the `Command::Splash` variant (doc: `/// Play the boot splash once and exit (for tuning the logo).`). Dispatch it before the testcard path: `Some(Command::Splash) => return muthur::run::run_splash(&cfg, cli.strict)`.

- [ ] **Step 4: Verify**

Run: `cargo test` → all pass.
Run: `cargo build --release && script -qc "./target/release/muthur --help" /dev/null | grep -i splash` → the `splash` subcommand is listed.

- [ ] **Step 5: Commit**

```bash
git add src/run.rs src/main.rs src/splash
git commit -m "feat: splash on launch and muthur splash preview"
```

---

### Task 5: Deploy files, Makefile and docs

**Files:**
- Create: `deploy/udev/99-wy60.rules`, `deploy/systemd/serial-getty@wy60.service.d/override.conf`, `deploy/fish/muthur.fish`, `Makefile`, `docs/deploy.md`
- Modify: `docs/ARCHITECTURE.md` (milestones table M6 row; `splash` module row)

**Interfaces:** none in code.

- [ ] **Step 1: Write the three deploy files** with the exact contents from the spec's "Deploy" section (udev rule, `override.conf`, `muthur.fish`).

- [ ] **Step 2: Write the `Makefile`** with targets `build`, `deploy`, `install-system`, `check-deploy` (all `.PHONY`), as in the spec's Makefile table. Details:
  - `deploy`: `ssh $(HOST) 'mkdir -p ~/.local/bin ~/.config/fish/conf.d'`; `scp $(BIN) $(HOST):.local/bin/muthur.new`; `ssh $(HOST) 'cd ~/.local/bin && { [ ! -e muthur ] || mv -f muthur muthur.old; } && mv muthur.new muthur'`; `scp deploy/fish/muthur.fish $(HOST):.config/fish/conf.d/muthur.fish`.
  - `install-system`: `tmp=$$(ssh $(HOST) mktemp -d)`, `scp` both files there, then one `ssh -t $(HOST) "set -e; ..."` running the spec's steps, each prefixed with `echo '==> <step>'`. Disable the old getty with `if systemctl is-enabled --quiet serial-getty@ttyUSB0; then sudo systemctl disable --now serial-getty@ttyUSB0; fi`. Remove the temp directory at the end.
  - `check-deploy`: `udevadm verify deploy/udev/99-wy60.rules`. Then, in a `mktemp -d` directory, write `serial-getty@wy60.service` = the local `/usr/lib/systemd/system/serial-getty@.service` with the drop-in's `[Service]` lines appended, and run `systemd-analyze verify --man=no` on it. Remove the temp directory afterwards.

- [ ] **Step 3: Verify**

Run: `make check-deploy` → exit 0. If the local `udevadm` is older than 255 and has no `verify`, say so in the report and skip that line with a Makefile guard (`udevadm verify --help >/dev/null 2>&1 &&`).
Run: `make -n deploy` and `make -n install-system` → read the commands and check they match the spec (no real SSH in this step).
Run: `fish -n deploy/fish/muthur.fish` → exit 0.

- [ ] **Step 4: Write `docs/deploy.md`** covering the spec's six walkthrough items, in that order, with the exact commands. Update `docs/ARCHITECTURE.md`:
  - M6 row: `Splash, deploy tooling (udev rule, getty override, fish snippet, make deploy). Spec: docs/superpowers/specs/2026-11-19-m6-splash-deploy-design.md`.
  - New row: `| **M7** | Polish, from operating a deployed build. |`.
  - Under "Deployment & process model", change the `config.fish` mention to `conf.d/muthur.fish`.

- [ ] **Step 5: Commit**

```bash
git add deploy Makefile docs/deploy.md docs/ARCHITECTURE.md
git commit -m "feat: deploy tooling for omicron"
```

---

## Hand-off to the operator (not tasks)

Run these on hardware, as with M3–M5:
- `make install-system` (needs sudo on omicron).
- `make deploy`.
- `muthur splash` on the WY-60 to tune `assets/wy-logo.txt`.
- Reboot, then check the getty on `/dev/wy60`, the splash and TUI after login, and the fall-back to fish when MUTHUR is killed.
