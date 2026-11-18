# M3 — Utility Tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The UTILITY folder is a live system monitor in the personal-terminal style. Boxed panels show:
- system (host, uptime, load);
- CPU (total gauge, per-core bars, history sparkline);
- memory and swap;
- disks;
- network (rates and sparkline);
- top processes;
- services (failed systemd units, containers).

It refreshes every 2 s and sends only diffs to the WY-60.

**Architecture:**
- **Collecting.** `metrics::MetricsSource` produces a `Snapshot` (plain data). `LocalSource` fills it from `sysinfo` 0.40 plus two slow external probes, re-run at most every 10 s:
  - failed units, via `systemctl` (absent on Alpine/OpenRC, which gives `None`);
  - containers, via `docker`/`podman`.
- **Delivery.** A collector thread sends `Event::Metrics(Box<Snapshot>)` on the existing event channel every `interval`.
- **Display.** `UtilityTab` keeps the latest snapshot plus history rings and renders panels with small custom widgets:
  - a gauge built from `█`, which the backend turns into reverse-video spaces;
  - a 5-level sparkline from the DEC scan-line glyphs `⎽⎼─⎻⎺`, all VT100-safe.
- **Reuse.** M5's MU/TH/UR tools will read the same `MetricsSource`.

**Tech Stack:** existing crates plus `sysinfo = "0.40"`. Note that in 0.40 `System::new_all()`, `Disks/Networks::new_with_refreshed_list()`, `System::uptime()` and `System::load_average()` return `Result`.

**Spec:** `docs/ARCHITECTURE.md` (Utility tab, Metrics, background tabs/activity).

## Scope notes
- **Layout.** Two columns when the area is at least 78 wide (fullscreen); one stacked column otherwise (windowed 61×40). Panels that don't fit are dropped from the bottom of their column. They are never squashed below their minimum height.
- **Activity marker.** The UTILITY folder's `*` lights when the failed-unit count *rises*.
- **Config `[utility]`:**
  - `interval_secs = 2`
  - `containers = "auto"` (`"auto" | "docker" | "podman" | "none"`; auto tries docker, then podman; a command that errors counts as unavailable)
  - `exclude_fs = ["tmpfs","devtmpfs","efivarfs","overlay","squashfs","proc","sysfs","cgroup2","ramfs","autofs"]`
- **Temperatures are out of scope.** They weren't in the agreed panel list.

## Global Constraints
- **Text.** Everything rendered is uppercase ASCII, or a glyph `map_glyph` maps to non-`?`.
- **Collection timing.** Collection never blocks the event loop. External commands run in the collector thread with a **2 s timeout**, and are skipped (giving `None`) if the binary is missing or exits non-zero.
- **Refresh interval:** 2 s by default.

## Review Focus
1. **Alpine host (no `systemctl`, no docker).** Panels show `N/A`, never an error or a crash. Test: Task 2 `missing_binary_is_none`, Task 5 `services_panel_handles_none`.
2. **A 32+ core machine in the windowed pane.** The per-core bars must fit (multi-column) without overflowing the panel. Test: Task 5 `many_cores_fit`.
3. **Before the first snapshot.** The tab shows `AWAITING TELEMETRY`, not empty or zero gauges. Test: Task 5 `awaiting_before_first_snapshot`.
4. **A hung `docker ps`** (daemon down). The collector thread survives and the rest of the snapshot still arrives. Test: Task 2 `timeout_is_none`.
5. **Values out of range.** CPU above 100 %, or a disk with total 0. Gauges clamp, with no panic and no division by zero. Test: Task 4 `gauge_clamps`.

---

## File Structure

| File | Responsibility |
|---|---|
| `src/metrics/mod.rs` | `Snapshot` and sub-structs, `MetricsSource` trait, `History`, formatting helpers. |
| `src/metrics/probes.rs` | `run_probe` (command + timeout), `parse_failed_units`, `parse_containers`. |
| `src/metrics/local.rs` | `LocalSource` (sysinfo + probes). |
| `src/utility/widgets.rs` | `gauge_line`, `sparkline`, panel helpers. |
| `src/utility/mod.rs` | `UtilityTab`: snapshot + histories, layout, panels, activity. |
| `src/run.rs`, `src/app/mod.rs`, `src/config.rs` | Collector thread, `Event::Metrics`, app wiring, `[utility]` config. |

---

### Task 1: Metrics model and formatting

