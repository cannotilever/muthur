# MUTHUR

A serial console front-end for a **Wyse WY-60** terminal, styled after the
computers of the *Alien* franchise: the personal terminals of *Alien: Isolation*
and the MU/TH/UR 6000 interface from the original film.

```
 USCSS OMICRON ── MU/TH/UR 6000 ────────────────────────────── 17 NOV 2137 14:02
 FOLDERS           ┌ UTILITY ──────────────────────────────────────────────────┐
 ┌──────────────┐  │                                                           │
 │ MU/TH/UR   * │  │                                                           │
 └──────────────┘  │                                                           │
 ┌──────────────┐  │                                                           │
 │ UTILITY      ├──┤                   content pane                            │
 └──────────────┘  │                                                           │
 ┌──────────────┐  │                                                           │
 │ CARGO        │  │                                                           │
 └──────────────┘  │                                                           │
 ┌──────────────┐  │                                                           │
 │ SHELL        │  │                                                           │
 └──────────────┘  └───────────────────────────────────────────────────────────┘
 F1-F4 SELECT .... ^] F FULLSCREEN .... SH-F15/16 SCROLL .... ^] ? HELP
```

The project does two jobs:

- **Functional.** The WY-60 prints escape sequences it does not understand
  instead of ignoring them. MUTHUR sits between modern software and the
  terminal and makes sure only sequences the terminal understands reach the
  wire, at 38400 baud with XON/XOFF flow control.
- **Aesthetic.** It should look and feel like a Weyland-Yutani console.

## Features

- **Boot splash.** Weyland-Yutani / MU/TH/UR 6000 boot text at teletype speed,
  with real host facts mixed in. Any key skips it.
- **MU/TH/UR tab.** A streaming LLM chat with a terse MU/TH/UR persona,
  teletype reveal and uppercase output. It talks to Anthropic or any
  OpenAI-compatible endpoint, such as vLLM. The persona has read-only tools for
  ship status and containers. `/DIRECT` switches to a technical agent that can
  run shell commands, and the persona can also `delegate` tasks to it.
- **Utility tab.** A system monitor: CPU, memory, disk, network sparklines,
  load, failed systemd units and top processes. `S` and `B` arm a
  **self-destruct** shutdown or reboot with a full-screen countdown and an
  abort questionnaire.
- **Cargo tab.** The container manifest for Docker or Podman, with
  start/restart/stop and a log viewer.
- **Shell tab.** fish on a PTY. Its output is translated through a virtual
  terminal (colour becomes bold, reverse or underline; Unicode becomes DEC
  Special Graphics), and it has scrollback, fullscreen, and an emergency
  untranslated passthrough mode.
- **Standby.** After a period with no keys, a static screen replaces the TUI
  and output stops, so the WY-60's own screen saver can blank the CRT.
- **Diffing renderer.** Only changed cells are sent, and scroll regions are
  used where possible. A `--strict` mode checks every byte against the VT100
  allowlist.

## Safety model for the agent

Commands from the technical agent pass four gates in order, and the first
refusal ends the call:

1. **Deny rules** in code plus configured regexes. These cannot be overridden.
2. **A reviewer LLM** returns allow, ask or deny. If the reviewer is
   unreachable, the call goes to *ask*, never *allow*.
3. **The operator** confirms with Y/N, types the sudo password for root
   calls, or types `OVERRIDE` to force a reviewer deny. Prompts ignore keys
   for their first second on screen, so type-ahead cannot approve anything.
4. **An audit log** records every call at `~/.local/state/muthur/audit.jsonl`.

Unprivileged commands run as a dedicated `muthur-agent` user with no sudo
rights and no network egress (nftables). Everything the agent reads is sent to
the LLM provider. For sensitive hosts, point the technical agent at a local
model. 

> [!NOTE]
> All LLM tool use can be disabled at build time by disabling the `agent` feature flag

Full details are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#tools--safety).

## Hardware

This project was built for and tested on the Wyse WY-60 but *should* work on anything that
can "speak" VT100.

