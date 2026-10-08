# tests

[日本語](README.ja.md)

The pytest suite. Run it with `uv run pytest`.

| File | Contents |
| --- | --- |
| `test_*.py` | Tests per feature |
| `test_oep_virtual_bench.py` | The OEP source over a real link to oep-client-python's virtual bench (`virtual_bench_serve`, on TCP). Skipped without oep-client-python |
| `test_real.py` and `data/real/` | Real-hardware captures that once exposed a bug in the checks (gzipped samples, with the expectations in `manifest.json`), so the bugs stay fixed |
| `demo_run.py` | Synthesizes a recorded test run with injected bugs. Used by `test_verify.py`; also runs on its own (`uv run python tests/demo_run.py OUT && uv run wireskein verify OUT`) |

How correct the inference is (accuracy on the corpus with ground truth) is measured in [research/](../research/README.md), not here.
