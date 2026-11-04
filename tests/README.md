# tests

[日本語](README.ja.md)

The pytest suite. Run it with `uv run pytest`.

| File | Contents |
| --- | --- |
| `test_*.py` | Tests per feature |
| `test_oep_virtual_bench.py` | The OEP source over a real link to oep-client-python's virtual bench (`virtual_bench_serve`, on TCP). Skipped without oep-client-python |
| `test_real.py` and `data/real/` | Real-hardware captures that once exposed a bug in the checks (gzipped samples or JSON, with the expectations in `manifest.json`), so the bugs stay fixed |
| `test_p4x_i1.py` and `data/p4x-i1/` | The checks of lines and peripherals on VCD captures from an ESP32-P4 probe, each with the settings that made it in the JSON of the same name |
| `test_examples.py` and `data/examples/` | The analyze examples (analysis-format §4): captures with the buses they hold (`<name>.truth.json`) |
| `test_bench_oep.py` | Captures from a real OEP probe. Skipped unless `WIRESKEIN_BENCH_OEP=oep://<unit_id>` is set |
| `demo_run.py` | Synthesizes a recorded test run with injected bugs. Used by `test_verify.py`; also runs on its own (`uv run python tests/demo_run.py OUT && uv run wireskein verify OUT`) |

How correct the inference is (accuracy on the corpus with ground truth) is measured in [eval/](../eval/README.md), not here.
