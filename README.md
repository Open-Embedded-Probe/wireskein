# WireSkein

![WireSkein: open-source logic analyzer library](https://raw.githubusercontent.com/Open-Embedded-Probe/wireskein/main/docs/images/wireskein-top.jpg)

[日本語 README](https://github.com/Open-Embedded-Probe/wireskein/blob/main/README.ja.md)

WireSkein turns logic-analyzer captures into **tests and analysis** (MIT license). Instead of ending with someone looking at a waveform, it checks in code whether the wire did what it should, and hands what was on the wire to scripts and AI in a form they can read.

- **Checking recorded hardware test runs.** A test records what it sent, the captures, and what each step should look like on the wire (a 1 kHz square wave, an I2C write to 0x42, a UART at F_CPU / BRR, a 3.3 V ± 0.1 V supply). `wireskein verify` checks every capture against these expectations and reports OK / NG with measured values, as text, JSON and JUnit XML. From pytest, use [pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein).
- **Decoding unknown captures.** `wireskein analyze` finds which pins carry I2C, SPI, UART, RVSWD / SWIO, SWD or CAN, and decodes them. Upper layers (NMEA, Modbus, known I2C / SPI devices) are tried on top.
- **Capturing, storing, viewing.** `wireskein capture` takes captures from a probe, stores them as `.wireskein`, and `wireskein gui` shows them in the browser.

Status: **beta**. Breaking changes may still happen; see [Stability](#stability).

## Why WireSkein

A logic analyzer is essential in embedded work, but the usual tools are built around a person looking at a screen. That leaves these problems:

| The problem | How WireSkein solves it |
| --- | --- |
| **Captures are not tests.** You look at the waveform and decide it is probably right. There is no way to check a capture against expectations, get OK / NG, and run it in CI | The test states its expectations in code (`square`, `uart`, `i2c`, `voltage`, ...) and `wireskein verify` checks them with measured values. JUnit XML and a pytest plugin put it straight into CI |
| **Tied to the hardware.** The easiest apps expect their own analyzers; cheap analyzers and home-made probes do not fit, and licenses may not allow embedding or changes | Any source: sigrok `.sr` and VCD files are read as they are; capture directly from sigrok devices or from OEP probes built on boards such as the ESP32. MIT license |
| **Decimated or slow channels are padded with fake samples.** Existing formats have one rate for all channels, so a channel kept at a lower rate is filled with samples that were never taken | A `.wireskein` keeps each channel at its own rate and stores only the samples taken; the viewer shows the real samples and how uncertain each edge is |
| **Logic and analog live apart.** Separate recordings, separate tools, times lined up by hand | Logic and analog in one file; `wireskein align` finds the offset and time scale from the edges of a signal seen on both; analog channels take logic checks (`threshold=`) and voltage checks |
| **Too much data.** Having people or AI read every waveform and event is heavy, yet summarizing can drop what matters (in one RVSWD investigation, a link that sped up mid-capture was taken for noise) | Structured output of just the layers needed (a command list, spans, or full detail), without hiding undecoded spans, the evidence for each candidate, or the capture quality |
| **Every use of the output needs glue code.** Re-parsing display strings, one conversion script per tool | Output is JSON with a stable structure; times are integer ticks that lead back to the capture |

### AI-friendly

- **CLI and JSON first.** Everything works without a screen: an AI agent can run a command, read the result and decide the next step.
- **Just what is needed.** `analyze --select` (layers), `--window` (time range) and `--segment` (a marker span) pass only the part a question needs, saving tokens.
- **The grounds for each answer.** Decoding candidates, how well they fit, where they do not, and undecoded spans come back, so an AI can tell what was read from what was not.
- **Checks come with measurements.** An NG gives its reason and the measured values (frequency, duty, baud error, voltage, ...), which points toward the cause.
- **Findings stay with the capture.** Notes, attachments, markers and decoding annotations go into the same file, so the next person, or the next AI, sees how it was investigated.

### Test-friendly

- **Expectations are code.** `wireskein.runlog` uses only the standard library, so it fits into any test script.
- **The test's structure is recorded.** Headings (`#` a test, `##` a step) build a tree of segments, and each capture belongs to one, so an NG names the step and the expectation.
- **Pins and roles are given.** Checks do not depend on automatic detection, so results are stable. How to choose capture windows and tolerances: [docs/capture-test-guide.ja.md](docs/capture-test-guide.ja.md).
- **Straight into CI.** JUnit XML, and with [pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein) each pytest test is recorded and checked. The captures of an NG stay as files to open in the viewer later.

### Sources: existing formats, and OEP probes

- **Existing formats work as they are.** sigrok / PulseView `.sr` and VCD (GTKWave, Saleae, DSView, simulators, ...) are recognized by their content, not their names, and can also be written. sigrok devices capture directly with `wireskein capture --source sigrok:<driver>`.
- **[Open Embedded Probe (OEP)](https://github.com/Open-Embedded-Probe/oep-spec)** is an open protocol between a small board wired to the chip you develop on (the probe) and the software on your PC. One probe is a debugger (RVSWD / SWIO for WCH's CH32, SWD for ARM), a console to the target and a test fixture (GPIO, UART, SPI / I2C devices, logic and analog capture) at once. Firmware: [oep-probe-arduino](https://github.com/Open-Embedded-Probe/oep-probe-arduino); host side: [oep-client-python](https://github.com/Open-Embedded-Probe/oep-client-python).
  - With an **ESP32-P4** as the probe, logic capture runs **from 16 channels at 20 Msps up to 160 Msps on 2 channels** (40 Msps on 8). Each test picks the pins it captures, with no change to wiring or firmware.
  - A classic ESP32 and other boards work too, more slowly: a probe declares what it can do, and WireSkein uses that. With ADC channels, analog is captured alongside.

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
            rec.capture(t, rate, interleaved=data, names=["PA1", "PA0"], start_ns=seg.start_ns, start_uncertainty_ns=seg.start_uncertainty_ns)
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
2 ok, 1 ng, 0 unchecked, 0 measured (4 segments, 3 captures)
```

The captures are stored as `.wireskein` files (below). The exit code is 1 when a check fails. A check that could not be made (its pins are not in the capture, ...) is **unchecked** (`--`) and **fails the run by default**, since it is usually a mistake in the test or the wiring (`--allow-unchecked` lets it pass). A check asked only to measure (`uart(baud=None)`, ...) is **measured** (`ME`) and does not fail. The run format and the result statuses: [docs/run-format.ja.md](docs/run-format.ja.md).

### Headings and segments

Headings split the run into a tree of segments. `#` is a test, `##` is a step, and a heading without a name (`##`) closes that level. `Recorder.section()` opens a heading and closes it when the `with` block ends. A capture belongs to the segment that was open when it was recorded (its path is stored with it). Expectations are stored under the segment path, for example `test_pwm/duty=64`. When a name repeats under the same parent, the first keeps its name and the repeats get an index (`duty=64`, `duty=64[1]`, `duty=64[2]`), each with its own expectations; adding a repeat later does not rename the earlier ones. A broken structure, such as a skipped level, is always reported as NG.

### Checks

| Helper | Checks |
| --- | --- |
| `square(pin, freq_hz, duty, tol_freq, tol_duty, max_jitter)` | A steady square wave: frequency (relative tolerance), duty (absolute), period spread |
| `level(pin, value)` | The pin does not move |
| `starts({pin: v})` / `ends({pin: v})` | The level at the first / last sample |
| `only_moving([pins])` | No other captured pin moves |
| `pulses(pin, count, period_s, tol_period)` | Number of rising edges and their period (`tol_period` relative) |
| `i2c(scl, sda, transactions, hz, tol_hz, released)` | Transactions (address, direction, bytes, ACK, `complete`), SCL rate, and a released bus at the end |
| `spi(clk, mosi, miso, cs, mode, mosi_bytes, miso_bytes, hz)` | Mode, bytes on both lines, SCK rate, and CS high at the end |
| `uart(pin, baud, data, tol_baud, idle, bits, parity, stop, max_errors)` | Bit rate measured from the edges, data, idle level, and framing / parity errors. `baud=None` only measures |
| `voltage(pin, volts, tol_v, min_v, max_v, ripple)` | An analog channel: mean within `volts` ± `tol_v` (V), every sample within `min_v`..`max_v`, peak-to-peak at most `ripple`. Only what is given is checked |

The logic checks (`square`, `level`, `starts` / `ends`, `pulses`, `i2c`, `spi`, `uart`) also run on analog channels when given `threshold=`: one voltage, or `(low, high)` for hysteresis (a noisy slow edge then makes one edge). Edges are placed where the line between two samples crosses the threshold; the resolution is one ADC sample, added to the tolerances. Without `threshold=`, an analog channel in a logic check is unchecked, and the reason says so.

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

 The file keeps the rate the device actually used, plus what the source knows: the probe's `start_ns` and `start_uncertainty_ns` (the probe clock of the first sample), a `time_base_slipped` mark (the probe knows some samples were taken late, e.g. at its buffer limit, so times may be stretched there; `wireskein info` says so, and a failed check names it), the trigger position (`trigger_index`). On a UART probe (a classic ESP32 jig, say), the link is raised to a faster rate for reading the capture back: 1.5 Mbaud, 921600, 500000, the first that holds with both directions busy (oep-client-python 0.0.27 or later). The client refuses a rate that breaks and drops back to the boot speed on repeated broken frames, so it does not end slower than the boot speed. Each trial costs up to a second, so wireskein tries at most two per capture, verifying only the read-back direction, and uses oep-client-python's record of what held and what failed per port and probe (`~/.cache/oep-client/link-speed.json`; a failure is skipped for 30 days): a port whose fast rates fail moves down the list instead of paying for them every time. An explicit `?fast=RATE` ignores the record, for measuring limits. `?fast=0` keeps the boot speed, `?fast=921600` (or a list) tries only those. A probe with USB (an ESP32-P4) is best opened as `oep:usb:VID:PID[:SERIAL]`: the vendor bulk interface reads back at megabytes per second without loss, where its USB serial port (CDC) is slower and can drop answers under load. `capture` prints how the link went (the rate, the trials, the read time), also kept in `meta.probe.link`. From Python: `wireskein.sources.capture(source, Request(...), out)`. Other packages can add sources through the `wireskein.sources` entry point group.

## Viewing captures in the browser

```sh
wireskein gui capture.wireskein    # opens the browser on that capture (a .sr or .vcd is converted on the fly)
wireskein gui runs/                # a page listing the captures and recorded runs below runs/
```

The viewer is [wireskein-web](https://github.com/Open-Embedded-Probe/wireskein-web), shipped in the wheel (also on
[GitHub Pages](https://open-embedded-probe.github.io/wireskein-web/) for files opened in the browser). It shows:

- logic and analog lanes on a time axis; hover to measure pulse widths, period, frequency and duty (analog: the value);
  Shift + wheel scrolls in time;
- the samples the probe really took (a decimated channel shows only its own), the trigger, and analog on the aligned
  time when the file has an alignment;
- decoding annotations (I2C transactions, UART characters, ...) under their data line: from the file
  (`wireskein annotate --save`), or decoded on request with a button to store them;
- markers: M puts one at the mouse, and they are saved into the file;
- for a capture of a recorded run, the run's check results (OK / NG and why);
- the metadata, acquisition settings, attachments and notes, with a form that appends a note.

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
], start_ns=seg.start_ns, start_uncertainty_ns=seg.start_uncertainty_ns)
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

`wireskein capture --source sigrok:<driver>` takes analog channels too (`--channels CLK=D0,VBUS=A0`). The analysis reads the logic channels; the checks read analog channels too (`voltage()`, and `threshold=` on the logic checks). The format is specified in `docs/wireskein-format.ja.md`.

A probe's analog start time is an estimate (an ADC may start some hundred microseconds off and run a little fast or slow). When the same signal is on a logic channel and an analog channel (the same net wired to both, or a marker pulse), `wireskein align` finds the offset and the time scale from its edges and stores them as `attach/alignment.json`; the samples and stored times are not changed:

```sh
wireskein align m.wireskein --reference SYNC --via SYNC_A --threshold 1.0,2.3 --save
# SYNC_A against SYNC: start +197.890 us, scale +1490.7 ppm, 166/166 edges matched, residual 5.665 us
```

In Python, `wireskein.align.find()` / `apply()` / `save()` / `load()`. A periodic signal is ambiguous when the start may be off by more than its period: give `--max-offset`, or align on an irregular marker pulse.

Captures from two probes taken at the same time line up the same way, from a signal both saw (wired to a pin of each):

```sh
wireskein align B.wireskein --to A.wireskein --reference SYNC --via SYNC --save
# B.wireskein onto A.wireskein: tick 0 at +3699.999 us, clock -79.98 +- 0.02 ppm, 264/264 edges matched, residual 0.031 us
```

The result goes into B's `alignment.json` (with A's identity); neither file's samples change. The viewer's "Add another probe's file" then draws B's channels on A's time axis (`align.between()` in Python).

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
wireskein convert c0001.wireskein c0001.vcd    # GTKWave etc.: only the changes, each channel at its own rate
wireskein convert saleae.vcd s.wireskein       # a VCD from elsewhere: the tick is the common divisor of its times
wireskein convert corpus/fixtures/real/<id> capture.sr
```

A `.sr` has one sample rate for all channels, so slow channels are repeated to the fastest rate there. The real rate of each channel is kept in `wireskein.json` inside the `.sr`. sigrok ignores this file, and WireSkein reads it back. In PulseView, the repeated samples look like real ones.

## Decoding a capture

```sh
wireskein analyze capture.wireskein          # a WireSkein file, a sigrok .sr, a VCD, or a fixture directory
wireskein analyze capture.sr --hint '{"protocols": ["i2c"]}'
wireskein analyze m.wireskein --threshold TX=1.0,2.3        # also decode an analog channel, read as logic
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
