# tests

[English](README.md)

pytest の試験です。`uv run pytest` で実行します。

| ファイル | 内容 |
| --- | --- |
| `test_*.py` | 機能ごとの試験 |
| `test_oep_virtual_bench.py` | OEP の取得元を、oep-client-python の virtual bench（`virtual_bench_serve`、TCP）に実際につないで試す。oep-client-python がなければ飛ばす |
| `test_real.py` と `data/real/` | 実機の記録のうち、照合の不具合を見つけたもの（gzip のサンプルか JSON と、期待を書いた `manifest.json`）。同じ不具合が戻らないことを確かめる |
| `test_p4x_i1.py` と `data/p4x-i1/` | ESP32-P4 のプローブで取った VCD での、線と周辺機能の検査。それぞれ、作ったときの設定を同じ名前の JSON に持つ |
| `test_examples.py` と `data/examples/` | analyze の例（analysis-format §4）: キャプチャと、それが持つバス（`<名前>.truth.json`） |
| `test_bench_oep.py` | 本物の OEP のプローブで取る試験。`WIRESKEIN_BENCH_OEP=oep://<unit_id>` が無ければ飛ばす |
| `demo_run.py` | 不具合を入れた試験の記録を合成する例。`test_verify.py` が使い、単体でも動く（`uv run python tests/demo_run.py OUT && uv run wireskein verify OUT`） |

推定の正しさの評価（正解付きのコーパスでの正答率など）は、ここではなく [eval/](../eval/README.ja.md) で行います。
