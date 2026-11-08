# tests

[日本語](README.ja.md)

The pytest suite. Run it with `uv run pytest`.

| File | Contents |
| --- | --- |
| `test_*.py` | Tests per feature |
| `test_oep_virtual_bench.py` | The OEP source over a real link to oep-client-python's virtual bench (`virtual_bench_serve`, on TCP). Skipped without oep-client-python |
| `test_real.py` and `data/real/` | Real-hardware captures that once exposed a bug in the checks (gzipped samples or JSON, with the expectations in `manifest.json`), so the bugs stay fixed |
| `test_p4x_i1.py` and `data/p4x-i1/` | The checks of lines and peripherals on VCD captures from an ESP32-P4 probe, each with the settings that made it in the JSON of the same name |
| `test_uart_receiver.py` and `data/uart_truth/` | The uart check as a receiver reads: glitches, breaks, errors one after another, on lines encoded in the test; and an OEP probe's ESP-IDF UART driver sending framing and parity errors and rates off by up to 3 %, with what it sent (`<name>.truth.json`) |
| `test_examples.py` and `data/examples/` | The analyze examples (analysis-format §4): captures with the buses they hold (`<name>.truth.json`) |
| `test_bench_oep.py` | Captures from a real OEP probe. Skipped unless `WIRESKEIN_BENCH_OEP=oep://<unit_id>` is set |
| `test_rvswd_long.py` and `data/rvswd_long_truth/` | RVSWD and SWIO from WCH-LinkE captures, with ch32rv's output as the truth; some are bad captures kept on purpose (too slow for the clock, noisy: the data's [README](data/rvswd_long_truth/README.md)) |
| `test_degrade.py` | `tools/degrade.py`: test data made worse from a capture, made again from its seed |
| `demo_run.py` | Synthesizes a recorded test run with injected bugs. Used by `test_verify.py`; also runs on its own (`uv run python tests/demo_run.py OUT && uv run wireskein verify OUT`) |

How correct the inference is (accuracy on the corpus with ground truth) is measured in [eval/](../eval/README.md), not here.
