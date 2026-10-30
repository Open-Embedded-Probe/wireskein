# wireskein-analysis/1: 役割の分かったキャプチャの復号

`wireskein decode CAPTURE --spec SPEC [--out FILE]`（Python では `wireskein.decode.decode_file(path, spec)`）が
この文書を書きます。残しておくための形です。データのリポジトリに、キャプチャとその記録と並べて置き、
wireskein の中を知らない道具が後で読みます。

`wireskein analyze` は、キャプチャに何があるかを見つけ、証拠が支える分だけを主張します。短いケース（転送が 1 つ）
では証拠が足りず、主張は空になります。配線が分かっているときに欲しいのは、証拠が弱くても、その役割のとおりの
復号です（verify も同じことをします）。`decode` はそれを出し、線がどれだけ合ったか、analyze なら主張したか、
読めなかった所と見えなかった所も一緒に示します。

## 1. 指定（spec）

```json
{"buses": [{"protocol": "i2c", "pins": {"scl": "SCL", "sda": "SDA"}},
           {"protocol": "spi", "pins": {"clk": "SCK", "mosi": "MOSI", "miso": "MISO", "cs": "CS"},
            "params": {"mode": 0}}],
 "lines": ["DRDY"],
 "devices": ["i2c/**"],
 "depth": "device"}
```

| キー | 意味 |
| --- | --- |
| `buses[].protocol` | 解析器の名前（`i2c`、`spi`、`uart` など） |
| `buses[].pins` | 役割 → チャンネルの名前。どのピンもキャプチャに無ければならない（無ければエラー）。ちょうどこのピンの上の復号を使う |
| `buses[].params` | 任意。指示として渡す（`uart` の `baud` など）。`spi` の `mode`（0〜3）は標本のエッジを決める。無ければ線からモードを読む（capture-test-guide §4.5）。active high の CS には `cs_active` |
| `buses[].idle_s` | 任意。これより離れた活動は別のかたまり（§3.3）。既定はそのバスの 16 ビット分、ビットの無いバスは 1 ms |
| `lines` | 変化をそのまま残す線（DRDY、INT など） |
| `devices` | バスの上で試すデバイスの定義（`analyze --devices` と同じ） |
| `depth` | 結果をどの層まで出すか: `transport`、`frames`、`protocol`、`device`（既定） |

## 2. 文書

```json
{"format": "wireskein-analysis/1",
 "wireskein": "0.0.16",
 "spec": {...},
 "capture": {"file": "c0001.wireskein", "sha256": "...", "id": "...", "tick_hz": [1000000, 1],
             "complete": true, "layout": "timeline", "parts": 1},
 "parts": [{"part": 0, "ticks": 8192, "tick0": 0, "start_ns": 6061908000,
            "buses": [...], "lines": [...], "undeclared": [...], "claims": [...]}]}
```

| キー | 意味 |
| --- | --- |
| `format` | `"wireskein-analysis/1"` |
| `wireskein` | 書いた版。後の版は同じキャプチャをもっとよく復号することがある。作り直せるよう、キャプチャを残す |
| `spec` | 使った指定（既定値を入れたもの） |
| `capture.file`、`sha256`、`id` | どのキャプチャか（ファイルの名前、中身のハッシュ、キャプチャの id） |
| `capture.tick_hz` | 1 秒の刻みの数（分子と分母） |
| `capture.complete`、`lost` | キャプチャのもの（wireskein-format §3.4）。データが落ちたキャプチャは失敗。取れた区間の結果は参考に残す |
| `capture.layout`、`parts` | 区間の並べ方と、その数 |
| `parts[]` | 区間ごとに 1 つ。解析は区間をまたがない |

### 2.1 区間

