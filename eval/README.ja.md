# 推定の評価

[English](README.md)

`wireskein analyze` の推定（方式、線の役割、パラメータ、復号した中身）を、正解付きのコーパスで測ります。package
には入りません。スクリプトは analyze の書き出し（`wireskein.decode`）だけを読むので、利用者が受け取るものを測り
ます。評価の考え方と理由は [設計の決定](../docs/design.ja.md) §6 です。

## 動かし方

```sh
uv sync
cd eval
uv run python evaluate.py --set heldout --tag NAME                 # 凍結した組 -> work/eval-NAME.json
uv run python evaluate.py --synth 200 --tag NAME                   # 実記録と、生成した 200 件
uv run python large_eval.py                                        # 100 万エッジの実記録
```

`evaluate.py` の主な引数:

| 引数 | 意味 |
| --- | --- |
| `--set NAME` | 凍結した組 `corpus/synth/NAME` を使う |
| `--synth N`、`--start S`、`--profile P`、`--stress S` | seed S から N 件を生成する |
| `--no-real`、`--large` | 実記録を外す / 100 万エッジの実記録も入れる |

生成のプロファイル: `mixed`（UART / I²C / SPI と囮）、`uartlike`（UART / LIN / DMX512）、`duplex`（SCPI）、`upper`
（NMEA / Modbus / テキスト / バイナリ）、`swdcan`（SPI と UART の横の SWD / CAN）。負荷の条件: `glitch`、`midstart`、
`lowrate`、`jitter`、`freqhop`、`baudhop`。

## 指標

| 指標 | 意味 |
| --- | --- |
| confirmed（正しい） | 線の割り当て、方式、主なパラメータが正解と合い、判定が `confirmed` のバスの割合 |
| confirmed + likely（正しい） | 同じく、判定が `confirmed` か `likely` |
| 誤った confirmed / 誤った likely | `confirmed` / `likely` と出した誤り。**いちばん大事で、0 に保つ** |
| 復号の一致 | confirmed と likely の主張の、復号した中身が正解とどれだけ合うか |

## コーパス

1 件は `<id>.wireskein`（チャンネルの名前を `D0…Dn` にし、seed で並びも入れ替えて、名前や並びから答えが分からない
ようにしたもの）と、その横の `<id>.truth.json`（元の名前、方式、線の役割、パラメータ、期待する復号、元のファイルの
SHA-256）です。正解は評価だけが読みます。

| 場所 | 中身 |
| --- | --- |
| `corpus/real/` | 実記録。`i2cdb-*`: I²C 100 kHz（SHT30 / QMP6988）と UART 115200 のマーカー。正解は復号済みの `.jsonl` から。`wch-*-target-info`、`wch-*-flash-pattern4k`: WCH の RVSWD（L103 / V203、50、100、160 MHz）と SWIO（V003）。正解はチャンネル名と wch-protocols から。UART / I²C / SPI の負例も兼ねる。`flash-pattern4k` は 100 万エッジを超えるので、`--large` と `large_eval.py` だけが使う |
| `corpus/synth/<組>/` | 生成した組を凍結したもの（`manifest.json` に seed の範囲）: `tuning`（seed 0 から、調整に使う）、`heldout`（seed 1000 から、調整に使わない）、負荷の組、`uartlike`、`duplex`、`upper`。ほかの言語で作り直した実装も、同じ入力で回帰の試験ができる |
| `raw/`、`work/` | git に入れない: 集めた元のファイル（`collect.py`）、評価の結果 |

## コーパスを作り直す

```sh
python3 eval/collect.py                       # 元のファイルを兄弟のリポジトリから raw/ に写す（SHA-256 付き）
cd eval && uv run python convert_real.py      # raw/ -> corpus/real/
cd eval && uv run python export_synth.py      # 生成器（synth.py、seed で決まる）-> corpus/synth/
```

元の場所が違うときは `collect.py --i2c-root DIR --wch-root DIR`。I²C のケースは I2CDeviceDB から取りました。今は
アーカイブされています（読むだけ。作り直すために残す）。`corpus/real/` には、正解ごと変換したものが入っているので、
評価するのに集め直す必要はありません。新しいケースの正解は、確立した道具（WireSkein の外の復号器やドライバ）か、
合成したキャプチャ（`synth.py`）から取り、WireSkein 自身の出力からは取りません。EmbedBenchDeviceData の runs/ は
分析する材料で、正解ではありません（その `cNNNN.i2c.json` は WireSkein の復号です）。
