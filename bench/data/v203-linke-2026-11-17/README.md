# v203-linke-2026-11-17

A WCH-LinkE's RVSWD to a CH32V203C8T6, taken by an OEP probe at the LinkE's pins at 160 MS/s. The bench's `linke:`
source (`bench/wsbench/data.py`) reads it. The raw streams and the tools that took, checked and cut them are in
wireskein-captures, `v203-linke-2026-11-17/` (its commit in `SOURCE.json`).

| | |
|---|---|
| target | CH32V203C8T6 (UID `dbb1abcd43e8bc6a`, chip ID `0x20310500`) |
| debugger | WCH-LinkE `FBC18F0680B0` (firmware 2.23), ch32rv 0.31.2 |
| capture | OEP ESP32-P4X `e8f60ae84dd7` (firmware 0.0.56+b9b46df), GPIO21 = SWCLK, GPIO16 = SWDIO, tapped at the LinkE, stream mode, no glitch filter |
| rate | 160 MS/s |

At the LinkE's speed high, SWCLK is high for some 19 ns of a 100 ns period: 3 samples at 160 MS/s, which is why this
set was taken (the September fixture's 50 MS/s lost about half the DMI accesses).

`runs/{high,low}/<op>` are the L103 set's 14 operations (`l103-linke-2026-11-17`) at the LinkE's speeds high and low;
`runs/high/x_*` a 64 KiB read, a 20 KiB RAM read, steps and repeated `target_info`. The wiring was loose: medium,
other operations and other capture rates were not taken (a lower rate is made from the 160 MS/s runs). A run's files,
the half set aside for checking and the truth are as in `l103-linke-2026-11-17`.

Every one of ch32rv's DMI operations is found in the 160 MS/s reading in all 37 runs (`truth.json`
`agreement_160M`), and the reads match `read.bin`.
