# M11 — Generalize the Install: Design

**Status:** approved in brainstorming on 2026-11-21.

## Goal

Someone other than the author, with a VT100-ish terminal on a USB-serial
adapter, clones the repo and runs one install step, locally or over ssh. They
end up with a login on the serial line that starts MUTHUR, whether their login
shell is bash, zsh or fish, without editing any file by hand. Re-running the
install is safe, and uninstalling is clean.

## What ties MUTHUR to one machine today

- The login hook exists only for fish (`deploy/fish/muthur.fish`) and
  hardcodes `stty rows 43 cols 80`.
- `[shell] command` defaults to `fish` (`src/config.rs`).
- The udev rule matches one specific FTDI serial number (`A9E91BA3`).
- Install is Makefile-only, over ssh to `HOST=omicron`, inside a nested
  `bash -c` string. A local install is impossible.
- `/usr/bin/nologin` is Arch-only (Debian: `/usr/sbin/nologin`). nftables
  persistence is left to the user.
- Builds target `x86_64-unknown-linux-musl` only.
- The README claims Alpine was tested, but the deploy requires systemd.
- `docs/deploy.md` is written for omicron.

## Decisions

| Topic | Decision |
|---|---|
| Installer shape | One POSIX `install.sh` that runs on the target host. `make deploy` copies it over with the binary and `deploy/`, then runs it over `ssh -t`. Local and remote installs share one code path. |
| Login hook | A shared POSIX launcher `~/.local/bin/muthur-login`, plus a one-line hook per shell: fish uses `conf.d/muthur.fish`; bash and zsh get a marked block (`# >>> muthur >>>` … `# <<< muthur <<<`) in their login file. |
| Adapter | Detected with `udevadm`, confirmed by the operator, or given by flags. Matched by VID, PID and serial; if there is no serial, by the port's `ID_PATH` (`ENV{ID_PATH}==`), with a warning. |
| Device name | `/dev/muthur-tty` and `serial-getty@muthur-tty`, replacing `wy60`. The installer migrates the old names. |
| Supported hosts | systemd Linux on x86_64 and aarch64. Non-systemd hosts get a documented manual path only. Alpine references are removed. |
| First run | If no config exists: copy the example with mode 600 and ask one question (Anthropic key, or OpenAI-compatible `base_url` and `model`). An existing config is never touched. |
| Binary location | Per-user `~/.local/bin/muthur`, keeping `muthur.old`. |
| Geometry | MUTHUR sets the tty window size from `[terminal]` itself, only when stdin's tty is not a pseudo-terminal; the launcher doesn't parse TOML. |
| Uninstall | `install.sh --uninstall` removes the hooks, udev rule, drop-in, agent sudoers, nft and sysusers/tmpfiles files. The binary is removed only after a prompt. The `muthur-agent` user, config and logs stay. |

## Components and file layout

```
install.sh                                    NEW  POSIX sh, idempotent, --uninstall
deploy/
  login/muthur-login                          NEW  launcher: tty check, stty, run muthur
  login/muthur.fish                           MOVED from deploy/fish/; calls muthur-login
  login/hook.sh                               NEW  bash/zsh marker block body
  udev/99-muthur-tty.rules.in                 NEW  template; replaces 99-wy60.rules
  systemd/serial-getty@muthur-tty.service.d/override.conf.in
                                              NEW  template; replaces the wy60 drop-in
  sysusers.d/muthur-agent.conf.in             CHANGED  nologin path templated
  sudoers.d/muthur-agent                      unchanged (OPERATOR placeholder)
  tmpfiles.d/, nftables/                      unchanged
Makefile                                      CHANGED  ARCH=; deploy runs install.sh
tests/install/run.sh                          NEW  POSIX shell tests for install.sh
src/config.rs                                 CHANGED  shell default = login shell
src/tools/deny.rs                             CHANGED  muthur-tty patterns added
src/link.rs                                   CHANGED  set TIOCSWINSZ on stdio tty
src/main.rs                                   CHANGED  clap `version` (for --version)
```

### Templates

