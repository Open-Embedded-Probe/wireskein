# wireskein-analysis/1: a capture decoded with known roles

`wireskein decode CAPTURE --spec SPEC [--out FILE]` (CAPTURE: a WireSkein file, a `.sr`, a VCD or a format added) (Python: `wireskein.decode.decode_file(path, spec)`) writes
this document. It is made to be kept: beside the capture and its run in a data repository, read later by tools
that know nothing of wireskein's internals.

`wireskein analyze` finds what a capture holds and claims only what the evidence carries; a short case (one
transfer) is too little to claim, and its claims are empty. When the wiring is known, the decode of exactly those
roles is what is wanted, however weak the evidence (verify does the same). `decode` gives that, and says how well
the lines fit, whether analyze would have claimed it, and what was not read or not seen.

## 1. Spec

```json
{"buses": [{"protocol": "i2c", "pins": {"scl": "SCL", "sda": "SDA"}},
           {"protocol": "spi", "pins": {"clk": "SCK", "mosi": "MOSI", "miso": "MISO", "cs": "CS"},
            "params": {"mode": 0}}],
 "lines": ["DRDY"],
 "devices": ["i2c/**"],
 "depth": "device"}
```

| Key | Meaning |
| --- | --- |
| `buses[].protocol` | An analyzer's name (`i2c`, `spi`, `uart`, ...) |
| `buses[].pins` | Role → channel name. Every pin must be in the capture (else an error). The decode used is the one on exactly these pins |
| `buses[].params` | Optional. Passed as hints (`baud` for `uart`, ...). For `spi`, `mode` (0–3) picks the sampling edge; without it the mode is read from the lines (capture-test-guide §4.5), `cs_active` for an active-high CS |
| `split` | Optional. How a long part is read in pieces ([long captures](long-captures.md) §3): a dict, `"key=value,..."` or `"off"` |
| `buses[].layers` | Optional. Lower layers to give in `result` beside the final one, by name (`bits`: every sampling edge `t` and its value; `frames`; SWIO's `pulses`: each pulse's start, width and short/long; `*`: all) |
| `buses[].idle_s` | Optional. Activity apart by more than this is two bursts (§3.3). Default: 16 bit times of the bus, or 1 ms when it has none |
| `lines` | Lines kept as their changes (DRDY, INT, ...) |
| `devices` | Device packs to try on the buses (as `analyze --devices`) |
| `depth` | How far up the results go: `transport`, `frames`, `protocol`, `device` (default) |

## 2. Document

```json
{"format": "wireskein-analysis/1",
 "wireskein": "...",
 "spec": {...},
 "capture": {"file": "c0001.wireskein", "sha256": "...", "id": "...", "tick_hz": [1000000, 1],
             "complete": true, "layout": "timeline", "parts": 1},
 "parts": [{"part": 0, "ticks": 8192, "tick0": 0, "start_ns": 6061908000,
            "buses": [...], "lines": [...], "undeclared": [...], "claims": [...]}]}
```

| Key | Meaning |
| --- | --- |
| `format` | `"wireskein-analysis/1"` |
| `wireskein` | The version that wrote it. A later version may decode the same capture better: keep the capture to remake it |
| `spec` | The spec as used (defaults filled in) |
| `capture.file`, `sha256`, `id` | Which capture (its file name, its bytes, its capture id) |
| `capture.tick_hz` | Ticks a second, as numerator and denominator |
| `capture.complete`, `lost` | As in the capture (wireskein-format §3.4). A capture that lost data is a failure: the results of the parts taken are kept for reference |
| `capture.layout`, `parts` | How the parts are laid out, and how many |
| `parts[]` | One entry a part, or a run of parts that follow on without a gap read as one (wireskein-format §3.4). Analysis never crosses a gap. A part of more changes than the split's `max_edges` is read in pieces and has `split` ([long captures](long-captures.md) §3.2) |

### 2.1 A part

