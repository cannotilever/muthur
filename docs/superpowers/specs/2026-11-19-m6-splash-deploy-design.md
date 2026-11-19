# M6 — Boot Splash and Deploy Tooling: Design

**Status:** approved in brainstorming on 2026-11-19. Implements "Boot splash" (Modes) and "Deployment & process model" in `docs/ARCHITECTURE.md`. "Polish" from the M6 row of the milestone table moves to a later milestone, after the operator has lived with a deployed build.

## Goal

Every login on the WY-60 opens with a Weyland-Yutani boot splash: a block-art logo, then boot lines typed at teletype pace that report real facts about the ship. Any key skips it. The splash runs inside the normal event loop, so the tabs, metrics, PTY and LLM client warm up behind it.

The server, `omicron` (Arch, systemd), can be set up from the dev machine:
- `make install-system` installs the udev rule and the getty override once.
- `make deploy` ships the binary and the fish login snippet on every change.

## Decisions

| Topic | Decision |
|---|---|
| Target OS | Arch / systemd only. No OpenRC files. |
| When the splash plays | Every launch. `[chrome] splash = true` by default; `false` disables it. |
| Style | Centred block-art logo, then a dotted-leader boot log (option B from brainstorming). The fallback, if the logo misbehaves on hardware, is the plain boot log without the logo. |
| Logo source | Data, not code: `assets/wy-logo.txt`, built in with `include_str!`. At most 22 rows × 80 columns. `█` = block (reverse-video space on the wire); any other character is printed as itself. |
| Vessel name | `ship_name`, as shown in the chrome (default `USCSS <HOSTNAME>`). |
| Placement | A `Splash` mode inside `App` and the existing event loop (approach 1), not a synchronous pre-roll. |
| Facts | Taken from the first metrics snapshot. Link facts come from config, interface status from `build_muthur`. |
| Skip | Any key goes straight to the TUI. The key is consumed. |
| Preview | `muthur splash` plays the splash once and exits, for tuning the logo on the terminal. |
| Deploy split | `make deploy` (no root, every change) and `make install-system` (sudo, once). |
| Device naming | udev matches the FT232R by vendor `0403`, product `6001` and serial `A9E91BA3`, creating `/dev/wy60`. |
| sudoers | `docs/sudoers.example` (M5) is not installed automatically. |

## Modules

```
assets/wy-logo.txt     The logo, including wordmark and slogan. Hand-tuned on hardware.
src/splash/
  mod.rs               Splash state machine, SplashInfo, boot-line facts, render.
  logo.rs              Parses the asset into rows of cells; budget constants.
deploy/
  udev/99-wy60.rules
  systemd/serial-getty@wy60.service.d/override.conf
  fish/muthur.fish
Makefile
docs/deploy.md
```

Changes elsewhere:
- **`app`:** a `splash: Option<Splash>` field and a `quit_after_splash` flag. Details below.
- **`run`:** builds `SplashInfo` and the `Splash` when `cfg.chrome.splash` is on. `build_muthur` also returns the interface status.
- **`main`:** a `Splash` subcommand next to `Testcard`.
- **`config`:** `ChromeConfig.splash: bool`, default `true`.
- **`docs/config.example.toml`:** documents `splash`.
- **`docs/ARCHITECTURE.md`:** the M6 milestone row is updated (polish deferred), and the `splash` module row links to this spec.

## Screen

The terminal is 80×43. Rows are 1-based.

| Rows | Content |
|---|---|
| 2–23 | Logo, centred horizontally. A shorter logo is centred vertically within this band. |
| 25 | Header: `MU/TH/UR 6000 — INTERFACE <year>`, centred. The year uses the same `year_offset` logic as the chrome. |
| 27–41 | Boot lines, starting at column 9 (left-aligned block). |
| 43 | `READY.`, at the same column as the boot lines. |

No chrome is drawn during the splash.

### Boot lines

Each line is `LABEL` padded with `.` to column 46 (one space either side of the dots), then the value. Values are uppercase.

| Label | Value | Source |
|---|---|---|
| `SYSTEM CHECK` | `OK` | Constant. |
| `VESSEL` | `USCSS OMICRON` | `ship_name`. |
| `PROCESSOR` | `16 CORES` | `snapshot.cpu_cores.len()`. |
| `MEMORY` | `31.2G` | `metrics::human_bytes(snapshot.mem.total)`. |
| `UPTIME` | `3D 04:12` | `metrics::human_uptime(snapshot.uptime_secs)`. Under one day: `04:12`. |
| `LINK` | `38400 BAUD  XON/XOFF` | `cfg.link.baud` and `cfg.link.flow` (`XON/XOFF`, `RTS/CTS` or `NONE`). |
| `SYSTEM UNITS` | `NOMINAL` / `2 FAILED` / `NOT MONITORED` | `snapshot.failed_units`: empty, non-empty, or `None`. |
| `INTERFACE` | `MU/TH/UR [ONLINE]` / `MU/TH/UR [OFFLINE: <REASON>]` | `build_muthur`'s status. |

