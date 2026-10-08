# Evaluating the inference

[日本語](README.ja.md)

Scripts that measure what `wireskein analyze` infers (protocol, line roles, parameters) against the corpus with ground truth ([corpus](../corpus/README.md)). They are not part of the package. How the evaluation is done and why: [design decisions](../docs/design.md) §6.

## Running

```sh
cd research
uv sync
uv run python evaluate.py --set heldout --tag NAME                 # a frozen set -> ../corpus/work/eval-NAME.json
uv run python evaluate.py --synth 200 --tag NAME                   # real records + 200 generated cases
```

Main arguments of `evaluate.py`:

| Argument | Meaning |
| --- | --- |
| `--set NAME` | Use the frozen set `corpus/fixtures/synth/NAME` |
| `--synth N`, `--start S`, `--profile P`, `--stress S` | Generate N cases from seed S |
| `--no-real`, `--large` | Leave out the real records / also include the real records with a million edges |

Generator profiles: `mixed` (UART / I²C / SPI and decoys), `uartlike` (UART / LIN / DMX512), `duplex` (SCPI), `upper` (NMEA / Modbus / text / binary). Stress conditions: `glitch`, `midstart`, `lowrate`, `jitter`, `freqhop`, `baudhop`.

## Measures

| Measure | Meaning |
| --- | --- |
| Confirmed (correct) | Share of buses whose line assignment, protocol and main parameters match the truth, with the verdict `confirmed` |
| Confirmed + likely (correct) | The same, with the verdict `confirmed` or `likely` |
| False confirmed / false likely | Wrong conclusions reported as `confirmed` / `likely`. **The most important; kept at 0** |
| Decode match | How well the decoded content of confirmed and likely claims matches the truth |

## Other scripts

| Script | What it checks |
| --- | --- |
| `stage_eval.py [n]` | Whether each stage reduces the information, whether the typed results are right, the time per stage |
| `device_eval.py [n]` | Devices with evidence are identified; nothing is claimed without evidence |
| `hint_eval.py [n]` | The effect of hints (a protocol list; per pin protocol, role and baud rate) |
| `upper_eval.py [n]` | Whether upper-layer support (NMEA, Modbus RTU, text) helps UART verdicts |
| `scpi_eval.py [n]` | TX / RX pairs and SCPI |
| `baud_segment_eval.py [n] [--stress baudhop]` | UART baud rate changes mid-stream |
| `plugin_reuse_eval.py [n]` | Whether UART, LIN and DMX512 can share RateBlocks -> Chars |
| `m2_eval.py [n]` | Single-line features (idle lines, idle level, bit time, clock-likeness) |
| `large_eval.py` | Real records with a million edges (RVSWD / SWIO flash writes) |

## Rebuilding the corpus

| Script | Contents |
| --- | --- |
| `convert_real.py` | Anonymize the records in `corpus/raw/` into `corpus/fixtures/real/` |
| `export_synth.py` | Freeze the generated sets into `corpus/fixtures/synth/` |
| `corpus.py` | List the evaluation cases (real records and generated data) |
