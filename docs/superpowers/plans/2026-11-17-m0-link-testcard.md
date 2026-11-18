# M0 — Link & Testcard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `muthur` binary that can own the WY-60 link (serial device or stdio), load a config, and run `muthur testcard` — an attribute/charset probe, a flow-control flood test, and a key-echo recorder — so the real terminal's capabilities and key sequences become known facts before M1.

**Architecture:** Library crate (`src/lib.rs`) plus thin binary (`src/main.rs`). `link` wraps a terminal fd in raw mode (preserving XON/XOFF) with guaranteed termios restore on drop and on panic. `testcard` is pure byte-generating functions plus a small key-echo loop over a `ReadTimeout` trait so it is testable without hardware. No tokio yet (arrives in M1).

**Tech Stack:** Rust 2024 edition, `nix` (termios, poll, pty for tests), `clap` (derive), `serde` + `toml`, `anyhow`, `dirs`.

**Spec:** `docs/ARCHITECTURE.md` (sections *Hardware & link*, *Deployment & process model*, *Keys*, *Testing & development*, milestone M0).

## Global Constraints

- Link: **38400 baud**, **XON/XOFF** default; raw mode must leave `IXON`/`IXOFF` enabled when flow control is XON/XOFF.
- Terminal: VT100 personality, **80×43**.
- Config at `~/.config/muthur/config.toml`; refuse to start if group/world readable (mode must have no `0o077` bits).
- termios always restored on exit, including via panic hook.
- Release target `x86_64-unknown-linux-musl`, fully static; no OpenSSL / C deps.
- Everything written to the wire is 7-bit (`< 0x80`).
- Package/binary name `muthur` (lowercase).

## Review Focus

1. **Panic while in raw mode** — the terminal must come back usable (echo, canonical mode). Test: Task 2 `panic_hook_restores_termios`.
2. **Config file absent** — first run on a fresh server must work with defaults, not error. Test: Task 1 `missing_file_yields_defaults`.
3. **Config with an unsupported baud (e.g. 38401)** — must fail with a message naming the value, not silently use some other rate. Test: Task 2 `unsupported_baud_is_error`.
4. **Key-echo receiving a lone ESC then nothing** — must be reported as `ESC`, not hang waiting for the rest of a sequence. Test: Task 4 `lone_escape_is_its_own_key`.
5. **Testcard on a smaller screen than 80×43** (dev terminal) — must not address rows/cols beyond the given size. Test: Task 3 `card_respects_geometry`.

---

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | Rename package to `muthur`; dependencies; release profile. |
| `.cargo/config.toml` | musl target static linking flags. |
| `src/lib.rs` | `pub mod config; pub mod link; pub mod testcard;` |
| `src/main.rs` | clap CLI; opens link; dispatches subcommands. |
| `src/config.rs` | `Config`, `LinkConfig`, `FlowControl`, load + permission check. |
| `src/link.rs` | `Link`, raw-mode termios transform, baud mapping, restore guard, panic hook, `ReadTimeout`. |
| `src/testcard/mod.rs` | `pub mod card; pub mod flood; pub mod keys;` |
| `src/testcard/card.rs` | Attribute / charset probe screen generator. |
| `src/testcard/flood.rs` | Flow-control flood pattern generator. |
| `src/testcard/keys.rs` | Key grouping, byte description, echo loop. |
| `tests/link_pty.rs` | Link integration tests against a real pty. |
| `docs/testcard.md` | How to run the testcard on the WY-60 and what to report back. |

---

### Task 1: Crate setup and config

**Files:**
- Modify: `Cargo.toml`
- Create: `.cargo/config.toml`, `src/lib.rs`, `src/config.rs`
- Modify: `src/main.rs` (stub `fn main` calling nothing yet is fine)

**Interfaces:**
- Produces:
  - `pub enum FlowControl { XonXoff, RtsCts, None }` — serde `rename_all = "kebab-case"` (`"xon-xoff"`, `"rts-cts"`, `"none"`), `Default = XonXoff`.
  - `pub struct LinkConfig { pub device: Option<PathBuf>, pub baud: u32, pub flow: FlowControl }` — defaults `None`, `38400`, `XonXoff`.
  - `pub struct Config { pub link: LinkConfig }` — `#[serde(default)]` throughout so partial files work.
  - `pub fn default_path() -> PathBuf` — `dirs::config_dir()/muthur/config.toml`.
  - `pub fn load(path: &Path) -> anyhow::Result<Config>` — missing file → `Config::default()`; existing file with any `0o077` mode bits → error whose message contains `"chmod 600"`; otherwise parse TOML.

