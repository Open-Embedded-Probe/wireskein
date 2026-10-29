# Run format `wireskein-run/3` and verification results

[日本語](run-format.ja.md)

This is the specification of a test run and of its verification results. Any language may write and read it by following this specification (in Python, `wireskein.runlog` writes it and `wireskein verify` reads it). The capture files themselves are described in [WireSkein file format](wireskein-format.md), and the segment rules in [Markers](markers.md). On an incompatible change, `format` is bumped, and readers reject runs with values they do not know ([Design decisions](design.md) §1).

## 1. Run

A run is a directory.

| Item | Contents |
| --- | --- |
| `run.json` | Log, list of captures, expectations (§2) |
| `cNNNN.wireskein` | One capture per acquisition (starting at `c0001`) |

The run is a directory, with each capture in a separate file, so that if the test crashes partway, the captures taken up to that point remain.

## 2. `run.json`

| Key | Type | Meaning |
| --- | --- | --- |
| `format` | string | `"wireskein-run/3"`. Readers reject runs with any other value |
| `meta` | object | Information about the run (what was passed to `Recorder(out, **meta)`) |
| `log` | array | Log: `{"t": seconds, "src": "marker" / "host" / "dut" / "note" / "lost", "text": string}`. `t` is seconds since the recorder started (host clock). Lines are in `t` order. Lines on the probe's clock also have `probe_ns` and more (§2.4) |
| `captures` | array | Captures: `{"file": name, "t0": seconds, "channels": [name...], "path": segment path}` |
| `expect` | object | Maps a segment path to `{"checks": [check...]}` |

### 2.1 Segments and paths

- Headings (`#` is a test, `##` is a step; a heading with no name closes) are written to `log` with `src: "marker"`, and build the segment tree.
- A segment path is the heading names joined with `/` (e.g. `test_pwm/duty=64`).
- **When the same name repeats under the same parent, the first keeps the name as is, and from the second on `[1]`, `[2]`, ... are appended** (e.g. `duty=64`, `duty=64[1]`). Adding the same name later does not change the paths of earlier segments.
- Numbering starts from the second occurrence so that adding the same name later does not change the paths of earlier segments or the expectation keys attached to them.
- Marker segment paths such as those of `wireskein analyze --segment` follow the same rule.

### 2.2 Capture assignment

- `captures[].path` is the path of the deepest segment open when the capture was taken. When taken outside any segment, the key itself is omitted.
- Verification treats a capture with `path` as belonging to that segment (and its ancestors).
- A capture without `path` (in a run written by another tool) is assigned to the segment that contains `t0`.
- A segment opened and closed by lines on the probe's clock is the exception: the captures holding it are assigned to it, cut to its time (§2.4).

### 2.3 Checks

