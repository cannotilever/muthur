# Installing MUTHUR

`install.sh` sets up a host so that a serial terminal on a USB adapter logs in and runs MUTHUR. It works with bash, zsh or fish as the login shell.

## Requirements

- A Linux host with systemd, x86_64 or aarch64.
- `sudo`, `udevadm` and `agetty` (from util-linux).
- For the agent account: `nft`, `bash` and `visudo`. Skip it with `--no-agent`.
- The USB serial adapter plugged in, and a `muthur` binary (see Building in the README).

## Quick start

On the host, with a built binary in `target/*/release/` (or `--bin PATH`):

```sh
./install.sh
```

Or from the source tree, which builds first:

```sh
make install
```

From a dev machine, build, upload and run the installer over ssh:

```sh
make HOST=omicron deploy
```

`ARCH` defaults to the remote `uname -m`. An aarch64 build needs cargo-zigbuild: `make BUILD=zigbuild ARCH=aarch64 build`. `INSTALL_ARGS="--yes --shell fish"` passes flags through to `install.sh`.

The script asks before it acts: which adapter, which LLM provider and its API key. It can be run again; it rewrites its own files in place. An existing config is never touched. An empty Anthropic key stops the config step without writing a config. `--yes` accepts defaults and asks nothing, and copies the example config for you to edit by hand (set `api_key`, or `provider`, `base_url`, `model` and `api_key` for an OpenAI-compatible server).

Afterwards, plug in the adapter (or replug it) and log in on the terminal. The last step prints what it found: whether `/dev/ttyMUTHUR` exists, whether the getty is active, the hook file and `muthur --version`.

On a WY-60, turn the screen saver on in its setup menu: MUTHUR's standby screen (`[chrome] standby_minutes`, default 10) only stops output so the terminal can blank; with the terminal's saver off the standby logo stays lit.

To tune the boot logo, edit `assets/wy-logo.txt` and run `muthur splash` on the terminal. A line starting with `^` is drawn double height (DECDHL, so also double width: at most 40 characters) and centred on the screen.

## Flags

| Flag | Meaning |
|---|---|
| `--yes` | Accept defaults, ask nothing. |
| `--shell SHELL` | `bash`, `zsh` or `fish`. Default: your login shell from the passwd database. |
| `--vid V --pid P` | USB vendor and product id (four hex digits) of the adapter. Needs `--serial` or `--port`. |
| `--serial S` | The adapter's USB serial number. |
| `--port ID_PATH` | Match by udev `ID_PATH` instead, for an adapter with no serial number. Moving it to another port breaks the match. |
| `--baud N` | Serial speed (default 38400). |
| `--no-agent` | Skip the agent user, sudoers and nftables setup. |
| `--no-system` | Skip the udev rule, serial getty and agent setup (implies `--no-agent`). |
| `--bin PATH` | Install this `muthur` binary. |
| `--uninstall` | Remove what the script installed. |
| `--help` | Show the usage text. |

Without `--vid`, `--pid` and `--serial`/`--port`, the script lists the USB serial adapters it finds and asks which one. With several adapters, `--yes` gives no default and the script stops.

## What gets installed

As your user:

- `~/.local/bin/muthur`, with the previous one kept as `muthur.old`.
- `~/.local/bin/muthur-login`: the launcher. It runs only when the tty is `/dev/ttyMUTHUR`, sets erase and flow control, and runs `muthur` (not `exec`, so quitting MUTHUR returns you to your shell).
- The login hook, for your shell:
  - bash: a marked block in the first of `~/.bash_profile`, `~/.bash_login`, `~/.profile` that exists (`~/.bash_profile` is created if none does, and sources `~/.profile` and `~/.bashrc`).
  - zsh: a marked block in `${ZDOTDIR:-$HOME}/.zprofile`.
  - fish: `~/.config/fish/conf.d/muthur.fish`.

  The hook sets and exports `MUTHUR_LOGIN` in the login shell, so MUTHUR starts at most once per login. Hooks for other shells are removed on a re-run.
- `~/.config/muthur/config.toml`, mode 600, on first run.

As root (skipped by `--no-system`):

- `/etc/udev/rules.d/99-ttyMUTHUR.rules`: matches the adapter by `ENV{ID_SERIAL_SHORT}` (or `ENV{ID_PATH}` with `--port`), creates `/dev/ttyMUTHUR` and starts the getty through `SYSTEMD_WANTS`.
- `/etc/systemd/system/serial-getty@ttyMUTHUR.service.d/override.conf`: agetty at the chosen baud, `TERM=vt100`.

udev starts the getty when the adapter appears and stops it when it goes, so replugging brings the login back and boot never waits for an absent adapter. The getty is not enabled as a unit. If `serial-getty@ttyUSB0` was enabled, the script disables it.

As root, for the agent (skipped by `--no-agent`): `/etc/sysusers.d/muthur-agent.conf`, `/etc/tmpfiles.d/muthur-agent.conf`, `/etc/sudoers.d/muthur-agent` and `/etc/nftables.d/muthur-agent.nft`. See The agent account below.

