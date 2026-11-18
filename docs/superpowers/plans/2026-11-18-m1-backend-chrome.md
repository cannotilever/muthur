# M1 — Dialect, Backend, Chrome & Tab Switching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Running `muthur` on the WY-60 shows the Alien-Isolation-style chrome (title bar with ship name and in-universe date, sidebar of boxed folders, footer), switches between three placeholder tabs with F1/F2/F3, toggles per-tab fullscreen with `^] f`, and quits with `^] q`. Every byte on the wire is from the VT100 allowlist.

**Architecture:** One output path: ratatui renders into a buffer, and `Wy60Backend<W>` turns ratatui's cell diff into VT100 bytes through a `Dialect`. Unicode box glyphs map to DEC Special Graphics, block glyphs map to reverse-video spaces, and anything else non-ASCII becomes `?`. A `StrictWriter` can wrap the link and reject any sequence the dialect doesn't allow. An input thread turns link bytes into `Key`s via a keymap (defaults from `docs/hardware.md`). A tokio main loop feeds keys to `App` (tabs, prefix state machine, fullscreen state) and redraws only when the kernel's output queue has drained (`TIOCOUTQ`), so a slow link never builds a backlog.

**Tech Stack:** Rust 2024, ratatui 0.30 (no crossterm), tokio (rt, macros, sync, time), chrono, tracing + tracing-subscriber + tracing-appender, vt100 0.18 (dev-dep, round-trip tests).

**Spec:** `docs/ARCHITECTURE.md` (Screen layout, Keys, Modules: link/dialect/backend/input/app, Testing & development) and `docs/hardware.md` (measured key codes and capabilities).

## Scope notes

- **DECSTBM scroll-region optimisation is deferred to M2.** It only pays off for scrolling content (Shell/chat), and ratatui's diff can't express scrolling, so it needs the Shell tab to exist first. `Dialect` already has the emitters.
- **Top-to-bottom painting** falls out of ratatui's row-major diff order. Nothing extra is needed.
- **`^] q` (quit)** is new; the spec had no way to exit. `^] d` is the spec's "real date one key away".

## Global Constraints

- Everything written to the wire is 7-bit and from the `Vt100Dialect` allowlist (Task 2). No `?25l`, no colour SGRs, no `ESC[?...h`.
- Geometry from config, default **80×43**; `--cols`/`--rows` override (global flags).
- Flow control **XON/XOFF**; keys Ctrl-S/Ctrl-Q are never available.
- Keys (from `docs/hardware.md`): F1 `ESC[?5i`, F2 `ESC[?3i`, F3 `ESC[2i`; PgUp = Shift-F15 `ESC[5~`, PgDn = Shift-F16 `ESC[6~`; Backspace `^H`; delete-char = PF2 `ESC O Q`.
- Prefix key **Ctrl-] (0x1d)**. Prefix commands: `f` fullscreen, `r` redraw, `]` literal ^], `?` help, `q` quit, `d` real-date toggle; `p` and `[` reserved for the Shell tab (M2) and show `NOT AVAILABLE`.
- All tabs start **windowed**; fullscreen state is per tab.
- Logs go to `~/.local/state/muthur/muthur.log`. Nothing may log to stdout/stderr while the TUI runs.
- Config keeps the `0600` check from M0.

## Review Focus

1. **Bottom-right cell** (row 43, col 80): writing it must not scroll the screen. Test: Task 4 `bottom_right_cell_does_not_scroll`.
2. **Split escape sequence across reads** (e.g. `ESC [` then `? 5 i` 10 ms later): it must still be F1, not ESC + garbage. Test: Task 6 `split_sequence_within_gap_is_one_key`.
3. **Unknown key sequence** (a key not in the keymap): it is dropped and logged, and never shows up as a stray ESC that breaks the prefix state machine. Test: Task 6 `unknown_sequence_is_unknown_key`.
4. **Link hangup while the TUI runs**: MUTHUR exits cleanly (termios restored), no spin. Test: Task 8 `hangup_event_quits`.
5. **A non-ASCII symbol in any widget** (e.g. `é`, emoji): it must reach the wire as `?`, never as UTF-8. Test: Task 4 `non_ascii_becomes_question_mark`.

---

## File Structure