**Files:** create `src/metrics/mod.rs`; add `pub mod metrics;` to `lib.rs`.

**Interfaces — Produces:**
```rust
pub struct Snapshot {
    pub host: String, pub uptime_secs: u64, pub load: [f64; 3],
    pub cpu_total: f32, pub cpu_cores: Vec<f32>,
    pub mem: Usage, pub swap: Usage, pub disks: Vec<Disk>,
    pub net: NetRate, pub top: Vec<Proc>,
    pub failed_units: Option<Vec<String>>, pub containers: Option<Vec<Container>>,
}
pub struct Usage { pub used: u64, pub total: u64 }            // bytes
pub struct Disk { pub mount: String, pub usage: Usage }
pub struct NetRate { pub rx_bps: u64, pub tx_bps: u64 }       // bytes/s
pub struct Proc { pub pid: u32, pub name: String, pub cpu: f32, pub mem: u64 }
pub struct Container { pub name: String, pub status: String }
pub trait MetricsSource { fn snapshot(&mut self) -> anyhow::Result<Snapshot>; }
```
All derive `Clone, Debug, PartialEq, Default`.

**Helpers:**
- `pub fn human_bytes(b: u64) -> String`: binary units, one decimal, uppercase. `1536` → `"1.5K"`, `0` → `"0B"`, `16_106_127_360` → `"15.0G"`.
- `pub fn human_rate(bps: u64) -> String`: `human_bytes` + `"/S"`.
- `pub fn human_uptime(secs: u64) -> String`: `"12D 04:13"`, and `"04:13"` when under a day.
- `impl Usage { pub fn percent(&self) -> f32 }`: 0 when `total` is 0.
- `pub struct History { cap: usize, values: VecDeque<f32> }` with `new(cap)`, `push(v)` (drops the oldest past `cap`), and `values() -> impl Iterator<Item=f32>`.

- [ ] **Step 1: Failing tests**:
```rust
#[test] fn bytes() { assert_eq!(human_bytes(0), "0B"); assert_eq!(human_bytes(1536), "1.5K"); assert_eq!(human_bytes(16_106_127_360), "15.0G"); assert_eq!(human_bytes(999), "999B"); }
#[test] fn rate() { assert_eq!(human_rate(2048), "2.0K/S"); }
#[test] fn uptime() { assert_eq!(human_uptime(3600*4+60*13), "04:13"); assert_eq!(human_uptime(86400*12+3600*4+60*13), "12D 04:13"); }
#[test] fn percent_handles_zero_total() { assert_eq!(Usage{used:5,total:0}.percent(), 0.0); assert_eq!(Usage{used:1,total:4}.percent(), 25.0); }
#[test] fn history_caps() { let mut h = History::new(3); for v in 0..5 { h.push(v as f32) } assert_eq!(h.values().collect::<Vec<_>>(), vec![2.0,3.0,4.0]); }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: metrics model and formatting helpers`

---

### Task 2: External probes

**Files:** create `src/metrics/probes.rs`.

**Interfaces — Produces:**
- `pub fn run_probe(cmd: &str, args: &[&str], timeout: Duration) -> Option<String>`:
  - Spawns with stdin null, stdout piped and stderr null.
  - Waits up to `timeout` (poll `try_wait` every 20 ms), then kills, reaps and returns `None`.
  - Also returns `None` when the binary is missing or the command exits non-zero.
  - Otherwise returns stdout as lossy UTF-8.
- `pub fn parse_failed_units(out: &str) -> Vec<String>`: input is `systemctl --failed --plain --no-legend` output. Output is the first whitespace-separated field of each non-empty line, uppercased, with `.service` stripped (other unit suffixes kept).
- `pub fn parse_containers(out: &str) -> Vec<Container>`:
  - Input is the output of `ps --format '{{.Names}}\t{{.Status}}'`.
  - Split each line on the first tab.
  - Uppercase both fields.
  - Shorten the status to its first word (`"Up 3 hours"` → `"UP"`, `"Exited (0) 2 days ago"` → `"EXITED"`).