Placeholders use the form `@NAME@` and are filled by `sed` in `install.sh`:

- `99-muthur-tty.rules.in`: `@MATCH@` is either
  `ATTRS{idVendor}=="V", ATTRS{idProduct}=="P", ENV{ID_SERIAL_SHORT}=="S"` (udev
  sanitises the serial, so the property, not the raw sysfs attribute, is matched) or
  `ATTRS{idVendor}=="V", ATTRS{idProduct}=="P"` plus `ENV{ID_PATH}=="<id_path>"` (a property of the tty
  device itself, so no parent walk is needed).
  The rest is the current rule with `SYMLINK+="muthur-tty"` and
  `ENV{SYSTEMD_WANTS}+="serial-getty@muthur-tty.service"`.
- `override.conf.in`: `@AGETTY@` (from `command -v agetty`, then
  `/sbin/agetty`, then `/usr/bin/agetty`) and `@BAUD@` (default 38400).
- `muthur-agent.conf.in`: `@NOLOGIN@`, the first of `/usr/sbin/nologin`,
  `/usr/bin/nologin` or `/sbin/nologin` that exists.

`make check-deploy` renders each template with dummy values into a temporary
directory and runs the existing checks on the result.

### `muthur-login`

POSIX sh. If the login tty is not `realpath /dev/muthur-tty`, it returns
immediately. Otherwise it runs `stty erase ^H ixon ixoff` and then
`~/.local/bin/muthur`, without `exec`, so the operator lands in their shell
when MUTHUR exits. The hooks invoke it only for interactive login shells:

- fish: `if status is-login; and status is-interactive; muthur-login; end`
  (with the full `~/.local/bin` path).
- bash/zsh block: `case $- in *i*) [ -x "$HOME/.local/bin/muthur-login" ] && "$HOME/.local/bin/muthur-login";; esac`.
  The block sits in a login-only file, so a login check isn't needed.

### Geometry in MUTHUR

When MUTHUR runs on stdio and stdin is a tty that is not a pseudo-terminal
(`ttyname` not under `/dev/pts/`), it sets `TIOCSWINSZ` to
`[terminal] cols × rows` at startup and leaves it set on exit, so the shell
afterwards sees the right size. On a pseudo-terminal (for example `cargo run`
in a desktop terminal) or a `--device` link, the window size isn't touched,
so the emulator's pty is never resized. A failure to set it is logged, not fatal.

### Shell tab default

`[shell] command` becomes optional. When it is unset: the passwd entry's shell
for the current uid (`getpwuid`), else `$SHELL`, else `/bin/sh`. The config
example comments it out.

### Deny rules

The lockout patterns add `serial-getty@muthur-tty`, `99-muthur-tty\.rules` and
`/dev/muthur-tty`. The `wy60` patterns stay, since they cost nothing and guard
hosts that haven't migrated.

## `install.sh` flow

```
install.sh [--yes] [--shell bash|zsh|fish] [--vid V --pid P [--serial S | --port ID_PATH]]
           [--baud N] [--no-agent] [--no-system] [--bin PATH] [--uninstall]
```

`--yes` accepts every default and fails rather than prompting when a required
answer has no default (for example, several adapters and no flags). The script
runs as the operator and calls `sudo` for system steps. Every step prints
`==> <step>` and is idempotent. `set -eu` is on throughout.

1. **Preflight.** Abort when run as root. Abort when `/run/systemd/system`
   doesn't exist, pointing to `docs/deploy.md#manual-install`. Require `sudo`,
   `udevadm` and `agetty`. With the agent enabled, also require `nft`,
   `bash` and `visudo`.
2. **Binary.** Install `--bin` (default `./muthur` beside the script, where
   `make deploy` stages it; otherwise `target/*/release/muthur` if exactly one
   exists) to `~/.local/bin/muthur`. The previous binary is kept as
   `muthur.old`. Also install `muthur-login`. If `~/.local/bin` isn't on
   `PATH`, warn (the hooks use absolute paths, so login still works).