| Key | Meaning |
| --- | --- |
| `part` | Its number, from 0 (of a run: its first part's) |
| `parts` | For a run of parts joined: its first and last part's numbers. Its ticks then run on from the first part's tick 0 across the others |
| `ticks` | Its length in ticks. Every tick below counts from this part's tick 0 |
| `tick0` | Where it starts after the first part's tick 0 (in a timeline) |
| `start_ns` | Its tick 0 on the probe's clock, when the capture gives it. A tick `s` is at `start_ns + s / tick_hz` seconds × 1e9 |
| `start_uncertainty_ns`, `boot_id` | As in the capture's part, when given (wireskein-format §3.3) |
| `buses[]` | One entry a spec bus, in the spec's order (§3) |
| `lines[]` | `{"pin", "initial", "edges"}`: the level before the part, and each change's tick |
| `undeclared[]` | Pins the spec does not name that changed: `{"pin", "edges", "s", "e", "bursts"}` (bursts apart by over 1 ms) |
| `claims` | analyze's claims on pins the spec does not name, for reference (as `analyze`'s export). Left out when none |

## 3. A bus

```json
{"protocol": "i2c", "pins": {"scl": "SCL", "sda": "SDA"},
 "decoded": true,
 "params": {"sample_edge": "rise"},
 "fit": {"score": 0.0647, "claimed": false},
 "result": {"transactions": [{"s": 211, "e": 301, "addr": 80, "rw": "write", "addr_ack": false, "bytes": "",
                              "acks": [], "start_cond": "S", "end_cond": "P", "scl_low_ticks": 6.0,
                              "scl_waits": []}]},
 "uninterpreted": [],
 "idle_ticks": 160}
```

### 3.1 decoded, params, fit

- `decoded`: `false` when nothing on these pins decodes as this protocol; `result` is then empty and every burst on
  the pins is uninterpreted. Said, not left out.
