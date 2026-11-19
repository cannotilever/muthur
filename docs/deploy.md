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
sudo systemctl disable --now serial-getty@wy60
```

## 6. Sudoers for tier 2 tools

Review `docs/sudoers.example`, then install it by hand (for example with `visudo -f /etc/sudoers.d/muthur`). It is never installed by the Makefile.
