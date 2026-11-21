# M11 Generalize the Install Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Anyone with a VT100-ish terminal on a USB-serial adapter can run `./install.sh` (or `make HOST=… deploy`) and get a MUTHUR login on the serial line under bash, zsh or fish, idempotently and reversibly.

**Architecture:** One POSIX `install.sh`, all logic in functions behind a `MUTHUR_INSTALL_LIB` guard so `tests/install/run.sh` can source it against a fake `$HOME` and a fake root. Every system path is `$ROOT/...` (default empty), and stub `sudo`/`systemctl`/`udevadm`/… on `PATH` log their argv to `$STUBLOG`. The Makefile shrinks to build + copy + `ssh -t sh install.sh`. Three small Rust changes: login-shell default, `muthur-tty` deny patterns, and setting the serial tty's window size.

**Tech Stack:** POSIX sh (checked with `shellcheck -s sh`, run under `dash` and `bash --posix` in tests), udev, systemd, nftables, Rust (`nix` with the `user`/`term`/`ioctl` features already enabled).

**Spec:** `docs/superpowers/specs/2026-11-21-m11-generalize-install-design.md`

## Global Constraints

- Device name `/dev/ttyMUTHUR`; unit `serial-getty@ttyMUTHUR.service`; rule `/etc/udev/rules.d/99-ttyMUTHUR.rules`; drop-in `/etc/systemd/system/serial-getty@ttyMUTHUR.service.d/override.conf`.
  - Note: M11's final review renamed `muthur-tty` to `ttyMUTHUR`; a dash in a systemd instance name unescapes to `/`, so `serial-getty@muthur-tty` bound to `/dev/muthur/tty` and never started. Later references to `muthur-tty` in this plan mean `ttyMUTHUR`.
- Old names migrated and removed: `/etc/udev/rules.d/99-wy60.rules`, `/etc/systemd/system/serial-getty@wy60.service.d/`; `serial-getty@ttyUSB0` disabled if enabled.
- Marker lines, exactly: `# >>> muthur >>>` and `# <<< muthur <<<`.
- Placeholders `@NAME@` (uppercase, `[A-Z_]+`); a rendered file containing `@[A-Z_]+@` is an error.
- Default baud `38400`. Binary `~/.local/bin/muthur` (previous kept as `muthur.old`); launcher `~/.local/bin/muthur-login`.
- Supported: systemd Linux (`/run/systemd/system` exists) on `x86_64` and `aarch64`. Never run as root.
- `install.sh` flags, exactly: `--yes --shell bash|zsh|fish --vid V --pid P --serial S --port ID_PATH --baud N --no-agent --no-system --bin PATH --uninstall --help`.
- `set -eu`; no bashisms (`local` is allowed: dash, bash and busybox support it).
- Every step prints `==> <step>`.
- The config file is never modified if it exists.
- Branch `m11-generalize-install`. Commit messages end with the session's attribution lines.

