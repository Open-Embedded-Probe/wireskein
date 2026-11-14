# Capturing well

[日本語](capturing.ja.md)

What WireSkein reads is only as good as the capture. When a bus does not read well, the cause is nearly always one
of two: the sample rate is too low for the clock (§2), or the lines carry noise (§3). They call for different
remedies, and the analysis tells which one it is.

## 1. The flow

1. **Capture, then decode or analyze.** After each bus, `decode` and `analyze` print how it read, on stderr:

   ```
   rvswd(clk=SWCLK, dio=SWDIO): 1736 frames read, 1323 ok, 404 unanswered, 33 with a lost clock restored
     speed: 1110 frames at a clock too fast for 50 MS/s (period 100 ns, high 20 ns, low 80 ns): ...
            capture at 250 MS/s at least, 500 MS/s best
       10 MHz (high 20 ns, low 80 ns): 427 frames, ticks 6162091..8073310, too fast
       0.325 MHz (high 1020 ns, low 2060 ns): 404 frames, ticks 2978224..10420978
     noise: 1.8% of the clock's changes are glitches, 3.6% of SWDIO's are ringing (...)
   ```

   The same is in the document: each bus's `reading`, `clock_speeds` and `undersampled`
   ([analysis-format](analysis-format.md) §3.1); each frame carries its clock (`period_ns`, `high_ns`).
2. **`speed:`** — capture again faster (§2): up to the probe's limit, at least the rate given.
3. **`noise:`** — look at the wiring (§3). The analysis reads past what it can (§4), but a cleaner capture reads
   more, and surely.
