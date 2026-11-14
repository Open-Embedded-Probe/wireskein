# I2C on the bench: lines that bounce

Recorded runs of ArduinoCore-CH32RV's `tests/wire` (2026-11-14), taken by an ESP32-P4 OEP probe at 100 MS/s with
the probe's 45 kΩ pull-ups on the lines. Each directory is a run as pytest-embedded-wireskein wrote it (`run.json`
and its capture). The expectations are the test's own: what the controller was told to send (an address with no
target answers NACK).

On these boards the lines rise slowly and the edges bounce: 10-20 ns pulses at the edges (X035, V205), 10-190 ns on
6-2 (H417), and a slow rise crossing the threshold more than once. The devices read their buses right; read as the
samples are, the captures did not (an address's NACK read as ACK, 0x13 read as 0x02, the address after an Sr read
as 0x40). Read as an I2C input sees the lines (spikes shorter than t_SP left out, a slow edge's crossings as one:
`timing.i2c_input`), they read as the devices did.

| Directory | Board | SCL | Read now |
| --- | --- | --- | --- |
| `x035_100k` | CH32X035 (PA11 SDA, PA10 SCL) | 100 kHz | yes |
| `x035_12k5` | CH32X035 | 12.5 kHz | yes |
| `v205_contract` | CH32V205 (PB9 SDA, PB8 SCL) | 100 kHz | yes |
| `h417_a` | CH32H417 (PB11 SDA, PB10 SCL) | 12.5 kHz | yes |
| `h417_b` | CH32H417 | 12.5 kHz | yes |

`h417_b` was kept as data that did not read. In "address 0x13, no target", SCL shows a high of 30 ns where a clock
was, then a low for 40 us: a controller counts its high from releasing SCL, so through a slow rise the line is high
for a moment only, and it holds SCL low while it waits for the line. One more clock of that transaction is shorter
than a sample and not in the capture at all. Read with such clocks kept, and the missing one restored where one
reading is left (`clock_restored`), it reads as the device sent it.

`h417_slow/` holds many more captures of the same board's I2C through the probe's weak pull-ups alone, with and
without the probe's glitch filter, and its own README.