**Spec refinements made while planning** (amend the spec in Task 1's commit):
- Port-path fallback matches `ENV{ID_PATH}=="…"` (the property behind `/dev/serial/by-path`), not `KERNELS==`: it is on the tty device itself, so no parent walk is needed. `--port` takes an `ID_PATH` value.
- The window size is set only when stdin's tty is not a pseudo-terminal (`ttyname` not under `/dev/pts/`). Otherwise `cargo run` in a desktop terminal would resize the emulator's pty.

## Review Focus

- **Re-running on an installed host** must change nothing: no second marker block, no second `include` line in `nftables.conf`, the udev rule byte-identical. Task 6 `t_rerun_is_noop`, Task 5 `t_block_idempotent`.
- **Symlinked dotfiles** (stow, chezmoi, home-manager) must stay symlinks; the edit goes to the target. Task 5 `t_block_follows_symlink`.
- **A dotfile without a trailing newline, or one that is read-only**, keeps every original byte; the block goes on its own lines, and the mode is preserved. Task 5 `t_block_no_trailing_newline`, `t_block_keeps_mode`.
- **Running from another directory** (`sh ~/src/muthur/install.sh`, or `make deploy`'s temp dir) finds `deploy/` beside the script, not in the cwd. Task 5 `t_script_dir_independent_of_cwd`.
- **Config answers with `sed`-special characters** (`/`, `&`, `\`, `|` in a `base_url` or key) are written verbatim. Task 8 `t_config_special_chars`.

---

### Task 1: `--version` and the login-shell default

**Files:**
- Modify: `src/main.rs` (the `#[command]` on `Cli`)
- Modify: `src/config.rs` (`ShellSection`, its tests)
- Modify: `src/run.rs:763` (where `ShellConfig.command` is filled)
- Modify: `docs/superpowers/specs/2026-11-21-m11-generalize-install-design.md` (the two refinements above)

**Interfaces:**
- Produces: `pub fn default_shell() -> String` in `src/config.rs`; `ShellSection.command: Option<String>`; `ShellSection::resolved_command(&self) -> String`.

- [ ] **Step 1: Write the failing tests** in `src/config.rs` tests:
  - `shell_command_unset_by_default`: `Config::default().shell.command == None`, and `scrollback == 1000` (replaces the `("fish", 1000)` assertion at line 660).
  - `shell_command_from_config_wins`: `[shell]\ncommand = "zsh"\n` → `resolved_command() == "zsh"`.
  - `pick_shell_order`: the pure helper `pick_shell(passwd: Option<&str>, env: Option<&str>) -> String`: `(Some("/usr/bin/zsh"), Some("/bin/bash")) → "/usr/bin/zsh"`; `(None, Some("/bin/bash")) → "/bin/bash"`; `(Some(""), None) → "/bin/sh"`; `(None, Some("")) → "/bin/sh"`.
  - `cli_has_version` in `src/main.rs` (a `#[cfg(test)]` module): `Cli::try_parse_from(["muthur", "--version"])` errs with `ErrorKind::DisplayVersion`.
- [ ] **Step 2: Run them** with `cargo test pick_shell shell_command cli_has_version`. Expected: compile errors or FAIL.
- [ ] **Step 3: Implement.** `#[command(version)]`. `default_shell()` passes the passwd shell (`nix::unistd::User::from_uid(getuid())`, its `shell` as a string) and `std::env::var("SHELL").ok()` to `pick_shell`; empty strings count as unset. Use `resolved_command()` in `run.rs`. In `docs/config.example.toml`, replace `command = "fish"` with `# command = "fish"   # default: your login shell (passwd), else $SHELL, else /bin/sh`.
- [ ] **Step 4: Run** `make check`. Expected: PASS in both feature modes.
- [ ] **Step 5: Commit** `feat: shell tab defaults to the login shell; muthur --version` (include the spec amendment).

### Task 2: Deny rules cover `muthur-tty`

**Files:**
- Modify: `src/tools/deny.rs:63-68` (lockout list) and its test table (~line 147)

- [ ] **Step 1: Add test rows** next to the `wy60` ones: `("systemctl stop serial-getty@muthur-tty", true, Some("LOCKOUT"))`, `("systemctl mask serial-getty@muthur-tty.service", true, Some("LOCKOUT"))`, `("systemctl status serial-getty@muthur-tty", false, None)`, `("rm /etc/udev/rules.d/99-muthur-tty.rules", true, Some("LOCKOUT"))`, `("stty -F /dev/muthur-tty 9600", true, Some("LOCKOUT"))`. Keep every existing `wy60` row.
- [ ] **Step 2: Run** `cargo test deny`. Expected: FAIL on the new rows.
- [ ] **Step 3: Implement.** Add the four `muthur-tty` patterns in the same shape as the `wy60` ones (`serial-getty@muthur-tty.*…`, `…serial-getty@muthur-tty`, `99-muthur-tty\.rules`, `/dev/muthur-tty`).
- [ ] **Step 4: Run** `cargo test deny`. Expected: PASS.
- [ ] **Step 5: Commit** `feat: deny rules guard the muthur-tty login path`.

### Task 3: Window size from `[terminal]` on a serial tty

**Files:**
- Modify: `src/link.rs` (new functions, `Link::stdio` signature, tests)
- Modify: `src/main.rs:106`, `src/run.rs:621` (callers)

**Interfaces:**
- Produces: `pub fn set_winsize(fd: BorrowedFd, cols: u16, rows: u16) -> io::Result<()>`; `pub fn is_pseudo_tty(name: &Path) -> bool`; `Link::stdio(flow: FlowControl, size: (u16, u16)) -> anyhow::Result<Link>`.

- [ ] **Step 1: Write the failing tests** in `src/link.rs` tests:
  - `set_winsize_on_pty`: `openpty`, `set_winsize(slave, 80, 43)`, then `TIOCGWINSZ` on the slave reads `ws_col == 80, ws_row == 43`.
  - `pseudo_tty_names`: `/dev/pts/3 → true`, `/dev/ttyUSB0 → false`, `/dev/ttyS0 → false`, `/dev/muthur-tty → false`.
- [ ] **Step 2: Run** `cargo test --lib link`. Expected: compile error.
- [ ] **Step 3: Implement.** Use `nix::ioctl_write_ptr_bad!` with `libc::TIOCSWINSZ` and `libc::winsize` (the read in the test can use `ioctl_read_bad!` / `TIOCGWINSZ`). `Link::stdio` calls `nix::unistd::ttyname(stdin)`; unless `is_pseudo_tty`, it calls `set_winsize`. A failure (of either call) is `tracing::warn!`, never an error. Pass `(cfg.terminal.cols, cfg.terminal.rows)` from both callers. `Drop` does not restore the size.
- [ ] **Step 4: Run** `make check`. Expected: PASS.
- [ ] **Step 5: Commit** `feat: set the serial tty window size from [terminal]`.

### Task 4: Templates, launcher and hooks

**Files:**
- Create: `deploy/udev/99-muthur-tty.rules.in`, `deploy/systemd/serial-getty@muthur-tty.service.d/override.conf.in`, `deploy/sysusers.d/muthur-agent.conf.in`, `deploy/login/muthur-login`, `deploy/login/hook.sh`, `deploy/login/muthur.fish`
- Delete: `deploy/udev/99-wy60.rules`, `deploy/systemd/serial-getty@wy60.service.d/`, `deploy/sysusers.d/muthur-agent.conf`, `deploy/fish/`
- Modify: `tests/deploy.rs` (render the templates before checking them)

**Interfaces:**
- Produces: template placeholders `@MATCH@` (udev), `@AGETTY@` and `@BAUD@` (drop-in), `@NOLOGIN@` (sysusers). `muthur-login` honours `MUTHUR_TTY` (default `/dev/muthur-tty`) and `MUTHUR_BIN` (default `$HOME/.local/bin/muthur`) for tests. `hook.sh` is the body between the markers.

Exact contents:

```
# 99-muthur-tty.rules.in
SUBSYSTEM=="tty", @MATCH@, SYMLINK+="muthur-tty", TAG+="systemd", ENV{SYSTEMD_WANTS}+="serial-getty@muthur-tty.service"
```
```
# override.conf.in
[Service]
ExecStart=
ExecStart=-@AGETTY@ -o '-p -- \\u' @BAUD@ %I vt100
```
`muthur-agent.conf.in` = the current sysusers file with `/usr/bin/nologin` → `@NOLOGIN@`.
```sh
# hook.sh (between markers)
case $- in *i*) [ -x "$HOME/.local/bin/muthur-login" ] && "$HOME/.local/bin/muthur-login" ;; esac
```
```fish
# muthur.fish
if status is-login; and status is-interactive
    test -x ~/.local/bin/muthur-login; and ~/.local/bin/muthur-login
end
```
`muthur-login`: `#!/bin/sh`. If `realpath "$MUTHUR_TTY"` fails or differs from `tty`, exit 0. Otherwise `stty erase '^H' ixon ixoff` and run `"$MUTHUR_BIN"` (not `exec`); exit 0 whatever MUTHUR returns, so a failed `[ -x … ] &&` never trips `set -e` in a user's profile.

- [ ] **Step 1: Update `tests/deploy.rs`.** Add a helper `render(rel, &[(&str, &str)]) -> String` that replaces each `@K@` and asserts that no `@[A-Z_]+@` remains. Point `sysusers_dry_run` and `files_have_expected_lines` at the rendered `.in` with `NOLOGIN=/usr/bin/nologin`. Add `udev_template_renders` (rendering with `MATCH=ATTRS{idVendor}=="0403", ATTRS{idProduct}=="6001", ATTRS{serial}=="X"` yields one line containing `SYMLINK+="muthur-tty"`; runs `udevadm verify` on it when available, skipping like the others). Add `dropin_template_renders` (`AGETTY=/sbin/agetty BAUD=38400` → contains `ExecStart=-/sbin/agetty -o '-p -- \\u' 38400 %I vt100`). Delete `makefile_creates_each_drop_in_dir_first` here; Task 6 re-creates the check against `install.sh`.
- [ ] **Step 2: Run** `cargo test --test deploy`. Expected: FAIL (the files are missing).
- [ ] **Step 3: Create the files** above; `chmod +x deploy/login/muthur-login`; `git rm` the old ones.
- [ ] **Step 4: Run** `cargo test --test deploy`, `shellcheck -s sh deploy/login/muthur-login deploy/login/hook.sh`, and `fish -n deploy/login/muthur.fish` (if installed). Expected: PASS and clean.
- [ ] **Step 5: Commit** `feat: templated udev/getty/sysusers files and a shell-neutral login launcher`.

### Task 5: `install.sh` core and the test harness

**Files:**
- Create: `install.sh` (executable), `tests/install/run.sh`, `tests/install/stubs/` (one stub per tool: `sudo udevadm systemctl getent id systemd-sysusers systemd-tmpfiles nft visudo agetty`)

**Interfaces:**
- Produces (all in `install.sh`; later tasks add more):
  - Globals: `ROOT` (default `""`), `SYSTEMD_RUN` (default `/run/systemd/system`), `SCRIPT_DIR` (the directory of `$0`, resolved via `cd -P`), `YES` (0/1), plus the flag values `OPT_SHELL OPT_VID OPT_PID OPT_SERIAL OPT_PORT OPT_BAUD OPT_AGENT OPT_SYSTEM OPT_BIN OPT_UNINSTALL`.
  - `say msg` → `==> msg`; `warn msg` → `warning: msg` on stderr; `die msg` → `error: msg` on stderr, exit 1.
  - `ask prompt default` → echoes the answer (`default` with `YES=1`; `die "… (pass --<flag>)"` when stdin is not a tty, unless `MUTHUR_ASSUME_TTY=1`, which the tests set when they feed answers on stdin). Callers pass the flag name as a third argument.
  - `confirm prompt` → status 0 for yes; default yes.
  - `parse_args "$@"`; `preflight`.
  - `render template out K=V …` → writes `out`; `die`s if `@[A-Z_]+@` remains.
  - `atomic_write file` → reads stdin, writes `file.muthur.tmp.$$` in the same directory, keeps the original's mode, `mv`s it into place; if `file` is a symlink, it writes the resolved target instead.
  - `block_write file`, which reads the block body on stdin; `block_remove file`; `block_present file` (status).
- Stubs: each logs `"$(basename "$0") $*"` to `$STUBLOG`. `sudo` runs `"$@"`. `getent` prints `$STUB_PASSWD`. `id -u` prints `$STUB_UID` (default 1000); `id -un` prints `tester`. The others exit `${STUB_<NAME>_RC:-0}`; `udevadm info` prints the file named by `$STUB_UDEV_<devname>`.

`tests/install/run.sh`: each `t_*` function runs in a subshell with a fresh `mktemp -d` as `HOME` and `ROOT`, the stubs first on `PATH`, `MUTHUR_INSTALL_LIB=1`, and `. ./install.sh`. Helpers `eq expected actual msg` and `ok cmd… msg`. It prints `ok <name>` / `FAIL <name>: …`, exits non-zero on any failure, and runs itself under `dash` when available (else `sh`).

- [ ] **Step 1: Write the failing tests:**
  - `t_block_insert_empty`: on a missing file, `printf 'X\n' | block_write f` gives exactly `# >>> muthur >>>\nX\n# <<< muthur <<<\n`.
  - `t_block_idempotent`: writing twice gives an identical file with one marker pair.
  - `t_block_replaces_stale`: a file `a\n<block with OLD>\nb\n` written with `NEW` becomes `a\n<block with NEW>\nb\n`.
  - `t_block_remove_exact`: `a\n` + block + `b\n` → `a\nb\n`, byte-identical (`cmp`).
  - `t_block_no_trailing_newline`: `a` (no newline) + write → `a\n# >>> muthur >>>…`; remove → `a\n`.
  - `t_block_follows_symlink`: `f -> real`; after the write, `f` is still a symlink and `real` holds the block.
  - `t_block_keeps_mode`: a file with mode 600 is still 600 afterwards.
  - `t_render_ok` / `t_render_leftover`: rendering the udev template with `MATCH=…` leaves no `@`; rendering without it `die`s with status 1.
  - `t_preflight_root`: `STUB_UID=0` → `die`s with a message containing `not as root`.
  - `t_preflight_no_systemd`: `SYSTEMD_RUN=$HOME/nope` → `die`s with a message containing `manual-install`.
  - `t_script_dir_independent_of_cwd`: `cd /` and source `$REPO/install.sh` → `SCRIPT_DIR` = the repo root, and `$SCRIPT_DIR/deploy/login/muthur-login` exists.
  - `t_ask_no_tty_names_flag`: with stdin `</dev/null` and `YES=0`, `ask "Shell?" "" shell` `die`s with a message containing `--shell`.
  - `t_parse_args_unknown`: `parse_args --bogus` `die`s; `parse_args --shell zsh --no-agent` sets `OPT_SHELL=zsh OPT_AGENT=0`.
- [ ] **Step 2: Run** `sh tests/install/run.sh`. Expected: FAIL (no `install.sh`).
- [ ] **Step 3: Implement** the functions above and `main` (for now: `parse_args`, `preflight`, then steps that later tasks fill in). Guard: `[ -n "${MUTHUR_INSTALL_LIB:-}" ] || main "$@"`. Use `sed -n '/^# >>> muthur >>>$/,/^# <<< muthur <<<$/!p'` (or an `awk` equivalent) for removal; insertion appends after ensuring the file ends in a newline. `preflight` requires `sudo udevadm` always, `agetty` unless `OPT_SYSTEM=0`, and `nft bash visudo` when `OPT_AGENT=1`, each through `command -v`.
- [ ] **Step 4: Run** `sh tests/install/run.sh` and `shellcheck -s sh install.sh tests/install/run.sh`. Expected: all `ok`; clean.
- [ ] **Step 5: Commit** `feat: install.sh core (args, preflight, render, marker blocks) with shell tests`.

### Task 6: Adapter detection, system files, agent account, migration

**Files:**
- Modify: `install.sh`, `tests/install/run.sh`
- Create: `tests/install/fixtures/udev-ftdi.txt`, `tests/install/fixtures/udev-ch340.txt` (`udevadm info --query=property` output; FTDI with `ID_SERIAL_SHORT=A9E91BA3`, CH340 `1a86:7523` without one, both with `ID_PATH` and `DEVLINKS` holding a `/dev/serial/by-id/…` entry)

**Interfaces:**
- Consumes: Task 5's helpers.
- Produces: `parse_udev_props` (reads stdin; sets `ADP_VID ADP_PID ADP_SERIAL ADP_PATH ADP_BYID`); `list_adapters` (one line per device: `dev<TAB>vid<TAB>pid<TAB>serial<TAB>idpath<TAB>byid`, from `$ROOT/dev/ttyUSB*` and `$ROOT/dev/ttyACM*`); `match_line` (echoes the `@MATCH@` value from `ADP_*`); `step_adapter`; `step_system`; `step_agent`; `find_nologin`; `find_agetty`.

- [ ] **Step 1: Write the failing tests:**
  - `t_parse_ftdi`: fixture → `ADP_VID=0403 ADP_PID=6001 ADP_SERIAL=A9E91BA3` and a non-empty `ADP_BYID`; `match_line` = `ATTRS{idVendor}=="0403", ATTRS{idProduct}=="6001", ATTRS{serial}=="A9E91BA3"`.
  - `t_parse_ch340_no_serial`: `match_line` = `ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="7523", ENV{ID_PATH}=="<fixture ID_PATH>"`; `step_adapter` warns with text containing `port`.
  - `t_adapter_none`: no `$ROOT/dev/tty*` → `die`s with text containing `--vid`.
  - `t_adapter_flags_skip_detection`: `--vid 0403 --pid 6001 --serial S` → no `udevadm info` in `$STUBLOG`.
  - `t_adapter_several_yes_dies`: two devices with `YES=1` and no flags → `die`s (no default).
  - `t_system_installs`: `step_system` creates `$ROOT/etc/udev/rules.d/99-muthur-tty.rules` and the drop-in at mode 644, and `$STUBLOG` has `systemctl daemon-reload`, `udevadm control --reload` and `udevadm trigger --subsystem-match=tty --action=add` in that order.
  - `t_system_migrates_wy60`: with `$ROOT/etc/udev/rules.d/99-wy60.rules` and the wy60 drop-in present → both are gone, and `systemctl stop serial-getty@wy60` precedes their removal in the log.
  - `t_agent_files`: `step_agent` installs the rendered sysusers file (no `@`), tmpfiles, sudoers with `tester` in place of `OPERATOR` (mode 440, after `visudo -cf` in the log) and `$ROOT/etc/nftables.d/muthur-agent.nft`; `nft -f` is logged. It creates each `$ROOT/etc/*.d` directory before installing into it (the check moved from `tests/deploy.rs`).
  - `t_agent_nft_include_once`: with `$ROOT/etc/nftables.conf` lacking the include and `YES=1`, the include line is added; run again → it is present exactly once.
  - `t_agent_nsswitch_warns`: `hosts: files resolve dns` → a warning containing `resolve`.
  - `t_rerun_is_noop`: `step_system; step_agent` twice → `$ROOT/etc` is identical (`find … -exec cksum`) between the runs.
- [ ] **Step 2: Run** `sh tests/install/run.sh`. Expected: the new tests FAIL.
- [ ] **Step 3: Implement.** The device list comes from globbing `$ROOT/dev/ttyUSB* $ROOT/dev/ttyACM*`; `udevadm info --query=property --name=<dev>` for each. Interactive choice: print numbered lines `N) /dev/ttyUSB0  0403:6001  serial A9E91BA3  (usb-FTDI_…)`, then `ask`. `find_nologin`: first existing of `/usr/sbin/nologin /usr/bin/nologin /sbin/nologin` under `$ROOT`, else `die`. `find_agetty`: `command -v agetty`, then `/sbin/agetty`, then `/usr/bin/agetty`. Installs are `sudo install -D -m 644` (sudoers `-m 440`). System paths are always `$ROOT/etc/…`. Commands from `step_agent` (`systemd-sysusers`, `systemd-tmpfiles --create`, `nft -f`, `systemctl enable nftables`) run through `sudo`. The `nftables.d` include check is a `grep -q 'nftables.d'`; the enable offer runs only when `systemctl is-enabled --quiet nftables` fails.
- [ ] **Step 4: Run** `sh tests/install/run.sh` and `shellcheck -s sh install.sh`. Expected: all `ok`; clean.
- [ ] **Step 5: Commit** `feat: install.sh adapter detection, system files, agent account and wy60 migration`.

### Task 7: Binary and login-hook steps

**Files:**
- Modify: `install.sh`, `tests/install/run.sh`

**Interfaces:**
- Produces: `step_binary`; `login_shell` (echoes `bash|zsh|fish` from `OPT_SHELL` or the basename of `getent passwd "$(id -un)"` field 7; `die`s on anything else, listing the supported shells); `bash_login_file` (echoes the first existing of `~/.bash_profile ~/.bash_login ~/.profile`, else `~/.bash_profile`); `zsh_login_file` (`${ZDOTDIR:-$HOME}/.zprofile`); `install_hook shell`; `remove_hooks` (all shells); `step_hook`.

- [ ] **Step 1: Write the failing tests:**
  - `t_binary_install`: `OPT_BIN` points at a fake executable → `~/.local/bin/muthur` matches it, and `muthur-login` is installed executable. A second run with a different file moves the first to `muthur.old`.
  - `t_binary_default_beside_script`: with no `--bin`, `$SCRIPT_DIR/muthur` is used; without it, a single `$SCRIPT_DIR/target/*/release/muthur` is used; with two, it `die`s naming `--bin`.
  - `t_shell_from_passwd`: `STUB_PASSWD='tester:x:1000:1000::/home/t:/usr/bin/zsh'` → `zsh`; `/bin/dash` → `die`s with text containing `bash, zsh, fish`.
  - `t_bash_file_choice`: only `.profile` → `.profile`; `.bash_login` and `.profile` → `.bash_login`; all three → `.bash_profile`.
  - `t_bash_creates_profile`: none exist → `~/.bash_profile` is created with `[ -f ~/.profile ] && . ~/.profile; [ -f ~/.bashrc ] && . ~/.bashrc` above the block.
  - `t_zsh_zdotdir`: `ZDOTDIR=$HOME/z` → the block lands in `$HOME/z/.zprofile` and not in `~/.zprofile`.
  - `t_fish_hook`: `~/.config/fish/conf.d/muthur.fish` matches `deploy/login/muthur.fish`.
  - `t_switch_shell_removes_old`: install for fish, then for bash → the fish conf.d file is gone and the block is in the bash file. Then zsh → no block left in the bash file.
  - `t_launcher_off_serial`: `MUTHUR_TTY` points at a missing path → `muthur-login` exits 0 without running `MUTHUR_BIN` (a stub that touches a file).
- [ ] **Step 2: Run** `sh tests/install/run.sh`. Expected: the new tests FAIL.
- [ ] **Step 3: Implement.** `step_hook` = `remove_hooks` then `install_hook "$(login_shell)"`. `remove_hooks` runs `block_remove` on all of `~/.bash_profile ~/.bash_login ~/.profile` and the zsh file, and removes the fish conf.d file. `install -m 755` is used for the binary and the launcher. Warn when `$HOME/.local/bin` is not in `PATH`.
- [ ] **Step 4: Run** `sh tests/install/run.sh` and `shellcheck -s sh install.sh`. Expected: all `ok`; clean.
- [ ] **Step 5: Commit** `feat: install.sh binary and bash/zsh/fish login hooks`.

### Task 8: First-run config, verify, uninstall, `main`

**Files:**
- Modify: `install.sh`, `tests/install/run.sh`

**Interfaces:**
- Produces: `step_config`; `set_toml_key file key value` (replaces the first `key = …` line after `^\[muthur\]$` with `key = "value"`); `delete_toml_key file key`; `step_verify`; `uninstall`; the complete `main`.

- [ ] **Step 1: Write the failing tests:**
  - `t_config_existing_untouched`: an existing `~/.config/muthur/config.toml` is byte-identical after `step_config`.
  - `t_config_anthropic`: with `MUTHUR_ASSUME_TTY=1`, answers `1` then key `sk-ant-abc` on stdin (prompts read from stdin; `stty -echo` goes through a stub) → mode 600, parent mode 700, `api_key = "sk-ant-abc"` under `[muthur]`, and `provider = "anthropic"` unchanged.
  - `t_config_openai`: answers `2`, `http://10.0.0.5:8000/v1`, `Qwen/Qwen3-32B`, an empty key → `provider = "openai"`, both values set, and no `api_key` line left in `[muthur]`.
  - `t_config_special_chars`: base_url `http://h/a&b|c\d` → written verbatim.
  - `t_config_rejects_quote`: a model containing `"` → it asks again (then a valid answer is accepted).
  - `t_config_yes`: `YES=1` → the example is copied unmodified and the output contains `api_key`.
  - `t_uninstall`: after `step_system; step_agent; step_binary; step_hook`, `uninstall` with `YES=1` → no muthur files under `$ROOT/etc`, no marker block in any login file, no fish hook, no `muthur-login`; the binary is removed (the confirm defaults to yes); the config is still present; the output contains `userdel muthur-agent`; the log has `nft delete table inet muthur_agent` and `systemctl daemon-reload`.
  - `t_verify_reports`: with no `$ROOT/dev/muthur-tty` → the output contains `not plugged in` and status 0. With `STUB_SYSTEMCTL_RC=3` → the output contains `serial-getty@muthur-tty` and `inactive`.
- [ ] **Step 2: Run** `sh tests/install/run.sh`. Expected: the new tests FAIL.
- [ ] **Step 3: Implement.** Do the value escaping in `awk` (pass the value via `ENVIRON`, never interpolated into the program) to avoid sed-escaping pitfalls. The section is `[muthur]` up to the next `^\[`. Read the key with `stty -echo` and a `trap` that restores `stty echo` on exit or interrupt. `main` order: `parse_args`; `preflight`; then with `--uninstall`, `uninstall` and exit; else `step_binary`, `step_adapter`, `step_system`, `step_agent`, `step_hook`, `step_config`, `step_verify`, honouring `OPT_SYSTEM` and `OPT_AGENT`. `--help` prints the usage block from the spec.
- [ ] **Step 4: Run** `sh tests/install/run.sh` and `shellcheck -s sh install.sh`. Expected: all `ok`; clean.
- [ ] **Step 5: Commit** `feat: install.sh first-run config, verify and --uninstall`.

### Task 9: Makefile and aarch64

**Files:**
- Modify: `Makefile`, `.cargo/config.toml`

- [ ] **Step 1: Rewrite the Makefile:**
  - `ARCH ?= x86_64`; `TARGET = $(ARCH)-unknown-linux-musl`; `BUILD ?= build` (`zigbuild` for cross).
  - `build: cargo $(BUILD) --release --target $(TARGET) $(FEATURES)`.
  - `deploy`: `ARCH` defaults to `$(shell ssh $(HOST) uname -m)` only when not set on the command line (use `origin`). It builds, then `tmp=$$(ssh $(HOST) mktemp -d)`, copies `$(BIN)` to `$$tmp/muthur` and `install.sh deploy/` with `scp -r`, runs `ssh -t $(HOST) sh $$tmp/install.sh --bin $$tmp/muthur $(INSTALL_ARGS)`, and removes `$$tmp` with `rm -rf` (a `trap`, so it runs even on failure). `AGENT=0` appends `--no-agent` to `INSTALL_ARGS`.
  - `install: build` → `./install.sh --bin $(BIN) $(INSTALL_ARGS)`.
  - Remove `install-system` and the `AGENT_SCP` and `AGENT_STEPS` blocks.
  - `check-deploy`: `cargo test --test deploy`; `shellcheck -s sh install.sh deploy/login/muthur-login deploy/login/hook.sh tests/install/run.sh` and `fish -n deploy/login/muthur.fish`, each skipped with a note when the tool is missing.
  - `check`: also `sh tests/install/run.sh`.
- [ ] **Step 2: Add** `[target.aarch64-unknown-linux-musl] rustflags = ["-C", "target-feature=+crt-static"]` to `.cargo/config.toml`.
- [ ] **Step 3: Verify:** `make check` and `make check-deploy` pass, and `make -n HOST=example ARCH=x86_64 deploy` prints the expected scp/ssh lines. If `cargo-zigbuild` and the target are installed, `make BUILD=zigbuild ARCH=aarch64 build` produces `target/aarch64-unknown-linux-musl/release/muthur` (otherwise note it as skipped).
- [ ] **Step 4: Commit** `build: make deploy runs install.sh; ARCH= for aarch64`.

### Task 10: Docs

**Files:**
- Modify: `docs/deploy.md` (rewrite), `README.md`, `docs/config.example.toml`, `docs/ARCHITECTURE.md`, `docs/hardware.md` (only where `/dev/wy60` or the fish snippet is named as the install path)

- [ ] **Step 1: Rewrite `docs/deploy.md`** with these sections:
  1. Requirements (systemd Linux, x86_64/aarch64, sudo, `udevadm`, `agetty`, plus `nft`/`bash`/`visudo` for the agent).
  2. Quick start (`./install.sh`, `make install`, `make HOST=… deploy`).
  3. Flags (a table of every flag).
  4. What gets installed (a list of every path).
  5. Migrating from `wy60`.
  6. Uninstalling.
  7. Recovering from a getty restart loop (new names).
  8. The agent account (the current text, with `install.sh` in place of `make install-system`).
  9. `## Manual install` for non-systemd hosts: run `muthur-login` from the login profile and a getty at the right baud.
  10. Checks before a release.

  Remove "omicron" except as an example `HOST=`.
- [ ] **Step 2: Update `README.md`:**
  - Features → "your login shell (bash, zsh or fish) on a PTY".
  - Hardware table: Host `Linux x86_64 or aarch64, systemd`; Alpine removed.
  - Building: add the aarch64 line.
  - Deploying: the new commands, plus "if MUTHUR exits you land in your login shell".
  - `install -Dm600` stays as the manual config path, with a note that `install.sh` does it.
- [ ] **Step 3: Update `docs/config.example.toml` and `docs/ARCHITECTURE.md`:**
  - `docs/config.example.toml`: the `device` comment becomes `/dev/muthur-tty`; `[terminal]` gets the comment "MUTHUR sets the serial tty to this size at startup".
  - `docs/ARCHITECTURE.md`: replace the `/dev/wy60`, `serial-getty@wy60` and fish-snippet references.
- [ ] **Step 4: Verify:** `grep -rn 'Alpine\|install-system\|deploy/fish\|99-wy60' README.md docs/*.md docs/config.example.toml` prints nothing, except the migration section in `deploy.md`.
- [ ] **Step 5: Commit** `docs: generic install for bash/zsh/fish hosts`.

## Manual checks (the operator, after the branch is done)

- `make HOST=omicron deploy` on the existing `wy60` install: it migrates, the login still starts MUTHUR, and `/dev/muthur-tty` exists.
- A throwaway account (`useradd -m -s /bin/bash t1`, then `/usr/bin/zsh`): log in on the serial line, and MUTHUR starts; quitting MUTHUR leaves you in the shell.
- `./install.sh --uninstall`, then a replug: there's no getty and no hook.
- aarch64 build on a Pi, if one is available.
