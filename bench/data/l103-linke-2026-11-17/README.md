# l103-linke-2026-11-17

A WCH-LinkE's RVSWD to a CH32L103C8T6, taken by an OEP probe at the LinkE's pins. The bench's `linke:` source
(`bench/wsbench/data.py`) reads it. The raw streams, before the cut, are in a repository of their own
(wireskein-captures, `l103-linke-2026-11-17/`; its commit in `SOURCE.json`).

## How it was taken

| | |
|---|---|
| target | CH32L103C8T6 (UID `3a6dabcda282bc48`, chip ID `0x10310710`) |
| debugger | WCH-LinkE `0E028F0692F1` (firmware 2.23), ch32rv 0.31.2 |
| capture | OEP ESP32-P4X `e8f60ae84dd7` (firmware 0.0.56+dea9e3c), GPIO12 = SWCLK, GPIO15 = SWDIO, tapped at the LinkE, stream mode, no glitch filter |
| rate | 160 MS/s (`runs/rate/`: whole and fractional dividers of 160 MHz) |

One ch32rv operation a run (wireskein-captures' `l103-linke-2026-11-17/tools/capture.py`): the stream is started,
ch32rv runs 3 s later, the capture's clock record must hold the whole operation, and a run that missed it or lost
data is taken again.

## Contents

- `runs/{high,medium,low}/<op>`: the same 14 operations at the LinkE's three speeds (`--speed`): `target_info`,
  `read_ram_256`, `read_flash_4k`, `flash_pattern4k`, `verify_pattern4k`, `read_pattern4k`, `dbg_halt`, `dbg_regs`,
  `dbg_step`, `dbg_resume`, `reset`, `erase_all`, `read_erased_1k`, `flash_sketch`.
- `runs/high/x_*`: 61 more (repeats, 64 KiB and 20 KiB reads, RAM writes read back, register reads and writes,
  steps in a row, direct DMI accesses, kinds of reset, release-dm, connect-under-reset, operations that fail,
  option bytes there and back, an image that changes the clock).
- `runs/rate/<MHz>/<op>`: 4 operations (`target_info`, `read_flash_4k`, `flash_pattern4k`, `low_read_ram_256`) taken
  by the probe itself at 160, 133.3, 50, 10, 8 and 5 MHz.

Each run is wireskein-run/1 (`run.json`, `c0001.wireskein`) with:

| File | What it holds |
|---|---|
| `c0001.wireskein` | the stretch from 20 ms before ch32rv ran to 100 ms after (wireskein-captures' `tools/publish.py`, `meta.excerpt`) |
| `truth.json` | the truth, from outside WireSkein: `ch32rv.ndjson`'s DmiOps (each one DMI frame on the wire) as `dmi_ops`, `read.bin`'s contents as `read_bytes`, whether the 160 MS/s reading agrees as `agreement_160M` |
| `reading.json` | that version of WireSkein's reading (not truth): frames, noise, clock speeds, its DMI as eval counts it (`dmi`) |
| `ch32rv.ndjson` / `ch32rv.json` / `ch32rv.stderr` | ch32rv's USB traffic (`--capture`), its output (`--json`), its errors |
| `read.bin` | what ch32rv read (the read operations) |
| `take.json` | how it was taken: arguments, exit code, times, attempts, whether it is set aside, the stretch cut |

The tools that took it (`capture.py`), made the truth (`truth.py`), cut it (`publish.py`), tabulate the rates
(`rate_report.py`) and compare real and decimated captures (`rate_compare.py`), and the images written, are with the
raw records in wireskein-captures' `l103-linke-2026-11-17/tools/`.

The half set aside for checking (holdout) was chosen when it was taken, by a hash of the scenario's name
(`run.json` `meta.holdout`; `rate/` by the operation's name, so an operation is on the same side at every rate).

## Truth

- At 160 MS/s: `read_bytes` (reads of 256 bytes or more) and `dmi_ops`.
- Taken down (`decimate`, `resample`): also the same run's 160 MS/s reading (`reading.json` `dmi`), only where every
  DmiOp is found in it in order (`agreement_160M.usable`).
- The probe's own lower-rate captures (`rate/`): `read_bytes` and `dmi_ops` only (their own reading is never truth).
