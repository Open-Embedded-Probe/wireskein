# tests

[English](README.md)

pytest の試験です。`uv run pytest` で実行します。

| ファイル | 内容 |
| --- | --- |
| `test_*.py` | 機能ごとの試験 |
| `test_oep_virtual_bench.py` | OEP の取得元を、oep-client-python の virtual bench（`virtual_bench_serve`、TCP）に実際につないで試す。oep-client-python がなければ飛ばす |
| `test_real.py` と `data/real/` | 実機の記録のうち、照合の不具合を見つけたもの（gzip のサンプルと、期待を書いた `manifest.json`）。同じ不具合が戻らないことを確かめる |
| `demo_run.py` | 不具合を入れた試験の記録を合成する例。`test_verify.py` が使い、単体でも動く（`uv run python tests/demo_run.py OUT && uv run wireskein verify OUT`） |

推定の正しさの評価（正解付きのコーパスでの正答率など）は、ここではなく [research/](../research/README.ja.md) で行います。