3. **Adapter** (skipped with `--no-system`). Unless flags give VID, PID and
   a serial or port: list `/dev/ttyUSB*` and `/dev/ttyACM*`, and for each read
   `ID_VENDOR_ID`, `ID_MODEL_ID`, `ID_SERIAL_SHORT`, `ID_PATH`, and the `/dev/serial/by-id` name from
   `udevadm info`. With none found, abort with a hint to plug the adapter in
   or pass flags. With one, confirm it. With several, pick by number. With no
   serial, use `ID_PATH` and warn that moving the adapter to another port
   breaks the match.
4. **System files** (skipped with `--no-system`).
   - Render and install the udev rule and the getty drop-in.
   - Migrate: if `/etc/udev/rules.d/99-wy60.rules` or
     `/etc/systemd/system/serial-getty@wy60.service.d` exists, stop
     `serial-getty@wy60`, then remove both. Disable
     `serial-getty@ttyUSB0` if it is enabled (as the Makefile does today).
   - `systemctl daemon-reload`, `udevadm control --reload`,
     `udevadm trigger --subsystem-match=tty --action=add`.
5. **Agent account** (skipped with `--no-agent` or `--no-system`).
   - Render and install the sysusers file; install the tmpfiles file. Run
     `systemd-sysusers` and `systemd-tmpfiles --create`. Warn if the `adm` or
     `systemd-journal` groups are missing.
   - Render sudoers with `$(id -un)`, check it with `visudo -cf`, and install
     it with mode 440.
   - Install `/etc/nftables.d/muthur-agent.nft` and load it with `nft -f`.
   - If `/etc/nftables.conf` doesn't already include `/etc/nftables.d/`,
     offer to append `include "/etc/nftables.d/*.nft"` (default yes). If
     `nftables.service` isn't enabled, offer to enable it (default yes).
   - Warn if `resolve` appears in the `hosts:` line of `/etc/nsswitch.conf`.
6. **Login hook.**
   - Shell: `--shell`, else the basename of the passwd shell from `getent`.
     An unsupported shell aborts, listing the supported ones.
   - fish: install `~/.config/fish/conf.d/muthur.fish`.
   - bash: write the marker block into the first of `~/.bash_profile`,
     `~/.bash_login` or `~/.profile` that exists. If none exists, create
     `~/.bash_profile` with
     `[ -f ~/.profile ] && . ~/.profile; [ -f ~/.bashrc ] && . ~/.bashrc` above the
     block.
   - zsh: the marker block in `${ZDOTDIR:-$HOME}/.zprofile`.
   - Writing a block replaces any existing block in that file and leaves the
     rest of the file byte-identical. Hooks for the other shells are removed
     (the fish conf.d file, and blocks in the other files).
7. **Config.** If `~/.config/muthur/config.toml` doesn't exist and not
   `--yes`: install the example with mode 600 in a mode-700 directory. Ask
   "Anthropic or OpenAI-compatible?" For Anthropic, read the key with echo
   off (`stty -echo`) and write `api_key`. For OpenAI-compatible, ask for
   `base_url` and `model`, set `provider = "openai"`, and optionally ask for
   a key (an empty answer deletes the placeholder `api_key` line). Values are written by
   `sed` on the first `provider`, `base_url`, `model` and `api_key` lines of
   the example's `[muthur]` section, which are live (uncommented) lines; input
   is escaped for `sed` and rejected if it contains a newline or `"`. With
   `--yes`, copy the example only and print what to edit.
