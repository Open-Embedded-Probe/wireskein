# 推定の評価

`wireskein analyze` の推定（方式・線の役割・パラメータ）を、正解付きのコーパス（[corpus](../corpus/README.ja.md)）で測るスクリプトです。package には入りません。評価の考え方は [設計の決定](../docs/design.ja.md) §6 にあります。

## 実行

```sh
cd research
uv sync --group research
uv run python evaluate.py --set heldout --engine staged --tag NAME   # 固定したセットで評価 → ../corpus/work/eval-NAME.json
uv run python evaluate.py --synth 200 --engine staged --tag NAME     # 実記録＋生成 200 件
```

`evaluate.py` の主な引数:

| 引数 | 意味 |
| --- | --- |
| `--engine staged` | 段構成のエンジン（`wireskein analyze` が使うもの）。`declarative` は I²C・SPI・RVSWD を TOML の定義で読む。`flat` は比較用の総当たりのエンジン |
| `--set NAME` | `corpus/fixtures/synth/NAME` の固定したセットを使う |
| `--synth N`、`--start S`、`--profile P`、`--stress S` | シード S から N 件を生成して使う |
| `--no-real`、`--large` | 実記録を使わない / 100 万エッジ級の実記録も入れる |

生成データのプロファイルは `mixed`（UART / I²C / SPI と囮）、`uartlike`（UART / LIN / DMX512）、`duplex`（SCPI）、`upper`（NMEA / Modbus / テキスト / バイナリ）。ストレス条件は `glitch`、`midstart`、`lowrate`、`jitter`、`freqhop`、`baudhop`。

## 指標

| 指標 | 意味 |
| --- | --- |
| 確定（正） | 線の割り当て・方式・主要パラメータが正解と一致し、判定が `confirmed` の割合 |
| 確定＋有力（正） | 同じく、判定が `confirmed` か `likely` の割合 |
| 誤確定 / 誤った有力 | 間違った結論を `confirmed` / `likely` とした数。**最も重視し、0 を保つ** |
| 復号一致 | 確定・有力の主張の復号内容と正解の一致率 |

## 個別のスクリプト

| スクリプト | 確かめること |
| --- | --- |
| `stage_eval.py [n]` | 段ごとに情報量が減っているか、型付きの結果が正しいか、段ごとの時間 |
| `exclusion_audit.py [n] [--stress S]` | 確実な除外の規則が正解の仮説を落とさないか、どれだけ減らすか |
| `device_eval.py [n]` | 証拠のあるデバイスを識別し、証拠のないものを主張しないか |
| `hint_eval.py [n]` | ヒント（方式の一覧、ピンごとの方式・役割・ボーレート）の効果 |
| `upper_eval.py [n]` | 上位（NMEA、Modbus RTU、テキスト）の支持が UART の判定に効くか |
| `scpi_eval.py [n]` | TX / RX の組と SCPI |
| `baud_segment_eval.py [n] [--stress baudhop]` | UART の途中のボーレート切り替え |
| `plugin_reuse_eval.py [n]` | RateBlocks → Chars を UART・LIN・DMX512 が共有できるか |
| `m2_eval.py [n]` | 単線の特徴（静止線、アイドルレベル、ビット時間、クロックらしさ） |
| `large_eval.py` | 100 万エッジ級の実記録（RVSWD / SWIO の書き込み） |

## コーパスの作り直し

| スクリプト | 内容 |
| --- | --- |
| `convert_real.py` | `corpus/raw/` の記録を匿名化して `corpus/fixtures/real/` に変換する |
| `export_synth.py` | 生成データのセットを `corpus/fixtures/synth/` に固定する |
| `corpus.py` | 評価のケースを列挙する（実記録と生成データ） |