- [ ] **Step 1: Failing tests**:
```rust
#[test] fn parses_failed_units() { assert_eq!(parse_failed_units("autorandr.service loaded failed failed autorandr hook\nfoo.mount loaded failed failed x\n\n"), vec!["AUTORANDR", "FOO.MOUNT"]); }
#[test] fn parses_containers() { assert_eq!(parse_containers("web\tUp 3 hours\ndb\tExited (0) 2 days ago\n"), vec![Container{name:"WEB".into(),status:"UP".into()}, Container{name:"DB".into(),status:"EXITED".into()}]); }
#[test] fn probe_returns_stdout() { assert_eq!(run_probe("sh", &["-c","echo hi"], Duration::from_secs(2)).as_deref(), Some("hi\n")); }
#[test] fn missing_binary_is_none() { assert_eq!(run_probe("definitely-not-a-binary-muthur", &[], Duration::from_secs(1)), None); }
#[test] fn failure_is_none() { assert_eq!(run_probe("sh", &["-c","exit 1"], Duration::from_secs(1)), None); }
#[test] fn timeout_is_none() { let t = Instant::now(); assert_eq!(run_probe("sleep", &["5"], Duration::from_millis(200)), None); assert!(t.elapsed() < Duration::from_secs(1)); }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: external metric probes with timeout`

---

### Task 3: LocalSource

**Files:** create `src/metrics/local.rs`.

**Interfaces:**
- Consumes the Task 1 model and Task 2 probes.
- Produces `pub enum ContainerRuntime { Auto, Docker, Podman, None }` (serde, kebab-case).
- Produces `pub struct LocalSource`, with `pub fn new(runtime: ContainerRuntime, exclude_fs: Vec<String>) -> anyhow::Result<Self>` and `impl MetricsSource`.

**What it collects:**
- **CPU and memory:** refresh CPU usage, memory, processes and networks on every call.
- **Network:** `NetRate` comes from received/transmitted deltas since the previous call, divided by the elapsed time. Interface `lo` is excluded, and the first call gives 0.
- **Disks:** filesystems whose type is not in `exclude_fs`, with `total > 0`. Duplicate mount sources are dropped. Mounts are sorted with `/` first, then alphabetically.
- **Processes:** the top 5 by CPU, with ties broken by memory. Names are uppercased and truncated to 15 characters.
- **Host:** the hostname, uppercased.
- **Failed units and containers:**
  - Refreshed only when 10 s have passed since the last probe run; the cached values are reused in between.
  - Failed units: `systemctl --failed --plain --no-legend`.
  - Containers: `docker ps --format …`, or `podman ps --format …` for Auto when docker fails.
  - Each yields `None` when unavailable.

- [ ] **Step 1: Failing tests** (run against this machine; assert only what is universally true):
```rust
#[test] fn snapshot_has_basics() {
    let mut s = LocalSource::new(ContainerRuntime::None, default_exclude()).unwrap();
    let a = s.snapshot().unwrap();
    assert!(!a.host.is_empty() && a.host == a.host.to_uppercase());
    assert!(!a.cpu_cores.is_empty() && a.mem.total > 0 && a.uptime_secs > 0);
    assert!(a.disks.iter().any(|d| d.mount == "/"));
    assert!(a.top.len() <= 5);
    assert_eq!(a.containers, None);
    assert_eq!(a.disks[0].mount, "/");
}
#[test] fn second_snapshot_has_rates() { /* two snapshots 200ms apart both Ok; net rates are finite (no panic on delta) */ }
```
- [ ] **Step 2–4:** FAIL → implement → pass. Put `pub fn default_exclude() -> Vec<String>` in `local.rs`; config's default uses it.
- [ ] **Step 5: Commit** `feat: local metrics source`

---

### Task 4: Widgets

**Files:** create `src/utility/widgets.rs` and `src/utility/mod.rs` (`pub mod widgets;` for now); add `pub mod utility;` to `lib.rs`.

**Interfaces — Produces:**
- `pub fn gauge_line(label: &str, pct: f32, detail: &str, width: u16) -> Line<'static>`:
  - Layout: `"{label:<5}[{bar}] {pct:>3}% {detail}"`, truncated to `width`.
  - `pct` is clamped to 0–100.
  - The bar is `█` × filled plus spaces, with bar width = `width − 5 − 2 − 6 − detail.len() − 1`, minimum 4.
- `pub fn sparkline(values: &[f32], max: f32, width: u16) -> String`:
  - Uses the last `width` values, right-aligned (left padded with spaces).
  - Each value is scaled to 0..=4 against `max` (`max ≤ 0` → treat every value as 0).
  - Level glyphs are `⎽ ⎼ ─ ⎻ ⎺`.
- `pub fn panel(title: &str) -> Block<'static>`: bordered, with title `" {TITLE} "`.