- [ ] **Step 1: Set up the crate**

`Cargo.toml`: `name = "muthur"`, edition 2024; deps `nix = { version = "0.30", features = ["term", "poll", "fs", "ioctl"] }`, `clap = { version = "4", features = ["derive"] }`, `serde = { version = "1", features = ["derive"] }`, `toml = "0.9"`, `anyhow = "1"`, `dirs = "6"`; dev-deps `tempfile = "3"`, `nix` with `"term"` + `"pty"` features added (enable `pty` in the main dep instead if simpler). `[profile.release] lto = true, strip = true`.
`.cargo/config.toml`: `[target.x86_64-unknown-linux-musl] rustflags = ["-C", "target-feature=+crt-static"]`.
Run `rustup target add x86_64-unknown-linux-musl`.

- [ ] **Step 2: Write failing tests in `src/config.rs` (`#[cfg(test)] mod tests`)**

```rust
#[test]
fn missing_file_yields_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let c = load(&dir.path().join("nope.toml")).unwrap();
    assert_eq!(c.link.baud, 38400);
    assert_eq!(c.link.flow, FlowControl::XonXoff);
    assert_eq!(c.link.device, None);
}

#[test]
fn partial_file_parses_with_defaults() {
    let (_d, p) = write_cfg("[link]\ndevice = \"/dev/wy60\"\nflow = \"rts-cts\"\n", 0o600);
    let c = load(&p).unwrap();
    assert_eq!(c.link.device.as_deref(), Some(Path::new("/dev/wy60")));
    assert_eq!(c.link.flow, FlowControl::RtsCts);
    assert_eq!(c.link.baud, 38400);
}

#[test]
fn world_readable_file_is_rejected() {
    let (_d, p) = write_cfg("", 0o644);
    let err = load(&p).unwrap_err().to_string();
    assert!(err.contains("chmod 600"), "{err}");
}
```
(`write_cfg(contents, mode) -> (TempDir, PathBuf)` is a test helper using `std::os::unix::fs::PermissionsExt`.)

- [ ] **Step 3: Run** `cargo test config` — expect compile failure / FAIL.

- [ ] **Step 4: Implement** the types and `load` / `default_path` per the Interfaces block.

- [ ] **Step 5: Run** `cargo test config` — expect 3 passed. Also `cargo build --release --target x86_64-unknown-linux-musl` succeeds and `file target/x86_64-unknown-linux-musl/release/muthur` reports `statically linked`.

- [ ] **Step 6: Commit** — `git commit -m "feat: crate setup and config loading"`

---

### Task 2: Link (raw mode, baud, restore, panic hook)

**Files:**
- Create: `src/link.rs`, `tests/link_pty.rs`

**Interfaces:**
- Consumes: `LinkConfig`, `FlowControl` (Task 1).
- Produces:
  - `pub fn baud_rate(baud: u32) -> anyhow::Result<nix::sys::termios::BaudRate>` — supports 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200; anything else → error containing the number.
  - `pub fn make_raw(t: &Termios, flow: FlowControl) -> Termios` — pure: clears `ICANON ECHO ECHONL ISIG IEXTEN`, `ICRNL INLCR IGNCR ISTRIP BRKINT PARMRK`, `OPOST`; sets `CS8`, `VMIN=1 VTIME=0`. `XonXoff` → set `IXON|IXOFF`, clear `CRTSCTS`; `RtsCts` → clear `IXON|IXOFF`, set `CRTSCTS`; `None` → clear both.
  - `pub struct Link` — owns an `OwnedFd`/`File` and the original `Termios`; `impl Read + Write`; `Drop` restores original termios (`TCSADRAIN`).
  - `impl Link { pub fn open_device(cfg: &LinkConfig) -> anyhow::Result<Link>; pub fn stdio(flow: FlowControl) -> anyhow::Result<Link>; pub fn from_fd(fd: OwnedFd, baud: Option<u32>, flow: FlowControl) -> anyhow::Result<Link> }` — `open_device` opens `cfg.device` (`O_RDWR|O_NOCTTY`) and sets baud; `stdio` dups fd 0 and does **not** touch baud (agetty set it); both delegate to `from_fd`.
  - `pub trait ReadTimeout { fn read_timeout(&mut self, buf: &mut [u8], timeout: std::time::Duration) -> std::io::Result<usize>; }` — returns `Ok(0)` on timeout; `Link` implements via `poll`.
  - `pub fn install_panic_hook()` — first `Link` created records `(raw fd, original termios)` in a `static OnceLock<Mutex<…>>`; hook restores it, writes `"\x1b[0m\x1b(B\r\n"`, then calls the previous hook.

