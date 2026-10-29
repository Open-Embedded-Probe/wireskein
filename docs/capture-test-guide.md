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
| `spi(clk, mosi, miso, cs, mode, mosi_bytes, miso_bytes, hz, tol_hz, bit_order, cs_active)` | Mode, bytes in both directions, SCK frequency, CS High at the end. The mode is read from the lines (§4.5) |
| `uart(pin, baud, data, tol_baud, idle, bits, parity, stop, max_errors)` | Baud rate, byte sequence, idle level, framing and parity errors (below) |
| `voltage(pin, volts, tol_v, min_v, max_v, ripple)` | Voltage of an analog channel. Mean is `volts` ± `tol_v` (V), all samples within `min_v` to `max_v`, peak-to-peak at most `ripple`. Only the given ones are checked |
| `phase(a, b, phase_deg, tol_deg, freq_hz, tol_freq, edge)` | Phase of two periodic lines: from an edge of a to b's next edge of the same direction, as an angle of a's period (0–360 degrees). Every pair within `phase_deg` ± `tol_deg` (degrees, wrapping near 0), both frequencies within `tol_freq` (relative) of `freq_hz` |
| `skew(a, b, tol_s, delay_s, edge)` | Skew of two lines that change together (patterns, parallel outputs): every edge of a has b's edge of the same direction at `delay_s` ± `tol_s`. An edge without a partner is NG. With `edge` `"rise"` / `"fall"` only those (two outputs of one timer at different duties) |
| `dead_time(hi, lo, min_s, max_s, active, allow_overlap)` | The two outputs of a half bridge (on at `active`): from one turning off to the other turning on, every time within `min_s`–`max_s`. Both on (shoot-through) is NG unless `allow_overlap` |
| `i2c_timing(scl, sda, mode, limits)` | UM10204's individual times (§4.4) |
| `rgb_led(pin, data, t0h, t1h, period, reset_min_s)` | A WS2812-like line (§4.6): the bytes of each frame between resets (hex, in the order sent), T0H, T1H, the bit period, the reset's length |
| `onewire(pin, data, presence, speed)` | A 1-Wire line (§4.6): the bytes after each reset, presence pulses, the reset, presence, slot and low times |
| `ir(pin, frames, carrier_hz, tol_carrier, tol_time, protocol)` | An IR remote line (§4.6): NEC frames (address, command, repeat), the command with its inverse, the carrier's frequency, the marks' and spaces' lengths |
| `lin(pin, baud, frames, tol_baud, min_break_bits, checksum)` | LIN frames (§4.7): identifier and data, sync, identifier parity, checksum, break and delimiter lengths, the baud measured from the sync field |
| `i2s(bclk, ws, sd, format, bits, fs_hz, tol_fs, samples, slot_bits, left_ws)` | I2S-like audio (§4.7): format, sample rate, BCLKs a channel, a run of samples found in a row |
| `quadrature(a, b, steps, cycles, direction, max_errors)` | A quadrature encoder (§4.7): the count (steps or cycles), the direction, changes of both lines at once |
| `sd_spi(clk, mosi, miso, cs, commands, mode, cs_active, max_ncr)` | An SD card in SPI mode (§4.8): the commands in order (index, argument, R1, R7 / R3 / R2, data blocks), answers within NCR, CMD0's and CMD8's CRC7, the card's blocks' CRC16 |

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

When given `threshold=`, the logic checks (`square`, `level`, `starts` / `ends`, `pulses`, `i2c`, `spi`, `uart`, `phase`, `skew`, `dead_time`, `i2c_timing`, `rgb_led`, `onewire`, `ir`, `lin`, `i2s`, `quadrature`, `sd_spi`) read an analog channel as logic at that voltage.

- Either a single value (e.g. `threshold=1.65`) or a hysteresis pair (e.g. `threshold=(1.0, 2.3)`: high on reaching 2.3 V, low on falling to 1.0 V). With a pair, a slow noisy edge becomes a single edge.
- Passing an analog channel to a logic check without `threshold=` makes it unchecked. A channel whose voltage conversion is not in the file is also unchecked.

### 4.4 Time limits and resolution

