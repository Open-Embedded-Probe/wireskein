# Evaluation corpus

[日本語](README.ja.md)

Records with ground truth, used to evaluate the inference and as regression tests. The evaluation scripts are in [research/](../research/README.md).

| Place | Git | Contents |
| --- | --- | --- |
| `raw/` | not tracked | The original `.sr` files and their sidecars, collected from sibling repositories. `collect.py` rebuilds it. `raw/manifest.json` records the origin, the source commit and the SHA-256 |
| `fixtures/real/<id>/` | tracked | `raw/` anonymized and converted (below) |
| `fixtures/synth/<set>/<id>/` | tracked | Generated data, frozen |
| `work/` | not tracked | Outputs that can be rebuilt, such as evaluation results |

## Fixture format

| File | Contents |
| --- | --- |
| `capture.json` | Sample rate, sample count, channels `D0…Dn` and each channel's initial level |
| `edges.npz` | Per channel, the differences of the change positions (sample numbers) as uint32, compressed |
| `truth.json` | The ground truth: the original channel names, protocol, line roles, parameters, expected decode, the SHA-256 of the original file |

- **Anonymized:** channel names become `D0…Dn` and their order is shuffled by a seed, so neither the names nor the original order give the answer away.
- **Truth kept apart:** the analysis reads only `capture.json` and `edges.npz`; only the evaluation reads `truth.json`.
- **Stored as edges:** the original `.sr` holds every sample and is large once unpacked.

## Real records

| id | Contents | Source of the truth |
| --- | --- | --- |
| `i2cdb-*` | I²C 100 kHz (SHT30 / QMP6988) + UART 115200 8N1 markers; most of the 8 channels idle | Channel names, I2CDeviceDB's decoded `.jsonl` |
| `wch-*-target-info`, `wch-*-flash-pattern4k` | WCH RVSWD (L103 / V203, 50, 100, 160 MHz) and SWIO (V003) | Channel names and the wch-protocols README. Also negative examples for UART / I²C / SPI |

`flash-pattern4k` has over a million edges, so the usual evaluation leaves it out; `evaluate.py --large` includes it.

## Generated sets

The generator is `src/wireskein/_engine/synth.py`, determined by a seed. The sets used for evaluation are frozen under `fixtures/synth/` (`manifest.json` lists the sets and their seed ranges), so an implementation rewritten in another language can run regression tests on the same input.

| Set | Contents |
| --- | --- |
| `tuning` | UART / I²C / SPI and decoys (seeds from 0; used for tuning) |
| `heldout` | The same conditions, seeds from 1000 (not used for tuning) |
| `glitch` `midstart` `lowrate` `jitter` `freqhop` `baudhop` | Stress conditions |
| `uartlike` | UART / LIN / DMX512 |
| `duplex` | SCPI (a TX / RX pair) and UART |
| `upper` | NMEA / Modbus RTU / text / binary over UART |

## Collecting and converting again

```sh
python3 corpus/collect.py                              # copy from ~/dev/I2CDeviceDB and ~/dev_wch/wch-protocols into raw/
cd research && uv run python convert_real.py           # raw/ -> fixtures/real/
cd research && uv run python export_synth.py           # generated data -> fixtures/synth/
```

If the sources live elsewhere: `collect.py --i2c-root DIR --wch-root DIR`.
