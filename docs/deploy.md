# Deploying MUTHUR to omicron

First-time walkthrough. The WY-60 hangs off an FTDI adapter on omicron; agetty logs in, and fish runs `muthur`.

## 1. Add the build target

```sh
rustup target add x86_64-unknown-linux-musl
```

## 2. Install system files, then the binary

```sh
make install-system   # udev rule + getty drop-in; prompts for sudo on omicron
make deploy           # builds, uploads the binary and the fish snippet
```

udev starts the getty when the adapter appears and stops it when the adapter goes (`SYSTEMD_WANTS` in the rule), so replugging the USB adapter brings the login back and boot never waits for an absent adapter. The getty is not enabled as a unit.

Override the host with `make HOST=other deploy`. `make deploy` keeps the previous binary as `~/.local/bin/muthur.old`. Run `make check-deploy` first to lint the udev rule and the getty drop-in locally.

To tune the boot logo, edit `assets/wy-logo.txt` and run `muthur splash` on the WY-60. A line starting with `^` is drawn double height (DECDHL, so also double width: at most 40 characters) and centred on the screen.

## 3. Check the result

```sh
ls -l /dev/wy60
systemctl status serial-getty@wy60
```

## 4. Roll back

On omicron:

```sh
cd ~/.local/bin && mv muthur.old muthur
```

## 5. Recover from a getty restart loop

From another session (ssh):

```sh
sudo systemctl stop serial-getty@wy60
sudo mv /etc/udev/rules.d/99-wy60.rules /root/99-wy60.rules.off
sudo udevadm control --reload
```

Stopping alone is not enough: udev starts the getty again on the next replug or boot, so move the rule aside until the cause is fixed. Put it back and run `make install-system` to restore.

## 6. The agent account

`make install-system` also sets up the technical agent's account (skip it with `make AGENT=0 install-system`, and build without the agent with `make AGENT=0 deploy`). It installs, from `deploy/`:

- `sysusers.d` and `tmpfiles.d`: the `muthur-agent` system user (groups `adm`, `systemd-journal`; no sudo rights, not in `docker` or `wheel`) and its home `/var/lib/muthur-agent`.
- `sudoers.d/muthur-agent`: lets the deploying user run commands as `muthur-agent` without a password. `OPERATOR` is replaced with that user, and `visudo -cf` checks the result before it goes to `/etc/sudoers.d/`.
- `nftables/muthur-agent.nft`: blocks all non-loopback egress from `muthur-agent`, and its DNS (ports 53 and 853) to loopback resolvers as well. TCP is rejected with a reset, so connects fail at once with "Connection refused"; everything else is dropped, which fails at once with "Operation not permitted". Installed as `/etc/nftables.d/muthur-agent.nft`.

The nft file is loaded at install time. To keep it across reboots, add this line to `/etc/nftables.conf` (and enable `nftables.service`):

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

## 7. Checks before a release

`make check-deploy` lints the deploy files (udev, getty drop-in, sudoers template, nft, sysusers); each check is skipped when its tool is missing. `make check` runs `cargo clippy -D warnings` and `cargo test` with default features and again with `--no-default-features`.