Memory and uptime reuse the existing `metrics` helpers, so they match the Utility tab.

A line whose value needs the snapshot is a **fact line**. Fact lines are `PROCESSOR`, `MEMORY`, `UPTIME` and `SYSTEM UNITS`.

## Sequence and timing

The phases are `Logo → Header → Lines → Ready → Done`.

1. **Logo.** The first frame clears the screen and draws the whole logo. The wire paints it top to bottom at line speed (about 0.7 s for 2.5 KB). There is no artificial animation.
2. **Header.** Drawn in the frame after the logo.
3. **Lines.** The boot lines type out through `muthur::reveal::Reveal` at 400 characters/s. After each value there is a 150 ms pause. Before the value of a fact line, if no snapshot has arrived, the reveal waits up to 1.5 s from when that line's dots finished. On timeout, or at once if metrics are offline, the value is `NO DATA`. Once one fact line has timed out, all later fact lines without data show `NO DATA` without waiting again.
4. **Ready.** `READY.` appears and holds for 1 s.
5. **Done.** `App` drops the splash and sets `redraw`. The TUI arrives with the normal full repaint.

The splash takes about 4 s in total with a warm snapshot. The 400 characters/s rate and the 150 ms / 1.5 s / 1 s durations are constants in `splash/mod.rs`. They are not config keys.

## `Splash` API

```rust
pub struct SplashInfo {
    pub ship_name: String,
    pub year: i32,
    pub link: String,          // "38400 BAUD  XON/XOFF"
    pub interface: Interface,  // Online | Offline(String)
}

impl Splash {
    pub fn new(info: SplashInfo, now: Instant) -> Self;
    pub fn on_metrics(&mut self, s: &Snapshot);
    pub fn on_metrics_offline(&mut self);
    /// Steps phases and the reveal; true when the screen changed.
    pub fn advance(&mut self, now: Instant) -> bool;
    pub fn render(&self, frame: &mut Frame);
    pub fn tick_interval(&self) -> Duration;   // reveal cadence while typing, else 100 ms
    pub fn is_done(&self) -> bool;
}
```

Time is injected, as in `Reveal`. No test sleeps.

## `App` integration

While `app.splash` is `Some`:
- `render` calls `splash.advance(Instant::now())`. If the splash is done, it is dropped and `redraw` is set, and with `quit_after_splash` `quit` is set instead. Otherwise only the splash is drawn.
- `handle_key` drops the splash and sets `redraw` (or `quit`, with `quit_after_splash`). The key goes nowhere else, Ctrl-C included.
- `on_metrics` and `on_metrics_offline` feed the splash and the Utility tab.
- `tick_interval` returns the splash's interval.
- Tabs keep running underneath but are not drawn. Their activity markers are not lit by anything that happens during the splash.

`Event::Hangup` quits as it does today.

## `muthur splash`

A subcommand next to `testcard`. It:
1. Loads the config if one exists, or uses defaults.
2. Opens the link the same way `run` does, with the same termios restore and panic hook.
3. Builds an `App` with placeholder tabs, the splash, and `quit_after_splash = true`. It starts `LocalSource` metrics the same way `run` does.
4. Runs the normal `event_loop`, then clears the screen and exits.

It ignores `splash = false`. Without an LLM key the `INTERFACE` line shows `[OFFLINE: NO KEY]`. It respects `--strict`.

## Deploy

### udev — `deploy/udev/99-wy60.rules`

```
SUBSYSTEM=="tty", ATTRS{idVendor}=="0403", ATTRS{idProduct}=="6001", ATTRS{serial}=="A9E91BA3", SYMLINK+="wy60", TAG+="systemd"
```

`TAG+="systemd"` creates `dev-wy60.device`, so the getty waits for the adapter at boot.

### getty — `deploy/systemd/serial-getty@wy60.service.d/override.conf`

```
[Service]
ExecStart=
ExecStart=-/sbin/agetty -o '-p -- \\u' 38400 %I vt100
```

This pins the line at 38400 and sets `TERM=vt100`. XON/XOFF is set by the fish snippet and by MUTHUR's link setup, not by agetty.

### fish — `deploy/fish/muthur.fish`

