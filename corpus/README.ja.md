# 評価コーパス

推定の評価と回帰試験に使う、正解付きの記録です。評価のスクリプトは [research/](../research/README.ja.md) にあります。

| 場所 | Git | 内容 |
| --- | --- | --- |
| `raw/` | 管理外 | 兄弟リポジトリから集めた元の `.sr` と付随ファイル。`collect.py` で作り直せる。`raw/manifest.json` に出所、参照元のコミット、SHA-256 を残す |
| `fixtures/real/<id>/` | 管理する | `raw/` を匿名化・変換したもの（下記） |
| `fixtures/synth/<set>/<id>/` | 管理する | 生成データを固定したもの |
| `work/` | 管理外 | 評価の結果など、作り直せる出力 |

## フィクスチャの形式

| ファイル | 内容 |
| --- | --- |
| `capture.json` | サンプルレート、サンプル数、チャンネル `D0…Dn` と各チャンネルの初期レベル |
| `edges.npz` | チャンネルごとの変化位置（サンプル番号）の差分を uint32 にして圧縮したもの |
| `truth.json` | 正解。元のチャンネル名、方式・線の役割・パラメータ、期待する復号内容、元ファイルの SHA-256 |

- **匿名化:** チャンネル名を `D0…Dn` に置き換え、並び順もシードで入れ替える。名前や元の並びから答えが分からないようにするため。
- **正解の分離:** 解析は `capture.json` と `edges.npz` だけを読む。`truth.json` は評価の側だけが読む。
- **エッジで持つ:** 元の `.sr` は全サンプルを持ち、展開すると大きいため。

## 実記録

| id | 内容 | 正解の出所 |
| --- | --- | --- |
| `i2cdb-*` | I²C 100 kHz（SHT30 / QMP6988）＋ UART 115200 8N1 のマーカー。8 チャンネル中の多くは静止 | チャンネル名、I2CDeviceDB の復号済み `.jsonl` |
| `wch-*-target-info`、`wch-*-flash-pattern4k` | WCH RVSWD（L103 / V203、50・100・160 MHz）と SWIO（V003） | チャンネル名と wch-protocols の README。UART / I²C / SPI の負例にもなる |

`flash-pattern4k` は 100 万エッジを超えるので、通常の評価では外し、`evaluate.py --large` で含めます。

## 生成データのセット

生成器は `src/wireskein/_engine/synth.py` で、シードから決まります。評価に使うセットを `fixtures/synth/` に固定しています（`manifest.json` にセットとシードの範囲）。固定しておけば、ほかの言語で作り直した実装でも、同じ入力で回帰試験ができます。

| セット | 内容 |
| --- | --- |
| `tuning` | UART / I²C / SPI と囮（シード 0〜。調整に使ったもの） |
| `heldout` | 同じ条件でシード 1000〜（調整に使っていない） |
| `glitch` `midstart` `lowrate` `jitter` `freqhop` `baudhop` | ストレス条件 |
| `uartlike` | UART / LIN / DMX512 |
| `duplex` | SCPI（TX / RX の組）と UART |
| `upper` | UART の上の NMEA / Modbus RTU / テキスト / バイナリ |

## 集め直す・変換し直す

```sh
python3 corpus/collect.py                              # ~/dev/I2CDeviceDB と ~/dev_wch/wch-protocols から raw/ へコピー
cd research && uv run python convert_real.py           # raw/ → fixtures/real/
cd research && uv run python export_synth.py           # 生成データ → fixtures/synth/
```

参照元の場所が違う場合は `collect.py --i2c-root DIR --wch-root DIR`。
