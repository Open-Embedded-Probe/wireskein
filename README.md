# WireSkein

[日本語 README](https://github.com/Open-Embedded-Probe/wireskein/blob/main/README.ja.md)

WireSkein reads logic-analyzer captures. It has two uses:

- **Checking recorded hardware test runs.** A test records what it sent, the captures, and what each step should look like on the wire (a 1 kHz square wave, an I2C write to 0x42, a UART at F_CPU / BRR). `wireskein verify` checks every capture against these expectations and reports OK / NG with measured values, as text, JSON and JUnit XML.
- **Decoding unknown captures.** `wireskein analyze` finds which pins carry I2C, SPI, UART, RVSWD / SWIO, SWD or CAN, and decodes them. Upper layers (NMEA, Modbus, known I2C / SPI devices) are tried on top.

Status: **beta** (`0.1.0b1`). Breaking changes may still happen; see [Stability](#stability).

## Install

```sh
pip install --pre wireskein        # or: uv add --prerelease=allow wireskein
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
            t = rec.armed()                     # right after arming the capture
            data = read_capture()               # bytes, one sample per byte, bit k = pin k
            rec.capture(data, rate, ["PA1", "PA0"], t, start_us=segment_start_us)
rec.close()
```

Then check it:

```sh
wireskein verify out/run1 --junit out/run1/report.xml --json out/run1/report.json
```

```text
OK  test_pwm/duty=64  square  c0001.bin
NG  test_pwm/duty=128  square  c0002.bin  duty 0.6999 vs 0.5020
OK  test_pwm/duty=0  level  c0003.bin
2 ok, 1 ng, 0 unchecked (4 segments, 3 captures)
```

The exit code is 1 when a check fails. A check whose pins are not in the capture is **unchecked** (`--`). Unchecked results do not fail the run.

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
from wireskein.verify import verify, junit

report = verify("out/run1")          # dict: results (with measured values), summary, log
xml = junit(report)
```

For pytest, [pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein) gives each test a `ws_run` recorder and runs `verify` after the test.

## Decoding a capture

```sh
wireskein analyze capture.sr                              # sigrok .sr, or a fixture directory
wireskein analyze capture.sr --hint '{"protocols": ["i2c"]}'
wireskein analyze capture.sr --mode all --out result.json
wireskein segments capture.sr --results                   # a capture with marker lines on a UART
```

`--hint` restricts what is tried. It can name protocols, or give pins with their roles and baud rates. The result is still scored by the decoders' own checks.

```python
from wireskein.analyze import load, analyze, export

cap = load("capture.sr")
res = analyze(cap, {"protocols": ["spi"]})
doc = export(res, cap)
```

## Stability

| Part | Promise during the beta |
| --- | --- |
| `wireskein.runlog` (names, arguments and meaning of `Recorder` and the check helpers) | Stable. New arguments get defaults that keep the old meaning |
| Run format (`run.json`, `FORMAT = "wireskein-run/0"`) | Stable. An incompatible change raises `FORMAT`, and `verify` refuses older runs with a clear error |
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
2. Run the `Release` workflow manually and enter the version, for example `0.1.0b1`.
3. The workflow does the rest. It updates the version in `pyproject.toml` and `src/wireskein/__init__.py`, moves the changelog entries under the new version, runs the tests, builds, commits, tags `v<version>`, creates a GitHub Release, and publishes to PyPI.

PyPI publishing uses Trusted Publishing.

## License

MIT