- [ ] **Step 1: Write failing unit tests in `src/link.rs`**

```rust
#[test]
fn xonxoff_raw_keeps_software_flow() {
    let t = make_raw(&sample_cooked(), FlowControl::XonXoff);
    assert!(t.input_flags.contains(InputFlags::IXON | InputFlags::IXOFF));
    assert!(!t.control_flags.contains(ControlFlags::CRTSCTS));
    assert!(!t.local_flags.intersects(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG));
    assert!(!t.output_flags.contains(OutputFlags::OPOST));
}
#[test]
fn rtscts_raw_disables_software_flow() {
    let t = make_raw(&sample_cooked(), FlowControl::RtsCts);
    assert!(!t.input_flags.intersects(InputFlags::IXON | InputFlags::IXOFF));
    assert!(t.control_flags.contains(ControlFlags::CRTSCTS));
}
#[test]
fn unsupported_baud_is_error() {
    assert!(baud_rate(38400).is_ok());
    assert!(baud_rate(38401).unwrap_err().to_string().contains("38401"));
}
```
(`sample_cooked()` obtains a real `Termios` via `tcgetattr` on a fresh `openpty` slave, which is cooked by default.)

- [ ] **Step 2: Write failing integration tests in `tests/link_pty.rs`**

```rust
#[test]
fn drop_restores_termios() {
    let pty = nix::pty::openpty(None, None).unwrap();
    let before = tcgetattr(&pty.slave).unwrap();
    let slave = pty.slave.try_clone().unwrap();
    { let _l = Link::from_fd(slave, Some(38400), FlowControl::XonXoff).unwrap();
      assert!(!tcgetattr(&pty.slave).unwrap().local_flags.contains(LocalFlags::ICANON)); }
    assert_eq!(tcgetattr(&pty.slave).unwrap().local_flags, before.local_flags);
}
#[test]
fn read_timeout_returns_zero_when_idle() {
    let pty = nix::pty::openpty(None, None).unwrap();
    let mut l = Link::from_fd(pty.slave, None, FlowControl::XonXoff).unwrap();
    let mut buf = [0u8; 8];
    assert_eq!(l.read_timeout(&mut buf, Duration::from_millis(20)).unwrap(), 0);
}
#[test]
fn panic_hook_restores_termios() {
    // Run in a child process: re-exec this test binary with an env var that makes it
    // create a Link on a pty slave passed via env, call install_panic_hook(), then panic.
    // Parent asserts the slave's ICANON is set again after the child exits non-zero.
}
```
For `panic_hook_restores_termios` use the standard re-exec pattern (`std::env::current_exe()` + `--exact panic_hook_child` + env var `MUTHUR_PANIC_CHILD=1`; the child test is a no-op unless the env var is set). Hand the pty to the child by opening `/dev/pts/N` from `nix::unistd::ttyname(&pty.slave)` inside the child.

- [ ] **Step 3: Run** `cargo test link` — expect FAIL.

- [ ] **Step 4: Implement** `src/link.rs` per the Interfaces block; add `pub mod link;` to `src/lib.rs`.

- [ ] **Step 5: Run** `cargo test` — all pass.

- [ ] **Step 6: Commit** — `git commit -m "feat: raw serial/stdio link with guaranteed termios restore"`

---

### Task 3: Testcard probe screen and flood pattern

**Files:**
- Create: `src/testcard/mod.rs`, `src/testcard/card.rs`, `src/testcard/flood.rs`

**Interfaces:**
- Produces:
  - `pub fn card(cols: u16, rows: u16) -> Vec<u8>` — the full probe screen.
  - `pub fn flood(cols: u16, lines: u32) -> Vec<u8>` — deterministic pattern.

**Card content.** Clear screen, home, then numbered, self-labelled sections so the user can report "items 3, 7 and 12 look wrong". Each item is one row: `NN LABEL......  sample`. Items:

1. Normal text · 2. Bold `ESC[1m` · 3. Underline `ESC[4m` · 4. Blink `ESC[5m` · 5. Reverse `ESC[7m` · 6. Bold+reverse · 7. Underline+reverse · 8. Bold+underline+blink+reverse · 9. Dim `ESC[2m` (probably unsupported in VT100 — that's the point) · 10. DEC Special Graphics: all chars `0x5f..=0x7e` after `ESC(0`, then `ESC(B` · 11. A 3-line box drawn with DEC graphics (`lqk`/`x`/`mqj`) · 12. Double-width line `ESC#6` · 13. Double-height pair `ESC#3` / `ESC#4` · 14. Scroll region: set `ESC[r` region to 3 lines, print 5 numbered lines inside so only the last 3 should remain, then reset `ESC[r` · 15. Last-column write: write `X` at col `cols` of a row and report whether it wraps.

Row 1 is a reverse-video header `MUTHUR TESTCARD  <cols>x<rows>`; the final row (`rows`) reads `BOTTOM ROW <rows> — PRESS ANY KEY` so the user verifies the full 43-line addressing. The card ends with `ESC[0m ESC(B` and the cursor parked on the final row.

**Flood.** `lines` lines, each `cols-1` chars: `"{n:05} "` followed by the repeating alphabet offset by `n`, ending `\r\n`. A dropped byte shifts the alphabet visibly. The last line is `FLOOD COMPLETE {lines} LINES`.

- [ ] **Step 1: Write failing tests (in each file's `mod tests`)**

```rust
#[test] fn card_is_seven_bit() { assert!(card(80, 43).iter().all(|&b| b < 0x80)); }
#[test] fn card_probes_every_feature() {
    let s = String::from_utf8(card(80, 43)).unwrap();
    for seq in ["\x1b[1m","\x1b[4m","\x1b[5m","\x1b[7m","\x1b[2m","\x1b(0","\x1b(B","\x1b#6","\x1b#3","\x1b#4","\x1b[r"] {
        assert!(s.contains(seq), "missing {seq:?}");
    }
    assert!(s.contains("BOTTOM ROW 43"));
    assert!(s.ends_with("\x1b[0m\x1b(B") || s.contains("\x1b[0m\x1b(B"));
}
#[test] fn card_respects_geometry() {
    for (c, r) in [(80, 43), (80, 24)] {
        for (row, col) in cup_targets(&card(c, r)) { assert!(row <= r && col <= c, "{row};{col} outside {c}x{r}"); }
    }
}
#[test] fn flood_lines_are_distinct_and_fit() {
    let s = String::from_utf8(flood(80, 100)).unwrap();
    let lines: Vec<_> = s.split("\r\n").collect();
    assert!(lines[..100].iter().all(|l| l.len() == 79));
    assert_ne!(lines[0][6..], lines[1][6..]);
    assert!(s.contains("FLOOD COMPLETE 100 LINES"));
}
```
(`cup_targets(&[u8]) -> Vec<(u16,u16)>` is a test helper parsing every `ESC[{row};{col}H`.)

- [ ] **Step 2: Run** `cargo test testcard` — FAIL.
- [ ] **Step 3: Implement** `card` and `flood`; add `pub mod testcard;` to `lib.rs`.
- [ ] **Step 4: Run** `cargo test testcard` — pass.
- [ ] **Step 5: Commit** — `git commit -m "feat: testcard probe screen and flood pattern"`

---

### Task 4: Key echo

**Files:**
- Create: `src/testcard/keys.rs`

**Interfaces:**
- Consumes: `ReadTimeout` (Task 2).
- Produces:
  - `pub fn describe(bytes: &[u8]) -> String` — tokens joined by spaces: `0x1b`→`ESC`, `0x7f`→`DEL`, `0x00..=0x1f`→`^@`…`^_` (caret notation), printable ASCII as itself, `0x20`→`SP`, `>=0x80`→`\xNN`; then two spaces and the lowercase hex bytes in parens. Example: `[0x1b, b'O', b'P']` → `"ESC O P  (1b 4f 50)"`.
  - `pub fn read_key(r: &mut impl ReadTimeout, gap: Duration) -> io::Result<Vec<u8>>` — blocks for the first byte, then keeps appending bytes until a read returns 0 within `gap`. Default gap at the call site is **50 ms** (≈190 byte-times at 38400, generous for one escape sequence).
  - `pub fn echo_loop(link: &mut (impl ReadTimeout + Write), log: &mut impl Write) -> io::Result<()>` — prints instructions, then for each key writes `describe(..)` + `\r\n` to the link and `describe(..)` + `\n` to `log`; exits when it reads two consecutive keys equal to `[0x1d]` (Ctrl-]).

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn describes_pf1() { assert_eq!(describe(&[0x1b, b'O', b'P']), "ESC O P  (1b 4f 50)"); }
#[test] fn describes_controls_and_high() { assert_eq!(describe(&[0x1d, 0x7f, 0x9b]), "^] DEL \\x9b  (1d 7f 9b)"); }
#[test] fn groups_burst_into_one_key() {
    let mut r = Scripted::new(vec![Some(b"\x1b[A".to_vec()), None]);
    assert_eq!(read_key(&mut r, Duration::from_millis(50)).unwrap(), b"\x1b[A");
}
#[test] fn lone_escape_is_its_own_key() {
    let mut r = Scripted::new(vec![Some(vec![0x1b]), None, Some(b"a".to_vec())]);
    assert_eq!(read_key(&mut r, Duration::from_millis(50)).unwrap(), vec![0x1b]);
    assert_eq!(read_key(&mut r, Duration::from_millis(50)).unwrap(), b"a");
}
#[test] fn double_ctrl_bracket_exits_and_logs() {
    let mut io = ScriptedIo::new(vec![Some(b"x".to_vec()), None, Some(vec![0x1d]), None, Some(vec![0x1d]), None]);
    let mut log = Vec::new();
    echo_loop(&mut io, &mut log).unwrap();
    assert_eq!(String::from_utf8(log).unwrap(), "x  (78)\n^]  (1d)\n^]  (1d)\n");
}
```
(`Scripted`: a test `ReadTimeout` whose `Some(bytes)` entries are returned one read each and whose `None` entries mean "timeout → Ok(0)". `ScriptedIo` adds a `Write` that collects into a `Vec`.)

- [ ] **Step 2: Run** `cargo test keys` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test keys` — pass.
- [ ] **Step 5: Commit** — `git commit -m "feat: testcard key echo recorder"`