| キー | 意味 |
| --- | --- |
| `part` | 番号（0 から） |
| `ticks` | 長さ（刻み）。以下の刻みはすべて、この区間の刻み 0 から数える |
| `tick0` | 最初の区間の刻み 0 から、どれだけ後に始まるか（timeline のとき） |
| `start_ns` | この区間の刻み 0 の、プローブの時計での時刻（キャプチャが持つとき）。刻み `s` は `start_ns + s / tick_hz` 秒 × 1e9 |
| `start_uncertainty_ns`、`boot_id` | キャプチャの区間が持てば、そのまま（run-format §2.4） |
| `buses[]` | 指定のバスごとに 1 つ、指定の順（§3） |
| `lines[]` | `{"pin", "initial", "edges"}`: 区間の前のレベルと、変化ごとの刻み |
| `undeclared[]` | 指定に無いピンで変化したもの: `{"pin", "edges", "s", "e", "bursts"}`（1 ms より離れたものを別のかたまりとする） |
| `claims` | 指定に無いピンについての analyze の主張（`analyze` の書き出しと同じ形）。参考。無ければ入れない |

## 3. バス

```json
{"protocol": "i2c", "pins": {"scl": "SCL", "sda": "SDA"},
 "decoded": true,
 "params": {"sample_edge": "rise"},
 "fit": {"score": 0.0647, "claimed": false},
 "result": {"transactions": [{"s": 211, "e": 301, "addr": 80, "rw": "write", "addr_ack": false, "bytes": "",
                              "acks": [], "start_cond": "S", "end_cond": "P", "scl_low_ticks": 6.0,
                              "scl_waits": []}]},
 "uninterpreted": [],
 "idle_ticks": 160}
```

### 3.1 decoded、params、fit

- `decoded`: このピンの上にこの方式として読めるものが無ければ `false`。そのとき `result` は空で、ピンの上の
  かたまりはすべて未解釈。黙って外さず、そう示す。
- `params`: 復号器が選んだもの（標本のエッジ、UART のボーレートなど）。
- `fit.score`: 線がどれだけ合ったか（0〜1、analyze の得点）。短いケースでは低い: 測るのは証拠の量で、正しさではない。
- `fit.claimed`: analyze が自分で探したら、ちょうどこの復号を主張したか。主張したなら `fit.verdict` にその判定
  （`confirmed` / `likely` / `ambiguous`）。

### 3.2 result

analyze の書き出し（`--mode final`、`depth` まで）の層。刻みは区間の刻み 0 から:

| 方式 | 層 |
| --- | --- |
| `i2c` | `transactions`: `s`、`e`、`addr`、`rw`、`addr_ack`、`bytes`（16 進）、`acks`、`start_cond`（S / Sr）、`end_cond`（P / Sr / null: 区間の終わりで開いたまま）、`scl_low_ticks`、`scl_waits`（`[{after_word, bit, ticks, s}]`）。区間の終わりで切れたものは `complete: false` と `pending_bits` |
| `spi` | `transfers`: `s`、`e`（最初と最後の標本のビット）と、データのピンごとのバイト（16 進、チャンネルの名前の下）。SD のコマンドを読めたら、横に `sd`（capture-test-guide §4.8） |
| `uart` | `lines`、`nmea`、`modbus_rtu`（読めたもの）と `bytes`（`{"s": [...], "hex"}`） |
| ほか | `analyze` の書き出しと同じ |

上の層を読めても、下の層は横に残す（`sd` と `transfers`）。`depth: "device"` では、`devices` にバスの上で
合ったデバイスを出す（`analyze --depth device` と同じ）。

### 3.3 uninterpreted

`[{"s", "e", "edges"}]`: バスのピンの活動のうち、復号したどの項目にも入らないもの。バスのすべてのピンの変化を、
`idle_ticks` のあいだ変化が無い所で切ってかたまりにする。項目がその中で（またはその `idle_ticks` 前から）
始まらず、どの項目の中にも無いかたまりが、未解釈: START の無い SCL のクロック、復号器が読めなかった転送、雑音など。

区間と `capture.lost` と合わせて、3 つを区別する:

- **何も起きなかった**: 区間の中で、ピンに変化が無い
- **見えなかった**: 区間の間、または欠落
- **読めなかった**: 未解釈のかたまり

## 4. 安定性

形式の番号は、最初のリリースまで 1 のまま（互換の無い変更でも）。読み手は `format` を確かめ、知らないキーは無視する。
