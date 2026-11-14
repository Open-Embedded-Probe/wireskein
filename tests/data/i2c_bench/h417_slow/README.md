# I2C through weak pull-ups: lines that do not read well

Taken by [oep] on 2026-11-15: CH32H417's Wire (PB11 SDA, PB10 SCL) addressing 0x13 with nothing there, at 100 kHz,
the lines pulled up by the ESP32-P4 probe's own pull-ups only (about 45 kOhm). What is on the wire, whatever the
capture shows: S, 0x13 write, NACK, P, and the device's `endTransmission()` returned 2 (an address NACK) every time.
Truth is that, not WireSkein's reading.

Through 45 kOhm a released line takes about 5 us to cross the threshold, as long as half of SCL: SDA's and SCL's
crossings race, and which comes first changes from one transaction to the next. The samples cannot show the order
the device saw; an external pull-up of a few kOhm is the cure, which this bench does not have.

- `noise/<tag>-<rate>-g<glitch_ns>/run<k>.wireskein`: one transaction each, 150 us from SDA's fall, 10 runs per
  setting. `nohys` the probe's pads without hysteresis, `hys` with it; rates 20, 50 and 100 MS/s; `g` the probe's
  glitch filter in ns (0: none). Some runs are missing where the capture failed.
- `stream/<tag>-<rate>-g<glitch_ns>.wireskein`: 50 transactions in one stream each, with `summary-<tag>.json` (the
  device's answers counted beside what oep decoded). `fexp` the pin's own glitch filter (fixed 25 ns), `fexp2` the
  flex filter at half its window's threshold. `pin`: `g0` no filter, `g25` the pin's filter, `g50` the pin's filter
  and the pads' hysteresis, `g1000` the hysteresis alone (the `g` numbers there are codes, not ns). `lk1`: after a
  restart, the flex filter at 100 ns, then none. `fhys`: the flex filter at 100 ns with hysteresis, then without,
  then none (`g1025` the first).

What they show:

- Through 45 kOhm a controller's clock can be clipped to a moment: it counts its high from releasing SCL, the line
  crosses the threshold for 20-40 ns, or for less than a sample and not at all in the capture, and the devices take
  it as a clock. Read with such clocks kept and the missing ones restored where one reading is left, the
  transactions match the waveform they must make (`eval/i2c_vs_truth.py`) in 1172 of 1200 (`impl-*`, `pin-*`,
  `nohys-*-g0`, `hys-*-g0`, 20-100 MS/s); as sampled, 0 to 38 of each 50.
- `impl-*`: the firmware after oep-spec 4ab3327 (the pin's fixed 25 ns filter, input hysteresis on by default):
  `g0` default, `h0` hysteresis off, `g25` the pin's filter. Hysteresis brings SCL's changes from 1108-1142 to
  1008-1014 for 1000 clock edges; read, 48 to 50 of 50 either way.
- With the P4's flex glitch filter (25-200 ns), 0 to 11 of 50: on these slow, noisy edges the filter locks up, the
  line changing no more from some point (SCL silent from 0.29 s at 100 ns, SDA going on), or oscillates (SCL 1.86
  million changes), with the pads' hysteresis too; on clean edges it lets a short pulse through and recovers.
  WireSkein tells both (`reading.stopped`, `reading.oscillating`, naming the filter). The pin's fixed filter
  (`fexp-50000000-g25`, `pin-*-g25`) does neither: 39 of 50 right.

tests/test_i2c_bench.py reads a few of them; the directory is left out of the sdist.
