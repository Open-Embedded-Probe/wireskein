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
            capture at 150 MS/s at least, 250 MS/s best
       10 MHz (high 20 ns, low 80 ns): 427 frames, ticks 6162091..8073310, too fast
       0.325 MHz (high 1020 ns, low 2060 ns): 404 frames, ticks 2978224..10420978
     noise: 3554 glitches on the clock, 1403 ringing pulses on SWDIO (...)
   ```

   The same is in the document: each bus's `reading`, `clock_speeds` and `undersampled`
   ([analysis-format](analysis-format.md) §3.1); each frame carries its clock (`period_ns`, `high_ns`).
2. **`speed:`** — capture again faster (§2): up to the probe's limit, at least the rate given.
3. **`noise:`** — look at the wiring (§3). The analysis reads past what it can (§4), but a cleaner capture reads
   more, and surely.
4. **Still short of the rate** (the probe's limit is below it): what can be read is read, frames a clock short only
   where that leaves one reading (`restored`); the rest stays unread, never guessed.

## 2. Speed

A clock's high or low that lasts less than a sample may fall between two samples: that clock is lost, and its frame
is a clock short (an RVSWD frame of 52 clocks). What counts is the **shorter of the clock's high and low where it runs
fastest**, not its frequency alone: a debugger's clock is often far from even (a WCH-LinkE at 10 MHz: high 20-25 ns,
low 75-80 ns).

| Samples in the shorter half | |
| --- | --- |
| under 3 | Clocks lost (`rate_ok` false; `least_rate_hz` is 3) |
| 3 | None lost: the least |
| 5 | Edges timed to a fifth of a half (about 10 samples a period of an even clock): the best (`best_rate_hz`) |

Measured on a WCH-LinkE at its fastest (highs of 20-25 ns): at 50 MS/s a third of its frames lost a clock, at
100 MS/s some still did, at 160 MS/s (4 samples) none.

- **A debugger changes speed on the way.** A LinkE connects at 0.33 MHz and reads at 10 MHz: the start may read well
  and the rest not. `clock_speeds` gives each speed with where its frames are.
- **Rate the probe up.** Many probes go faster with fewer channels: take only the bus's lines (and a ground).
- **A clock or a fast line at every sample.** A reduced mode (`sample`, `any_high`, ... at D) is for slow lines
  beside a fast bus, never for its clock ([wireskein-format](wireskein-format.md) §4.5).
- **A long capture at a high rate** is no problem for the analysis ([long captures](long-captures.md)).

## 3. Noise

Noise is pulses that are no signal: a sample or two on a line, at edges or anywhere. The analysis counts two kinds
(`reading`), where they cannot be signal:

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
  threshold may cross it more than once. Pull it up or down; a stronger pull-up for a faster rise. A line nothing to do
  with the bus: leave it out ([long captures](long-captures.md) §5).
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
| Activity no frame reads | — | `uninterpreted` |

On 4 KiB flashed by a LinkE at 50 MS/s, 174 of 3026 frames a clock short read so, each one a 160 MS/s capture of the
same reads gives too; restoring at the most likely place alone misread 15 % of them. Capture fast enough instead.
