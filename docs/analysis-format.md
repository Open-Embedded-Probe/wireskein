# wireskein-analysis/1: a capture decoded with known roles

`wireskein decode CAPTURE --spec SPEC [--out FILE]` (Python: `wireskein.decode.decode_file(path, spec)`) writes
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
| `buses[].idle_s` | Optional. Activity apart by more than this is two bursts (§3.3). Default: 16 bit times of the bus, or 1 ms when it has none |
| `lines` | Lines kept as their changes (DRDY, INT, ...) |
| `devices` | Device packs to try on the buses (as `analyze --devices`) |
| `depth` | How far up the results go: `transport`, `frames`, `protocol`, `device` (default) |

## 2. Document

```json
{"format": "wireskein-analysis/1",
 "wireskein": "0.0.16",
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
| `parts[]` | One entry a part. Analysis never crosses parts |

### 2.1 A part

| Key | Meaning |
| --- | --- |
| `part` | Its number, from 0 |
| `ticks` | Its length in ticks. Every tick below counts from this part's tick 0 |
| `tick0` | Where it starts after the first part's tick 0 (in a timeline) |
| `start_ns` | Its tick 0 on the probe's clock, when the capture gives it. A tick `s` is at `start_ns + s / tick_hz` seconds × 1e9 |
| `start_uncertainty_ns`, `boot_id` | As in the capture's part, when given (run-format §2.4) |
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

### 3.2 result

The layers of analyze's export (`--mode final`, at `depth`), with ticks from the part's tick 0:

| Protocol | Layers |
| --- | --- |
| `i2c` | `transactions`: `s`, `e`, `addr`, `rw`, `addr_ack`, `bytes` (hex), `acks`, `start_cond` (S / Sr), `end_cond` (P / Sr / null: open at the part's end), `scl_low_ticks`, `scl_waits` (`[{after_word, bit, ticks, s}]`); `complete: false` and `pending_bits` for one cut by the part's end |
| `spi` | `transfers`: `s`, `e` (the first and last sampled bit), and each data pin's bytes in hex under its channel name. `sd` beside it when SD commands were read (capture-test-guide §4.8) |
| `uart` | `lines`, `nmea`, `modbus_rtu` (when read) and `bytes` (`{"s": [...], "hex"}`) |
| others | as `analyze`'s export |

A higher reading keeps its lower one beside it (`sd` and `transfers`). With `depth: "device"`, `devices` lists the
devices matched on the bus (as `analyze --depth device`).

### 3.3 uninterpreted

`[{"s", "e", "edges"}]`: activity on the bus's pins that no decoded item covers. The changes on all the bus's pins
are cut into bursts where none comes for `idle_ticks`; a burst with no item starting in it (or within `idle_ticks`
before it) and inside none is uninterpreted: SCL clocked without a START, a transfer the decoder could not read,
noise.

With the parts and `capture.lost`, three cases are told apart:

- **nothing happened**: no change on the pins, inside a part
- **not seen**: between parts, or in a loss
- **not read**: an uninterpreted burst

## 4. Stability

The format number stays 1 until the first release, also through incompatible changes. A reader checks `format`
and ignores keys it does not know.