Will probably work on any *NIX like.
Note that some features such as service management and power control rely on systemd.

**Tested Configuration:**

| Item | Value |
|---|---|
| Terminal | Wyse WY-60, VT100 personality, enhanced mode |
| Geometry | 80×43, WY-60 status line disabled |
| Cable | USB FTDI → DB9, DB9 → DB25 null modem |
| Link | 38400 baud, XON/XOFF |
| Host | Linux x86_64 (Arch or Alpine) |

Measured key sequences and capabilities are in [docs/hardware.md](docs/hardware.md).

## Building

Requires a recent stable Rust toolchain (edition 2024).

```sh
rustup target add x86_64-unknown-linux-musl
make build            # static musl release binary
make AGENT=0 build    # without the technical agent / LLM tools
make check            # clippy + tests, with and without the agent feature
```

## Running locally

With no `--device`, MUTHUR uses stdio, so it runs in any desktop terminal of
at least 80×43:

```sh
cargo run --                    # the full TUI
cargo run -- --strict           # also check every byte against the allowlist
cargo run -- splash             # play the boot splash once
cargo run -- destruct 30        # preview the countdown (runs nothing)
cargo run -- testcard           # attribute / line-drawing probe
cargo run -- testcard keys      # record the bytes each key sends
cargo run -- testcard flood     # flow-control stress test
```

Global options: `--config`, `--device`, `--baud`, `--cols`, `--rows`, `--strict`.

## Configuration

Copy the annotated example and lock it down. MUTHUR refuses to start if the
file is readable by group or others, because it holds the API key.

```sh
install -Dm600 docs/config.example.toml ~/.config/muthur/config.toml
```

Every key is optional. The default LLM is Anthropic `claude-sonnet-5-5`. Set
`[muthur] provider = "openai"` with a `base_url` and `model` for vLLM or
another OpenAI-compatible server. Logs go to `~/.local/state/muthur/`.

## Deploying

The production layout is: udev creates `/dev/wy60`, `serial-getty@wy60` logs
in, and a fish snippet runs `muthur`. If MUTHUR exits, you land in plain fish.

```sh
make install-system   # udev rule, getty drop-in, agent account (sudo on host)
make deploy           # build and upload the binary and fish snippet
make HOST=other deploy
```

[docs/deploy.md](docs/deploy.md) covers the first-time walkthrough, rollback,
recovering from a getty loop, and the agent account.

## Keys

| Key | Action |
|---|---|
| F1 / F2 / F3 / F4 | MU/TH/UR / Utility / Cargo / Shell |
| `Ctrl-]` `f` | Fullscreen toggle (per tab) |
| `Ctrl-]` `p` | Emergency passthrough |
| `Ctrl-]` `[` | Shell scrollback |
| `Ctrl-]` `r` | Force full redraw |
| `Ctrl-]` `?` | Help |
| Break | Re-initialise after the WY-60 was power-cycled |

## Testing

```sh
cargo test                       # unit + golden screen tests
cargo test --no-default-features
```

Golden tests in `tests/golden/` snapshot the screens. Live LLM round trips are
opt-in and need credentials:

```sh
MUTHUR_LIVE_ANTHROPIC_KEY=... cargo test --test llm_live anthropic -- --ignored --nocapture
```

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): design, modules and modes
- [docs/config.example.toml](docs/config.example.toml): every config key
- [docs/deploy.md](docs/deploy.md): installation on the host
- [docs/hardware.md](docs/hardware.md): WY-60 measurements
- [docs/testcard.md](docs/testcard.md): the hardware probe
- [docs/superpowers/specs/](docs/superpowers/specs): per-milestone design specs

## Credits

The visual design draws on
[brenns10/alien-console](https://github.com/brenns10/alien-console), which is
itself based on *Alien: Isolation*. *Alien*, MU/TH/UR and
Weyland-Yutani belong to their respective owners. This is a fan project.

## AI Disclosure

This project was almost entirely vibe-coded. While I have tried to ensure it was
built with best practices, the code has not been human-reviewed for security or saftey.
This is primarily a toy project; don't deploy it on anything important.