| File | Responsibility |
|---|---|
| `src/config.rs` | + `TerminalConfig {cols, rows}`, `ChromeConfig {ship_name, year_offset}`, `keys: BTreeMap<String,String>` overrides. |
| `src/dialect.rs` | `Attrs`, `Charset`, `Glyph`, `map_glyph`, `Dialect` trait, `Vt100Dialect` (emitters + allowlist). |
| `src/strict.rs` | `StrictWriter<W>`: tokenises outgoing bytes and checks them against the dialect. |
| `src/backend.rs` | `Wy60Backend<W>: ratatui::backend::Backend`. |
| `src/link.rs` | + `LinkReader` (dup'd fd implementing `ReadTimeout`), `Link::output_queue()`. |
| `src/input.rs` | `Key`, `Keymap` (defaults + overrides), incremental `KeyParser`. |
| `src/app/mod.rs` | `App`: tab focus, prefix state machine, fullscreen, quit/redraw flags, `handle_key`, `render`. |
| `src/app/chrome.rs` | Title bar, sidebar, connector, footer, prefix strip, help overlay. |
| `src/app/tabs.rs` | `Tab` trait, `TabId`, `PlaceholderTab`. |
| `src/run.rs` | Logging setup, input thread, tokio event loop, draw scheduling, teardown. |
| `src/main.rs` | Global `--cols/--rows/--strict`; no subcommand → `run::run`. |
| `tests/golden.rs` + `tests/golden/*.txt` | Full-app screen snapshots via vt100 round-trip. |

---

### Task 1: Config additions and global geometry flags

**Files:** Modify `src/config.rs`, `src/main.rs`; Cargo.toml (add deps for later tasks now: `ratatui = { version = "0.30", default-features = false, features = ["std", "layout-cache"] }`, `tokio = { version = "1", features = ["rt", "macros", "sync", "time"] }`, `chrono = "0.4"`, `tracing = "0.1"`, `tracing-subscriber = "0.3"`, `tracing-appender = "0.2"`; nix feature `"hostname"`; dev-dep `vt100 = "0.18"`).

**Interfaces — Produces:**
- `pub struct TerminalConfig { pub cols: u16, pub rows: u16 }` defaults `80`, `43`.
- `pub struct ChromeConfig { pub ship_name: Option<String>, pub year_offset: i32 }`. Defaults: `None` (meaning `"USCSS " + HOSTNAME uppercased`) and `111` (2026 → 2137, Alien: Isolation).
- `Config { link, terminal: TerminalConfig, chrome: ChromeConfig, keys: BTreeMap<String, String> }` (all `#[serde(default)]`).
- CLI: `--cols`, `--rows` become **global** `Option<u16>` overriding `cfg.terminal`; `testcard` uses `cfg.terminal` instead of its own flags. New global `--strict` flag (bool).

- [ ] **Step 1: Failing tests** in `config.rs`:
```rust
#[test] fn terminal_and_chrome_defaults() {
    let c = Config::default();
    assert_eq!((c.terminal.cols, c.terminal.rows), (80, 43));
    assert_eq!(c.chrome.year_offset, 111);
    assert!(c.chrome.ship_name.is_none() && c.keys.is_empty());
}
#[test] fn keys_section_parses_escapes() {
    let (_d, p) = write_cfg("[keys]\nF1 = \"\\u001b[11~\"\n", 0o600);
    assert_eq!(load(&p).unwrap().keys["F1"], "\x1b[11~");
}
```
- [ ] **Step 2: Run** `cargo test config` → FAIL (missing fields).
- [ ] **Step 3: Implement** the config structs, then refactor `main.rs` flags. `muthur testcard --rows 24` must keep working, since `--rows` is now global.
- [ ] **Step 4: Run** `cargo test` → all pass, M0 tests included.
- [ ] **Step 5: Commit** `feat: terminal/chrome/keys config and global geometry flags`

---

### Task 2: Dialect

**Files:** Create `src/dialect.rs`.

**Interfaces — Produces:**
- `#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)] pub struct Attrs { pub bold: bool, pub dim: bool, pub underline: bool, pub blink: bool, pub reverse: bool }`; `impl From<ratatui::style::Modifier> for Attrs` (BOLD, DIM, UNDERLINED, SLOW_BLINK|RAPID_BLINK, REVERSED; everything else ignored).
- `#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum Charset { Ascii, Graphics }`.
- `#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum Glyph { Ascii(u8), Graphics(u8), ReverseSpace }`.
- `pub fn map_glyph(symbol: &str) -> Glyph`:
  - Printable ASCII (one char 0x20–0x7e) maps to `Ascii`.
  - Box drawing maps to `Graphics`; light, heavy, double and rounded variants all map to the same byte: `─━═`→`q`, `│┃║`→`x`, `┌┏╔╭`→`l`, `┐┓╗╮`→`k`, `└┗╚╰`→`m`, `┘┛╝╯`→`j`, `├┣╠`→`t`, `┤┫╣`→`u`, `┬┳╦`→`w`, `┴┻╩`→`v`, `┼╋╬`→`n`.
  - Other DEC graphics: `◆`→`` ` ``, `▒░▓`→`a`, `°`→`f`, `±`→`g`, `≤`→`y`, `≥`→`z`, `π`→`{`, `≠`→`|`, `£`→`}`, `·`→`~`.
  - Horizontal scan lines: `⎺`→`o`, `⎻`→`p`, `⎼`→`r`, `⎽`→`s`.
  - Blocks: `█▇▆▅▌▐▀▄`→`ReverseSpace`, `▁▂▃▏▎▍`→`Ascii(b' ')`.
  - Empty string (wide-char continuation) → `Ascii(b' ')`. Anything else → `Ascii(b'?')`.
- `pub trait Dialect`. The emitters append to `out: &mut Vec<u8>`; rows and columns are 0-based in, 1-based on the wire:
  - `fn cup(&self, out, row: u16, col: u16)`, `fn cuf(&self, out, n: u16)`
  - `fn sgr(&self, out, a: Attrs)`: always resets first, e.g. `ESC[0;1;7m`; plain `ESC[0m` when empty. Parameters 1=bold, 2=dim, 4=underline, 5=blink, 7=reverse.
  - `fn charset(&self, out, c: Charset)`: `ESC(B` / `ESC(0`.
  - `fn clear(&self, out)`: `ESC[0m ESC(B ESC[r ESC[2J ESC[H`.
  - `fn set_scroll_region(&self, out, top: u16, bottom: u16)`, `fn reset_scroll_region(&self, out)`
  - `fn is_allowed(&self, seq: &[u8]) -> bool`
- `pub struct Vt100Dialect;` Allowlist:
  - CSI with only digits/`;` as parameters and a final byte in `H m J K r C`. For `m`, the parameters must be ⊆ {0,1,2,4,5,7}; for `J`/`K`, ⊆ {0,1,2}.
  - `ESC ( 0`, `ESC ( B`, `ESC # 3/4/5/6`.
  - Everything else is disallowed, including any `?`-prefixed CSI.

- [ ] **Step 1: Failing tests**:
```rust
#[test] fn box_glyphs_map_to_dec() { assert_eq!(map_glyph("┌"), Glyph::Graphics(b'l')); assert_eq!(map_glyph("╯"), Glyph::Graphics(b'j')); assert_eq!(map_glyph("━"), Glyph::Graphics(b'q')); }
#[test] fn blocks_and_unknowns() { assert_eq!(map_glyph("█"), Glyph::ReverseSpace); assert_eq!(map_glyph("é"), Glyph::Ascii(b'?')); assert_eq!(map_glyph(""), Glyph::Ascii(b' ')); assert_eq!(map_glyph("A"), Glyph::Ascii(b'A')); }
#[test] fn emitters_are_one_based() { let mut o = vec![]; Vt100Dialect.cup(&mut o, 0, 0); Vt100Dialect.sgr(&mut o, Attrs { bold: true, reverse: true, ..Default::default() }); assert_eq!(o, b"\x1b[1;1H\x1b[0;1;7m"); }
#[test] fn allowlist() {
    let d = Vt100Dialect;
    for ok in [&b"\x1b[12;80H"[..], b"\x1b[0;1;4;5;7m", b"\x1b[m", b"\x1b[2J", b"\x1b[r", b"\x1b[3;20r", b"\x1b(0", b"\x1b#6", b"\x1b[5C"] { assert!(d.is_allowed(ok), "{ok:?}"); }
    for bad in [&b"\x1b[?25l"[..], b"\x1b[38;5;2m", b"\x1b[31m", b"\x1b]0;x\x07", b"\x1b[3~", b"\x1b(A"] { assert!(!d.is_allowed(bad), "{bad:?}"); }
}
```
- [ ] **Step 2: Run** `cargo test dialect` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → pass.
- [ ] **Step 5: Commit** `feat: VT100 dialect, glyph mapping and allowlist`

---

### Task 3: StrictWriter

**Files:** Create `src/strict.rs`.

**Interfaces:**
- Consumes `Dialect`.
- Produces `pub struct StrictWriter<W, D> { inner: W, dialect: D, pending: Vec<u8>, pub violations: Vec<Vec<u8>> }`, `impl Write`, and `pub fn new(inner: W, dialect: D) -> Self`.
  - Bytes pass through to `inner` unchanged; strict mode reports problems, it never filters.
  - Allowed: CR, LF, BS, and printable ASCII.
  - Each complete escape sequence is checked with `is_allowed`.
  - A sequence split across `write` calls is buffered in `pending` until it is complete.
  - Violations: a disallowed sequence, a byte ≥ 0x80, or any other C0 control byte. Each one is pushed to `violations` and logged with `tracing::error!`. With `cfg(test)` or `debug_assertions` plus the env var `MUTHUR_STRICT_PANIC=1`, it panics instead.
- Tokeniser: `ESC [` runs to the first byte in 0x40–0x7e. `ESC (`/`ESC #` take exactly one more byte. Any other `ESC x` is a 2-byte sequence.

- [ ] **Step 1: Failing tests**:
```rust
#[test] fn passes_allowed_and_records_violations() {
    let mut w = StrictWriter::new(Vec::new(), Vt100Dialect);
    w.write_all(b"\x1b[1;1HOK\r\n\x1b[?25l\xc3\xa9").unwrap();
    assert_eq!(w.inner, b"\x1b[1;1HOK\r\n\x1b[?25l\xc3\xa9");
    assert_eq!(w.violations, vec![b"\x1b[?25l".to_vec(), vec![0xc3], vec![0xa9]]);
}
#[test] fn sequence_split_across_writes() {
    let mut w = StrictWriter::new(Vec::new(), Vt100Dialect);
    w.write_all(b"\x1b[1").unwrap(); w.write_all(b";1H").unwrap();
    assert!(w.violations.is_empty());
}
```
(Make `inner` `pub` or add `into_inner()`; either is fine.)
- [ ] **Step 2–4:** Run → FAIL; implement; run → pass.
- [ ] **Step 5: Commit** `feat: strict-mode output checker`

---

### Task 4: Wy60Backend

**Files:** Create `src/backend.rs`.

**Interfaces:**
- Consumes `Dialect`, `Attrs`, `Charset`, `Glyph`, `map_glyph`.
- Produces `pub struct Wy60Backend<W: Write, D: Dialect>` with `pub fn new(writer: W, dialect: D, cols: u16, rows: u16) -> Self`, `pub fn writer_mut(&mut self) -> &mut W`, and `impl ratatui::backend::Backend` with `type Error = std::io::Error`.
- Behaviour:
  - Output is buffered in a `Vec<u8>` and written by `flush`.
  - It tracks the cursor (`Option<(row, col)>`; `None` = unknown), the current `Attrs`, and the current `Charset`, and emits CUP/SGR/charset only when they change.
  - **Cursor moves:** if the target is the tracked position, nothing is emitted. If it's on the same row to the right by `n` and `n ≤ 4`, emit `cuf(n)` (`ESC[nC`). Otherwise emit `cup`.
  - After a cell at the last column, the cursor becomes `None`, because of the VT100 pending-wrap state.
  - Glyph handling: `ReverseSpace` is a space with `reverse` toggled relative to the cell's own attrs. Colours are ignored.
  - `clear` emits `dialect.clear` and resets all tracking.
  - `hide_cursor` and `show_cursor` do nothing (the VT100 subset has no DECTCEM).
  - `get_cursor_position` returns the tracked position, or (0, 0).
  - `set_cursor_position` emits a CUP.
  - `size` and `window_size` return the configured geometry (pixels 0).
  - `clear_region(ClearType::All)` behaves like `clear`. The other `ClearType` variants map to `ESC[J` / `ESC[1J` / `ESC[K` / `ESC[1K`, with `ClearType::UntilNewLine` → `ESC[K`.
  - Before `flush`, `draw` leaves the attributes reset (`ESC[0m`) and the charset at `Ascii`, so anything that writes to the terminal outside the backend starts from a known state.
- Tests use a helper `fn screen(bytes: &[u8], cols, rows) -> vt100::Parser` that feeds bytes into `vt100::Parser::new(rows, cols, 0)`. vt100 0.18 ignores `ESC ( 0`, so graphics cells show their raw byte (e.g. `q`). If that turns out to be false, compare against the mapped Unicode instead and record a ruling.

- [ ] **Step 1: Failing tests** (render through `ratatui::Terminal::new(Wy60Backend::new(Vec::new(), Vt100Dialect, 80, 43))`):
```rust
#[test] fn round_trip_text_and_attrs() {
    // draw Paragraph "HELLO" bold at (2,5), and a reversed "X" at (79,0)
    // vt100 screen: cell(2,5..10) == "HELLO" && bold; cell(0,79) == "X" && inverse
}
#[test] fn borders_become_dec_graphics() {
    // Block::bordered() over Rect(0,0,4,3): bytes contain "\x1b(0" and "lqqk"; screen row0 == "lqqk"
}
#[test] fn second_identical_frame_emits_nothing() { /* draw same frame twice; second flush writes 0 bytes */ }
#[test] fn adjacent_cells_need_no_cursor_moves() { /* "ABCDEF" on one row → exactly one CUP in output */ }
#[test] fn bottom_right_cell_does_not_scroll() { /* write "Z" at (79,42) and "T" at (0,0); screen cell(0,0)=="T", cell(42,79)=="Z" */ }
#[test] fn non_ascii_becomes_question_mark() { /* "é" drawn → bytes contain b'?' and are all < 0x80 */ }
#[test] fn output_passes_strict_mode() { /* render a frame with borders+attrs+blocks through StrictWriter; violations empty */ }
#[test] fn block_glyph_is_reverse_space() { /* "█" → screen cell is ' ' with inverse */ }
```
Write each test body out in full: use the named widget, draw it, then assert exactly what the comment says.

- [ ] **Step 2: Run** `cargo test backend` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → pass.
- [ ] **Step 5: Commit** `feat: Wy60Backend for ratatui`

---

### Task 5: Link reader handle and output-queue depth

**Files:** Modify `src/link.rs`; tests in `tests/link_pty.rs`.

**Interfaces — Produces:**
- `pub struct LinkReader { file: File }`, which implements `ReadTimeout` with the same EOF→error semantics as `Link`. Move the shared poll/read logic into one private fn over `&File`.
- `impl Link { pub fn reader(&self) -> io::Result<LinkReader>; pub fn output_queue(&self) -> io::Result<usize> }`.
  - `reader` uses `try_clone`.
  - `output_queue` uses the `TIOCOUTQ` ioctl (`nix::ioctl_read_bad!(tiocoutq, libc::TIOCOUTQ, libc::c_int)`).

- [ ] **Step 1: Failing tests**:
```rust
#[test] fn reader_sees_input() {
    let pty = openpty(None, None).unwrap();
    let l = Link::from_fd(pty.slave, None, FlowControl::XonXoff).unwrap();
    let mut r = l.reader().unwrap();
    nix::unistd::write(&pty.master, b"k").unwrap();
    let mut b = [0u8; 4];
    assert_eq!(r.read_timeout(&mut b, Duration::from_millis(200)).unwrap(), 1);
}
#[test] fn output_queue_reports_unread_bytes() {
    let pty = openpty(None, None).unwrap();
    let mut l = Link::from_fd(pty.slave, None, FlowControl::XonXoff).unwrap();
    assert_eq!(l.output_queue().unwrap(), 0);
    l.write_all(&[b'x'; 100]).unwrap();
    assert!(l.output_queue().unwrap() > 0);   // master hasn't read
}
```
- [ ] **Step 2–4:** FAIL → implement → pass. If `output_queue` on a pty reads 0 because the pty pushes bytes straight to the master buffer, change the assertion to `l.output_queue().is_ok()`, record the ruling, and note in `docs/hardware.md` that on the real FTDI port the value is to be checked with `--strict` logging.
- [ ] **Step 5: Commit** `feat: link reader handle and output queue depth`

---

### Task 6: Input — keys, keymap, parser

**Files:** Create `src/input.rs`.

**Interfaces:**
- Consumes `Config.keys`.
- `#[derive(Clone, PartialEq, Eq, Debug)] pub enum Key { Char(char), Ctrl(char), Enter, Backspace, Tab, Esc, Up, Down, Left, Right, Home, PageUp, PageDown, DeleteChar, InsertKey, F(u8), ShiftF(u8), Pf(u8), Unknown(Vec<u8>) }`.
  - `Char` covers printable ASCII.
  - `Ctrl(c)` covers 0x01–0x1f except the named ones: `^M`→Enter, `^H`→Backspace, `^I`→Tab, `0x1b` alone→Esc. For example `0x1d` → `Ctrl(']')`.
  - `0x7f` → `Backspace`.
- `pub struct Keymap { seqs: Vec<(Vec<u8>, Key)> }`:
  - `pub fn wy60_default() -> Self`, filled from `docs/hardware.md`. All F1–F16 and Shift-F1–F16.
    - Shift-F15 `ESC[5~` → `PageUp` and Shift-F16 `ESC[6~` → `PageDown`, **not** `ShiftF(15/16)`.
    - PF1 `ESC O P` → `Pf(1)`, PF2 `ESC O Q` → `DeleteChar`, PF3 → `Pf(3)`, PF4 `ESC O S` → `InsertKey`.
    - Arrows, and Home `ESC[H`.
  - `pub fn with_overrides(self, overrides: &BTreeMap<String, String>) -> anyhow::Result<Self>`. Names are `"F1"`…`"F16"`, `"SHIFT-F1"`…, `"UP"`, `"DOWN"`, `"LEFT"`, `"RIGHT"`, `"HOME"`, `"PAGEUP"`, `"PAGEDOWN"`, `"DELETECHAR"`, `"INSERT"`, `"PF1"`, `"PF3"`. An unknown name is an error that names it.
- `pub struct KeyParser { keymap: Keymap, pending: Vec<u8> }`:
  - `pub fn new(keymap: Keymap) -> Self`
  - `pub fn feed(&mut self, bytes: &[u8]) -> Vec<Key>`: emits every complete key. While `pending` is a strict prefix of some keymap sequence, it waits.
  - `pub fn timeout(&mut self) -> Vec<Key>`: called after `KEY_GAP` (50 ms, reuse `testcard::keys::KEY_GAP`) of silence.
    - If `pending` is exactly `[ESC]`, emit `Esc`.
    - If it is an ESC-prefixed partial that matches nothing, emit `Unknown(pending)`.
    - Otherwise decode it byte by byte.
  - In `feed`, an ESC sequence that diverges from every keymap entry is consumed until its terminator (CSI: final byte 0x40–0x7e; SS3 `ESC O x`: one byte) and emitted as `Unknown`.

- [ ] **Step 1: Failing tests**:
```rust
#[test] fn f1_f2_f3() { let mut p = parser(); assert_eq!(p.feed(b"\x1b[?5i\x1b[?3i\x1b[2i"), vec![Key::F(1), Key::F(2), Key::F(3)]); }
#[test] fn page_keys_and_delete() { let mut p = parser(); assert_eq!(p.feed(b"\x1b[5~\x1b[6~\x1bOQ"), vec![Key::PageUp, Key::PageDown, Key::DeleteChar]); }
#[test] fn split_sequence_within_gap_is_one_key() { let mut p = parser(); assert!(p.feed(b"\x1b[").is_empty()); assert_eq!(p.feed(b"?5i"), vec![Key::F(1)]); }
#[test] fn lone_esc_after_timeout() { let mut p = parser(); assert!(p.feed(b"\x1b").is_empty()); assert_eq!(p.timeout(), vec![Key::Esc]); }
#[test] fn unknown_sequence_is_unknown_key() { let mut p = parser(); assert_eq!(p.feed(b"\x1b[99zA"), vec![Key::Unknown(b"\x1b[99z".to_vec()), Key::Char('A')]); }
#[test] fn controls() { let mut p = parser(); assert_eq!(p.feed(b"\x1d\x08\r\x7fq"), vec![Key::Ctrl(']'), Key::Backspace, Key::Enter, Key::Backspace, Key::Char('q')]); }
#[test] fn override_replaces_sequence() {
    let km = Keymap::wy60_default().with_overrides(&BTreeMap::from([("F1".into(), "\x1b[11~".into())])).unwrap();
    assert_eq!(KeyParser::new(km).feed(b"\x1b[11~"), vec![Key::F(1)]);
}
#[test] fn override_unknown_name_errors() { assert!(Keymap::wy60_default().with_overrides(&BTreeMap::from([("F99".into(), "x".into())])).unwrap_err().to_string().contains("F99")); }
```
(`parser()` = `KeyParser::new(Keymap::wy60_default())`.)
- [ ] **Step 2–4:** FAIL → implement → pass.
- [ ] **Step 5: Commit** `feat: WY-60 keymap and incremental key parser`

---

### Task 7: App — tabs, prefix, fullscreen, chrome

**Files:** Create `src/app/mod.rs`, `src/app/chrome.rs`, `src/app/tabs.rs`.

**Interfaces:**
- Consumes `Key`, `ChromeConfig`.
- `tabs.rs`:
  - `#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum TabId { Utility, Muthur, Shell }`, with `pub const ALL: [TabId; 3]` and `pub fn label(self) -> &'static str` (`"UTILITY"`, `"MU/TH/UR"`, `"SHELL"`).
  - `pub trait Tab { fn render(&mut self, area: Rect, buf: &mut Buffer); fn handle_key(&mut self, key: &Key); fn take_activity(&mut self) -> bool { false } }`
  - `pub struct PlaceholderTab { pub id: TabId }`: draws a bordered box titled with its label, with `"MODULE OFFLINE"` centred and `"AWAITING INSTALLATION"` under it.
- `mod.rs`:
  - `pub struct App { pub active: TabId, fullscreen: [bool; 3], prefix: bool, overlay: Option<Overlay>, show_real_date: bool, pub quit: bool, pub redraw: bool, tabs: [Box<dyn Tab>; 3], activity: [bool; 3], chrome: ChromeView }`.
  - `pub fn new(chrome: ChromeView) -> Self` with placeholder tabs.
  - `pub fn handle_key(&mut self, key: Key)`
  - `pub fn render(&mut self, frame: &mut ratatui::Frame, now: chrono::NaiveDateTime)`
  - `pub struct ChromeView { pub ship_name: String, pub year_offset: i32 }`, built in `run.rs` from config and hostname.
  - `enum Overlay { Help, Message(&'static str) }`
- **Key handling** (no prefix pending):
  - `F(1..=3)` switches tab.
  - `Ctrl(']')` sets prefix.
  - Anything else goes to the active tab's `handle_key`.
- **With prefix pending:**
  - `f` toggles this tab's fullscreen.
  - `r` sets `redraw`.
  - `q` sets `quit`.
  - `?` shows the Help overlay.
  - `d` toggles the real date.
  - `]` sends `Ctrl(']')` to the tab.
  - `p` or `[` shows `Message("NOT AVAILABLE")`.
  - Anything else cancels.
  - The prefix clears after any key.
  - Any key dismisses an overlay first and is otherwise ignored.
- **Layout (windowed), on 80×43:**
  - Row 0 is the title bar, in reverse video across the full width:
    - left: ` {SHIP} ── MU/TH/UR 6000`
    - right: `{DD MON YYYY HH:MM} ` in uppercase (e.g. `17 NOV 2137 14:02`), where the year is `now.year() + year_offset` unless the real date is shown.
  - Row 42 is the footer, with dotted leaders like the game: `F1-F3 SELECT .... ^] F FULLSCREEN .... SH-F15/16 SCROLL .... ^] ? HELP`.
  - Sidebar: columns 0–17, rows 2–41.
    - `FOLDERS` label at row 2, column 1.
    - Each tab is a bordered box 16 wide × 5 tall, starting at row 4, with a 1-row gap between boxes.
    - The label sits inside at row+1, column+2.
    - The active box's label is in reverse video, and its box is joined to the content pane by a connector `─` from the box's right edge to the content border at the box's middle row.
    - An activity `*` appears at the box's inside right edge.
  - Content pane: `Rect { x: 19, y: 2, width: 61, height: 40 }`. The tab draws inside it, including its own border.
  - **Fullscreen:** the tab gets the whole `Rect(0, 0, 80, 43)`, and the chrome is not drawn.
  - **Prefix strip:** while the prefix is pending (windowed or fullscreen), the last row is replaced by a reverse-video `PREFIX: F FULL  R REDRAW  D DATE  ? HELP  Q QUIT  ] SEND ^]`.
  - **Help overlay:** a centred bordered box listing the keys.
- Layout constants live in `chrome.rs` as `const` items, so later milestones can read them.

- [ ] **Step 1: Failing tests** (`ratatui::backend::TestBackend` 80×43 and a fixed `now = 2026-11-17 14:02`):
```rust
#[test] fn title_bar_shows_ship_and_in_universe_date() { /* row 0 contains "USCSS TEST" and "17 NOV 2137 14:02"; cell(0,0) has REVERSED */ }
#[test] fn real_date_toggle() { /* keys Ctrl(']'), Char('d') → row 0 contains "17 NOV 2026" */ }
#[test] fn f_keys_switch_tabs() { /* F(3) → active == Shell; the SHELL box label is reversed */ }
#[test] fn fullscreen_is_per_tab_and_starts_windowed() {
    /* all tabs start windowed (row 0 is the title bar); ^] f on Utility → row 0 no longer has the title;
       F(3) → Shell is windowed again; F(1) → Utility still fullscreen */
}
#[test] fn prefix_strip_appears_and_clears() { /* after Ctrl(']') row 42 starts with "PREFIX:"; after Char('x') the footer is back */ }
#[test] fn prefix_q_quits_and_r_redraws() { /* flags set */ }
#[test] fn prefix_p_is_not_available_yet() { /* overlay text "NOT AVAILABLE" rendered; next key dismisses */ }
#[test] fn other_keys_go_to_active_tab() { /* use a recording test Tab: Char('a') reaches it; F(2) does not */ }
#[test] fn renders_only_dialect_safe_glyphs() { /* every cell symbol in the buffer maps via map_glyph to something other than Ascii(b'?') */ }
```
Write each test body out in full: set up the state, render, then assert exactly what the comment says.
- [ ] **Step 2: Run** `cargo test app` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → pass.
- [ ] **Step 5: Commit** `feat: app shell with chrome, tab switching, prefix and fullscreen`

---

### Task 8: Run loop, logging, main wiring, golden screens

**Files:** Create `src/run.rs`, `tests/golden.rs`, `tests/golden/{windowed_utility,fullscreen_shell,prefix_strip}.txt`; modify `src/main.rs`, `src/lib.rs`.

**Interfaces:**
- `pub enum Event { Keys(Vec<Key>), Hangup, Tick }`.
- `pub fn spawn_input(reader: LinkReader, parser: KeyParser, tx: tokio::sync::mpsc::UnboundedSender<Event>) -> std::thread::JoinHandle<()>`.
  - It runs a loop of `read_timeout(KEY_GAP)`. Data → `feed`, idle → `timeout`, and non-empty results are sent as `Keys`.
  - On `Err`, it sends `Hangup` and exits.
- `pub fn should_draw(dirty: bool, outq: usize) -> bool`: `dirty && outq < OUTQ_LOW` with `pub const OUTQ_LOW: usize = 256`.
- `pub async fn event_loop<W: Write>(app: &mut App, terminal: &mut Terminal<Wy60Backend<W, Vt100Dialect>>, rx: &mut UnboundedReceiver<Event>, outq: impl Fn() -> usize, clock: impl Fn() -> NaiveDateTime) -> anyhow::Result<()>`
  - It `select!`s on `rx.recv()` and a 1 s tick (the clock, plus a 20 ms retry tick while `dirty` and the queue is too full).
  - Key events go to `app.handle_key`.
  - `app.redraw` triggers `terminal.clear()`, then a full draw.
  - It draws when `should_draw` allows.
  - It returns on `app.quit`, `Hangup`, or a closed channel.
- `pub fn run(cfg: &Config, strict: bool) -> anyhow::Result<()>`, in order:
  1. Initialise tracing to `~/.local/state/muthur/` with daily rolling `muthur.log` via `tracing_appender::rolling::daily`, keeping the guard alive.
  2. `install_panic_hook`.
  3. Open the link as in M0.
  4. Build the keymap with overrides, then `spawn_input`.
  5. Build the `Terminal` over `Wy60Backend<Box<dyn Write>>`, which is `StrictWriter<&mut Link>` when `strict` is on and the link otherwise. Use a `Write` adapter that borrows the `Link`; boxing is fine.
  6. `terminal.clear()`.
  7. Start a current-thread tokio runtime with `block_on(event_loop(...))`.
  8. Teardown: write `dialect.clear`, then let `Link` drop to restore termios.
  9. Hostname via `nix::unistd::gethostname`, uppercased; ship name is `cfg.chrome.ship_name` or `"USCSS {HOST}"`.
- `main.rs`: no subcommand → `run::run(&cfg, cli.strict)`, replacing the M0 placeholder message.
- Golden tests (`tests/golden.rs`):
  - Drive `App` + `Terminal<Wy60Backend<Vec<u8>>>` with a fixed clock and a scripted key list. Feed the produced bytes into `vt100::Parser(43, 80)`.
  - Compare `screen().contents()` (text) with `tests/golden/<name>.txt`. When `UPDATE_GOLDEN=1`, write the file instead.
  - Also assert that the bytes pass `StrictWriter` with zero violations.
  - Scenes: `windowed_utility` (start state), `fullscreen_shell` (`F3`, `^] f`), `prefix_strip` (`^]`).

- [ ] **Step 1: Failing tests**:
```rust
// src/run.rs
#[test] fn should_draw_waits_for_queue() { assert!(should_draw(true, 0)); assert!(!should_draw(true, 256)); assert!(!should_draw(false, 0)); }
#[tokio::test] async fn hangup_event_quits() { /* send Event::Hangup; event_loop returns Ok within 100ms */ }
#[tokio::test] async fn quit_key_ends_loop_and_redraw_clears() { /* send Keys([Ctrl(']'), Char('r')]) then Keys([Ctrl(']'), Char('q')]); loop returns; output bytes contain "\x1b[2J" twice (initial + redraw) */ }
// tests/golden.rs — the three scenes above
```
(Add `"rt-multi-thread"` only if `#[tokio::test]` requires it. The default `#[tokio::test]` uses current_thread, so `rt` + `macros` should be enough.)
- [ ] **Step 2: Run** `cargo test` → FAIL.
- [ ] **Step 3: Implement.** Generate the goldens with `UPDATE_GOLDEN=1 cargo test --test golden`. **Read every `.txt` golden yourself** and confirm it matches the Task 7 layout spec before committing. A golden is a claim, not a recording.
- [ ] **Step 4: Verify:**
  - `cargo test` passes, `cargo clippy --all-targets -- -D warnings` is clean, and the musl release build is static.
  - Smoke test via a pty harness (python `pty`, rows 43): launch `muthur --strict`, send `ESC[2i` (F3), `0x1d f`, `0x1d q`. Expected: the process exits 0, termios is restored (ICANON on), and `~/.local/state/muthur/muthur.log` contains no `strict` violation lines.
- [ ] **Step 5: Commit** `feat: run loop, logging and golden screens`
