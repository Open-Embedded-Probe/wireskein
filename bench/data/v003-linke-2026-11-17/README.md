# v003-linke-2026-11-17

A WCH-LinkE's SWIO to a CH32V003F4P6, taken by an OEP probe at the LinkE's pins at 160 MS/s. The bench's `linke:`
source (`bench/wsbench/data.py`) reads it. The raw streams and the tools that took, checked and cut them are in
wireskein-captures, `v003-linke-2026-11-17/` (its commit in `SOURCE.json`).

| | |
|---|---|
| target | CH32V003F4P6 (UID `ef17abcd5957be73`, chip ID `0x00300510`) |
| debugger | WCH-LinkE `FBC18F0680B0` (firmware 2.23), ch32rv 0.31.2 |
| capture | OEP ESP32-P4X `e8f60ae84dd7`, GPIO16 = SWIO (the LinkE's SWDIO) alone, tapped at the LinkE, stream mode, no glitch filter; the probe's firmware 0.0.56+645fbad-diag or 0.0.56+958a231 by run (`meta.probe.firmware`) |
| rate | 160 MS/s (`runs/rate/`: also 133.3, 50, 10, 5 MHz) |

The LinkE's two-line try on attach (some 100 ms with SWCLK moving too) is not in this copy (the raw `runs/attach2/`).

`runs/{high,medium,low}/<op>` and `runs/high/x_*` are the L103 set's operations sized for the V003 (16 KiB flash,
2 KiB RAM); `runs/rate/<MHz>/<op>` 4 operations taken by the probe at 160, 133.3, 50, 10 and 5 MHz. A run's files, the
half set aside for checking and the truth are as in `l103-linke-2026-11-17`.

Every one of ch32rv's DMI operations is found in the 160 MS/s reading in all 123 runs (`truth.json` `agreement_160M`, remade by bench/data/refresh_linke.py after WireSkein's SWIO fix: hole b of bench/notes/2026-11-17-test-plan; before it, 22 short operations' frames came out unread, the LinkE's two-wire try on attach outnumbering SWIO's own pulses). Their truth is ch32rv's DMI operations and reads, and, taken down, the 160 MS/s reading.