`phase`, `skew`, `dead_time`, `i2c_timing`, `rgb_led`, `onewire`, `ir` and `lin` compare times between edges with limits. An edge changed somewhere in the sample (the channel's step) before its tick, so a time between edges of two lines is uncertain by the larger of their steps (the resolution).

- When the measured value ± the resolution is all within the limit: OK; all outside: NG; straddling it: unchecked (`near` in the reason). Capture again at a faster rate.
- A limit applies to every instance; the worst decides, and the reason gives its value and time.
- With nothing expected, the check only measures (`measured`).

`i2c_timing` picks the limits of UM10204 (rev. 7) table 10 by `mode` (`"standard"`, `"fast"`, `"fast-plus"`). `limits` replaces them by name (one value: a minimum; a pair: `(min, max)`; with `mode=None` only `limits`).

| Name | Measures | standard / fast / fast-plus |
| --- | --- | --- |
| `f_scl` | SCL frequency (rise to rise inside a transaction; the shortest period) | ≤ 100 / 400 / 1000 kHz |
| `t_low` | SCL low | ≥ 4.7 / 1.3 / 0.5 µs |
| `t_high` | SCL high (those holding no START or STOP) | ≥ 4.0 / 0.6 / 0.26 µs |
| `t_hd_sta` | START (and repeated START) to SCL falling | ≥ 4.0 / 0.6 / 0.26 µs |
| `t_su_sta` | before a repeated START, from SCL rising | ≥ 4.7 / 0.6 / 0.26 µs |
| `t_hd_dat` | SCL falling to SDA's first change | ≥ 0 |
| `t_vd_dat` | SCL falling to SDA's last change in that low | ≤ 3.45 / 0.9 / 0.45 µs |
| `t_su_dat` | SDA's last change to SCL rising | ≥ 250 / 100 / 50 ns |
| `t_su_sto` | before a STOP, from SCL rising | ≥ 4.0 / 0.6 / 0.26 µs |
| `t_buf` | STOP to the next START | ≥ 4.7 / 1.3 / 0.5 µs |

- Times are taken at the line's threshold, so on a slow edge they differ a little from the standard's 30 % / 70 % points. Rise and fall times (tr, tf) are not measured (look at an analog channel).
- An SDA change with SCL high on both sides is a START or STOP; with SCL low on either side it counts as a data change.
- When the target drives the ACK, `t_vd_dat` has the same limit (tVD;ACK).

```python
i2c_timing("PC2", "PC1", "fast")
i2c_timing("PC2", "PC1", "standard", limits={"t_su_dat": 300e-9})
```

### 4.5 SPI mode

`spi` reads the mode from the lines themselves. CPOL is SCK's level when CS becomes active (without CS, its level over the longest pause). CPHA comes from the SCK edge before each change of MOSI (else MISO): after a leading edge (leaving the idle level) the bit was put out there and is sampled on the trailing one, CPHA 1; after a trailing edge, CPHA 0. Changes between CS becoming active and the first edge count only when there are no others (in either mode a device may put its first bit out on CS). The decoder's choice stays as `decoded_mode`. For an active-high CS give `cs_active=1`.

### 4.6 One-line protocols

- **`rgb_led`**: the line idles low; a bit is a high of T0H or T1H and the low after it, MSB first. A low longer than 5 bit periods ends a frame. 0 and 1 are split at the widest gap between the highs' widths (when all are alike, a high shorter than half the period is a 0). The default limits are WS2812B's datasheet: T0H 0.4 µs, T1H 0.8 µs (± 150 ns), period 1.25 µs (± 600 ns), reset at least 50 µs. For other LEDs (SK6812, WS2811's slow mode, newer WS2812B's 280 µs reset) pass their values. When the window ends after the last frame before a reset's length, whether the LEDs took it is not known: unchecked.
- **`onewire`**: the line idles high; every pulse is low. A low of at least `speed`'s `reset_detect` (standard 288 µs, overdrive 48 µs) is a reset, a low starting in the presence window after it the presence pulse, every other low a slot: 1 when its low is shorter than `split` (standard 15 µs, overdrive 2 µs), else 0, LSB first. Read slots read as the bits they carried (who pulled the line low is not on the line). The limits are Maxim AN126's and DS18B20's: at standard speed a reset of at least 480 µs, presence wait 15–60 µs, presence 60–240 µs, slot (low start to the next) at least 61 µs, high between slots at least 1 µs, a 0's low at most 120 µs, a 1's low 1–15 µs.
- **`ir`**: reads the line driving the LED (with a carrier, idling low) and a receiver module's output (idling high, marks low). When the active pulses are mostly shorter than 100 µs the line carries a carrier: pulses closer than 150 µs make one mark, and the carrier's frequency and duty are measured. NEC: a 9 ms mark, then 4.5 ms for data (32 bits, LSB first: address, inverted address or the address's high byte, command, inverted command) or 2.25 ms for a repeat. An address that does not match its inverse is a 16-bit extended address in `addr`. A frame whose command does not match its inverse is NG. With `tol_time` (relative, default 0.1) every mark and space is held against NEC's lengths (a receiver module changes mark widths: widen it for its output).

```python
rgb_led("PA7", ["ff0000", "00ff00"])                              # two writes of one LED each
onewire("PD2", ["cc44", "ccbe"])                                  # Skip ROM + Convert T, Skip ROM + Read Scratchpad
ir("PB1", [{"addr": 0x04, "cmd": 0x08}, {"repeat": True}], carrier_hz=38e3)
```

### 4.7 LIN, I2S, quadrature encoders

- **`lin`**: the line idles high. A low of at least 11 bits at `baud` is a break, the high after it the delimiter. The characters (8N1) up to the next break or a pause longer than 2 characters are sync, protected identifier, data and checksum (the last). Each frame's bit time is measured from its sync field (0x55: its five falling edges span 8 bits), as a slave synchronizes, and its characters are read at that rate. A break must be at least `min_break_bits` (default 13, LIN 2.x's master minimum), a delimiter at least one bit. With `checksum="auto"` the diagnostic ids 0x3C / 0x3D take the classic checksum, the others the enhanced one.
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
