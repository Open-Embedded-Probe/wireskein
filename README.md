# WireSkein

![WireSkein: open-source logic analyzer library](https://raw.githubusercontent.com/Open-Embedded-Probe/wireskein/main/docs/images/wireskein-top.jpg)

[日本語 README](https://github.com/Open-Embedded-Probe/wireskein/blob/main/README.ja.md)

WireSkein reads logic-analyzer captures. It has two uses:

- **Checking recorded hardware test runs.** A test records what it sent, the captures, and what each step should look like on the wire (a 1 kHz square wave, an I2C write to 0x42, a UART at F_CPU / BRR). `wireskein verify` checks every capture against these expectations and reports OK / NG with measured values, as text, JSON and JUnit XML.
- **Decoding unknown captures.** `wireskein analyze` finds which pins carry I2C, SPI, UART, RVSWD / SWIO, SWD or CAN, and decodes them. Upper layers (NMEA, Modbus, known I2C / SPI devices) are tried on top.

Status: **beta**. Breaking changes may still happen; see [Stability](#stability).

## Install

```sh
pip install wireskein              # or: uv add wireskein
```

Python 3.13 or newer. The only dependency is numpy.

## Checking a test run

A test records a run with `wireskein.runlog`. This module uses only the standard library.

```python
from wireskein.runlog import Recorder, square, level, only_moving

rec = Recorder("out/run1", target="x035")
with rec.section(1, "test_pwm"):
    for duty in (64, 128, 0):
        want = [square("PA1", 1000, duty / 255), only_moving(["PA1"])] if duty else [level("PA1", 0)]
        with rec.section(2, f"duty={duty}", expect=want):
            rec.command(f"PWM {duty}")          # what the host sent
            rec.reply(reply_line)               # what the device answered
            t = rec.armed()                     # time.monotonic() right after arming the capture
            data = read_capture()               # the probe's samples: one byte per sample, bit k = pin k
            rec.capture(t, rate, interleaved=data, names=["PA1", "PA0"], start_us=segment_start_us)
rec.close()
```

Then check it:

```sh
wireskein verify out/run1 --junit out/run1/report.xml --json out/run1/report.json
```

```text
OK  test_pwm/duty=64  square  c0001.wsc
NG  test_pwm/duty=128  square  c0002.wsc  duty 0.6999 vs 0.5020
OK  test_pwm/duty=0  level  c0003.wsc
2 ok, 1 ng, 0 unchecked (4 segments, 3 captures)
```

The captures are stored as `.wsc` files (below). The exit code is 1 when a check fails. A check whose pins are not in the capture is **unchecked** (`--`). Unchecked results do not fail the run.

### Headings and segments

Headings split the run into a tree of segments. `#` is a test, `##` is a step, and a heading without a name (`##`) closes that level. `Recorder.section()` opens a heading and closes it when the `with` block ends. A capture belongs to the segment in which it was armed. Expectations are stored under the segment path, for example `test_pwm/duty=64`. When a name repeats under the same parent, each repetition gets an index (`duty=64[0]`, `duty=64[1]`) and keeps its own expectations. A broken structure, such as a skipped level, is always reported as NG.

### Checks

| Helper | Checks |
| --- | --- |
| `square(pin, freq_hz, duty, tol_freq, tol_duty, max_jitter)` | A steady square wave: frequency (relative tolerance), duty (absolute), period spread |
| `level(pin, value)` | The pin does not move |
| `starts({pin: v})` / `ends({pin: v})` | The level at the first / last sample |
| `only_moving([pins])` | No other captured pin moves |
| `pulses(pin, count, period_s, tol)` | Number of rising edges and their period |
| `i2c(scl, sda, transactions, hz, tol_hz, released)` | Transactions (address, direction, bytes, ACK, `complete`), SCL rate, and a released bus at the end |
| `spi(clk, mosi, miso, cs, mode, mosi_bytes, miso_bytes, hz)` | Mode, bytes on both lines, SCK rate, and CS high at the end |
| `uart(pin, baud, data, tol_baud, idle, bits, parity, stop, max_errors)` | Bit rate measured from the edges, data, idle level, and framing / parity errors. `baud=None` only measures |

Pins and roles are given, so these checks are verification, not discovery. `docs/capture-test-guide.ja.md` explains how to choose capture windows and tolerances, with examples from real runs.

If a capture has the meta `time_base_slipped: true` (the probe knows some samples were taken late), the verdict does not change, but a failure's reason says so.

### Python API

```python
from wireskein.verify import verify, junit, dumps, lines, summary_line

report = verify("out/run1")          # dict: results (with measured values), summary, log
xml = junit(report)                  # JUnit XML
text = dumps(report)                 # JSON
print(summary_line(report), *lines(report), sep="\n")   # NG and unchecked lines, as the CLI prints them
```

For pytest, [pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein) gives each test a `ws_run` recorder and runs `verify` after the test.

## Capture files (.wsc) and conversion

A `.wsc` keeps each channel at its own sample rate. A probe that decimates some channels to fit its link (every 32nd sample, say) stores only the samples it took, with `step=32`. Nothing is repeated to fill the gaps, so a viewer can show exactly the samples that exist. The module `wireskein.wsc` reads and writes it with the standard library only:

```python
from wireskein import wsc

wsc.write("c.wsc", 100_000_000, [
    wsc.Channel("PA5", wsc.pack(pa5_samples), n),                 # samples: one byte per sample, 0 or 1
    wsc.Channel("PB0", wsc.pack(pb0_samples), n // 32, step=32),  # a channel kept at 1/32 of the rate
], start_us=segment_start_us)
channels = wsc.from_interleaved(data, ["PA5", "PA7"], width=8)    # a probe's stream: width bits per sample, bit k = channel k
```

In a recorded run, pass the same thing to `rec.capture(t, tick_hz, channels=[...])`.

A `.wsc` can also carry anything else about the capture: acquisition settings, a wiring note, analysis results. Attachments are named files (text, JSON or bytes) and can be replaced. Notes form an append-only log, one entry per call, with its time. Both can be added to an existing file without rewriting the channels:

```python
wsc.attach("c.wsc", "probe.json", {"fw": "1.2", "plan": plan})   # dict / list -> JSON, str -> text, bytes as is
wsc.note("c.wsc", "PA5 looked noisy; shorter wire next time")
wsc.note("c.wsc", {"i2c": transactions}, kind="analysis")
wsc.attachments("c.wsc"), wsc.notes("c.wsc")
```

```sh
wireskein info c.wsc                                  # channels and rates, metadata, attachments, notes
wireskein note c.wsc "re-captured after reflow"
wireskein attach c.wsc setup.txt --text "10k pull-ups on SDA/SCL"
wireskein attach c.wsc scope.png scope.png            # any file
```

`rec.capture(..., attachments={...})` stores attachments with a capture of a recorded run. Attachments and notes go along when a capture is converted to `.sr` and back.

Convert between formats on the command line (the format follows the extension):

```sh
wireskein convert c0001.wsc c0001.sr     # for PulseView: one rate, slow channels repeated
wireskein convert c0001.sr c0001.wsc     # back: channels get their real rate again
wireskein convert corpus/fixtures/real/<id> capture.sr
```

A `.sr` has one sample rate for all channels, so slow channels are repeated to the fastest rate there. The real rate of each channel is kept in `wireskein.json` inside the `.sr`. sigrok ignores this file, and WireSkein reads it back. In PulseView, the repeated samples look like real ones.

## Decoding a capture

```sh
wireskein analyze capture.wsc                             # .wsc, sigrok .sr, or a fixture directory
wireskein analyze capture.sr --hint '{"protocols": ["i2c"]}'
wireskein analyze capture.sr --mode all --out result.json
wireskein segments capture.sr --results                   # a capture with marker lines on a UART
```

`--hint` restricts what is tried. It can name protocols, or give pins with their roles and baud rates. The result is still scored by the decoders' own checks.

```python
from wireskein.analyze import load, save, analyze, export

cap = load("capture.wsc")                                     # or .sr / a fixture directory
res = analyze(cap, {"protocols": ["spi"]})
doc = export(res, cap)
```

## Stability

| Part | Promise during the beta |
| --- | --- |
| `wireskein.runlog` (names, arguments and meaning of `Recorder` and the check helpers) | Stable. New arguments get defaults that keep the old meaning |
| Run format (`run.json` + `.wsc` captures, `FORMAT = "wireskein-run/1"`) and the capture format (`.wsc`, `wireskein-capture/0`) | Stable. An incompatible change raises `FORMAT`, and `verify` refuses older runs with a clear error |
| `wireskein.verify.verify` / `junit`, `wireskein verify` | Stable. Report fields may be added |
| `wireskein.analyze`, `wireskein analyze` / `segments` output | May change |
| `wireskein._engine` | Internal |

## Repository layout

```text
src/wireskein/          the package (runlog, verify, analyze, cli, _engine, decl/ data)
tests/                  pytest
research/               evaluation scripts, benchmarks and findings (not packaged)
corpus/                 real and synthetic fixtures for research/
docs/                   design notes (Japanese)
```

To run the research scripts: `uv sync --group research`, then `uv run python research/evaluate.py --synth 200 --engine staged`. See `research/README.ja.md`.

## Development

```sh
uv run pytest
uv build
```

## Release

Releases use GitHub Actions, the same way as pytest-embedded-arduino-cli.

1. Update the `## Unreleased` section of `CHANGELOG.md`.
2. Run the `Release` workflow manually and enter the version, for example `0.0.2`.
3. The workflow does the rest. It updates the version in `pyproject.toml` and `src/wireskein/__init__.py`, moves the changelog entries under the new version, runs the tests, builds, commits, tags `v<version>`, creates a GitHub Release, and publishes to PyPI.

PyPI publishing uses Trusted Publishing.

## License

MIT
