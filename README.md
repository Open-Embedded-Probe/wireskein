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
OK  test_pwm/duty=64  square  c0001.wireskein
NG  test_pwm/duty=128  square  c0002.wireskein  duty 0.6999 vs 0.5020
OK  test_pwm/duty=0  level  c0003.wireskein
2 ok, 1 ng, 0 unchecked (4 segments, 3 captures)
```

The captures are stored as `.wireskein` files (below). The exit code is 1 when a check fails. A check whose pins are not in the capture is **unchecked** (`--`). Unchecked results do not fail the run.

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

## Capturing

`wireskein capture` takes logic channels from a device and saves a `.wireskein`:

```sh
pip install "wireskein[oep]"      # for OEP probes (oep-client-python); sigrok needs sigrok-cli on PATH
wireskein capture --source oep:/dev/ttyACM0 --channels SDA=47,SCL=48 --rate 20M --samples 200k -o i2c.wireskein
wireskein capture --source sigrok:fx2lafw --channels SDA=D0,SCL=D1 --rate 12M --samples 1M \
                  --trigger SCL:fall --pretrigger 1k --note "after reflow" -o i2c.wireskein
```

| Source | Device | Channel ids |
| --- | --- | --- |
| `oep:<target>` | An OEP probe through oep-client-python. `<target>` is a serial port, `tcp://HOST:PORT` (a broker) or `usb[:VID:PID[:SERIAL]]` | The probe's channel numbers |
| `sigrok:<driver>` | Any device sigrok supports, through `sigrok-cli` (`fx2lafw`, `dreamsourcelab-dslogic`, `demo`, ...) | sigrok's channel names (`D0`, ...) |

