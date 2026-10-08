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
        want = ([square("PA1", 1000, duty / 255, tol_freq=0.02, tol_duty=0.01)] if 0 < duty < 255
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
- Write the expectation and the capture `names` with DUT pin names (`PA1` etc.). Keep the mapping between probe channel numbers and DUT pin names in one place on the test side.

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

## 4. Checks

| Check | Description |
| --- | --- |
| `square(pin, freq_hz, duty, tol_freq, tol_duty, max_jitter)` | A regular square wave. Tolerance is relative for frequency and absolute for duty cycle. `max_jitter` is the upper limit of period variation (relative). The resolution from the sample rate is added automatically |
| `level(pin, value)` | Constant throughout the window |
| `starts({pin: v})` / `ends({pin: v})` | Level at the first / last sample of the window |
| `only_moving([pins])` | No other captured pin moves. Looks only at logic channels |
| `pulses(pin, count, period_s, tol_period)` | Number of rising edges and the period. `tol_period` is relative |
| `i2c(scl, sda, transactions, hz, tol_hz, released)` | Sequence of transactions (address, read/write, bytes, ACK), SCL frequency, both High at the end |
| `spi(clk, mosi, miso, cs, mode, mosi_bytes, miso_bytes, hz, tol_hz, bit_order)` | Mode, bytes in both directions, SCK frequency, CS High at the end |
| `uart(pin, baud, data, tol_baud, idle, bits, parity, stop, max_errors)` | Baud rate, byte sequence, idle level, framing and parity errors (below) |
| `voltage(pin, volts, tol_v, min_v, max_v, ripple)` | Voltage of an analog channel. Mean is `volts` ± `tol_v` (V), all samples within `min_v` to `max_v`, peak-to-peak at most `ripple`. Only the given ones are checked |

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
- If `max_errors` is given, the result is NG when the total of framing and parity errors exceeds it (if not given, they are only counted and reported).
- The baud rate is found by a least-squares fit of the bit width from the intervals between same-direction edges within one character. This method cancels the difference between rising and falling delays. The expected value is used only to choose among the candidates derived from the edges. It is reported when there are 8 or more samples per bit.
- To **measure only**, pass `None` to `baud`. If there are no other expectations (`data`, `max_errors`), the result is `measured`, with the measured baud rate, character count and error count. Use this to record what the DUT outputs for out-of-range settings.
- If every error character has edges clearly more shifted than the other characters, the NG reason says "suspect the capture's time base" (for example when a software-paced sampler stalled). For captures where the probe itself reported a delay (`time_base_slipped`), the NG reason also says so.

```python
uart("PA2", 8_000_000 / 9, tol_baud=0.015, max_errors=0)                  # 8N1
uart("PA2", 8_000_000 / 9, tol_baud=0.015, parity="even", max_errors=0)   # 8E1
uart("PB0", None)                                                         # measure only
```

### 4.3 Applying logic checks to analog lines (`threshold=`)

When given `threshold=`, `square`, `level`, `starts` / `ends`, `pulses`, `i2c`, `spi` and `uart` read an analog channel as logic at that voltage.

- Either a single value (e.g. `threshold=1.65`) or a hysteresis pair (e.g. `threshold=(1.0, 2.3)`: high on reaching 2.3 V, low on falling to 1.0 V). With a pair, a slow noisy edge becomes a single edge.
- Passing an analog channel to a logic check without `threshold=` makes it unchecked. A channel whose voltage conversion is not in the file is also unchecked.

## 5. Choosing tolerances

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

There are no checks yet for I2C tLOW and tHIGH, SPI CS timing, or WS2812 bit widths.

## 7. Time accuracy

The mapping between the PC clock and the capture start time is accurate to ms (there is a delay before the start request reaches the probe). This is enough for assigning captures to segments, but not for measuring, in µs, the offset between a command and a transition inside the capture. If you need that accuracy, have the DUT or the probe toggle a marker pin and include it in the capture.