- `params`: what the decoder chose (the sampling edge, a UART's baud, ...).
- `fit.score`: how well the lines fit, 0–1 (analyze's score). Low on a short case: it measures evidence, not
  correctness.
- `fit.claimed`: whether analyze, finding things alone, would have claimed this very decode; `fit.verdict` then
  gives its verdict (`confirmed` / `likely` / `ambiguous`).

- `reading`: how well the bus read, in counts one acts on ([capturing](capturing.md)): `frames` (read frame by
  frame), `ok` (their checks held), `unanswered` (a frame no target answered), `restored`, `repaired` (frames read with
  a lost clock restored, with glitch clocks dropped), `unread` and `unread_edges` (stretches no frame read, their
  changes), `rate_ok` (every clock speed sampled well enough); for a clocked bus also `glitches` (the clock's pulses of
  a sample where the clock is slow enough that no half of it is that short) and `ringing` (data line → its pulses
  under 3 samples at a clock slow enough that no bit is that short), with the changes they are counted in
  (`clock_edges`, `data_edges`), and as shares of them (a pulse is two changes): `glitch_share`, `ringing_share`
  (data line → share), `noisy` when one is over 1 %: noise, a reflection, a neighbouring line picked up or a ground
  far away. A count grows with the capture; the share is what to judge by. analyze's claims give it too, without `unread`. In pieces, the counts are each piece's in its
  own ticks, summed.
- `clock_speeds` (a clocked bus): the speeds its clock ran at, from the frames it read (each frame's `period_ns`
  and `high_ns`, medians over its clocks, so a reflection's glitch does not count, are on the frame): frames whose
  periods are within 1.25 times a speed's fastest are that speed, wherever they are. Fastest first, each
  `period_ns`, `freq_hz`, `high_ns`, `low_ns` (medians), `frames`, `s` and `e` (its first frame's start, its last's
  end), and `rate_ok`: false when its high or low lasts under 3 samples, so a high may fall between two samples and
  its clock be lost (a debugger's speed changes on the way: a WCH-LinkE connects slowly, then reads fast).
- `undersampled`: when a speed is too fast for the rate (`rate_ok` false): `clock`, `rate_hz`, `frames` (read at
  such speeds), `period_ns`, `high_ns`, `low_ns` (the worst of them), `least_rate_hz` (3 samples in the shorter of
  its high and low: none lost) and `best_rate_hz` (5: about 10 a period of an even clock), from the high and low as
  measured at this rate, `restored` (RVSWD frames read with a lost clock restored). Frames a clock short are read only
  where that leaves one reading (every place the clock may have been lost, about as likely, gives the same bits, and
  they pass the checks); the rest are unread. `decode` and `analyze` say it on stderr too.

### 3.2 result

The layers of analyze's export (`--mode final`, at `depth`), with ticks from the part's tick 0:

| Protocol | Layers |
| --- | --- |
| `i2c` | `transactions`: `s`, `e`, `addr`, `rw`, `addr_ack`, `bytes` (hex), `acks`, `start_cond` (S / Sr), `end_cond` (P / Sr / null: open at the part's end), `scl_low_ticks`, `scl_waits` (`[{after_word, bit, ticks, s}]`); `complete: false` and `pending_bits` for one cut by the part's end |
| A protocol of your own ([protocols](protocols.md)) | Under its bus, by its name: `{"messages", "checks_passed", "checks_failed"}`, shown when its checks held, and then the bus's final reading |
| `spi` | `transfers`: `s`, `e` (the first and last sampled bit), and each data pin's bytes in hex under its channel name. `sd` beside it when SD commands were read (capture-test-guide §4.8). With a CS, `modes`: the two modes of SCK's idle level, each with `readable` (no data line changes at its sampling edges) and the `bytes` it reads (MSB first); `modes_ok` the readable ones. One readable mode tells the mode; readable modes with the same bytes need not be told apart |
| `uart` | `lines`, `nmea`, `modbus_rtu` (when read) and `bytes` (`{"s": [...], "hex"}`) |
| `can` | `frames`: `s`, `id`, `ide` (1: an extended identifier), `dlc`, `data` (hex, the bytes the DLC carries), `ack`, `ok` |
| `lin` | `frames`: `s` (the break), `id`, `data` (hex), `ok` (sync, identifier parity and checksum held) |
| `dmx512` | `packets`: `s` (the break), `start_code`, `data` (hex, the slots) |
| `rvswd`, `swio` | WCH's debug transports (two wires, one wire); the layers above them are RISC-V's, the same for both. `dmi`: the RISC-V DMI accesses they carry, `s`, `e` (RVSWD: the frame's last sampling edge), `op` (`R` / `W`), `addr`, `reg` (the register's name in the RISC-V Debug Specification 0.13; `wch_7c`..`wch_7f` for WCH's registers outside it; null for an address of neither), `data`, `ok` (the frame's checks held); for RVSWD also `form` (`short`: WCH's 52-bit frame; `long`: the 85-clock frame a CH32V103 takes) and `status` (the DMI result in the long form: 0 done, 2 failed, 3 busy), and `answered: false` for a long frame no target answered (its `data` and `status` null: what was asked, not done). For RVSWD also `bits` (the frame's bits as sampled, the don't-care ones too, to compare frames bit for bit), `period_ns` (the median of its clock's sampling intervals), `fields` (the frame's fields by name as bit strings: short `addr rw header_parity park ctl data data_parity park2 status pad stop` with `header_parity_ok` / `data_parity_ok`; a burst's `header`, `aux` (each word's six) and `trailer`; long `host_addr host_data op bit41 target_addr target_data status bit83 last`) `repaired` (the ticks of glitch clocks dropped, when a frame of a length no form has read with them left out) and `restored` (the estimated tick of a clock lost between two samples, when a frame a clock short of a form was read with it restored: `undersampled`, §3.1). For SWIO also `fast` (a 33-pulse continuation read of data0, taken only after a read of data0: a lone 33-pulse burst, as a LinkE sends before connecting, is uninterpreted), `space_ns` (the median idle time between pulses, the same whatever the bit), `pitch_1_ns` / `pitch_0_ns` (the median from a pulse's start to the next one's after a 1 and after a 0: a pulse's low plus the space, so a 0's is longer), `short_median_ns` / `long_median_ns`, and the frame's pulses against the split between short (1) and long (0) lows (`params.split_ns`, from the line's two width clusters): `short_max_ns` (its longest short low), `long_min_ns` (its shortest long low) and `rw_low_ns` (the R/W bit's low: a long low that shrinks past the split turns a read into a write), `rw_gap_ns` (from the R/W bit's end to the first data pulse: longer for a read, where the target answers). For a long frame answered, `answer_delay` (`median`, `max`): when the target moved SWDIO to the bit it echoes, in clock periods after the rising edge before that bit (0 to 1: in time; over 1: after the edge it is sampled at), timed where the echo (the host's address, and after a write its data) changes, `answer_delay_ns` the same in ns, and `late: 1` when the median is over 1 (the answer slid a clock: the bits read are not the target's, a CH32V103 does it at times while it is reset). `dm`: what the debugger did to the hart, read from the RISC-V Debug Module (abstract commands, and programs run in the program buffer; where the data registers sit in memory from hartinfo, or learnt from the programs: the address that reads the most memory): `op` (`MEMR` / `MEMW` / `REGR` / `REGW`; `BURST` for a burst of a register that is no data register; a burst of data0 is its reads, a `MEMR` each), `addr` (memory, or the register number of Access Register), `value` (null: not seen on the wire), `size` (bytes), `s` (the tick of the DMI access that made it: the abstract command, the program run) and `e` (that of the read that showed its value) |
| others | as `analyze`'s export |

A higher reading keeps its lower one beside it (`sd` and `transfers`). With `depth: "device"`, `devices` lists the
devices matched on the bus (as `analyze --depth device`).

### 3.3 uninterpreted

`[{"s", "e", "edges"}]`: activity on the bus's pins that no decoded item covers. The changes on all the bus's pins
are cut into bursts where none comes for `idle_ticks`; a burst with no item starting in it (or within `idle_ticks`
before it) and inside none is uninterpreted: SCL clocked without a START, a transfer the decoder could not read,
noise. Where the items have an end (`e`), as a clocked bus's frames do, a burst holding some is looked into too:
its changes outside every item (a clock or two around it) are uninterpreted, cut where none comes for `idle_ticks`
(a frame no layout read, between two that were). A clocked bus's `bits` and `frames` are where it was cut, not what
was read, and cover nothing here.

With the parts and `capture.lost`, three cases are told apart:

- **nothing happened**: no change on the pins, inside a part
- **not seen**: between parts, or in a loss
- **not read**: an uninterpreted burst

## 4. What analyze finds

`wireskein analyze CAPTURE` (Python: `wireskein.decode.analyze_file(path, hints, ...)`) writes the same family of
document, with no roles given: `format`, `wireskein`, `capture` (§2) and `parts[]`, but `hints` (what was asked to be
tried, `--hint` / `--devices`) in place of `spec`, and in each part, in place of `buses`:

| Key | Meaning |
| --- | --- |
| `part`, `ticks`, `tick0`, `start_ns`, ... | As in §2.1 |
| `claims[]` | What the evidence carries: `protocol`, `roles` (role → channel), `verdict` (`confirmed` / `likely` / `ambiguous` / `unknown-sync`), `score`, `margin`, `params`, and `result` (`--mode final`, at `--depth`), `layers` and `evidence` (`--mode all`) or the layers `--select` names, `devices` (`--depth device`), `runner_up` (`--alternatives`). Ticks from the part's tick 0 |
| `relations[]` | Pairs of claims read together (a command line and its answers): `scpi` with its `exchanges`, and protocols of others on a UART pair (`uart_pair`, [protocols](protocols.md) §3.1) with their `messages`, `checks_passed`, `checks_failed` |
| `analysis_seconds` | At the top: how long the analysis took (differs every run) |
| `window`, `segment`, `events` | With `--window` / `--segment` (a range inside one part: `--part` names it when there are several) |

Examples to run it on are in tests/data/examples (a SWIO and an RVSWD session from a WCH-LinkE, and generated I2C and SPI buses side by side), each with the buses it holds in `<name>.truth.json`: `wireskein analyze tests/data/examples/mixed-0.wireskein`. A WireSkein file is analysed part by part; a `.sr` or a VCD is one part. What is claimed, and
how sure, may change between versions (the analysis is tuned on an evaluation corpus); the keys are kept.

## 5. Stability

The format number stays 1 until version 1, also through incompatible changes. A reader checks `format`
and ignores keys it does not know.