Each element of `checks` is an object `{"kind": ..., ...}`. `kind` is one of `square`, `level`, `starts`, `ends`, `only_moving`, `pulses`, `i2c`, `spi`, `uart`, `voltage`. The keys and meaning of each check are in [Testing with captures](capture-test-guide.md) §4 (the functions of the same names in Python's `wireskein.runlog` build these objects).

Tolerance names:

| Name | Relative / absolute | Used by |
| --- | --- | --- |
| `tol_freq` | Relative (deviation of the frequency ratio) | `square` |
| `tol_duty` | Absolute (difference of duty; between values in 0–1) | `square` |
| `tol_baud` | Relative | `uart` |
| `tol_hz` | Relative | `i2c`, `spi` |
| `tol_period` | Relative | `pulses` |
| `tol_v` | Absolute (V) | `voltage` |

- When `threshold` (V, or `(low, high)`) is given, the logic checks (`square`, `level`, `starts`, `ends`, `pulses`, `i2c`, `spi`, `uart`) read an analog channel as logic.
- `only_moving` looks at logic channels only.

### 2.4 Lines on the probe's clock

Code on the probe may keep a log on its own clock (oep-probe-arduino's `OepProbeLog.h`, oep-client-python's `oep_client.probe_log`). The lines are kept instead of sent, so it is faster than a UART and needs no pin. `Recorder.probe_lines(rows, lost=...)` writes them into the log.

| Key | Type | Meaning |
| --- | --- | --- |
| `probe_ns` | integer | The line's time on the probe's clock (ns since its boot; the clock of OEP core §7.7's uptime_ns) |
| `boot_id` | integer (OEP's boot_id) | The boot of that clock |
| `probe` | string (optional) | Which probe (e.g. `"usb:30eda0ea068b"`; written as the end of a capture's `meta.source`) |
| `uncertainty_ns` | number (optional) | Uncertainty of the line's own time (±ns) |

- `t` is always given too (the probe's time converted to the host clock). The segment tree and the order of lines come from `t`.
- Heading lines (`src: "marker"`) open and close segments like any heading. Other lines are `src: "dut"`.
- **A segment whose opening and closing lines both have `probe_ns`** is placed by its position in the captures. When a capture part's `meta.start_ns` is on the same clock (the same `boot_id`, and the same `probe` when given), a line's tick is (`probe_ns` − `start_ns`) × the tick rate. A capture holding the segment is assigned to it whatever its recorded `path`, and only the segment's time is cut out and checked.
- The bounds' uncertainty is the capture part's `start_uncertainty_ns` plus the lines' `uncertainty_ns`. A check runs on two windows, the bounds moved out and moved in; when the verdicts differ, it is `unchecked` (the verdict depends on where the bounds fall), never silently `ok`. A segment shorter than its uncertainty is `unchecked`.
- A segment that begins before a capture (or ends after it) is cut at the capture's edge, said in the result's `measured` `clipped`.
- With no capture on the same clock, the segment's expectations are `ng` for having no capture. A capture near in time is never used instead.
- A capture's boot is read from `meta.boot_id`, `meta.probe.boot_id`, or the `boot_id` of `meta.probe.clock`; `Recorder.capture(..., boot_id=...)` puts it in.
- What the probe dropped of its lines (its buffer overflowed, marks skipped) becomes a `src: "lost"` line, and verification gives a `probe_log` `ng`: which segments are right is not known.
- `Recorder.close()` also copies the probe's lines into each capture file's markers (`by: "probe-log"`, [file format](wireskein-format.md) §5.2), so a viewer shows them on the waveform.
- Expectations for a segment of the probe's lines are given with `Recorder.expect(path, [checks...])`.

## 3. Verification results

Verification returns one result per check (the report of `wireskein verify`).

| Key | Meaning |
| --- | --- |
| `path` | Segment path |
| `capture` | Capture file name (null if no capture matched) |
| `check` | Check kind (`kind`) |
| `status` | One of the four below |
| `ok` | true if `status` is `ok`, false if `ng`, null otherwise (reading `status` is more reliable) |
| `expected`, `measured`, `reason` | Expectation, measured value, reason |

Statuses:

| `status` | Meaning | Fails by default |
| --- | --- | --- |
| `ok` | As expected | No |
| `ng` | Differs from the expectation | Yes |
| `unchecked` | Could not be checked (pin not in the capture, no voltage conversion, no segment or capture, etc.) | **Yes** (No with `--allow-unchecked`) |
| `measured` | Requested as measured only (`uart(baud=None)` etc.). Returns the measured value | No |

- `unchecked` fails by default because most such cases are mistakes in the test or missing wiring or capture.
- The summary (`summary`) is the counts of `ok`, `ng`, `unchecked` and `measured`, plus the number of segments and the number of captures.
- The shapes of the report JSON (`--json`) and JUnit XML (`--junit`) follow this table. In JUnit, `ng` is failure, `unchecked` is failure by default (skipped when allowed), and `measured` is passed (with the measured value in system-out).
- Text lines (`lines()`) are for people to read, and their shape is not guaranteed. Programs read the JSON.