4. **Still short of the rate** (the probe's limit is below it): what can be read is read, frames a clock short only
   where that leaves one reading (`restored`); the rest stays unread, never guessed.

## 2. Speed

What a rate is enough for depends on **T, the shortest length the question looks at**:

- decoding a bus: the clock's shorter half (high or low) where it runs fastest, or a bit. A debugger's clock is often
  far from even (a WCH-LinkE at 10 MHz: high 20-25 ns, low 75-80 ns), so it is the half, not the frequency.
- a time a check bounds (a setup, a hold, a delay, a pulse's width): that time, or its tolerance.
- spikes (the `i2c` check's `spike_max_s`, 50 ns): the shortest pulse to tell.

Count the samples in T. Measured on synthetic captures (`eval/rates.py`: 100 each at a random phase of the sample grid,
read through the checks a test uses):

| Samples in T | What holds |
| --- | --- |
| under 1 | A pulse of T may fall between two samples, or show a whole sample long: at half a sample, none of 100 was told right |
| 2 to 4 | Clean lines decode (I2C, SPI, UART: all 100 from 2). With edges jittering by T/20, readings fail: UART 70 of 100 at 2, 92 at 3, 99 at 4; I2C 57 at 2, 79 at 2.5. A time is sure to 1 / k of T: 48 % of T at 2, 25 % at 4 |
| **5: the least** | Every reading held, with the jitter too; a time is sure to 19 % of T. Checks answer from here (capture-test-guide §4.11) |
| **10: the norm** | A time to 9 % of T |
| 20: plenty | A time to 4.5 % of T |

So capture at **10 / T, at least 5 / T**; 20 / T is plenty. Several questions on one capture: the shortest T sets the
rate. A slower capture is still read as far as it goes, and every check still made; what was read under 5 samples
is told (the notice `under_rate`, with the rate to take), never passed silently. `decode` and `analyze` say the same of a clock (`speed:`, `undersampled` with
`least_rate_hz` and `best_rate_hz`).

Spikes need their own T. I2C at 100 kHz with three pulses of 20 ns on each line, read through the check's spike filter
(50 ns): at 5 to 20 MS/s 83 to 87 of 100 read right (a pulse seen a whole sample long is taken for the signal), at
40 MS/s and over all 100. The bus itself (halves of 5 us) needs only 1 MS/s; the spikes ask for 100 MS/s (5 / 50 ns).
A probe's glitch filter (`glitch_filter_ns`, the ESP32-P4's: the pin's fixed 25 ns) drops spikes shorter than its
width, and so hides them: spikes are told only above the filter's width. It does nothing for a slow edge's wobble.
For slow edges, the probe's input hysteresis (`input_hysteresis`, on by default where a probe has it,
`meta.probe.input_hysteresis`) is what helps: through 45 kOhm pull-ups it brought SCL's changes from 1108-1142 to
1008-1014 for 1000 clock edges. The analysis reads I2C right either way (below). Older ESP32-P4 firmware had a flex
glitch filter that locked up or oscillated on slow, noisy edges, leaving SCL silent from some point on or swinging a
million times; the analysis tells a line that stops or oscillates (`reading.stopped`, `oscillating`) in such
captures.

**I2C through weak pull-ups.** A released line takes long to rise through a weak pull-up (about 5 us through
45 kOhm), and a controller counts its high from releasing SCL, so a clock can be on the line for a moment only (20-40
ns), or shorter than a sample and not in the capture at all; the devices on the bus still take it as a clock. The
analysis keeps such a clipped clock (an SCL high between whole lows), and restores one that is not captured where
one reading is left (`clock_restored`, `restored` on the transaction). Checked against the waveform the transactions
must have made (`eval/i2c_vs_truth.py`, 1200 transactions of oep's CH32H417 captures at 20-100 MS/s, with and without
hysteresis and the pin's filter), 1172 read exactly right; as sampled, 0 to 38 of each 50 showed the right changes.
A stronger pull-up (a few kOhm) is still the cure for the bus itself. **Take slow I2C without a glitch filter**,
with hysteresis alone: a clock clipped to 20-40 ns is about as short as the filter's 25 ns, and a filter can drop it
(with the pin's filter, 48 and 49 of 50 read exactly right at 100 and 50 MS/s, against 50 and 50 without).

Measured on a WCH-LinkE at its fastest (highs of 20-25 ns): at 50 MS/s a third of its frames lost a clock, at
100 MS/s some still did, at 160 MS/s (3 to 4 samples) none, and that is still under the least: told.

- **A debugger changes speed on the way.** A LinkE connects at 0.33 MHz and reads at 10 MHz: the start may read well
  and the rest not. `clock_speeds` gives each speed with where its frames are.
- **Rate the probe up.** Many probes go faster with fewer channels: take only the bus's lines (and a ground).
- **A clock or a fast line at every sample.** A reduced mode (`sample`, `any_high`, ... at D) is for slow lines
  beside a fast bus, never for its clock ([wireskein-format](wireskein-format.md) §4.5).
- **A long capture at a high rate** is no problem for the analysis ([long captures](long-captures.md)).

## 3. Noise

Noise is pulses that are no signal: a sample or two on a line, at edges or anywhere. The analysis counts two kinds
(`reading`), where they cannot be signal, and judges by their share of the line's changes (`glitch_share`,
`ringing_share`; over 1 % is told as `noise:`), since a count grows with the capture's length:

| Seen | What | Usual causes |
| --- | --- | --- |
| `glitches` | The clock's pulses of a sample where the clock runs slow enough that no half of it is that short | A reflection; a neighbouring line's edge picked up; a ground far away |
| `ringing` | A data line's pulses under 3 samples at a clock slow enough that no bit is that short | A reflection at the edges; a target and a host both driving at a turnaround |

The usual causes, and what to do:

- **Ground far away.** Each signal's current returns through ground; a long ground lead makes a loop that turns every
  edge into a spike. Take ground **at the tap**, as close to the signals as can be: a ground wire beside each signal
  (or every other wire of a ribbon), not one ground clip a hand away.
- **A neighbouring line picked up (crosstalk).** Wires side by side couple: a fast data edge shows on the clock. Keep
  the clock apart from data, or a ground wire between them; shorter wires; twisted pairs (signal with ground).
- **Reflection.** A wire longer than its edge (a few cm for edges of 1 ns) reflects at an end it does not match, and the
  line rings after each edge. Tap near the receiver (the target's pin), keep wires short; a series resistor
  (22-100 ohms) at the driver damps it, where the design allows.
- **A floating or slow line.** An input left floating trembles; an open-drain line (I2C) rising slowly through the
  threshold may cross it more than once (told as `slow_edge`; a slow rise can also leave a whole high under the
  probe's threshold, which nothing reads back). Pull it up or down; a stronger pull-up for a faster rise. A line
  nothing to do with the bus: leave it out ([long captures](long-captures.md) §5).
- **The threshold.** A probe's logic threshold should sit midway in the signal's swing (1.8 V logic read with a 3.3 V
  threshold crosses it late and noisily).

## 4. What the analysis does

Nothing it does is a guess: each is told where it was done, and anything else is left unread.

| Seen | Done | Told |
| --- | --- | --- |
| Ringing on data at a slow clock | Read as it settles | `ringing` |
| Glitch clocks in a frame of a length no form has | Dropped when that gives a known length and the checks hold | `repaired` (the ticks) |
| A clock lost between samples (a frame a clock short) | Restored only where every likely place gives the same bits and they pass the checks | `restored` (the tick), `undersampled` |
| A clock too fast for the rate | — | `undersampled`, `clock_speeds` |
| I2C: a pulse shorter than 50 ns (t_SP), but an SCL high where a clock is due with whole lows around it (a clock a slow rise clipped short: kept); an edge crossing the threshold more than once within a third of SCL's half and 1000 ns (t_r; from 5 MS/s) | Left out; read as one at its last change, as an I2C input does (its spike filter, its hysteresis) | `params.deglitch: i2c_input`; the `i2c` check's `bus_glitch`, `slow_edge` |
| Activity no frame reads | — | `uninterpreted` |

On 4 KiB flashed by a LinkE at 50 MS/s, 174 of 3026 frames a clock short read so, each one a 160 MS/s capture of the
same reads gives too; restoring at the most likely place alone misread 15 % of them. Capture fast enough instead.
