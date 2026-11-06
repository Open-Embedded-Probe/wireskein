# wireskein-analysis/1: 役割の分かったキャプチャの復号

`wireskein decode CAPTURE --spec SPEC [--out FILE]`（CAPTURE は WireSkein のファイル、`.sr`、VCD、足した形式。Python では `wireskein.decode.decode_file(path, spec)`）が
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
| `buses[].layers` | 任意。最後の読みのほかに `result` に出す下の層の名前（`bits`: サンプルの縁 `t` とその値のすべて、`frames`、SWIO の `pulses`: パルスごとの始まり・幅・短い / 長い、`*`: すべて） |
| `buses[].idle_s` | 任意。これより離れた活動は別のかたまり（§3.3）。既定はそのバスの 16 ビット分、ビットの無いバスは 1 ms |
| `lines` | 変化をそのまま残す線（DRDY、INT など） |
| `devices` | バスの上で試すデバイスの定義（`analyze --devices` と同じ） |
| `depth` | 結果をどの層まで出すか: `transport`、`frames`、`protocol`、`device`（既定） |

## 2. 文書

```json
{"format": "wireskein-analysis/1",
 "wireskein": "...",
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
| `parts[]` | 区間ごとに 1 つ。隙間なく続く区間の並びは、1 つとして読んで 1 つ（wireskein-format §3.4）。解析は隙間をまたがない |

### 2.1 区間

| キー | 意味 |
| --- | --- |
| `part` | 番号（0 から。つないだ並びでは、その最初の区間の番号） |
| `parts` | 区間をつないだ並びのとき、その最初と最後の区間の番号。刻みは、最初の区間の刻み 0 から、ほかの区間をまたいで続く |
| `ticks` | 長さ（刻み）。以下の刻みはすべて、この区間の刻み 0 から数える |
| `tick0` | 最初の区間の刻み 0 から、どれだけ後に始まるか（timeline のとき） |
| `start_ns` | この区間の刻み 0 の、プローブの時計での時刻（キャプチャが持つとき）。刻み `s` は `start_ns + s / tick_hz` 秒 × 1e9 |
| `start_uncertainty_ns`、`boot_id` | キャプチャの区間が持てば、そのまま（wireskein-format §3.3） |
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
| 独自のプロトコル（[独自のプロトコル](protocols.ja.md)） | バスの下に、その名前で: `{"messages", "checks_passed", "checks_failed"}`。チェックが通ったときに出て、そのときはバスの最終の読み |
| `spi` | `transfers`: `s`、`e`（最初と最後の標本のビット）と、データのピンごとのバイト（16 進、チャンネルの名前の下）。SD のコマンドを読めたら、横に `sd`（capture-test-guide §4.8）。CS があれば `modes`: SCK の待機のレベルに合う 2 つのモードそれぞれの `readable`（そのサンプルの辺でどのデータ線も変わらない）と、そのモードで読んだ `bytes`（MSB から）。`modes_ok` は読めるもの。読めるモードが 1 つならモードが決まる。読めるモードのバイトが同じなら、見分けなくてよい |
| `uart` | `lines`、`nmea`、`modbus_rtu`（読めたもの）と `bytes`（`{"s": [...], "hex"}`） |
| `can` | `frames`: `s`、`id`、`ide`（1: 拡張の識別子）、`dlc`、`data`（16 進。DLC が運ぶバイト）、`ack`、`ok` |
| `lin` | `frames`: `s`（break）、`id`、`data`（16 進）、`ok`（sync、識別子のパリティ、チェックサムが合う） |
| `dmx512` | `packets`: `s`（break）、`start_code`、`data`（16 進。スロット） |
| `rvswd`、`swio` | WCH のデバッグの伝送路（2 線と 1 線）。その上の層は RISC-V のもので、どちらでも同じ。`dmi`: 運ぶ RISC-V の DMI のアクセスで、`s`、`e`（RVSWD: フレームの最後のサンプルの縁）、`op`（`R` / `W`）、`addr`、`reg`（RISC-V Debug Specification 0.13 のレジスタの名前。仕様の外の WCH のレジスタは `wch_7c`〜`wch_7f`、どちらでもないアドレスは null）、`data`、`ok`（フレームの検査が合った）。RVSWD ではさらに `form`（`short`: WCH の 52 ビットのフレーム、`long`: CH32V103 が受ける 85 クロックのフレーム）と `status`（long 形式の DMI の結果。0 は完了、2 は失敗、3 は busy）、どの target も答えなかった long のフレームには `answered: false`（その `data` と `status` は null。頼んだだけで、行われていない）。RVSWD ではさらに `bits`（フレームのビットを、don't-care のものも含めてサンプルしたまま。フレームどうしをビットで比べる）、`period_ns`（クロックのサンプルの間隔の中央値）、`fields`（フレームの欄を名前で、ビットの文字列で: short は `addr rw header_parity park ctl data data_parity park2 status pad stop` と `header_parity_ok` / `data_parity_ok`、バーストは `header`、`aux`（語ごとの 6 ビット）、`trailer`、long は `host_addr host_data op bit41 target_addr target_data status bit83 last`）、`repaired`（どの形にも無い長さのフレームを、ひげのクロックを抜いて読んだとき、抜いたものの tick）。SWIO ではさらに `fast`（data0 を続けて読む 33 パルスのフレーム。data0 の読みの後でだけ取る。LinkE が接続の前に送るような、ぽつんとある 33 パルスは未解釈）、`space_ns`（パルスの間のアイドルの時間の中央値。ビットによらず同じ）、`pitch_1_ns` / `pitch_0_ns`（1 の後と 0 の後の、パルスの始まりから次の始まりまでの中央値。パルスの low と間の和なので、0 の方が長い）、`short_median_ns` / `long_median_ns`、フレームのパルスを短い（1）と長い（0）の low の境（`params.split_ns`。線の幅の 2 つのかたまりから決める）と比べたもの: `short_max_ns`（いちばん長い短い low）、`long_min_ns`（いちばん短い長い low）、`rw_low_ns`（R/W のビットの low。長い low が縮んで境を越えると、読みが書き込みになる）、`rw_gap_ns`（R/W のビットの終わりから、最初のデータのパルスまで。target が答える読みの方が長い）。答えのあった long のフレームには `answer_delay`（`median`、`max`）: target が、こだまするビットへ SWDIO を変えた時点を、そのビットの前の立ち上がりからのクロックの周期の数で表したもの（0〜1 は間に合った、1 を超えると読まれる縁より後）。こだま（host の番地と、書き込みなら host のデータ）が変わるビットで測る。`answer_delay_ns` は同じものを ns で、中央値が 1 を超えたら `late: 1`（答えが 1 クロックずれた。読んだビットは target のものではない。CH32V103 がリセットの間にときどきそうする）。`dm`: デバッガが hart に何をしたかを、RISC-V の Debug Module（抽象コマンドと、プログラムバッファで動かしたプログラム。data レジスタのメモリの番地は hartinfo から、無ければプログラムから学ぶ: いちばん多くメモリを読める番地）から読んだもの。`op`（`MEMR` / `MEMW` / `REGR` / `REGW`。data レジスタでないレジスタのバーストは `BURST`。data0 のバーストはその読みで、1 語ずつ `MEMR`）、`addr`（メモリのアドレスか、Access Register のレジスタ番号）、`value`（null は線の上に見えなかった）、`size`（バイト数）、`s`（それを起こした DMI のアクセスの tick: 抽象コマンド、プログラムの実行）、`e`（値を見せた読みの tick） |
| ほか | `analyze` の書き出しと同じ |

上の層を読めても、下の層は横に残す（`sd` と `transfers`）。`depth: "device"` では、`devices` にバスの上で
合ったデバイスを出す（`analyze --depth device` と同じ）。

### 3.3 uninterpreted

`[{"s", "e", "edges"}]`: バスのピンの活動のうち、復号したどの項目にも入らないもの。バスのすべてのピンの変化を、
`idle_ticks` のあいだ変化が無い所で切ってかたまりにする。項目がその中で（またはその `idle_ticks` 前から）
始まらず、どの項目の中にも無いかたまりが、未解釈: START の無い SCL のクロック、復号器が読めなかった転送、雑音など。
クロックのあるバスのフレームのように項目に終わり（`e`）があるときは、項目を含むかたまりの中も見る: どの項目（の前後
1、2 クロック）にも入らない変化を、`idle_ticks` のあいだ変化が無い所で切って未解釈とする（読めたフレームの間の、
どのレイアウトでも読めなかったフレーム）。クロックのあるバスの `bits` と `frames` は、切った所であって読んだものではなく、
ここでは何も覆わない。

区間と `capture.lost` と合わせて、3 つを区別する:

- **何も起きなかった**: 区間の中で、ピンに変化が無い
- **見えなかった**: 区間の間、または欠落
- **読めなかった**: 未解釈のかたまり

## 4. analyze が見つけたもの

`wireskein analyze CAPTURE`（Python では `wireskein.decode.analyze_file(path, hints, ...)`）は、役割を与えずに、同じ
家族の文書を書きます: `format`、`wireskein`、`capture`（§2）と `parts[]`。ただし `spec` の代わりに `hints`（試すよう
頼んだもの。`--hint` / `--devices`）を持ち、各区間は `buses` の代わりに次を持ちます。

| キー | 意味 |
| --- | --- |
| `part`、`ticks`、`tick0`、`start_ns` など | §2.1 と同じ |
| `claims[]` | 証拠が支えるもの: `protocol`、`roles`（役割 → チャンネル）、`verdict`（`confirmed` / `likely` / `ambiguous` / `unknown-sync`）、`score`、`margin`、`params`。さらに `result`（`--mode final`、`--depth` まで）、`layers` と `evidence`（`--mode all`）、または `--select` で選んだ層、`devices`（`--depth device`）、`runner_up`（`--alternatives`）。刻みは区間の刻み 0 から |
| `relations[]` | 一緒に読む主張の組（コマンドの行とその応答）。`scpi` とその `exchanges`、UART の組の上のほかの人のプロトコル（`uart_pair`、[独自のプロトコル](protocols.ja.md) §3.1）とその `messages`、`checks_passed`、`checks_failed` |
| `analysis_seconds` | 文書の一番上: 解析にかかった秒（実行ごとに変わる） |
| `window`、`segment`、`events` | `--window` / `--segment` のとき（1 つの区間の中の範囲。区間が 2 つ以上なら `--part` で選ぶ） |

試す例は tests/data/examples にあります（WCH-LinkE の SWIO と RVSWD のやりとり、生成した I2C と SPI のバスを並べたもの）。それぞれ、持っているバスを `<名前>.truth.json` に書いてあります: `wireskein analyze tests/data/examples/mixed-0.wireskein`。WireSkein のファイルは区間ごとに解析します。`.sr` と VCD は 1 つの区間です。何をどれだけ確かに
主張するかは版で変わることがあります（評価のコーパスで調整するため）。キーは保ちます。

## 5. 安定性

形式の番号は、版 1 まで 1 のまま（互換の無い変更でも）。読み手は `format` を確かめ、知らないキーは無視する。