---

### Task 5: CLI wiring and hardware instructions

**Files:**
- Modify: `src/main.rs`
- Create: `docs/testcard.md`

**Interfaces:**
- Consumes: everything above.
- CLI (clap derive):
  ```
  muthur [--config PATH] [--device PATH] [--baud N] <COMMAND>
    testcard [--cols 80] [--rows 43]   # probe screen, waits for a key, then clears
    testcard flood [--lines 2000]
    testcard keys [--out muthur-keys.txt]
  ```
  `--device`/`--baud` override the config. Link selection: a device from the flag or config → `Link::open_device`; otherwise `Link::stdio(cfg.link.flow)`. Call `install_panic_hook()` before opening the link. A bare `muthur` with no subcommand prints `"MU/TH/UR: M0 build — only 'testcard' is available"` and exits 0.

- [ ] **Step 1: Implement** `main.rs`.

- [ ] **Step 2: Smoke-test in a desktop terminal**

Run: `cargo run -- testcard --rows 24` → card renders, any key clears it, and the shell is sane afterwards (`stty -a` shows `icanon echo`).
Run: `cargo run -- testcard keys --out /tmp/k.txt`, press Up, F1, Ctrl-] Ctrl-] → the file holds three or more described lines.
Run: `cargo run -- testcard flood --lines 200` → ends with `FLOOD COMPLETE 200 LINES`.

- [ ] **Step 3: Write `docs/testcard.md`**

Cover: building (`cargo build --release --target x86_64-unknown-linux-musl`) and copying to the server. Running either from the logged-in serial session (stdio) or with `--device /dev/ttyUSB0` while no getty holds the port (`systemctl stop serial-getty@ttyUSB0`). For the card: report which numbered items render wrong. For flood: report whether any line's alphabet is shifted or missing (which means flow control is failing). For keys: press every F-key (F1–F16, shifted too), the arrows, Home, PgUp/PgDn, Insert/Delete, Backspace, Return, Enter (keypad), and the keypad digits, then send back `muthur-keys.txt`. Include the deferred RTS/CTS test procedure: set the WY-60 handshake to DTR/RTS in setup, `flow = "rts-cts"` in config, re-run flood.

- [ ] **Step 4: Verify** — `cargo test` passes, `cargo clippy --all-targets -- -D warnings` is clean, and the musl release build is static.

- [ ] **Step 5: Commit** — `git commit -m "feat: muthur testcard CLI and hardware test guide"`