Installed to `~/.config/fish/conf.d/muthur.fish`. The user's own `config.fish` is untouched.

```fish
if status is-login; and status is-interactive
    and test (tty) = (realpath /dev/wy60 2>/dev/null)
    stty rows 43 cols 80 erase ^H ixon ixoff
    ~/.local/bin/muthur
end
```

`tty` reports the real device (`/dev/ttyUSB0`), so the comparison resolves the symlink. It runs `muthur`, not `exec muthur`: if MUTHUR exits or crashes, the operator lands in plain fish.

### Makefile

```
HOST   ?= omicron
TARGET := x86_64-unknown-linux-musl
BIN    := target/$(TARGET)/release/muthur
```

| Target | Does |
|---|---|
| `build` | `cargo build --release --target $(TARGET)`. |
| `deploy` | `build`; `ssh` creates `~/.local/bin` and `~/.config/fish/conf.d`; `scp` the binary to `~/.local/bin/muthur.new`; on the host, `mv muthur muthur.old` (if present) then `mv muthur.new muthur`; `scp` `muthur.fish` into `conf.d`. |
| `install-system` | `scp` the udev rule and the drop-in to a `mktemp -d` directory on the host. Then, over `ssh -t` so sudo can prompt, run with `set -e`: `sudo install -m 644` each file into `/etc/udev/rules.d/` and `/etc/systemd/system/serial-getty@wy60.service.d/`; `udevadm control --reload`; `udevadm trigger`; `systemctl daemon-reload`; disable `serial-getty@ttyUSB0` if enabled; `systemctl enable --now serial-getty@wy60`. Each step echoes its name first. Every step is safe to re-run. |
| `check-deploy` | `udevadm verify deploy/udev/99-wy60.rules` and `systemd-analyze verify` on the drop-in, run locally. |

The rename in `deploy` is atomic, so a running MUTHUR keeps its old binary instead of failing with "text file busy". Rolling back is `mv muthur.old muthur` on the host.

### `docs/deploy.md`

A first-time walkthrough covering:
- `rustup target add x86_64-unknown-linux-musl`
- `make install-system`, then `make deploy`
- checking `/dev/wy60` and `systemctl status serial-getty@wy60`
- rollback
- recovering from a getty restart loop (`systemctl disable --now serial-getty@wy60` from another session)
- installing `docs/sudoers.example` by hand, after reviewing it

## Error handling

| Case | Behaviour |
|---|---|
| `LocalSource` fails to start | `on_metrics_offline`: fact lines show `NO DATA` immediately. |
| Snapshot slow | Wait up to 1.5 s once, then `NO DATA` (see Sequence). |
| Hangup during the splash | Quit; termios restored as today. |
| Ctrl-C during the splash | Skips the splash. |
| Logo over budget or malformed | Caught by a unit test at build time. No runtime check. |
| `install-system` step fails | `set -e` stops it; the echoed step name shows where. |

## Testing

TDD, as in M3–M5.

**`splash` unit tests** (time-injected):
- Phase order and the times at which each phase starts with a ready snapshot.
- The reveal reaches `READY.`, which holds 1 s before `is_done`.
- Fact line waits 1.5 s, then shows `NO DATA`, and later fact lines don't wait again.
- `on_metrics_offline` gives `NO DATA` with no wait.
- Fact formatting from a fixed `Snapshot`: cores, memory, uptime, `NOMINAL` / `2 FAILED` / `NOT MONITORED`.
- `INTERFACE` online and offline.
- `logo.rs`: the shipped asset is within 22×80, and parsing maps `█` to blocks and keeps other characters.

**`App` tests:**
- A key during the splash skips it, is not delivered to any tab, and sets `redraw`.
- Finishing the splash sets `redraw`. With `quit_after_splash`, finishing or skipping sets `quit`.
- Metrics reach both the splash and the Utility tab.
- Without a splash, the app starts in the TUI as today.

**Golden:** `tests/golden/splash.txt`, the fully revealed splash rendered through `Wy60Backend`, including `--strict`.

**Byte budget:** the encoded logo frame is at most 3 KB (about 0.8 s at 38400 baud).

**Manual, by the operator on hardware:**
- `muthur splash` on the WY-60, while tuning the logo.
- Boot with the adapter attached, and check that the getty comes up on `/dev/wy60`.
- Log in, and check that the splash plays and the TUI follows.
- Kill MUTHUR, and check that plain fish remains.

## Out of scope

- Polish items (a later milestone).
- DECDHL / double-width text.
- OpenRC / Alpine deploy files.
- Installing the sudoers file.
- A once-per-boot splash.
- Config keys for splash timing.