MUTHUR sets the serial tty's rows and columns from `[terminal]` in the config at startup (only on a real serial tty). `[shell] command` defaults to your login shell, else `$SHELL`, else `/bin/sh`.

To roll back the binary: `cd ~/.local/bin && mv muthur.old muthur`.

## Migrating from wy60

Earlier versions used `/dev/wy60`, `99-wy60.rules` and `serial-getty@wy60`. Run `install.sh` again: it stops the old getty, removes the old rule and drop-in, and installs the new names. The old fish snippet in `conf.d/muthur.fish` is replaced by the new hook.

## Uninstalling

```sh
./install.sh --uninstall
```

This removes the login hooks and `muthur-login`; asks before removing `muthur` and `muthur.old`; removes the udev rule and getty drop-in (unless `--no-system`); and removes the agent files and the `muthur_agent` nft table (unless `--no-agent`). It keeps the config, the logs, the `muthur-agent` user (remove it with `sudo userdel muthur-agent`) and the include line in `/etc/nftables.conf`.

## Recovering from a getty restart loop

From another session (ssh):

```sh
sudo systemctl stop serial-getty@ttyMUTHUR
sudo mv /etc/udev/rules.d/99-ttyMUTHUR.rules /root/99-ttyMUTHUR.rules.off
sudo udevadm control --reload
```

Stopping alone is not enough: udev starts the getty again on the next replug or boot, so move the rule aside until the cause is fixed. Put it back and run `./install.sh` to restore.

## The agent account

`install.sh` also sets up the technical agent's account (skip it with `--no-agent`, or build without the agent with `make AGENT=0 deploy`, which also passes `--no-agent`). It installs, from `deploy/`:

- `sysusers.d` and `tmpfiles.d`: the `muthur-agent` system user (groups `adm`, `systemd-journal`; no sudo rights, not in `docker` or `wheel`) and its home `/var/lib/muthur-agent`.
- `sudoers.d/muthur-agent`: lets the deploying user run commands as `muthur-agent` without a password. `OPERATOR` is replaced with that user, and `visudo -cf` checks the result before it goes to `/etc/sudoers.d/`.
- `nftables/muthur-agent.nft`: blocks all non-loopback egress from `muthur-agent`, and its DNS (ports 53 and 853) to loopback resolvers as well. TCP is rejected with a reset, so connects fail at once with "Connection refused"; everything else is dropped, which fails at once with "Operation not permitted". Installed as `/etc/nftables.d/muthur-agent.nft`.

The nft file is loaded at install time. To keep it across reboots, `install.sh` offers to add this line to `/etc/nftables.conf` and to enable `nftables.service`; or do it by hand:

```
include "/etc/nftables.d/*.nft"
```

Check that the isolation works. This must fail at once with "write error: Operation not permitted":

```sh
sudo -u muthur-agent bash -c 'exec 3>/dev/udp/192.0.2.1/9 && echo x >&3'
```

TCP fails at once too, with "Connection refused", but that is no proof of the rule: a remote host can refuse a connection as well. Only the UDP check proves it.

Two loopback paths remain that the rule cannot close, so check them by hand:

- **nss-resolve.** With `resolve` in the `hosts:` line of `/etc/nsswitch.conf` (the Arch default), glibc asks systemd-resolved over its varlink socket, not over UDP, and `getent hosts <name>` from `muthur-agent` still resolves. A name built from data leaks that data. Remove `resolve` from `hosts:` (keep `files myhostname dns`) or accept the gap.
- **Local proxies.** Any HTTP or SOCKS proxy listening on loopback is reachable from the account. Do not run one on this host, or bind it to a non-loopback address only.

At startup MUTHUR probes the account and the isolation, each step limited to 5 s, and the result is fixed for the session. Isolated means a UDP datagram to 192.0.2.1 fails with `Operation not permitted`; success, a timeout or `Network is unreachable` all count as not isolated. If either fails, `/DIRECT` shows `AGENT ACCOUNT OFFLINE -- SEE DEPLOY` or `AGENT NETWORK NOT ISOLATED -- SEE DEPLOY` and the `run` tool is withheld.

## Manual install

On a host without systemd, install the binary and `deploy/login/muthur-login` yourself, run a getty on the serial port at the right speed (38400, `TERM=vt100`), and start `muthur-login` from the login profile of the account (`~/.profile`, `~/.zprofile` or `conf.d`), for example the line in `deploy/login/hook.sh`. `muthur-login` checks the tty against `/dev/ttyMUTHUR`; set `MUTHUR_TTY` to your device path, and `MUTHUR_BIN` if the binary lives elsewhere. The config is `install -Dm600 docs/config.example.toml ~/.config/muthur/config.toml`.

## Checks before a release

`make check-deploy` lints the deploy files and tests the templates; each lint is skipped when its tool is missing. `make check` runs `cargo clippy -D warnings` and `cargo test` with default features and again with `--no-default-features`, then the installer tests (`sh tests/install/run.sh`).
