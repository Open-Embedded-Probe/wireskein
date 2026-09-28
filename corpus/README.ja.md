# 評価コーパス

推定パイプラインの評価と回帰試験に使う記録を置く。方針は [プロトタイプ計画](../docs/prototype-plan.ja.md) を参照。

| 場所 | Git | 内容 |
| --- | --- | --- |
| `raw/` | 管理外 | 兄弟リポジトリから集めた元の `.sr` と付随ファイル。`collect.py` で再作成でき、`raw/manifest.json` に出所・参照元コミット・SHA-256 を残す |
| `fixtures/real/<id>/` | 管理する | `raw/` を匿名化・変換したフィクスチャ（下記） |
| `fixtures/synth/<set>/<id>/` | 管理する | 生成データを固定したもの（`prototype/export_synth.py`）。どの言語で作り直しても、同じ入力で回帰試験ができる |
| `work/` | 管理外 | 評価結果など再生成できる出力 |

生成データは `prototype/wsproto/synth.py` がシード値から作り、評価に使うセットを `fixtures/synth/` に固定した（`manifest.json` にセットとシードの範囲）。

| セット | 件数 | 内容 |
| --- | ---: | --- |
| `tuning` | 200 | UART/I²C/SPI と囮（シード 0〜199。プロトタイプの調整に使った） |
| `heldout` | 200 | 同じ条件でシード 1000〜1199（調整に使っていない） |
| `glitch` `midstart` `lowrate` `jitter` `freqhop` `baudhop` | 各 50 | ストレス条件 |
| `uartlike` | 60 | UART / LIN / DMX512 |
| `duplex` | 60 | SCPI（TX/RX の組）と UART |
| `upper` | 80 | UART の上の NMEA / Modbus RTU / テキスト / バイナリ |

評価: `cd prototype && PYTHONPATH=. uv run python evaluate.py --set heldout --engine staged`

## 集め直す・変換し直す

~~~sh
python3 corpus/collect.py                 # ~/dev/I2CDeviceDB と ~/dev_wch/wch-protocols から raw/ へコピー
cd prototype && PYTHONPATH=. uv run python convert_real.py   # raw/ → fixtures/real/
~~~

参照元の場所が違う場合は `collect.py --i2c-root DIR --wch-root DIR`。76 MB の `monitor_sdi` などの大きな記録は対象から外している。

## フィクスチャの形式

元の `.sr` は全サンプルを持つため扱いにくい。フィクスチャでは次のように変換した。

| ファイル | 内容 |
| --- | --- |
| `capture.json` | サンプルレート、サンプル数、チャンネル `D0…Dn` と各チャンネルの初期レベル |
| `edges.npz` | チャンネルごとの変化位置（サンプル番号）の差分を uint32 にして圧縮したもの |
| `truth.json` | 正解。元のチャンネル名、方式・線の役割・パラメータ、期待する復号内容、元ファイルの SHA-256 |

- **匿名化:** チャンネル名を `D0…Dn` に置き換え、並び順もシードで入れ替える。名前や元の並びから答えが分からないようにするため。
- **正解の分離:** 解析は `capture.json` と `edges.npz` だけを読む。`truth.json` は評価側だけが読む。
- **サイズ:** 13 記録で合計約 3.1 MB（元の `.sr` は 5.9 MB、展開すると数百 MB）。

## 収録している記録

| id | 内容 | 正解の出所 |
| --- | --- | --- |
| `i2cdb-*`（4件） | I²C 100 kHz（SHT30 / QMP6988）＋ UART 115200 8N1 のマーカー、8 MHz、8ビット中5ビットは静止 | チャンネル名、I2CDeviceDB の復号済み `.jsonl`（取引列） |
| `wch-*-target-info`、`wch-*-flash-pattern4k` | WCH RVSWD（L103 / V203、50・100・160 MHz）と SWIO（V003） | チャンネル名と wch-protocols の README。**UART / I²C / SPI の負例**として使う |

RVSWD / SWIO の `flash-pattern4k` は 100 万エッジを超えるため、通常の評価では外し、`evaluate.py --large` で含める。