8. **Verify and summarise.** Check for `/dev/muthur-tty` (missing means a
   warning that the adapter isn't plugged in), `systemctl is-active
   serial-getty@muthur-tty`, the hook being present, and
   `~/.local/bin/muthur --version` running. Print one line per check plus
   the next steps.

`--uninstall` undoes steps 2 (the binary only after a prompt; `muthur-login`
always) and 4–6. It reloads systemd, udev and nft (`nft delete table inet
muthur_agent`). It prints the `sudo userdel muthur-agent` command rather than
running it. Config and logs are kept.

All logic lives in functions. `main "$@"` runs only when
`MUTHUR_INSTALL_LIB` is unset, so the tests can source the script.

### Makefile

- `ARCH ?= x86_64`; `TARGET := $(ARCH)-unknown-linux-musl`. In `deploy`,
  `ARCH` defaults to the remote `uname -m`. `.cargo/config.toml` gains the
  aarch64 musl target with static CRT. aarch64 builds are documented with
  `cargo-zigbuild` (`make BUILD=zigbuild ARCH=aarch64 build`); `BUILD` defaults
  to `build`.
- `deploy`: build; `tmp=$(ssh HOST mktemp -d)`; scp the binary as
  `$tmp/muthur`, plus `install.sh` and `deploy/`; then
  `ssh -t HOST sh $tmp/install.sh $(INSTALL_ARGS)` and remove `$tmp`. `AGENT=0`
  adds `--no-agent`.
- `install-system` is removed. `make install` runs `./install.sh` locally
  after `build`.
- `check-deploy` gains template rendering, `shellcheck -s sh` on `install.sh`,
  `muthur-login` and `hook.sh`, and `fish -n` on the fish hook (each skipped
  when the tool is missing). `check` also runs `sh tests/install/run.sh`.

## Error handling

- Every system step's failure aborts with the step's name; earlier steps are
  already idempotent, so re-running is the recovery.
- Rendered files are checked before install: no `@…@` left, `visudo -cf`
  for sudoers, `nft -c` for nft where privileges allow.
- Dotfile edits are written to a temporary file in the same directory and
  moved into place, so an interrupted run never leaves half a file. The
  original's mode is kept.
- With no tty on stdin and no `--yes`, a prompt aborts with a message naming
  the flag that answers it.

## Testing

`tests/install/run.sh`: POSIX sh, no framework. It sources `install.sh` with
`MUTHUR_INSTALL_LIB=1` against a temporary `HOME`, with stub `sudo`,
`udevadm`, `systemctl`, `getent` and `id` on `PATH`. Cases:

- The marker block is inserted into an empty file, not duplicated on re-run,
  replaced when stale, and removed with the surrounding text byte-identical.
- bash login-file choice: each of `.bash_profile`, `.bash_login` and
  `.profile`, and the created `.bash_profile` when none exists.
- zsh respects `ZDOTDIR`.
- Switching shells removes the old hook.
- Template rendering leaves no placeholders, for both the serial and the
  port-path match.
- Adapter parsing from canned `udevadm info` output: FTDI with a serial,
  CH340 without one.
- Preflight aborts as root and without `/run/systemd/system` (the path is
  overridable for the test).

Rust:

- Shell default: passwd shell, then `$SHELL`, then `/bin/sh`.
- Deny rules: `muthur-tty` lockout cases next to the existing `wy60` ones.
- Window size: the startup routine sets `cols × rows` from `[terminal]` on a
  non-pty tty, and leaves a pseudo-terminal (`/dev/pts/`) alone.

Manual (the operator):

- Migration on omicron from the `wy60` install.
- A fresh account with bash, then with zsh, as the login shell.
- An aarch64 build on a Pi, if one is available.

## Docs

- `docs/deploy.md`: rewritten for a generic host. Covers the quick start
  (`./install.sh` and `make HOST=… deploy`), the flags, what each step
  installs, migrating from the `wy60` names, uninstalling, a
  `## Manual install` section for non-systemd hosts, getty-loop recovery
  with the new names, and the existing agent account and isolation notes.
- `README.md`: Deploying and Building updated (aarch64 added), the Alpine
  references removed, and Features says "your login shell" rather than fish.
- `docs/config.example.toml`: `[shell] command` commented out with the new
  default, `device` comment updated to `/dev/muthur-tty`, `[terminal]` notes
  that MUTHUR sets the tty size.
- `docs/ARCHITECTURE.md`: references updated only.

## Out of scope

- Non-systemd init systems in the installer.
- System-wide (`/usr/local`) installs and distro packages.
- A full interactive configuration wizard.
- Renaming `Wy60Backend` or other internal names.
