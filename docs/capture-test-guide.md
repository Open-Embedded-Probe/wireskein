# Testing with captures

[日本語](capture-test-guide.ja.md)

This guide explains how to add checks against logic-analyzer captures to hardware tests. The example tests the Arduino core for CH32 ([ArduinoCore-CH32RV](https://github.com/ch32-riscv-ug/ArduinoCore-CH32RV)) with an OEP probe such as the ESP32-P4. From pytest, use [pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein).

The purpose here is not to infer the protocol of an unknown signal, but to **confirm that the intended signal is actually output**. This rests on two assumptions:

- **The test holds the expectation.** The test passes the protocol, pins, parameters and tolerances to the checking side.
- **The test creates the boundaries and the quiet segments.** Markers show which part of the run corresponds to which expectation.

## 1. What bugs captures are for

A pair test, which exchanges values and looks at OK / NG, mainly finds value mismatches. Captures are for finding bugs where the values are right but the waveform is out of spec.

| Category | Example | Visible in a pair test? |
| --- | --- | --- |
| Cleanup | SDA or SCL does not return High after I2C. SPI CS does not return High. A pin stays an output after `end()` | No (it first breaks in the next test) |
| Startup disturbance | A glitch when `begin()` switches a pin's function. A garbage character during UART initialization | Not if the receiver discards it |
| Timing parameters | PWM frequency or duty cycle error, baud rate error, I2C tLOW and tHIGH, SPI CS-to-clock time | Not if within the receiver's tolerance |
| Format | SPI mode (CPOL/CPHA), bit order, number of stop bits, I2C Repeated Start | Not if the receiver happens to use the same setting |
| Extra activity | Another pin moves (remap error). Extra clocks are output. PWM drops a pulse on update | No |
| Transmission with no receiver | RGB LED (WS2812), tone(), clock output | Decoding on the receiving side is tedious |

The most effective check is **"every pin other than the ones allowed to move in this step is idle"** (`only_moving`). If you connect as many DUT pins as possible to the probe, you can find remap errors and missing pin initialization in one go.

## 2. Making a run

### 2.1 Recorder

The test script records into one directory with `wireskein.runlog.Recorder`. The run format is described in [Run format](run-format.md).

| What to record | Example | Call |
| --- | --- | --- |
| Heading markers | `# test_pwm`, `## duty=64` | `section(level, name, expect=[...])` (used with `with`), `heading(level, name)` |
| Commands sent | `PWM 64` | `command(text)` |
| Replies received | `PWM duty=64` | `reply(text)` |
| Notes | `capture incomplete` | `note(text)` |
| Captures | `c0001.wireskein` | `capture(t, rate, interleaved=data, names=[...])` after `t = armed()`, or `capture(t, tick_hz, channels=[...])` |

- Markers are written to the PC-side log. They are not sent to the DUT, so the DUT firmware does not need to change ([Markers](markers.md) §3.1).
- A capture is assigned to the segment that is open when it is taken.
- Commands and replies remain as events inside the segment (include them in the report with `wireskein verify --json OUTPUT --log`).
- If you also pass the start time returned by the probe and its uncertainty, they are kept in the file: `capture(..., start_ns=seg.start_ns, start_uncertainty_ns=seg.start_uncertainty_ns)`.

### 2.2 One condition per step

```python
from wireskein.runlog import Recorder, square, level, only_moving

rec = Recorder("out/run1", target="x035")
with rec.section(1, "test_pwm"):
    for duty in (64, 128, 192, 255, 0):
        want = ([square("PA1", 1000, duty / 255, freq_rel=0.02, duty_abs=0.01)] if 0 < duty < 255
                else [level("PA1", 1 if duty == 255 else 0)]) + [only_moving(["PA1"])]
        with rec.section(2, f"duty={duty}", expect=want):
            rec.command(f"PWM {duty}")
            rec.reply(send_and_wait(f"PWM {duty}"))
            t = rec.armed()                        # right after starting the capture
            data = read_capture()                  # width bits per sample; bit k is channel k
            rec.capture(t, rate, interleaved=data, names=["PA1", "PA0", "PC16", "PC17"])
rec.close()
```

- When you try several frequencies and duty cycles, make one step per combination. If the whole test is one segment, you cannot tell which condition failed.
- In a step that runs several peripherals at once, write all of them in that step's expectation, and list every pin allowed to move in `only_moving`.
- Write the expectation and the capture `names` with DUT pin names (`PA1` etc.). Keep the mapping between the probe's lines and DUT pin names in one place on the test side.
- A capture that comes whole - in parts, with what was lost (a stream, a repeat) - is added as it is: `rec.add(t, cap)` with a `wireskein.capture.Capture` (on an OEP probe, `wireskein.sources.oep.capture_of(acquire.capture(hst, req))` on the session the test holds), or the path of a `.wireskein` already written.
- Once `rec.close()` has written the run (pytest-embedded-wireskein closes it when the test body ends), anything more raises: nothing is silently left out of the run.

### 2.3 Checking

```sh
wireskein verify out/run1 --junit out/run1/report.xml --json out/run1/report.json
```

This builds the segment tree, checks each segment's captures against its expectation, and outputs a result per item (`ok`, `ng`, `unchecked`, `measured`) and the measured values. If there is even one `ng` or `unchecked`, the exit code is 1 (`--allow-unchecked` keeps `unchecked` from failing). NG captures stay as `.wireskein` files, so you can view them with `wireskein gui`.

`tests/demo_run.py` is an example that synthesizes a run of this shape, injects bugs and checks it.

## 3. Choosing the capture window

For a single-shot capture, put "quiet segment before, activity, quiet segment after" inside one capture window.

| What to confirm | Capture start | Window length |
| --- | --- | --- |
| Steady-state values (PWM and tone frequency and duty cycle) | A little after receiving the reply | 20 periods or more |
| Communication content and cleanup (I2C, SPI, UART) | **Before** sending the command | Until, after the communication, a quiet time longer than the driver's longest timeout is included |
| Startup disturbance (`begin()` glitch) | Before sending the initialization command | A few ms after initialization |
| Transitions (changing frequency without stopping) | Before the change command | 10 periods or more both before and after the change |

- The end state (`ends`, `released` of `i2c`) is judged on the last sample of the window, and the initial state (`starts`) on the first sample. If the window ends right after the communication, they cannot be judged, so leave a margin at the end.
- `square` assumes the whole window is steady state. For things that move a fixed number of times and stop (toggles, millis), check the count and period with `pulses`, and the level after stopping with `ends`.
- `only_moving` looks only at the pins in that capture. **Add the pins you want to check to the capture** (on some probes, more channels lowers the available length or rate).

### 3.1 One capture through several conditions

A test with several parameters can keep all its conditions in one file. One capture runs while the test goes through the conditions, and each condition is a segment of it. When a check fails, the segment's path says which condition it was, and the file shows every condition on one time axis.

```python
from oep_client import acquire
from wireskein.runlog import becomes, level, only_moving
from wireskein.sources.oep import capture_of

def ask(wait, text, reply, timeout_s=2.0):
    """Send a command over the console and look for its reply between short waits (the stream keeps being read)."""
    rec.command(text)
    send(text)
    for _ in range(int(timeout_s / 0.01)):
        wait(0.01)
        line = read_reply(reply)                   # a reply line already received, or None
        if line:
            rec.reply(line)
            return line
    raise TimeoutError(f"{text!r}: no reply")

def steps(wait):
    rec.use_clock(lambda: hst.clock_best(2), probe=hst.address.place)
    with rec.section(1, "servo PB6", expect=[only_moving(["PB6"])]):
        with rec.section(2, "rest", expect=[level("PB6", 0)]):
            wait(0.05)
        for us in (544, 1000, 1500, 2000, 2400):
            with rec.section(2, f"us={us}", expect=[becomes("PB6", period_s=0.02, high_s=us * 1e-6, high_abs_s=20e-6)]):
                ask(wait, f"servo 6 {us}", "SERVO")
                wait(0.5)                          # 25 periods
        with rec.section(2, "detach", expect=[becomes("PB6", level=0)]):
            ask(wait, "servooff", "SERVOOFF")
            wait(0.05)
    rec.use_clock(None)

t = rec.armed()
lines = [("PB6", "x035@pb6"), ("PB7", "x035@pb7"), ("PB8", "x035@pb8")]   # the pin, and its neighbours for only_moving
req = acquire.Request(lines, 1_000_000, 0, mode="stream", during=steps)
rec.add(t, capture_of(acquire.capture(hst, req)))
```

- **The capture** is a stream on the session the test holds. With `during=steps` it runs until `steps(wait)` returns, and `wait(seconds)` reads the stream while the test waits (oep-client-python 0.0.47 or newer). What the steps send to the probe (console commands, fixture operations) goes over the same session.
- **Call `wait` often.** Between waits, only the probe's stream buffer holds the data. Anything else that blocks inside the steps, such as waiting for a reply, is kept short, or the reply is looked for between short waits.
- **The headings go on the probe's clock.** `use_clock` reads the probe's clock at every heading, so each segment is cut out of the capture ([Run format](run-format.md) §2.4). `probe` is the probe's address as the capture's `meta.probe.address` gives it (in oep-client-python, `hst.address.place`). Without `use_clock`, the whole capture belongs to the segment that was open when it was armed, and the conditions are not told apart.
- **One segment per condition, from its command to the next.** The segment holds the change and what follows it, and its expectation says both (§3.2). `becomes` is the default: it judges the change and the state it reaches. A steady check with the change trimmed off (`trim`) is for a test that has decided the change is not its question; the stretch it judged is in the result (`trimmed`). No segment is left without an expectation.
- **The pins around.** `only_moving` sees only the pins in the capture: put the pin's neighbours (and any pin the test must leave alone) in the capture's channels.
- **Rest before and after.** A segment before the first condition and one after the last (the output stopped) hold the line's state outside the test.
- **A released pin.** Once the pin is an input again, the line is at whatever the board pulls it to; without a pull it is not defined, and the probe cannot tell a line left low from one driven low. Check the release with a level only where the board or the probe pulls the line; elsewhere the release segment has `only_moving` (nothing else moved) and the level is not asserted.
- **The file** is one capture, `c0001.wireskein`, with the headings as its markers, which the viewer shows on the waveform. `wireskein verify` reports each segment with the stretch it judged (`c0001.wireskein@A:B`), and `wireskein states` lists what the line did (§3.3).
- **Losses:** with `on_loss="stop"` (the default), a loss raises out of `wait` and the stream ends there. What was taken is kept, and the run fails.
- A stream needs the probe's pushes. Through a relay that refuses subscriptions (OEP core §10.3), a stream cannot run; take one single-shot capture per condition (§2.2) instead.

### 3.2 Which check for which signal

A condition's segment starts with a change. A test asks one of two things of it:

- **How it changed**: the line went from the state before to the state asked for once, and what happened at the change. `becomes(pin, ...)` reads the state before from the capture just before the segment, finds the change, and judges every whole period after it. At the change it allows `irregular_max` pulses of neither state (default 1: a timer cut mid-period, which most microcontrollers cannot avoid when an output starts or stops) and can ask for some with `irregular_min`. It measures the change's time from the segment's start (`change_s`, limited with `within_s`) and the odd pulses themselves (`irregular_pulses`).
- **The steady state only**: the change is not the question, only the value once settled. Any check takes `trim(check, head_s=..., tail_s=...)` or `trim(check, head_periods=...)` and judges the segment without its ends.

| Signal | What to ask | Checks |
| --- | --- | --- |
| A continuous wave whose setting changes (PWM duty, tone frequency, servo pulse width, clock output) | The new value, every period of it, and a clean change | `becomes` with the frequency or period and the duty or high time. `irregular_max=0` where the hardware changes at a period's end (a timer with preload) |
| The same, when only the value matters | The value once settled | `trim(square(...), head_periods=...)` |
| An output that starts or stops (PWM on / off, `tone` / `noTone`, `attach` / `detach`) | The first and last pulses, the level after | `becomes(pin, level=...)` for the stop; `becomes(...)` from a rest for the start |
| A level (GPIO write, PWM at 0 or 100 %) | The level, held | `becomes(pin, level=v)` when it changes in the segment, `level(pin, v)` when it does not |
| A fixed number of pulses (toggles, a burst) | The count and the period, the level after | `pulses` and `ends` |
| A transaction bus (I2C, SPI, UART, 1-Wire, LIN) | The content in order, the timing, the bus left idle | The bus's check (`i2c`, `spi`, `uart`, ...) over the segment, with `starts` / `ends` for the idle state at its bounds; no trimming (a transaction is a whole) |
| Two lines together (half bridge, phase, parallel outputs) | Their relation | `dead_time`, `phase`, `skew` |
| An analog level | Mean, range, ripple | `voltage`, with `trim` when the segment starts with a step |
| Anything else that must not move | Silence | `only_moving` on the outer segment |

A continuous wave and a transaction bus differ in where the question lies. A wave is judged period by period, so a change is a point between two states and its neighbourhood is the interesting part. A transaction is judged as a whole, from its start to the bus going idle, so its segment is the transaction and the time after it (the longest driver timeout), not trimmed.

### 3.3 A summary, the details, or a verdict

WireSkein turns a capture into what a reader needs, so the samples themselves do not have to be read. How much comes out depends on the question.

| Want | Use | What comes out |
| --- | --- | --- |
| What happened on a line, briefly | `wireskein states FILE [--pin P]` | Each stretch (a level, or a wave: frequency, high time, duty, periods) and the odd pulses between them, one line each |
| The same, segment by segment of a test run, without expectations | `wireskein states RUN --section 'PATH'` (Python: `wireskein.verify.sections(run)` gives each segment cut from its capture, for any analysis) | The stretches of each segment whose path matches |
| What a bus carried | `wireskein analyze` / `decode` | The decoded transactions |
| Whether the test passed, and where not | `wireskein verify` | One result per check, with the values measured |
| Every edge in a stretch | `wireskein gui`, or the file read in Python (`wireskein.capture`) | Everything |

- Start from the summary and go to the details only where it shows something: `states` says at which time an odd pulse sits, and the viewer shows that place.
- The checks take tolerances up front (`freq_rel`, `duty_abs`, `irregular_max`, ...) and say what was measured either way. A check meant only to measure, with no tolerance to judge, is written that way (`uart(baud=None)`), so nothing is lost by setting tolerances.
- `states` takes its own tolerance (`--tol`, `--min-periods`): pulses alike within it are one wave. A tighter tolerance shows more stretches.
- A rule no check has is the test's own: it reads a segment's analysis (`wireskein.verify.sections(run)`, then `states`, `decode`, ...), judges it, and records the verdict with `rec.result(path, name, status, measured=..., reason=..., notices=[{"kind", "tick"}])`. Verification reports and counts it with its own checks, so one run and one report keep every verdict.

### 3.4 Seconds or periods

| Quantity | Unit | Why |
| --- | --- | --- |
| How long a segment holds a wave | Periods (20 or more for a frequency and a duty; `min_periods` of `becomes` at least) | The check judges periods; the same count gives the same confidence at any frequency |
| What to trim off a wave's segment | Periods (`head_periods`) | The change lasts a period or two of the signal, whatever its frequency |
| How long to wait for a reply or a bus to go idle | Seconds | Set by the firmware and the driver (a timeout), not by the signal |
| When a change must have happened | Seconds (`within_s`) | It is the time the DUT takes to act on a command |
| Rest before and after a test | Seconds, longer than the slowest period of the waves around it | A level is told from a wave's low half only when it lasts longer than a period |

## 4. Checks

| Check | Description |
| --- | --- |
| `square(pin, freq_hz, duty, freq_rel, duty_abs, jitter_max, outliers_max)` | A regular square wave through the whole window: every whole period within `freq_rel` (relative) and its high time within `duty_abs` (absolute, of the period), at most `outliers_max` periods off (default 0). `jitter_max` is the upper limit of period variation (relative). The resolution from the sample rate is added automatically |
| `becomes(pin, level=v)` / `becomes(pin, freq_hz=or period_s=, duty=or high_s=, high_abs_s or high_rel, irregular_max, irregular_min, min_periods, within_s)` | The line ends the window in a state (a level, or a wave), reached by at most one change from the state before the window (§3.2). Measures `change_s`, `irregular_pulses` |
| `trim(check, head_s, tail_s, head_periods=, tail_periods=)` | Any check, judged without the ends of its window (§3.2) |
| `level(pin, value)` | Constant throughout the window |
| `starts({pin: v})` / `ends({pin: v})` | Level at the first / last sample of the window |
| `only_moving([pins])` | No other captured pin moves. Looks only at logic channels |
| `pulses(pin, count, period_s, period_rel)` | Number of rising edges and the period. `period_rel` is relative |
| `i2c(scl, sda, transactions, freq_hz, freq_rel, released)` | Sequence of transactions (address, read/write, bytes, ACK), SCL frequency, both High at the end. Each transaction may name `start` (`S`, `Sr`) and `end` (`P`, `Sr`), compared when given |
| `spi(clk, mosi, miso, cs, mode, mosi_bytes, miso_bytes, freq_hz, freq_rel, bit_order, cs_active, bits, notices)` | Mode, bytes in both directions, SCK frequency, CS High at the end. The mode is read from the lines (§4.5). With `cs`: the clocks in each CS window (`frame_bits`; `bits` compares them), and no SCK level inside a window shorter than half the median one (a glitch); SCK moving while CS is inactive and a window with no clock are notices (§4.9) |
| `uart(pin, baud, data, baud_rel, idle, bits, parity, stop, errors_max)` | Baud rate, byte sequence, idle level, framing and parity errors (below) |
| `voltage(pin, level_v, level_abs_v, level_min_v, level_max_v, ripple_max_v)` | Voltage of an analog channel. Mean is `level_v` ± `level_abs_v`, all samples within `level_min_v` to `level_max_v`, peak-to-peak at most `ripple_max_v`. Only the given ones are checked |
| `phase(a, b, phase_deg, phase_abs_deg, freq_hz, freq_rel, edge)` | Phase of two periodic lines: from an edge of a to b's next edge of the same direction, as an angle of a's period (0–360 degrees). Every pair within `phase_deg` ± `phase_abs_deg` (degrees, wrapping near 0), both frequencies within `freq_rel` (relative) of `freq_hz` |
| `skew(a, b, delay_abs_s, delay_s, edge)` | Skew of two lines that change together (patterns, parallel outputs): every edge of a has b's edge of the same direction at `delay_s` ± `delay_abs_s`. An edge without a partner is NG. With `edge` `"rise"` / `"fall"` only those (two outputs of one timer at different duties) |
| `dead_time(hi, lo, dead_min_s, dead_max_s, active, allow_overlap)` | The two outputs of a half bridge (on at `active`): from one turning off to the other turning on, every time within `dead_min_s`–`dead_max_s`. Both on (shoot-through) is NG unless `allow_overlap` |
| `i2c_timing(scl, sda, mode, limits)` | UM10204's individual times (§4.4) |
| `rgb_led(pin, data, t0h_min_s, t0h_max_s, t1h_min_s, t1h_max_s, period_min_s, period_max_s, reset_min_s)` | A WS2812-like line (§4.6): the bytes of each frame between resets (hex, in the order sent), T0H, T1H, the bit period, the reset's length |
| `onewire(pin, data, presence, speed)` | A 1-Wire line (§4.6): the bytes after each reset, presence pulses, the reset, presence, slot and low times |
| `ir(pin, frames, carrier_hz, carrier_rel, timing_rel, protocol)` | An IR remote line (§4.6): NEC frames (address, command, repeat), the command with its inverse, the carrier's frequency, the marks' and spaces' lengths |
| `lin(pin, baud, frames, baud_rel, break_min_bits, checksum)` | LIN frames (§4.7): identifier and data, sync, identifier parity, checksum, break and delimiter lengths, the baud measured from the sync field |
| `i2s(bclk, ws, sd, format, bits, fs_hz, fs_rel, samples, slot_bits, left_ws)` | I2S-like audio (§4.7): format, sample rate, BCLKs a channel, a run of samples found in a row |
| `quadrature(a, b, steps, cycles, direction, errors_max)` | A quadrature encoder (§4.7): the count (steps or cycles), the direction, changes of both lines at once |
| `sd_spi(clk, mosi, miso, cs, commands, mode, cs_active, ncr_max_bytes)` | An SD card in SPI mode (§4.8): the commands in order (index, argument, R1, R7 / R3 / R2, data blocks), answers within NCR, CMD0's and CMD8's CRC7, the card's blocks' CRC16 |

Anything that could not be checked, for example because a pin used by the check is not in the capture, is unchecked (`unchecked`).

### 4.1 I2C transactions that stopped midway

The last transaction, when the window ends without a STOP, has `"complete": False`, and holds the bits clocked after the last complete byte in `pending_bits`. If the expectation does not state `complete`, it is compared as complete; `pending_bits` is compared only when stated. A transaction that stopped in the middle of the address has `addr` set to `None`. This is for confirming "where it stopped" in fault-reproduction tests.

```python
# Read 1 byte from 0x42; after the ACK the target keeps holding SDA
i2c("PC16", "PC17", [{"addr": 0x42, "rw": "read", "bytes": [0x00], "complete": False}], released=False)
```

### 4.2 UART

- Pass to `baud` not the nominal value but the value the transmitter should actually produce (for CH32, F_CPU / BRR).
- The format is `bits` (number of data bits), `parity` (`"none"` / `"even"` / `"odd"`), `stop` (1, 1.5, 2).
- A window that starts or ends in the middle of a character is not an error (reading starts after at least one character's worth of idle; what comes before is counted as `lead_in`, and a character cut at the end as `cut_at_end`).
- If `errors_max` is given, the result is NG when the total of framing and parity errors exceeds it (if not given, they are only counted and reported).
- The baud rate is found by a least-squares fit of the bit width from the intervals between same-direction edges within one character. This method cancels the difference between rising and falling delays. The expected value is used only to choose among the candidates derived from the edges. It is reported when there are 8 or more samples per bit.
- To **measure only**, pass `None` to `baud`. If there are no other expectations (`data`, `errors_max`), the result is `measured`, with the measured baud rate, character count and error count. Use this to record what the DUT outputs for out-of-range settings.
- If every error character has edges clearly more shifted than the other characters, the NG reason says "suspect the capture's time base" (for example when a software-paced sampler stalled). For captures where the probe itself reported a delay (`time_base_slipped`), the NG reason also says so.

```python
uart("PA2", 8_000_000 / 9, baud_rel=0.015, errors_max=0)                  # 8N1
uart("PA2", 8_000_000 / 9, baud_rel=0.015, parity="even", errors_max=0)   # 8E1
uart("PB0", None)                                                         # measure only
```

### 4.3 Applying logic checks to analog lines (`threshold=`)

When given `threshold=`, the logic checks (`square`, `level`, `starts` / `ends`, `pulses`, `i2c`, `spi`, `uart`, `phase`, `skew`, `dead_time`, `i2c_timing`, `rgb_led`, `onewire`, `ir`, `lin`, `i2s`, `quadrature`, `sd_spi`) read an analog channel as logic at that voltage.

- Either a single value (e.g. `threshold=1.65`) or a hysteresis pair (e.g. `threshold=(1.0, 2.3)`: high on reaching 2.3 V, low on falling to 1.0 V). With a pair, a slow noisy edge becomes a single edge.
- Passing an analog channel to a logic check without `threshold=` makes it unchecked. A channel whose voltage conversion is not in the file is also unchecked.

### 4.4 Time limits and resolution

`phase`, `skew`, `dead_time`, `i2c_timing`, `rgb_led`, `onewire`, `ir` and `lin` compare times between edges with limits. An edge changed somewhere in the sample (the channel's step) before its tick, so a time between edges of two lines is uncertain by the larger of their steps (the resolution).

- When the measured value ± the resolution is all within the limit: OK; all outside: NG; straddling it: unchecked (`near` in the reason). Capture again at a faster rate.
- A limit applies to every instance; the worst decides, and the reason gives its value and time.
- With nothing expected, the check only measures (`measured`).

`i2c_timing` picks the limits of UM10204 (rev. 7) table 10 by `mode` (`"standard"`, `"fast"`, `"fast-plus"`). `limits` replaces one side of a limit by name: `t_low_min_s`, `t_vd_dat_max_s`, `f_scl_max_hz` (with `mode=None` only `limits`).

| Name | Measures | standard / fast / fast-plus |
| --- | --- | --- |
| `f_scl` | SCL frequency (rise to rise inside a transaction; the shortest period) | ≤ 100 / 400 / 1000 kHz |
| `t_low` | SCL low | ≥ 4.7 / 1.3 / 0.5 µs |
| `t_high` | SCL high (those holding no START or STOP) | ≥ 4.0 / 0.6 / 0.26 µs |
| `t_hd_sta` | START (and repeated START) to SCL falling | ≥ 4.0 / 0.6 / 0.26 µs |
| `t_su_sta` | before a repeated START, from SCL rising | ≥ 4.7 / 0.6 / 0.26 µs |
| `t_hd_dat` | SCL falling to SDA's first change | ≥ 0 |
| `t_vd_dat` | SCL falling to SDA's last change in that low (data bits only: the low before a high holding a START or STOP sets SDA up for the condition and is not counted) | ≤ 3.45 / 0.9 / 0.45 µs |
| `t_su_dat` | SDA's last change to SCL rising | ≥ 250 / 100 / 50 ns |
| `t_su_sto` | before a STOP, from SCL rising | ≥ 4.0 / 0.6 / 0.26 µs |
| `t_buf` | STOP to the next START | ≥ 4.7 / 1.3 / 0.5 µs |

- Times are taken at the line's threshold, so on a slow edge they differ a little from the standard's 30 % / 70 % points. Rise and fall times (tr, tf) are not measured (look at an analog channel).
- An SDA change with SCL high on both sides is a START or STOP; with SCL low on either side it counts as a data change.
- When the target drives the ACK, `t_vd_dat` has the same limit (tVD;ACK).

```python
i2c_timing("PC2", "PC1", "fast")
i2c_timing("PC2", "PC1", "standard", limits={"t_su_dat_min_s": 300e-9})
```

### 4.5 SPI mode

`spi` reads the mode from the lines themselves. CPOL is SCK's level when CS becomes active (without CS, its level over the longest pause). CPHA comes from the SCK edge before each change of MOSI (else MISO): after a leading edge (leaving the idle level) the bit was put out there and is sampled on the trailing one, CPHA 1; after a trailing edge, CPHA 0. Changes between CS becoming active and the first edge count only when there are no others (in either mode a device may put its first bit out on CS). The decoder's choice stays as `decoded_mode`. For an active-high CS give `cs_active=1`.

### 4.6 One-line protocols

- **`rgb_led`**: the line idles low; a bit is a high of T0H or T1H and the low after it, MSB first. A low longer than 5 bit periods ends a frame. 0 and 1 are split at the widest gap between the highs' widths (when all are alike, a high shorter than half the period is a 0). The default limits are WS2812B's datasheet: T0H 0.4 µs, T1H 0.8 µs (± 150 ns), period 1.25 µs (± 600 ns), reset at least 50 µs. For other LEDs (SK6812, WS2811's slow mode, newer WS2812B's 280 µs reset) pass their values. When the window ends after the last frame before a reset's length, whether the LEDs took it is not known: unchecked.
- **`onewire`**: the line idles high; every pulse is low. A low of at least `speed`'s `reset_detect` (standard 288 µs, overdrive 48 µs) is a reset, a low starting in the presence window after it the presence pulse, every other low a slot: 1 when its low is shorter than `split` (standard 15 µs, overdrive 2 µs), else 0, LSB first. Read slots read as the bits they carried (who pulled the line low is not on the line). The limits are Maxim AN126's and DS18B20's: at standard speed a reset of at least 480 µs, presence wait 15–60 µs, presence 60–240 µs, slot (low start to the next) at least 61 µs, high between slots at least 1 µs, a 0's low at most 120 µs, a 1's low 1–15 µs.
- **`ir`**: reads the line driving the LED (with a carrier, idling low) and a receiver module's output (idling high, marks low). When the active pulses are mostly shorter than 100 µs the line carries a carrier: pulses closer than 150 µs make one mark, and the carrier's frequency and duty are measured. NEC: a 9 ms mark, then 4.5 ms for data (32 bits, LSB first: address, inverted address or the address's high byte, command, inverted command) or 2.25 ms for a repeat. An address that does not match its inverse is a 16-bit extended address in `addr`. A frame whose command does not match its inverse is NG. With `timing_rel` (relative, default 0.1) every mark and space is held against NEC's lengths (a receiver module changes mark widths: widen it for its output).

```python
rgb_led("PA7", ["ff0000", "00ff00"])                              # two writes of one LED each
onewire("PD2", ["cc44", "ccbe"])                                  # Skip ROM + Convert T, Skip ROM + Read Scratchpad
ir("PB1", [{"addr": 0x04, "cmd": 0x08}, {"repeat": True}], carrier_hz=38e3)
```

### 4.7 LIN, I2S, quadrature encoders

- **`lin`**: the line idles high. A low of at least 11 bits at `baud` is a break, the high after it the delimiter. The characters (8N1) up to the next break or a pause longer than 2 characters are sync, protected identifier, data and checksum (the last). Each frame's bit time is measured from its sync field (0x55: its five falling edges span 8 bits), as a slave synchronizes, and its characters are read at that rate. A break must be at least `break_min_bits` (default 13, LIN 2.x's master minimum), a delimiter at least one bit. With `checksum="auto"` the diagnostic ids 0x3C / 0x3D take the classic checksum, the others the enhanced one.
- **`i2s`**: SD and WS are read on BCLK's rising edges; the rising edges until WS changes are one channel (a slot). `format` `"i2s"` (Philips) puts the MSB one BCLK after WS changes, `"left"` at the change, `"right"` ends the word at the slot's last bit (`bits` needed). Words are two's complement, MSB first. The left channel is WS low in `"i2s"`, high otherwise (`left_ws` for others). `samples` is OK when found in a row among the frames read (the window starts mid-stream). The share of WS and SD changes made while BCLK is low is reported (not judged).
- **`quadrature`**: +1 for each edge with A changing ahead of B (`steps`; `cycles` is that over 4). Both lines changing on one tick is counted as an error: a state skipped (sampled too slowly for the encoder, or a fault).

```python
lin("PA2", 19200, [{"id": 0x10, "data": "01020304"}])
i2s("PB13", "PB12", "PB15", "i2s", 16, fs_hz=48000, samples=[(0, 0), (1000, -1000)])
quadrature("PA0", "PA1", cycles=24, direction="forward")
```

### 4.8 SD cards in SPI mode

`sd_spi` reads the SPI bytes inside CS windows (mode 0 by default; `mode=None` reads it from the lines) as an SD host (the DUT) talking to a card.

- A command is 6 bytes on MOSI (`01` and the index, a 32-bit argument, CRC7 and the end bit); its R1 is the first byte on MISO with bit 7 clear within 8 bytes after it (NCR). CMD8 and CMD58 have 4 more bytes (R7, R3), CMD13 one (R2), CMD12 / 28 / 29 / 38 busy (0x00) bytes. The command after CMD55 is an ACMD.
- After a read (CMD17, 18, 9, 10, ACMD13, 22, 51) the card's blocks are read (token 0xFE, data, CRC16), for CMD18 up to CMD12; their length is the one set by CMD16 (512 by default) or the command's own. After a write (CMD24, 25): the host's blocks (0xFE / 0xFC, data, CRC16), the card's data response (0x05: accepted) and busy, and the busy after 0xFD.
- `commands` is compared in order on the keys given (`cmd` or `acmd`, `arg`, `r1`, `resp`: R7 / R3 / R2 as hex, `data`: the blocks as hex).
- Every command must have its R1 within NCR and its end bit set, CMD0 and CMD8 a right CRC7 (the only ones SPI mode needs it on), the card's blocks a right CRC16. The CRC16 of blocks the host writes is often unused in SPI mode, so it is only reported.

```python
sd_spi("PA5", "PA7", "PA6", "PA4", [{"cmd": 0, "r1": 0x01}, {"cmd": 8, "arg": 0x1AA, "resp": "000001aa"},
                                     {"cmd": 55}, {"acmd": 41, "r1": 0x00}, {"cmd": 17, "r1": 0x00}])
```

`wireskein analyze` also looks for SD commands on SPI (trying MOSI and MISO both ways round; the lines idling at 0xFF around each command are the evidence). When found, they are in the SPI export's `sd`.

### 4.9 Notices

Some behaviour is allowed by the protocol but worth a look: a test may want to make it fail, or allow it a few times. The checks find it and say where, but it fails nothing by itself. What the protocol does not allow (a framing or parity error, a glitch on SCK inside a CS window, a byte cut short) is `ng`, not a notice.

- Each notice is one place: its `kind`, its `tick` and `part` in the capture, and the kind's values. They are in the result (`notices`), always, counted in the summary, listed by `wireskein verify -v`, and written into the capture's markers with `--mark`, so the viewer shows each one on the waveform.
- An expectation limits them in its own segment: `notices={"sck_outside_cs": 1}` allows one, `notices={"sck_outside_cs": "ng"}` none. A kind the check does not find is an error of the expectation (`ng`), so a misspelled name never passes silently.

| Check | Notice | What it is |
| --- | --- | --- |
| `spi` (with `cs`) | `sck_outside_cs` | SCK moved while CS was inactive (at the first edge of that stretch; `count`: its edges). Allowed on a bus shared with other devices; a reconfiguration's stray edges show here too |
| `spi` (with `cs`) | `empty_cs_window` | A window of CS active with no clock (at CS becoming active; `length_ticks`) |
| `uart` | `baud_near_limit` | The bit rate within `baud_rel` but more than half of it off (at the line's first edge; `value`: the error, `limit`) |



- **Include the oscillator error.** Running on an internal RC oscillator (such as the CH32 HSI), the frequency deviates by ±1–2 %. Separate these from tests running on a crystal, or include it in the tolerance.
- **Include the resolution set by the sample rate.** If one period is N samples, the duty cycle resolution is 1/N. To check duty cycle to 1 %, you need 100 or more samples per period. For frequency, measuring the average over many periods increases the resolution.
- **Rule of thumb:** to measure timing parameters, keep the signal frequency at 1/10 of the sample rate or less. SPI decoding needs about 5 samples per clock.
- **Put a self-test of the capture system first.** The probe itself outputs a known signal (such as a square wave of a fixed frequency) and measures it. This keeps sample rate or wiring errors from being mistaken for DUT bugs.

## 6. Check items per peripheral

| Peripheral | Measure in the steady-state segment | Confirm in quiet segments and at boundaries |
| --- | --- | --- |
| GPIO | Level, toggle timing | Other pins are idle |
| PWM | Frequency, duty cycle, per-period variation, polarity | The first pulse is not missing, no pulse is dropped or stretched on update, level after stopping |
| tone() | Frequency, duration | Level after it ends |
| UART | Baud rate error, data bits, parity, stop bits, content | No garbage on initialization, idle level, pin state after `end()` |
| I2C | SCL frequency, START, STOP, Repeated Start, ACK and NACK, content | SDA and SCL both High after the end, SDA not stuck Low |
| SPI | Mode (CPOL/CPHA), bit order, clock frequency, content | CS High between transactions, no extra clocks |
| Power / analog | Mean voltage, range, ripple (`voltage`) | Voltage before and after switching |

WS2812 and tone() have no receiver, so checking the capture is the main verdict for them. I2C and SPI are judged both by value checks with a stub on the probe side (pair test) and by waveform checks on the capture.

I2C tLOW and tHIGH are checked by `i2c_timing` (§4.4), WS2812 bit widths by `rgb_led` (§4.6). There is no check of SPI CS timing (CS to the first clock, the last clock to CS).

## 7. Time accuracy

The mapping between the PC clock and the capture start time is accurate to ms (there is a delay before the start request reaches the probe). This is enough for assigning captures to segments, but not for measuring, in µs, the offset between a command and a transition inside the capture. Headings on the probe's clock (§3.1) are exact on the capture's clock, but the time from a command to its effect also holds the DUT's own delay. So a test checks what follows a change, in a steady segment after a gap, rather than the moment of the change.
