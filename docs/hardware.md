# WY-60 hardware findings

Measured on the real terminal with `muthur testcard` (2026-11-18).
Settings: VT100 personality, enhanced mode, 80×43, status line off,
38400 8N1, autowrap on.

## Link

| Item | Result |
|---|---|
| XON/XOFF | **Works.** Requires the WY-60 *receive handshake* set to XON/XOFF in setup — with it off, the card overruns after ~300 bytes. Flood test clean at 38400. |
| RTS/CTS | **Does not work** with the current FTDI + null-modem cabling. XON/XOFF is the permanent choice. |

## Output (testcard probe)

All 15 probes render correctly: normal, bold, underline, blink, reverse and
their combinations, dim, DEC Special Graphics / line drawing, double-width,
double-height, scroll regions (DECSTBM), and full 43-row addressing.

Autowrap: writing past column 80 wraps to the next line. The wrap is
**deferred** (VT100 pending-wrap): writing the bottom-right cell (row 43,
column 80) does not scroll — testcard item 16, confirmed 2026-11-18. The
renderer may write every cell. Leave autowrap
**on** — the `vt100` terminfo entry assumes it, and passthrough mode depends
on the terminal matching terminfo.

## Keys

| Key | Bytes |
|---|---|
| F1 / F2 / F3 / F4 / F5 | `ESC[?5i` / `ESC[?3i` / `ESC[2i` / `ESC[@` / `ESC[M` |
| F6–F10 | `ESC[17~` `ESC[18~` `ESC[19~` `ESC[20~` `ESC[21~` |
| F11–F16 | `ESC[23~` `ESC[24~` `ESC[25~` `ESC[26~` `ESC[28~` `ESC[29~` |
| Shift-F1 … Shift-F5 | `ESC[5i` / `ESC[?1i` / `ESC[0i` / `ESC[L` / `ESC[K` |
| Shift-F6–F10 | `ESC[31~` … `ESC[35~` |
| Shift-F11–F16 | `ESC[1~` … `ESC[6~` (Shift-F15/F16 = standard PgUp/PgDn codes) |
| Up / Down / Right / Left | `ESC[A` / `ESC[B` / `ESC[C` / `ESC[D` |
| Home | `ESC[H` |
| Line Ins/Char | `ESC O P` (PF1) |
| Line Del/Char | `ESC O Q` (PF2) |
| Scrn/Clr Line | `ESC O R` (PF3) |
| Insert | `ESC O S` (PF4) |
| Backspace | `^H` |
| Return, keypad Enter | `^M` |
| Keypad digits `.` `-` `,` | plain ASCII (numeric keypad mode) |
| Break | `0x00` (NUL) in raw mode (`BRKINT`, `IGNBRK`, `PARMRK` off). MUTHUR treats it as **re-initialise screen**, 500 ms after the line goes quiet: output sent during the break is lost, and on power-up the terminal sends noise (including more NULs) and echoes received output back for a few hundred ms. Ctrl-@ / Ctrl-Space also send NUL, if the WY-60 sends them at all, so they act as Break too and never reach the shell. |
| Page (any modifier) | `^J` — indistinguishable from Ctrl-J; **unusable** |
| Del | Hold Screen: XOFF on first press, XON on second (swallowed by the tty under XON/XOFF). Unusable as a key. *(Pending confirmation with `flow = "none"`.)* |

## Consequences for the design

- **Tab keys:** F1/F2/F3 as planned.
- **Scrolling:** Shift-F15 / Shift-F16 act as PgUp / PgDn.
- **Delete-char:** Line Del/Char (PF2).
- **Input translation** (Shell tab): before the shell sees them, map PF2 → `ESC[3~`
  (Delete) and Shift-F15/F16 → `ESC[5~`/`ESC[6~` (they already are), so
  apps get standard codes.
- **Login shell:** `stty rows 43 cols 80 erase ^H` on the serial tty.
- **Screen saver:** the WY-60 blanks itself only when the line has been idle,
  so MUTHUR's clock and refreshes kept it awake. MUTHUR's standby screen
  (`[chrome] standby_minutes`) stops all output instead; the terminal's own
  screen saver must be **on** in its setup menu, or the standby logo stays lit.

## Open questions

- **Does `TIOCOUTQ` report queued bytes on the FTDI port?** The redraw
  scheduler waits for the kernel output queue to drain below 256 bytes before
  drawing. On a pty this is always 0 (writes block while XOFF'd instead of
  queueing), so it can only be confirmed on the real port: the run loop logs the
  queue depth at debug level in `~/.local/state/muthur/muthur.log` when run
  with `MUTHUR_DEBUG=1`.