- [ ] **Step 1: Failing tests**:
```rust
#[test] fn gauge_clamps() { let l = gauge_line("CPU", 250.0, "", 30).to_string(); assert!(l.contains("100%")); let l = gauge_line("DISK", -3.0, "", 30).to_string(); assert!(l.contains("  0%") && !l.contains('█')); }
#[test] fn gauge_fills_proportionally() { let s = gauge_line("MEM", 50.0, "1.0G/2.0G", 40).to_string(); let filled = s.matches('█').count(); let bar = s[s.find('[').unwrap()+1..s.find(']').unwrap()].chars().count(); assert_eq!(filled, bar / 2); assert!(s.ends_with("1.0G/2.0G")); }
#[test] fn sparkline_levels_and_width() { assert_eq!(sparkline(&[0.0, 25.0, 50.0, 75.0, 100.0], 100.0, 7), "  ⎽⎼─⎻⎺"); assert_eq!(sparkline(&[1.0; 10], 0.0, 3), "⎽⎽⎽"); }
#[test] fn widgets_render_dialect_safe() { for ch in sparkline(&[0.,50.,100.], 100., 3).chars() { assert_ne!(crate::dialect::map_glyph(&ch.to_string()), crate::dialect::Glyph::Ascii(b'?')); } }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: utility gauge and sparkline widgets`

---

### Task 5: UtilityTab

**Files:** `src/utility/mod.rs` (+ `src/utility/tests.rs`).

**Interfaces:**
- Consumes the Task 1 model and Task 4 widgets, plus `Tab`.
- Produces `pub struct UtilityTab` with:
  - `pub fn new() -> Self`
  - `pub fn on_snapshot(&mut self, s: Snapshot)`: stores it and pushes `cpu_total` and `rx+tx` into 60-sample histories. If the failed-unit count is higher than before, sets `activity`.
  - `impl Tab`: `take_activity`; `handle_key` ignores keys.

**Rendering** (`render(area, buf)`):
- Before the first snapshot: a `panel("UTILITY")` with `AWAITING TELEMETRY` centred.
- After that, these panels, each with its content height + 2:

  | Panel | Content |
  |---|---|
  | `SYSTEM - {host}` | Line 1: `UPTIME {human_uptime}   LOAD {l1:.2} {l5:.2} {l15:.2}` |
  | `CPU` | 1. `gauge_line("ALL", total, "", w)`. 2. Per-core rows `C00 [####  ] 45%`, packed into as many columns as fit, each column ≥ 18 wide. 3. `sparkline(cpu history, 100.0, w)`. |
  | `MEMORY` | `gauge_line("MEM", …, "{used}/{total}")` and `gauge_line("SWAP", …)`. If swap total is 0, the SWAP line reads `SWAP  NONE`. |
  | `DISKS` | One `gauge_line(mount truncated to 5, …, "{used}/{total}")` per disk, at most 6. |
  | `NETWORK` | `RX {rate}   TX {rate}`, then `sparkline(net history, max(history), w)`. |
  | `PROCESSES` | Header `PID     NAME            CPU%    MEM`, then up to 5 rows. |
  | `SERVICES` | Failed units in REVERSED, one per line, prefixed `FAILED `. If there are none: `ALL SYSTEMS NOMINAL`. If `None`: `UNITS N/A`. Then `CONTAINERS`, followed by `{name} {status}` lines, or `NONE`, or `N/A`. |

- **Columns:**
  - With `area.width >= 78`, two columns of `area.width/2`. Left: SYSTEM, CPU, MEMORY, NETWORK. Right: DISKS, PROCESSES, SERVICES.
  - Otherwise one column: SYSTEM, CPU, MEMORY, DISKS, NETWORK, PROCESSES, SERVICES.
  - The last panel in a column takes the remaining height (`Min`).
  - A panel whose minimum doesn't fit is not drawn.

