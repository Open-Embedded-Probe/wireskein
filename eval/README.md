# Evaluating the inference

[日本語](README.ja.md)

What `wireskein analyze` infers (protocol, line roles, parameters, decoded content) measured against a corpus with
ground truth. Not part of the package. The scripts read only what analyze exports (`wireskein.decode`), so they
measure what a user gets; how the evaluation is done and why: [design decisions](../docs/design.md) §6.

## Running

```sh
uv sync
cd eval
uv run python evaluate.py --set heldout --tag NAME                 # a frozen set -> work/eval-NAME.json
uv run python evaluate.py --synth 200 --tag NAME                   # the real records + 200 generated cases
uv run python large_eval.py                                        # the real records with a million edges
uv run python rates.py --trials 100                                # how checks read at k samples in T -> work/rates.json
uv run python i2c_vs_truth.py FILE...                              # an I2C capture against the waveform it must show
```

Main arguments of `evaluate.py`:

| Argument | Meaning |
| --- | --- |
| `--set NAME` | Use the frozen set `corpus/synth/NAME` |
| `--synth N`, `--start S`, `--profile P`, `--stress S` | Generate N cases from seed S |
| `--no-real`, `--large` | Leave out the real records / also include the real records with a million edges |

Generator profiles: `mixed` (UART / I²C / SPI and decoys), `uartlike` (UART / LIN / DMX512), `duplex` (SCPI), `upper`
(NMEA / Modbus / text / binary), `swdcan` (SWD / CAN beside SPI and UART). Stress conditions: `glitch`, `midstart`,
`lowrate`, `jitter`, `freqhop`, `baudhop`.

## Measures

| Measure | Meaning |
| --- | --- |
| Confirmed (correct) | Share of buses whose line assignment, protocol and main parameters match the truth, with the verdict `confirmed` |
| Confirmed + likely (correct) | The same, with the verdict `confirmed` or `likely` |
| False confirmed / false likely | Wrong conclusions reported as `confirmed` / `likely`. **The most important; kept at 0** |
| Decode match | How well the decoded content of confirmed and likely claims matches the truth |

## The corpus

A case is `<id>.wireskein` (channels renamed `D0…Dn` in a seeded order, so neither names nor order give the answer
away) beside `<id>.truth.json` (the original names, protocol, line roles, parameters, expected decode, the source's
SHA-256), which only the evaluation reads.

| Place | Contents |
| --- | --- |
| `corpus/real/` | Real records. `i2cdb-*`: I²C 100 kHz (SHT30 / QMP6988) and UART 115200 markers, truth from the decoded `.jsonl`. `wch-*-target-info`, `wch-*-flash-pattern4k`: WCH RVSWD (L103 / V203, 50, 100, 160 MHz) and SWIO (V003), truth from the channel names and wch-protocols, also negative examples for UART / I²C / SPI. `flash-pattern4k` has over a million edges: only `--large` and `large_eval.py` take it |
| `corpus/synth/<set>/` | Generated sets, frozen (`manifest.json`: their seeds): `tuning` (seeds from 0, used for tuning), `heldout` (seeds from 1000, never tuned on), the stress sets, `uartlike`, `duplex`, `upper`. A reimplementation in another language can run on the same input |
| `raw/`, `work/` | Not in git: the collected originals (`collect.py`), the evaluation results |

## Rebuilding the corpus

```sh
python3 eval/collect.py                       # copy the originals from sibling repositories into raw/ (with SHA-256)
cd eval && uv run python convert_real.py      # raw/ -> corpus/real/
cd eval && uv run python export_synth.py      # the generator (synth.py, seeded) -> corpus/synth/
```

If the sources live elsewhere: `collect.py --i2c-root DIR --wch-root DIR`. The I²C cases come from I2CDeviceDB, now
archived (read only, kept for rebuilding); `corpus/real/` already holds them converted, with their truth, so
nothing needs collecting to evaluate. Truth for new cases comes from an established tool (a decoder or driver
outside WireSkein) or from synthetic captures (`synth.py`), never from WireSkein's own output: EmbedBenchDeviceData's
runs/ are material to analyse, not truth (their `cNNNN.i2c.json` is WireSkein's decode).
