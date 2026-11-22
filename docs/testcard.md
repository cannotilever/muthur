# Running the testcard on the WY-60

`muthur testcard` turns guesses about the terminal into facts. Run all three
probes and send back the results; they decide the VT100 dialect allowlist and
the default key map for M1.

## Build and copy

```sh
cargo build --release --target x86_64-unknown-linux-musl
scp target/x86_64-unknown-linux-musl/release/muthur server:~/bin/
```

The binary is fully static and runs on any Linux host of its architecture.

## Two ways to run it

1. **From the logged-in serial session (stdio).** Log in on the WY-60, then
   run `muthur testcard …`. Baud is whatever agetty set.
2. **Driving the port directly.** From another session, with no getty on the port:
   ```sh
   sudo systemctl stop serial-getty@ttyUSB0
   muthur --device /dev/ttyUSB0 --baud 38400 testcard …
   ```
   You need read/write access to the device (e.g. the `uucp` or `dialout` group).

Defaults: 80×43, 38400 baud, XON/XOFF. Override them with `--cols`, `--rows`
or `--baud`, or in `~/.config/muthur/config.toml` (must be `chmod 600`):

```toml
[link]
device = "/dev/ttyUSB0"
baud = 38400
flow = "xon-xoff"   # or "rts-cts", "none"
```

## 1. Attribute card: `muthur testcard`

Shows 16 numbered probes. **Report every item number that looks wrong.**
Here is what each item should look like:

| # | Probe | Expected |
|---|---|---|
| 1–8 | Normal, bold, underline, blink, reverse, and combinations | Each attribute (or combination) is visible as labelled |
| 9 | Dim (SGR 2) | May be ignored or rendered normally. Say which |
| 10 | DEC graphics `5F–7E` | Line-drawing glyphs, **not** the letters `` _`abc…~ `` |
| 11 | Line-drawn box | A clean closed rectangle around `MU/TH/UR` |
| 12 | Double-width line | The row is drawn in wide characters |
| 13 | Double-height pair | One tall line of text spanning two rows |
| 14 | Scroll region | Exactly `SCROLL 3`, `SCROLL 4`, `SCROLL 5` under the label |
| 15 | Last column | `X` in column 80. Note whether `Y` appears at the start of the next row |
| — | Bottom row | `BOTTOM ROW 43 - PRESS ANY KEY` on the very last line |
| 16 | Bottom-right cell | `#` in the very last cell, **and the title bar still on row 1**. If the whole card has shifted up a line, the terminal wraps immediately instead of deferring the wrap. |

Also note any stray characters, such as `[1m` or `#6` printed literally.
Those mean the terminal doesn't understand that sequence. Press any key to exit.

## 2. Flow-control flood: `muthur testcard flood`

Streams 2000 numbered lines (`--lines N` to change). Each line's alphabet
starts one letter later than the previous line's. **If any line's alphabet is
shifted, broken or missing letters, bytes were dropped, which means flow
control isn't working.** The run ends with `FLOOD COMPLETE 2000 LINES`. Use the
terminal's scroll-lock or Ctrl-S/Ctrl-Q to pause and inspect.

## 3. Key echo: `muthur testcard keys`

Shows, and saves to `muthur-keys.txt` (`--out PATH` to change), the bytes each
key sends. Press each of these **once, in this order**:

1. F1 … F16, then Shift+F1 … Shift+F16
2. Up, Down, Left, Right
3. Home, PgUp (Prev Page), PgDn (Next Page), Insert, Delete, Backspace
4. Return, keypad Enter, keypad 0–9, keypad `.` `,` `-`
5. Any other special keys you'd like to use (Send, Print, Clr Line, …)

Press **Ctrl-] Ctrl-]** to finish, then send back `muthur-keys.txt`.

## Later: trying hardware (RTS/CTS) flow control

This only works if the null-modem cable crosses RTS/CTS.

1. In the WY-60 setup, set the receive handshake to **DTR** (or DTR+XON/XOFF).
2. Set `flow = "rts-cts"` in the config.
3. Re-run the flood test, and also try pressing Ctrl-S. With RTS/CTS it reaches
   the application instead of pausing output.

If the flood shows dropped bytes, or output stalls forever, the cable doesn't
carry the handshake lines. Go back to `xon-xoff`.