- [ ] **Step 1: Failing tests** (`fn snap() -> Snapshot` builds a fixed fixture: 8 cores, 2 disks, 1 failed unit, containers `Some([WEB UP])`):
```rust
#[test] fn awaiting_before_first_snapshot() { /* render 61x40 → contains "AWAITING TELEMETRY" */ }
#[test] fn windowed_shows_all_panels() { /* on_snapshot(snap()); render Rect(19,2,61,40) → contains SYSTEM - HOST, CPU, MEMORY, DISKS, NETWORK, PROCESSES, SERVICES titles; "FAILED AUTORANDR" reversed; "WEB UP" */ }
#[test] fn fullscreen_uses_two_columns() { /* render 80x43 → "DISKS" title x-position ≥ 40 */ }
#[test] fn many_cores_fit() { /* 32 cores, windowed → every "C31" visible; no cell outside the area written (render into a larger buffer, compare outside region untouched) */ }
#[test] fn services_panel_handles_none() { /* failed_units: None, containers: None → "UNITS N/A" and "N/A" */ }
#[test] fn activity_on_new_failure() { /* snapshot with 0 failed then 1 failed → take_activity true once; same count again → false */ }
#[test] fn renders_only_dialect_safe_glyphs() { /* every symbol in the rendered buffer maps to non-'?' (or is '?') */ }
```
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: utility tab panels`

---

### Task 6: Wiring

**Files:** `src/config.rs`, `src/run.rs`, `src/app/mod.rs`, `src/app/tabs.rs`.

**Interfaces:**
- **Config:** `pub struct UtilitySection { pub interval_secs: u64, pub containers: ContainerRuntime, pub exclude_fs: Vec<String> }`, with defaults `2`, `Auto`, `default_exclude()`, exposed as `Config.utility`.
- **Run loop:**
  - `run::Event` gains `Metrics(Box<Snapshot>)`.
  - `pub fn spawn_metrics(source: Box<dyn MetricsSource + Send>, interval: Duration, tx: UnboundedSender<Event>) -> JoinHandle<()>` loops: snapshot, send, sleep. An `Err` is logged and the loop continues. A send failure means the app is gone, and the thread returns.
  - `event_loop` on `Metrics(s)` calls `app.on_metrics(*s)` and marks the screen dirty.
- **Tab trait:** add `fn on_metrics(&mut self, _s: &Snapshot) {}`. `UtilityTab` implements it by calling `on_snapshot(s.clone())`.
- **App:**
  - `App::with_shell` becomes `App::assemble(chrome, utility: UtilityTab, shell: ShellTab)`, keeping the MU/TH/UR placeholder. Update its callers: `run`, the app tests, and the golden test.
  - `pub fn on_metrics(&mut self, s: Snapshot)` routes to the Utility tab.
- **`run()`:** builds `LocalSource::new(cfg.utility.containers, cfg.utility.exclude_fs.clone())`. If that fails, it logs and runs without metrics, so the tab stays on AWAITING. It then calls `spawn_metrics` with the configured interval.

- [ ] **Step 1: Failing tests**:
```rust
// run.rs
#[tokio::test] async fn metrics_event_reaches_utility() { /* App::assemble(..) with UtilityTab::new(); send Event::Metrics(Box::new(fixture with host "NOSTROMO")) then quit; drawn output contains "NOSTROMO" */ }
#[test] fn collector_sends_snapshots() { /* FakeSource returning fixed Snapshot; spawn_metrics(…, 10ms, tx); receive 2 Metrics events within 1s */ }
#[test] fn collector_survives_errors() { /* FakeSource alternating Err/Ok → still receives an Ok snapshot */ }
// config.rs
#[test] fn utility_defaults() { /* interval 2, containers Auto, exclude contains "tmpfs" */ }
```
- [ ] **Step 2–4:** FAIL → implement → pass. Regenerate any golden that changes because Utility is no longer a placeholder (`windowed_utility`, `prefix_strip`) using the "AWAITING TELEMETRY" state. Then **read** them.
- [ ] **Step 5: Commit** `feat: wire utility tab and metrics collector`

---

### Task 7: End-to-end verification

- [ ] **Step 1: New golden `utility_fullscreen`.** Use the fixture snapshot and `^] f`. **Read it** and check the two-column layout and gauges against the Task 5 spec.
- [ ] **Step 2: pty smoke** (release musl, `--strict`, `[shell] command="sh"`, scratch `XDG_*`):
  1. Start and wait 5 s. The output must contain `UPTIME` and the real hostname, uppercased.
  2. Measure the bytes sent in a 6 s idle window (3 refreshes) and report bytes per refresh. Expect it to be well under one repaint.
  3. `^] f`, then `^] q`.

  Expected: exit 0, ICANON restored, and zero strict violations.
- [ ] **Step 3:** `cargo test`, `cargo clippy --all-targets -- -D warnings`, and the musl static build.
- [ ] **Step 4: Commit** `test: utility golden and end-to-end verification`
