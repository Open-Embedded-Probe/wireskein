# l103-linke-2026-11-17

A WCH-LinkE's RVSWD to a CH32L103C8T6, taken by an OEP probe at the LinkE's pins. The bench's `linke:` source
(`bench/wsbench/data.py`) reads it. The raw streams, before the cut, are in a repository of their own
(wireskein-bench-raw, `l103-linke-2026-11-17/`; its commit in `SOURCE.json`).

## How it was taken

| | |
|---|---|
| target | CH32L103C8T6 (UID `3a6dabcda282bc48`, chip ID `0x10310710`) |
| debugger | WCH-LinkE `0E028F0692F1` (firmware 2.23), ch32rv 0.31.2 |
| capture | OEP ESP32-P4X `e8f60ae84dd7` (firmware 0.0.56+dea9e3c), GPIO12 = SWCLK, GPIO15 = SWDIO, tapped at the LinkE, stream mode, no glitch filter |
| rate | 160 MS/s (`runs/rate/`: whole and fractional dividers of 160 MHz) |

One ch32rv operation a run (`tools/capture.py`): the stream is started, ch32rv runs 3 s later, the capture's clock
record must hold the whole command, and a run that missed it or lost data is taken again.

## Contents

- `runs/{high,medium,low}/<op>`: the same 14 operations at the LinkE's three speeds (`--speed`).
- `runs/high/x_*`: 61 more (repeats, 64 KiB and 20 KiB reads, RAM writes read back, register reads and writes,
  steps in a row, direct DMI accesses, kinds of reset, release-dm, connect-under-reset, operations that fail,
  option bytes there and back, a clock-changing image).
- `runs/rate/<MHz>/<op>`: 4 operations taken by the probe itself at 160, 133.3, 50, 10, 8 and 5 MHz.

Each run is wireskein-run/1 (`run.json`, `c0001.wireskein`, cut from 20 ms before ch32rv started to 100 ms after it
ended: `meta.excerpt`) with `truth.json` (truth from outside WireSkein: ch32rv's DmiOps over USB, one DMI frame on
the wire each, as `dmi_ops`; what it read as `read_bytes`; whether the 160 MS/s reading agrees, `agreement_160M`),
`reading.json` (WireSkein's reading, not truth: frames, noise, clock speeds, its DMI as eval counts it), ch32rv's
`ch32rv.ndjson` / `.json` / `.stderr`, `read.bin` and `take.json` (how it was taken). The half set aside for checking
was chosen by the scenario's name when it was taken (`run.json` `meta.holdout`; `rate/` by the operation's name).

## Truth

- At 160 MS/s: `read_bytes` (reads of 256 bytes or more) and `dmi_ops`.
- Taken down (`decimate`, `resample`): also the same run's 160 MS/s reading (`reading.json` `dmi`), only where every
  DmiOp is found in it in order (`agreement_160M.usable`).
- The probe's own lower-rate captures (`rate/`): `read_bytes` and `dmi_ops` only.
