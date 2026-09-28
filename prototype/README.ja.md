# 推定パイプラインのプロトタイプ（使い捨て）

[プロトタイプ計画](../docs/prototype-plan.ja.md) に沿って、未知のデジタル記録から UART / I²C / SPI とその上位を推定する試作。**本番へ移植しない前提**で、測定できることを優先して書いている。結果は [findings.ja.md](findings.ja.md) に記録する。

## 実行

~~~sh
cd prototype
uv sync
PYTHONPATH=. uv run python m2_eval.py 300                    # L1-L3 の単線特徴だけを評価
PYTHONPATH=. uv run python evaluate.py --synth 200 --tag v4  # 全段の評価 → ../corpus/work/eval-v4.json
PYTHONPATH=. uv run python evaluate.py --synth 100 --stress glitch --no-real --tag glitch
PYTHONPATH=. uv run python prune_study.py v4                 # 枝切り条件を置いたら何を失うか
PYTHONPATH=. uv run python boundary_probe.py --probe         # 段ごとの呼び出し回数・データ量・時間（M7）
~~~

Rust の境界ベンチマーク（M7）。入力は `PYTHONPATH=. uv run python export_bench_input.py` で実記録の UART 線から作る。

~~~sh
PYTHONPATH=. uv run python export_bench_input.py
cd boundary-bench
cargo build --release -p plugin_wasm --target wasm32-unknown-unknown
cargo build --release -p plugin_cdylib -p bench
./target/release/bench ../../corpus/work/uart_edges.i64 ../../corpus/work/uart_edges.json
~~~

事前に [corpus](../corpus/README.ja.md) の `fixtures/real/` が必要（Git に含まれる）。

## 構成

| モジュール | 段 | 内容 |
| --- | --- | --- |
| `wsproto/model.py`, `srio.py`, `fixture.py` | L0 | エッジ列モデル、`.sr` 読み込み、匿名化フィクスチャ |
| `wsproto/gen.py`, `synth.py` | — | 連続時間で波形を作り、サンプル格子へ量子化する生成器。シードで決まる評価シナリオ |
| `wsproto/features.py` | L1–L3 | 静止・アイドル・バースト、ラン幅ヒストグラム、基本時間単位の候補、周期、役割スコア（クロック様・非同期様） |
| `wsproto/stack.py` | エンジン | 型付きストリームを受け渡す解析器を**枝切りなし**で再帰展開。スコアは Scorer に分離。線を重複なく割り当てる組み合わせ探索（分枝限定） |
| `wsproto/analyzers/` | L5 | `uart`（全チャンネル×ボーレート候補×極性×18 形式）、`i2c`（全順序対）、`spi`（クロック×CS×端×ビット順、データ線は全線を復号して採否）、`upper`（行、NMEA、Modbus RTU、マーカー文法） |
| `wsproto/scoring.py` | L6 | 層スコア（指標の積）と上位の支持の合成。差し替えて比較する |
| `wsproto/pipeline.py` | L6 | 組み合わせの最良説明、主張ごとの余裕（その主張を外した最良説明との差を、割り当てが変わった線の数で割る）、判定 |
| `wsproto/kernels.py` | コア候補 | 解析器が使う一括の数値処理。呼び出しごとに時間と入出力の大きさを数える |
| `evaluate.py`, `prune_study.py` | 評価 | 正解との照合、誤確定・保留・復号一致、枝切りの影響 |
| `boundary_probe.py`, `boundary-bench/` | M7 | Python 側の計測と、Rust での境界方式（ネイティブ、cdylib、WASM、別プロセス）の比較 |
