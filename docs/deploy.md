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

To tune the boot logo, edit `assets/wy-logo.txt` and run `muthur splash` on the WY-60.

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

## 6. Sudoers for tier 2 tools

Review `docs/sudoers.example`, then install it by hand (for example with `visudo -f /etc/sudoers.d/muthur`). It is never installed by the Makefile.
