# CARGO Container Actions and Log View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In the CARGO folder, the operator selects a container with the arrow keys and starts, restarts or stops it (restart and stop need an armed Y/N), or reads its last 200 log lines in a scrollable full-pane view.

**Architecture:** `CargoTab` gets injected `CargoDeps`, shaped like `MuthurDeps`. `run` starts a blocking `SystemRunner` call, whose result comes back as `Event::CargoDone`, and `App` routes it to the tab through a new `Tab::on_cargo_done` hook. The probe now carries container ID, real-case name, full status and which runtime answered. MU/TH/UR and CARGO share one `AuditLog` through `Rc<RefCell<…>>`. The tab is split into `list` (selection and list screen), `action` (argv, prompt, messages) and `logview` (cleaning, wrapping, scrolling).

**Tech Stack:** Rust 2024 (rustc 1.100), ratatui, tokio (current-thread runtime), nix, vt100 + `StrictWriter` for golden tests.

**Spec:** `docs/superpowers/specs/2026-11-19-cargo-actions-design.md`

## Global Constraints

- Branch `cargo-tab`. It carries the uncommitted CARGO baseline (F3 folder, `ps --all`, trouble-first sort). Commit that baseline first, in Task 0, so the later task commits stay readable.
- Commands are argv only, never a shell. They target the container **ID**, never the display name.
- `<rt>` is `Snapshot::container_runtime` (`"docker"` or `"podman"`). No elevation.
- Timeouts: start/restart/stop **30 s**; logs **10 s**; `ps` refresh **2 s**.
- Logs: `<rt> logs --tail 200 <id>`, merged stdout+stderr.
- Arming delay: the existing 1 s `ARMING`, shared rather than copied.
- Everything on the screen is upper-case, except log text, which keeps its case. Only ASCII is written, apart from glyphs the backend already maps (`│`, `─` etc.); the in-flight status uses `...`, not `…`.
- Audit tool names `cargo_start` / `cargo_restart` / `cargo_stop` / `cargo_logs`; tier 2 + `confirmed` for restart and stop, tier 1 + `auto` for start and logs; `exit` is `json!(code)` / `json!("not_found")` / `json!("timeout")`; output goes through `tools::cap`.
- The repo is not rustfmt-formatted (wide lines). Match the surrounding style; do not run `cargo fmt`.
- Every task ends with `cargo test` fully green and `cargo clippy --all-targets` printing no warnings.
- Golden files are regenerated only with `UPDATE_GOLDEN=1 cargo test --test golden`, and the implementer **reads** every changed golden before committing.
- Commit messages end with:
  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01ECg5aUEPENYy8emeCKVgXW
  ```

## Review Focus

1. **A snapshot arrives while a stop prompt is up and moves the target's row.** Y must still stop the ID in the prompt, not whatever is now in that row. Test: `confirm_targets_pinned_id_after_resort` (Task 5).
2. **Logs with ANSI colour, `\r` progress bars or very long lines.** These must not reach the wire as escapes or overflow the pane. Tests: `clean_strips_ansi_cr_tabs_controls` and `wrap_never_exceeds_width` (Task 6), plus the strict golden (Task 7).
3. **Pressing L twice quickly, or Esc while a fetch is in flight.** A late result must not reopen the view or overwrite newer output. Test: `stale_log_result_after_esc_is_dropped` (Task 6).
4. **The runtime binary vanishes, or the daemon hangs.** The operator must get `RUNTIME NOT FOUND` / `STOP TIMED OUT` and be able to act again (busy cleared). Test: `errors_clear_busy_and_show_reverse` (Task 5).
5. **The container list shrinks to empty, or the selected container disappears.** No panic from the selection index; action keys do nothing on `HOLD EMPTY`. Tests: `selection_clamps_when_selected_vanishes` and `actions_ignored_when_empty` (Task 4 / Task 5).

---

## File Structure

| File | Responsibility |
|---|---|
| `src/metrics/mod.rs` | `Container` gains `id`, `detail`; `Snapshot` gains `container_runtime`. |
| `src/metrics/probes.rs` | `parse_containers` for the 3-field format, keeping case. |
| `src/metrics/local.rs` | `CONTAINER_FORMAT` with ID; `pub fn ps_argv`; records the runtime that answered. |
| `src/tools/exec.rs` | `ship_status` upper-cases names. |
| `src/tools/runner.rs` | `Runner::run_merged`; `SystemRunner` implements it with one pipe. |
| `src/tools/audit.rs` | `pub type SharedAudit = Rc<RefCell<AuditLog>>`. |
| `src/app/mod.rs` | `pub const ARMING`; `App::on_cargo_done`. |
| `src/app/tabs.rs` | `Tab::on_cargo_done` default no-op. |
| `src/muthur/*` | Uses `SharedAudit` and `crate::app::ARMING`. |
| `src/cargo/mod.rs` | `CargoTab`, `CargoDeps`, state, key dispatch, `Tab` impl. |
| `src/cargo/list.rs` | `Selection` (ID-following cursor + viewport) and the list render. |
| `src/cargo/action.rs` | `Verb`, argv builders, `Prompt`, outcome → message, audit entry. |
| `src/cargo/logview.rs` | `clean`, `wrap`, `LogView` scroll and render. |
| `src/cargo/tests.rs` | Tab-level tests with a fake `run`. |
| `src/run.rs` | `Event::CargoDone`; real `CargoDeps`; one shared audit log. |
| `tests/golden.rs`, `tests/golden/*.txt` | Golden screens. |
| `docs/ARCHITECTURE.md` | Cargo tab section. |

---

### Task 0: Commit the CARGO baseline

**Files:** everything currently modified or untracked on `cargo-tab` (`git status`).

- [ ] **Step 1:** Run `cargo test` and `cargo clippy --all-targets`. Expected: all green, no warnings.
- [ ] **Step 2:** Commit:
  ```bash
  git add -A src tests docs
  git commit -m "feat: CARGO folder for containers (F3), dead containers listed first"
  ```
  Expected: `git status --short` is empty.

---

### Task 1: Container ID, real-case name, full status, runtime

**Files:**
- Modify: `src/metrics/mod.rs` (`Container`, `Snapshot`)
- Modify: `src/metrics/probes.rs` (`parse_containers` + tests)
- Modify: `src/metrics/local.rs` (`CONTAINER_FORMAT`, `CONTAINER_ARGS`, `containers_from`, `refresh_probes`, `snapshot`)
- Modify: `src/tools/exec.rs` (`format_snapshot` container line)
- Modify: `src/cargo/mod.rs` (display upper-cases `name`)
- Test: in-file `tests` modules of the above; `src/cargo/tests.rs`

**Interfaces:**
- Produces:
  - `pub struct Container { pub id: String, pub name: String, pub status: String, pub detail: String }`, deriving `Clone, Debug, PartialEq, Eq, Default`.
  - `Snapshot { pub container_runtime: Option<String>, .. }`
  - `pub const CONTAINER_FORMAT: &str = "{{.ID}}\t{{.Names}}\t{{.Status}}";` in `metrics::local`.
  - `pub fn ps_argv(rt: &str) -> Vec<String>` returns `[rt, "ps", "--all", "--format", CONTAINER_FORMAT]`.
  - `pub fn parse_containers(out: &str) -> Vec<Container>` in `metrics::probes`.

- [ ] **Step 1: Write failing tests**

  In `probes.rs`, replace `parses_containers`:
  ```rust
  #[test]
  fn parses_containers() {
      let out = "a1b2\tweb\tUp 3 hours\nc3d4\tMyDB\tExited (137) 2 hours ago\nshort\tline\n\n";
      assert_eq!(parse_containers(out), vec![
          Container { id: "a1b2".into(), name: "web".into(), status: "UP".into(), detail: "Up 3 hours".into() },
          Container { id: "c3d4".into(), name: "MyDB".into(), status: "EXITED".into(), detail: "Exited (137) 2 hours ago".into() },
      ]);
  }
  ```
  In `local.rs`, replace `container_probe_includes_stopped`:
  ```rust
  #[test]
  fn ps_argv_lists_all_with_ids() {
      assert_eq!(ps_argv("podman"), vec!["podman", "ps", "--all", "--format", "{{.ID}}\t{{.Names}}\t{{.Status}}"]);
  }
  #[test]
  fn no_runtime_means_no_runtime_name() {
      let mut s = LocalSource::new(ContainerRuntime::None, default_exclude()).unwrap();
      let a = s.snapshot().unwrap();
      assert_eq!((a.containers, a.container_runtime), (None, None));
  }
  ```
  In `exec.rs` tests, add `ship_status_upper_cases_container_names`. Build a `Snapshot` with `containers: Some(vec![Container { id: "x".into(), name: "web".into(), status: "UP".into(), detail: "Up".into() }])`, then assert `format_snapshot(&s).contains("CONTAINERS WEB UP")`.

  In `src/cargo/tests.rs`, make the `ship` helper and the inline `Container` literals use real-case names with `..Default::default()`, and add:
  ```rust
  #[test]
  fn names_display_upper_case() {
      let mut t = CargoTab::new();
      t.on_metrics(&snap(Some(vec![Container { id: "1".into(), name: "web".into(), status: "UP".into(), ..Default::default() }])));
      assert!(text(&render(&mut t, WINDOWED)).contains("WEB"));
  }
  ```

- [ ] **Step 2:** Run `cargo test`. Expected: compile errors (no `id`/`detail`/`container_runtime`/`ps_argv`).

- [ ] **Step 3: Implement**
  - `parse_containers`: split on `'\t'` into exactly 3 fields and skip any other line. `id` and `detail` are trimmed; `name` is trimmed with case kept; `status` is the upper-cased first word of `detail` (`"?"` if empty).
  - `containers_from(cmd)` returns `Option<(String, Vec<Container>)>`. `refresh_probes` stores `self.container_runtime` alongside `self.containers`, and `snapshot()` copies both.
  - `ps_argv` is used by `containers_from` (drop `CONTAINER_ARGS`; `run_probe` takes `&[&str]`, so pass `&argv[1..]` as strs).
  - `format_snapshot` writes `c.name.to_uppercase()`.
  - CARGO's row text uses `k.name.to_uppercase()`.
  - Fix every other `Container { .. }` literal the compiler reports (tests and `tests/golden.rs`) by adding `id` and `..Default::default()`.

- [ ] **Step 4:** Run `cargo test && cargo clippy --all-targets`. Expected: green, no warnings; goldens unchanged.

- [ ] **Step 5: Commit** — `feat: container ID, real-case name and full status in snapshots`

---

### Task 2: `SystemRunner` merged-output mode

**Files:**
- Modify: `src/tools/runner.rs`

**Interfaces:**
- Produces: on `trait Runner`, `fn run_merged(&self, argv: &[String], timeout: Duration) -> RunOutput`. The default implementation calls `run` and returns `Exited { code, stdout: stdout + &stderr, stderr: String::new() }`, so existing fakes keep compiling. `SystemRunner` overrides it.

- [ ] **Step 1: Write failing tests** in `runner.rs` tests:
  ```rust
  #[test]
  fn merged_keeps_order() {
      assert_eq!(
          SystemRunner.run_merged(&v(&["sh", "-c", "echo a; echo b >&2; echo c"]), Duration::from_secs(2)),
          RunOutput::Exited { code: 0, stdout: "a\nb\nc\n".into(), stderr: String::new() }
      );
  }
  #[test]
  fn merged_times_out_and_kills() {
      let t = Instant::now();
      assert_eq!(SystemRunner.run_merged(&v(&["sleep", "5"]), Duration::from_millis(200)), RunOutput::TimedOut);
      assert!(t.elapsed() < Duration::from_secs(2));
  }
  #[test]
  fn merged_not_found() {
      assert_eq!(SystemRunner.run_merged(&v(&["definitely-not-a-binary-muthur"]), Duration::from_secs(1)), RunOutput::NotFound);
  }
  ```
- [ ] **Step 2:** Run `cargo test --lib runner`. Expected: `merged_keeps_order` FAILS with the default implementation (`"a\nc\nb\n"`).
- [ ] **Step 3: Implement.** Factor the existing body into `fn spawn_and_wait(&self, argv, timeout, merged: bool) -> RunOutput`. When `merged`, create one `std::io::pipe()` and give the child `Stdio::from(writer.try_clone()?)` for stdout and `Stdio::from(writer)` for stderr. Drop the parent's writer copies **before** draining, or the read never sees EOF. Drain only that reader, with `stderr` set to `""`. Spawn failures map to `NotFound` as now. `run` calls it with `false`; `run_merged` overrides with `true`.
- [ ] **Step 4:** Run `cargo test && cargo clippy --all-targets`. Expected: green.
- [ ] **Step 5: Commit** — `feat: merged stdout+stderr mode for SystemRunner`

---

### Task 3: Shared audit log and shared `ARMING`

**Files:**
- Modify: `src/tools/audit.rs`, `src/muthur/mod.rs`, `src/muthur/turn.rs`, `src/app/mod.rs`, `src/run.rs`, `src/app/tests.rs`, `src/muthur/tests.rs`, `tests/golden.rs`

**Interfaces:**
- Produces:
  - `pub type SharedAudit = std::rc::Rc<std::cell::RefCell<AuditLog>>;` and `pub fn shared(log: AuditLog) -> SharedAudit` in `tools::audit`.
  - `MuthurTab::new(cfg, tools, ship: &str, log: ChatLog, audit: SharedAudit, deps: MuthurDeps)`.
  - `pub const ARMING: Duration = Duration::from_secs(1);` in `crate::app` (removed from `muthur::turn`; `turn` uses `crate::app::ARMING`).
  - `build_muthur(cfg, ship, tx, slot, audit: SharedAudit)`; `run()` calls `open_audit_log()` once.

- [ ] **Step 1: Write a failing test** in `src/muthur/tests.rs`: `shared_audit_sees_muthur_entries`. Build the tab with `let audit = shared(AuditLog::open(path)?)`, keeping a clone. Drive one tier-1 tool call through the existing harness helpers (same flow as the current audit test near line 499). Then assert `audit.borrow().is_online()` and that the file at `path` holds one line.
- [ ] **Step 2:** Run `cargo test`. Expected: compile error (`shared` / signature).
- [ ] **Step 3: Implement.** Change `MuthurTab.audit` to `SharedAudit` and use `self.audit.borrow_mut().append(..)` / `self.audit.borrow().is_online()`. Update the six `MuthurTab::new` call sites to wrap with `shared(..)`. Move `ARMING`.
- [ ] **Step 4:** Run `cargo test && cargo clippy --all-targets`. Expected: green.
- [ ] **Step 5: Commit** — `refactor: share one audit log and the arming delay`

---

### Task 4: Selection and the new list layout

**Files:**
- Create: `src/cargo/list.rs`
- Modify: `src/cargo/mod.rs`, `src/cargo/tests.rs`, `src/run.rs`, `src/app/tests.rs`, `tests/golden.rs`

**Interfaces:**
- Consumes: `Container` (Task 1), `SharedAudit` (Task 3).
- Produces (`cargo::mod`):
  ```rust
  pub struct CargoDeps {
      pub run: Box<dyn FnMut(u64, Vec<String>, Duration, bool)>, // seq, argv, timeout, merged
      pub now: Box<dyn Fn() -> Instant>,
      pub timestamp: Box<dyn Fn() -> String>,
  }
  impl CargoTab {
      pub fn new(deps: CargoDeps, audit: SharedAudit) -> Self;
      pub fn detached() -> Self; // no-op run, Instant::now, empty timestamp, AuditLog::offline()
  }
  ```
- Produces (`cargo::list`, `pub(super)`):
  ```rust
  pub(super) struct Selection { id: Option<String>, index: usize, scroll: usize, page: usize }
  impl Selection {
      fn sync(&mut self, list: &[Container]);  // re-find id → index; else clamp index; update id
      fn up(&mut self, list: &[Container]); fn down(..); fn page_up(..); fn page_down(..); fn home(..);
      fn selected<'a>(&self, list: &'a [Container]) -> Option<&'a Container>;
  }
  pub(super) fn draw_list(sel: &mut Selection, list: &[Container], status_override: Option<(&str, &str)>,
                          message: (&str, bool), hint: &str, area: Rect, buf: &mut Buffer);
  ```
  - `status_override` is `(id, "RESTARTING...")` for the in-flight row.
  - `message` is `(text, reverse)` for the detail/message line. With no message active, the tab passes `"<NAME>: <DETAIL>"`, upper-cased.
- Layout:
  - **Inner rows:** row 0 is the heading; then `page = inner.height - 3` rows; then the message line; then the hint line. That gives 35 rows windowed and 38 fullscreen.
  - **Columns:** a 1-column gutter (`>` on the selected row), then the name column (`inner.width - 1 - status_w - 1`), a space, then STATUS.
  - **Styles:** the selected row is underlined across the full inner width; a status other than `UP` is reverse video, as now.
  - **Title:** `CARGO - N CONTAINERS (a-b)` when the list overflows, as now.
- Paging:
  - PageDown: `index = min(index + page, len - 1)` and `scroll = min(scroll + page, max_scroll)`, then make `index` visible. PageUp mirrors it.
  - Up/Down move `index` by one and scroll minimally.
  - Home sets both to 0.

- [ ] **Step 1: Write failing tests** in `src/cargo/tests.rs`. Switch the existing tests to `CargoTab::detached()`. Change the expected ranges to the new page size: `fullscreen_fits_more` expects `(1-38)`. Rewrite `long_manifest_pages` for the selection semantics:
  - first render shows `(1-35)` with `>CRATE000`;
  - PageDown shows `(36-70)` with `>CRATE035`;
  - two more PageDowns clamp at `(66-100)` with `>CRATE099`;
  - Home returns to `(1-35)` with `>CRATE000`;
  - PageUp at the top stays at `(1-35)`.

  Add:
  ```rust
  #[test]
  fn arrows_move_selection_marker() {
      let mut t = CargoTab::detached();
      t.on_metrics(&snap(Some(ship(3))));
      let buf = render(&mut t, WINDOWED);
      let (x, y) = find(&buf, ">CRATE000").unwrap();
      assert!(buf[(x + 1, y)].modifier.contains(Modifier::UNDERLINED));
      t.handle_key(&Key::Down);
      assert!(text(&render(&mut t, WINDOWED)).contains(">CRATE001"));
  }
  #[test]
  fn selection_follows_id_across_resort() {
      let mut t = CargoTab::detached();
      t.on_metrics(&snap(Some(ship(5))));
      t.handle_key(&Key::Down); t.handle_key(&Key::Down); // CRATE002
      let mut l = ship(5); l[4].status = "EXITED".into(); // sorts first, shifts rows
      t.on_metrics(&snap(Some(l)));
      assert!(text(&render(&mut t, WINDOWED)).contains(">CRATE002"));
  }
  #[test]
  fn selection_clamps_when_selected_vanishes() {
      let mut t = CargoTab::detached();
      t.on_metrics(&snap(Some(ship(5))));
      for _ in 0..4 { t.handle_key(&Key::Down); }
      t.on_metrics(&snap(Some(ship(2))));
      assert!(text(&render(&mut t, WINDOWED)).contains(">CRATE001"));
      t.on_metrics(&snap(Some(vec![])));
      assert!(text(&render(&mut t, WINDOWED)).contains("HOLD EMPTY"));
      t.handle_key(&Key::Down); // no panic
  }
  #[test]
  fn detail_and_hint_lines() {
      let mut t = CargoTab::detached();
      t.on_metrics(&Snapshot { container_runtime: Some("docker".into()), ..snap(Some(vec![
          Container { id: "1".into(), name: "db".into(), status: "EXITED".into(), detail: "Exited (137) 2 hours ago".into() }])) });
      let s = text(&render(&mut t, WINDOWED));
      assert!(s.contains("DB: EXITED (137) 2 HOURS AGO") && s.contains("U START  R RESTART  D STOP  L LOGS"), "{s}");
      t.on_metrics(&Snapshot { container_runtime: None, ..snap(Some(ship(1))) });
      assert!(text(&render(&mut t, WINDOWED)).contains("NO CONTAINER RUNTIME"));
  }
  #[test]
  fn paging_keeps_selection_visible() {
      let mut t = CargoTab::detached();
      t.on_metrics(&snap(Some(ship(100))));
      render(&mut t, WINDOWED);
      t.handle_key(&Key::PageDown);
      let s = text(&render(&mut t, WINDOWED));
      assert!(s.contains("(36-70)") && s.contains(">CRATE035"), "{s}");
  }
  ```
  `snap()` sets `container_runtime: Some("docker".into())` whenever `containers` is `Some`, except where a test overrides it.
- [ ] **Step 2:** Run `cargo test --lib cargo`. Expected: compile errors, then failures.
- [ ] **Step 3: Implement** `list.rs` and the constructor change. `CargoTab` keeps `list: Option<Vec<Container>>` (sorted trouble-first as now) and `runtime: Option<String>`. Replace `CargoTab::new()` call sites: `run.rs` gets `CargoTab::detached()` for now (Task 5 wires the real deps), and the app tests and goldens get `detached()`.
- [ ] **Step 4:** Run `cargo test`. Expected: only the `windowed_cargo` golden fails. Run `UPDATE_GOLDEN=1 cargo test --test golden windowed_cargo` and **read** `tests/golden/windowed_cargo.txt`. Check for `>` on the first row, the detail line, the hint line and `(1-35)`. Then run `cargo test && cargo clippy --all-targets`. Expected: green.
- [ ] **Step 5: Commit** — `feat: CARGO selection, detail and hint lines`

---

### Task 5: Actions — start, restart, stop, with confirmation, audit and wiring

**Files:**
- Create: `src/cargo/action.rs`
- Modify: `src/cargo/mod.rs`, `src/cargo/tests.rs`, `src/app/tabs.rs`, `src/app/mod.rs`, `src/app/tests.rs`, `src/run.rs`

**Interfaces:**
- Consumes: `CargoDeps`, `Selection` (Task 4); `SharedAudit`, `ARMING` (Task 3); `ps_argv`, `parse_containers` (Task 1); `RunOutput` (`tools::runner`).
- Produces:
  - `cargo::action`:
    ```rust
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Verb { Start, Restart, Stop, Logs }
    impl Verb {
        fn key(c: char) -> Option<Verb>;      // u/r/d/l, either case
        fn word(self) -> &'static str;        // "start" | "restart" | "stop" | "logs"
        fn progress(self) -> &'static str;    // "STARTING" | "RESTARTING" | "STOPPING" | "FETCHING LOGS"
        fn done(self) -> &'static str;        // "STARTED" | "RESTARTED" | "STOPPED"
        fn confirm(self) -> bool;             // Restart | Stop
        fn argv(self, rt: &str, id: &str) -> Vec<String>; // logs: [rt, "logs", "--tail", "200", id]
        fn timeout(self) -> Duration;         // 30 s; Logs 10 s
    }
    pub(super) struct Prompt { verb: Verb, id: String, name: String, drawn: bool, armed_at: Option<Instant> }
    pub(super) fn failure(verb: Verb, out: &RunOutput) -> Option<String>; // None on exit 0
    pub(super) fn audit_entry(verb: Verb, id: &str, name: &str, argv: Vec<String>, out: &RunOutput, ms: u64) -> AuditEntry;
    ```
  - `Tab::on_cargo_done(&mut self, _seq: u64, _out: crate::tools::runner::RunOutput, _ms: u64) {}` in `app::tabs`.
  - `App::on_cargo_done(&mut self, seq: u64, out: RunOutput, ms: u64)` routes to `TabId::Cargo`.
  - `run::Event::CargoDone { seq: u64, output: RunOutput, ms: u64 }`. The event loop calls `app.on_cargo_done` and sets `dirty = true`.
  - `fn build_cargo(tx: UnboundedSender<Event>, audit: SharedAudit) -> CargoTab` in `run.rs`. Its `run` closure does `tokio::task::spawn_blocking`, then `SystemRunner.run`/`run_merged`, then sends `Event::CargoDone`. `now` is `Instant::now`; `timestamp` is `chrono::Local::now().to_rfc3339()`.
- Messages (spec):
  - progress: `"{PROGRESS} {NAME} ..."`;
  - success: `"{NAME} {DONE}"`;
  - prompt: `"{VERB} {NAME}? Y/N"`, verb upper-cased;
  - failures: `"{VERB} FAILED: {first non-empty line of stderr, else stdout, upper-cased}"`, `"RUNTIME NOT FOUND"`, `"{VERB} TIMED OUT"`;
  - others: `"BUSY"`, `"{NAME} GONE"`, `"TELEMETRY OFFLINE"`.

  Errors are reverse video. A result or error message stays until the next key.
- Pending state: `action: Option<(u64, Verb, String /*id*/, String /*name*/, Vec<String> /*argv*/)>` and `refresh: Option<u64>`.
  - A snapshot clears `refresh` and drops a prompt whose ID is gone.
  - A done U/R/D result starts the refresh (`ps_argv(rt)`, 2 s, unmerged) whether it succeeded or not.
  - The refresh result is parsed and sorted like a snapshot and replaces the list. A refresh failure is ignored.
  - A U/R/D/L result that arrives while obscured sets `activity`; `take_activity` returns it once.
  - Prompt arming copies `MuthurTab`'s semantics: `drawn` is set on the first unobscured render, `armed_at = now + ARMING` on the next, and `set_obscured(true)` resets both. Y counts only when `now >= armed_at`; N and Esc cancel at any time.

- [ ] **Step 1: Write failing tests** in `src/cargo/tests.rs`. Add a harness:
  ```rust
  type Calls = Rc<RefCell<Vec<(u64, Vec<String>, Duration, bool)>>>;
  struct H { t: CargoTab, calls: Calls, clock: Rc<Cell<Instant>>, audit_path: PathBuf, _dir: tempfile::TempDir }
  fn harness(list: Vec<Container>) -> H; // runtime "docker"; on_metrics(list); one render
  fn arm(h: &mut H);                     // render, render, advance clock by ARMING
  fn last_argv(h: &H) -> Vec<String>;
  ```
  Tests (names are fixed; assertions are the spec's values):
  - `start_runs_at_once_with_id_and_runtime`: `harness([web id "a1"])`, key `Char('U')`, then `last_argv == ["docker","start","a1"]`, timeout 30 s, merged false. The screen shows `STARTING WEB ...` and `STARTING...` in the row's STATUS column.
  - `restart_and_stop_need_armed_y`: key `r` shows `RESTART WEB? Y/N` and no call. `Char('y')` before `arm` makes no call. After `arm`, `Char('Y')` gives `["docker","restart","a1"]`. The same for `d` → `stop`.
  - `n_and_esc_cancel_any_time`: for `Char('n')` and `Esc`, the prompt is gone, there are no calls, and no audit line is written.
  - `obscured_render_clears_arming`: `arm`, then `set_obscured(true)`, then `set_obscured(false)`, then `Char('y')` makes no call. After `arm` again, `y` makes the call.
  - `confirm_targets_pinned_id_after_resort`: list `[a1 web, b2 db]`; select `db`, press `d`, `arm`. A snapshot then puts an EXITED `c3` first; `Y` gives `["docker","stop","b2"]`.
  - `vanished_target_drops_prompt`: a prompt for `db`, then a snapshot without `b2` shows `DB GONE`, and Y makes no call.
  - `busy_while_in_flight`: `U` then `R` shows `BUSY` with one call; arrows still move the selection.
  - `success_message_then_refresh`: `U`, then `on_cargo_done(seq, Exited{0,..}, 12)` shows `WEB STARTED`. The next call is `ps_argv("docker")` with 2 s. Its result `"a1\tweb\tUp 1 second\n"` updates the row to `UP`.
  - `errors_clear_busy_and_show_reverse`: three cases on `D`+arm+`Y`. `Exited{1, stderr:"Error: no such container\n"}` shows `STOP FAILED: ERROR: NO SUCH CONTAINER`. `NotFound` shows `RUNTIME NOT FOUND`. `TimedOut` shows `STOP TIMED OUT`. Each message cell is `REVERSED`, and a following `U` issues a new call (not `BUSY`).
  - `stale_seq_ignored`: `on_cargo_done(999, ..)` changes nothing.
  - `actions_ignored_when_empty`: on `HOLD EMPTY` and on no runtime, `U/R/D/L` make no calls.
  - `audit_entries_per_action`: a confirmed stop and an auto start write two lines to `audit.jsonl`, with `"tool":"cargo_stop"` + `"decision":"confirmed"` + `"tier":2` and `"tool":"cargo_start"` + `"decision":"auto"` + `"tier":1`. A cancelled prompt writes none.
  - `offline_keeps_list_and_actions`: `on_metrics_offline("X")` after a list keeps `>WEB` on screen with `TELEMETRY OFFLINE` on the message line, and `U` still issues a call.
  - `background_done_lights_activity`: `U`, then `set_obscured(true)` + `on_cargo_done` makes `take_activity()` true once.

  In `src/app/tests.rs`: `cargo_done_reaches_cargo` (a `Spy` in the `TabId::Cargo` slot counts `on_cargo_done`).
- [ ] **Step 2:** Run `cargo test --lib cargo`. Expected: compile errors, then failures.
- [ ] **Step 3: Implement** `action.rs`, the tab state machine in `mod.rs`, the trait hook, `App::on_cargo_done`, `Event::CargoDone` and `build_cargo`. In `run()`, open the audit log once, pass clones to `build_muthur` and `build_cargo`, and replace `CargoTab::detached()` with `build_cargo(..)`. With telemetry offline and a list already shown, the tab keeps the list and shows `TELEMETRY OFFLINE` on the message line; without a list it shows the existing offline panel.
- [ ] **Step 4:** Run `cargo test && cargo clippy --all-targets`. Expected: green.
- [ ] **Step 5: Commit** — `feat: start/restart/stop containers from CARGO, audited`

---

### Task 6: Log view

**Files:**
- Create: `src/cargo/logview.rs`
- Modify: `src/cargo/mod.rs`, `src/cargo/tests.rs`

**Interfaces:**
- Consumes: `Verb::Logs` and the pending-action handling (Task 5).
- Produces (`cargo::logview`):
  ```rust
  pub(super) fn clean(raw: &str) -> String;                  // spec cleaning rules
  pub(super) fn wrap(text: &str, width: usize) -> Vec<String>; // by chars; empty line kept
  pub(super) struct LogView { id: String, name: String, lines: Vec<String> /* cleaned, unwrapped */, scroll_from_bottom: usize }
  impl LogView {
      pub(super) fn new(id: String, name: String, raw: &str) -> Self;
      pub(super) fn key(&mut self, key: &Key, page: usize); // Up/Down/PageUp/PageDown/Home
      pub(super) fn render(&mut self, area: Rect, buf: &mut Buffer);
  }
  ```
- Behaviour:
  - **Title:** `LOGS - {NAME} ({n} LINES)`, where `n` is cleaned lines before wrapping.
  - **Hint:** the last inner row is `L REFRESH  ESC BACK  SH-F15/16 SCROLL`.
  - **Empty logs:** show `NO LOG OUTPUT`.
  - **Scrolling:** `scroll_from_bottom = 0` means the bottom.
- `L` on the list starts `Verb::Logs` (10 s, merged = true). On exit 0 the tab opens `LogView`; on failure it stays on the list with `LOGS FAILED: …`.
- In the view:
  - `L` re-fetches the same ID and replaces `lines` at the bottom when it returns.
  - `Esc` closes the view, keeps the selection and clears the pending log `seq`.
  - `U/R/D` do nothing.
- Logs are audited as `cargo_logs`, tier 1, `auto`.

- [ ] **Step 1: Write failing tests**
  In `logview.rs`:
  ```rust
  #[test]
  fn clean_strips_ansi_cr_tabs_controls() {
      assert_eq!(clean("\x1b[31mred\x1b[0m\r\nA\tB\nbar\r50%\x07\x1b]0;title\x07end\n"), "red\nA       B\nbar50%end\n");
  }
  #[test]
  fn wrap_never_exceeds_width() {
      let w = wrap(&"x".repeat(130), 59);
      assert_eq!(w.iter().map(|l| l.chars().count()).collect::<Vec<_>>(), vec![59, 59, 12]);
      assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
  }
  ```
  In `src/cargo/tests.rs`:
  - `logs_open_at_bottom_and_scroll`: `L` gives argv `["docker","logs","--tail","200","a1"]`, merged true, 10 s. `on_cargo_done` with `stdout` of 100 numbered lines `line000..line099` shows the title `LOGS - WEB (100 LINES)` with `line099` visible and `line000` not. `PageUp` shows earlier lines, and `Home` shows `line000`.
  - `logs_keep_case_and_wrap`: one line of 130 chars shows no row wider than the pane (as in `lists_containers_with_status_column`, nothing escapes `WINDOWED`).
  - `l_refreshes_and_esc_returns_with_selection`: in the view, `L` issues a second logs call for the same ID. `Esc` shows the list again with `>WEB`.
  - `stale_log_result_after_esc_is_dropped`: `L`, `Esc`, then `on_cargo_done(log_seq, Exited{0,"x"})` keeps the list on screen.
  - `logs_failed_stays_on_list`: `Exited{1, stdout: "Error: No such container: a1\n"}` (merged, so the error is in stdout) shows `LOGS FAILED: ERROR: NO SUCH CONTAINER: A1` on the list screen.
  - `empty_logs_say_so`: `Exited{0, ""}` shows `NO LOG OUTPUT`.
- [ ] **Step 2:** Run `cargo test --lib cargo`. Expected: failures.
- [ ] **Step 3: Implement.** `clean` handles:
  - `ESC [` … up to a final byte in `@`–`~`;
  - `ESC ]` … up to `BEL` or `ESC \`;
  - any other `ESC x` pair, dropped;
  - `\r\n` → `\n`, and a lone `\r` dropped;
  - `\t` → spaces to the next multiple of 8, counted from the line start;
  - other chars `< 0x20` (except `\n`) and `0x7f`–`0x9f`, dropped.
- [ ] **Step 4:** Run `cargo test && cargo clippy --all-targets`. Expected: green.
- [ ] **Step 5: Commit** — `feat: CARGO log view`

---

### Task 7: Golden screens and docs

**Files:**
- Modify: `tests/golden.rs`, `tests/golden/*.txt`, `docs/ARCHITECTURE.md`

**Interfaces:**
- Consumes: the `CargoTab` harness pattern from Task 5. For goldens, build `CargoTab::new(deps, shared(AuditLog::offline()))` with a fake `run` that records into an `Rc<RefCell<Vec<u64>>>`, so the test can feed `on_cargo_done` the right `seq`.

- [ ] **Step 1: Add golden scenes** next to `windowed_cargo`. Each sets up 50 `REFINERY-UNIT-NN` containers as now, with `container_runtime: Some("docker".into())`, `id: format!("{i:012x}")`, and real-case names (`refinery-unit-NN`).
  - `cargo_stop_prompt`: Down ×2, then `D`, then one render. Row 0 is the EXITED unit-03, so this selects unit-01, and the golden shows `STOP REFINERY-UNIT-01? Y/N`.
  - `cargo_logs`: `L`, then `on_cargo_done(seq, Exited{0, <40 lines, with one 150-char line and one line containing "\x1b[32mOK\x1b[0m">})`, then render.

  All screens go through `StrictWriter`, with `violations.is_empty()`.
- [ ] **Step 2:** Run `UPDATE_GOLDEN=1 cargo test --test golden`, then **read** `cargo_stop_prompt.txt` and `cargo_logs.txt`. Check: the prompt is on the message line; the log text keeps its case; the long line wraps inside the pane; `OK` appears with no escape residue; the title reads `LOGS - REFINERY-UNIT-03 (40 LINES)` (the EXITED unit sorts first and starts selected).
- [ ] **Step 3: Docs.** In `docs/ARCHITECTURE.md`:
  - **Cargo tab section:** add selection, U/R/D/L, the Y/N arming for restart and stop, the log view, and audit.
  - **Module table:** add `tabs::cargo` → "Container manifest and actions (see below)".
  - **Keys:** add `U / R / D / L` under the CARGO folder.
  - **Deferred list:** add the spec's out-of-scope items.
- [ ] **Step 4:** Run `cargo test && cargo clippy --all-targets`. Expected: green.
- [ ] **Step 5: Commit** — `test: CARGO prompt and log goldens; docs: CARGO actions`