`--channels` names each logic channel (`NAME=ID`). `--analog NAME=ID[@FRONTEND],...` adds analog channels (`--analog-rate`, `--analog-samples`; by default as long as the logic capture). On an OEP probe, logic and analog are started together (capture-group); each analog channel keeps its own rate and start (the probe's estimate, with its uncertainty), raw values, input range, reference and the probe's factory calibration. With sigrok, analog ids ride at the device's one rate.

 The file keeps the rate the device actually used, plus what the source knows: the probe's `start_us`, a `time_base_slipped` mark, the trigger position. From Python: `wireskein.sources.capture(source, Request(...), out)`. Other packages can add sources through the `wireskein.sources` entry point group.

## Viewing captures in the browser

```sh
wireskein gui capture.wireskein          # opens the browser on that capture (a .sr is converted on the fly)
wireskein gui runs/                # a page listing the .wireskein / .sr files and recorded runs below runs/
```

The viewer is [wireskein-web](https://github.com/Open-Embedded-Probe/wireskein-web), shipped in the wheel. It shows
logic and analog lanes, dots on the samples the probe really took (a decimated channel shows only its own samples), and
the metadata, acquisition settings, attachments and notes.

The server listens on 127.0.0.1 only and prints a URL with a one-time token; requests without it, or for another host
name, are refused. For a bench machine, forward the port (`ssh -L PORT:127.0.0.1:PORT bench`) and open the printed URL.
It serves nothing but the viewer and the capture files below the directory given.

## WireSkein files (.wireskein) and conversion

A WireSkein file (`.wireskein`, a zip) holds a capture and what goes with it: attachments and notes now, markers and decoding / check results later. Tools tell it apart by its content, not its name. A capture keeps each channel at its own sample rate. A probe that decimates some channels to fit its link (every 32nd sample, say) stores only the samples it took, with `step=32`. Nothing is repeated to fill the gaps, so a viewer can show exactly the samples that exist. The module `wireskein.fileformat` reads and writes it with the standard library only:

```python
from wireskein import fileformat as wf

wf.write("c.wireskein", 100_000_000, [
    wf.Channel("PA5", wf.pack(pa5_samples), n),                 # samples: one byte per sample, 0 or 1
    wf.Channel("PB0", wf.pack(pb0_samples), n // 32, step=32),  # a channel kept at 1/32 of the rate
], start_us=segment_start_us)
channels = wf.from_interleaved(data, ["PA5", "PA7"], width=8)    # a probe's stream: width bits per sample, bit k = channel k
```

Analog channels can sit in the same file, each with its own rate and start time (`t0_ticks`), so an ADC whose real rate is not a whole number of logic ticks keeps its true timing. ADC values are kept raw with their linear conversion (`zero`, `scale_nv`), and how they were taken (pin, attenuation, reference voltage, the probe's factory calibration values) is stored too, whether or not the analysis uses it:

```python
wf.write("m.wireskein", 20_000_000, [
    wf.Channel("CLK", wf.pack(clk_samples), n),
    wf.analog_raw("VBUS", raw_values, Fraction(80_000_000, 1667), width=16, value_bits=12, zero=0, scale_nv=805_860,
                   pin=22, attenuation_db=12, reference={"source": "vdd", "mv": 3300}),
    wf.analog_volts("SINE", volts, 1_000_000),                     # volts, e.g. from sigrok or Saleae
], probe={"chip": "ESP32-P4", "calibration": {"scheme": "curve-fitting-v1", "raw": "..."}})
```

`wireskein capture --source sigrok:<driver>` takes analog channels too (`--channels CLK=D0,VBUS=A0`). The analysis reads the logic channels as before; checks on analog channels come later. The format is specified in `docs/wireskein-format.ja.md`.

In a recorded run, pass the same thing to `rec.capture(t, tick_hz, channels=[...])`.

A WireSkein file can also carry anything else about the capture: acquisition settings, a wiring note, analysis results. Attachments are named files (text, JSON or bytes) and can be replaced. Notes form an append-only log, one entry per call, with its time. Both can be added to an existing file without rewriting the channels:

```python
wf.attach("c.wireskein", "probe.json", {"fw": "1.2", "plan": plan})   # dict / list -> JSON, str -> text, bytes as is
wf.note("c.wireskein", "PA5 looked noisy; shorter wire next time")
wf.note("c.wireskein", {"i2c": transactions}, kind="analysis")
wf.attachments("c.wireskein"), wf.notes("c.wireskein")
```

```sh
wireskein info c.wireskein                                  # channels and rates, metadata, attachments, notes
wireskein note c.wireskein "re-captured after reflow"
wireskein attach c.wireskein setup.txt --text "10k pull-ups on SDA/SCL"
wireskein attach c.wireskein scope.png scope.png            # any file
```

`rec.capture(..., attachments={...})` stores attachments with a capture of a recorded run. Attachments and notes go along when a capture is converted to `.sr` and back.

Convert between formats on the command line (the format follows the extension):

```sh
wireskein convert c0001.wireskein c0001.sr     # for PulseView: one rate, slow channels repeated
wireskein convert c0001.sr c0001.wireskein     # back: channels get their real rate again
wireskein convert corpus/fixtures/real/<id> capture.sr
```

A `.sr` has one sample rate for all channels, so slow channels are repeated to the fastest rate there. The real rate of each channel is kept in `wireskein.json` inside the `.sr`. sigrok ignores this file, and WireSkein reads it back. In PulseView, the repeated samples look like real ones.

## Decoding a capture

```sh
wireskein analyze capture.wireskein                             # .wireskein, sigrok .sr, or a fixture directory
wireskein analyze capture.sr --hint '{"protocols": ["i2c"]}'
wireskein analyze capture.sr --mode all --out result.json
wireskein segments capture.sr --results                   # a capture with marker lines on a UART
```

`--hint` restricts what is tried. It can name protocols, or give pins with their roles and baud rates. The result is still scored by the decoders' own checks.

```python
from wireskein.analyze import load, save, analyze, export

cap = load("capture.wireskein")                                     # or .sr / a fixture directory
res = analyze(cap, {"protocols": ["spi"]})
doc = export(res, cap)
```

## Stability

| Part | Promise during the beta |
| --- | --- |
| `wireskein.runlog` (names, arguments and meaning of `Recorder` and the check helpers) | Stable. New arguments get defaults that keep the old meaning |
| Run format (`run.json` + `.wireskein` captures, `FORMAT = "wireskein-run/2"`) and the capture format (`.wireskein`, `wireskein/0`) | Stable. An incompatible change raises `FORMAT`, and `verify` refuses older runs with a clear error |
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
