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
| `buses[].params` | 任意。指示として渡す（`uart` の `baud` など）。`spi` の `mode`（0〜3）は標本のエッジを決める。無ければ線からモードを読む（capture-test-guide §4.5）。active high の CS には `cs_active`。`bit_order`（既定の `msb` か `lsb`。でたらめなデータからは分からない）。バスは線を共有してよい: CS ごとに相手のいる 1 つの SPI のバスは、相手ごとに 1 つのバスとして書き、CLK、MOSI、MISO をどれにも書く（§3.3）。`i2c` の `freq_hz`（SCL の速さ）は、1 つの遅い縁のまたぎがどこまで広がってよいかを決める（その半周期の 3 分の 1、多くて 1000 ns（t_r）。無ければ縁ごとにその辺りのクロックから測る。5 MS/s 未満ではしない）。I2C は、いつも I2C の入力が見るとおりに読む: 50 ns（t_SP）より短いパルスを除き（ただし、クロックが来るはずの所にあり、前後にまるごとの Low がある SCL の High は除かない: 遅い立ち上がりで短く切られたクロックで、バスの機器はクロックとして扱う）、しきい値を何度もまたいだ遅い縁は、最後の変化で 1 つとして読む（それで何か変わったときは `params.deglitch` が `i2c_input`） |
| `split` | 任意。長い区間を区切って読む方法（[長いキャプチャ](long-captures.ja.md) §3）: 辞書、`"key=value,..."`、`"off"` |
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
| `parts[]` | 区間ごとに 1 つ。隙間なく続く区間の並びは、1 つとして読んで 1 つ（wireskein-format §3.4）。解析は隙間をまたがない。変化が `max_edges` より多い区間は区切って読み、`split` が付く（[長いキャプチャ](long-captures.ja.md) §3.2） |

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
- `roles_unknown`: クロックのある方式（I2C、SPI、RVSWD）に、その方式にない役目を渡したとき（RVSWD に `swdio`。RVSWD の
  役目は `clk`・`dio`）`{"given": [...], "roles": [...]}`。そうした役目はヒントにならず、バスは線だけで見つかる所しか
  読めない。1 本のピンを 2 つの方式で指定したバスは、分けて解析してそれぞれ読む。
- `params`: 復号器が選んだもの（標本のエッジ、UART のボーレートなど）。UART の文字の最後のビットがほかのビットの
  パリティになっているときは `parity`（`even` か `odd`）と `data_bits` があり、バイトはパリティを除いたもの（7 ビットと
  パリティは、パリティのない 8 ビットと同じ長さ。`L` はスタートとストップを含む文字のビット数）。
- `fit.score`: 線がどれだけ合ったか（0〜1、analyze の得点）。短いケースでは低い: 測るのは証拠の量で、正しさではない。
- `fit.claimed`: analyze が自分で探したら、ちょうどこの復号を主張したか。主張したなら `fit.verdict` にその判定
  （`confirmed` / `likely` / `ambiguous`）。

- `reading`: バスがどう読めたかを、することにつながる数で（[うまく取る](capturing.ja.md)）: `frames`（フレームとして
  読めたもの）、`ok`（検査が合った）、`unanswered`（どの target も答えなかったフレーム）、`restored`、`repaired`
  （抜けたクロックを補って読んだ、ひげのクロックを抜いて読んだフレーム）、`unread` と `unread_edges`（どのフレームとも
  読めない所とその変化の数）、`rate_ok`（どの速さも十分なサンプリング）。クロックのあるバスでは `glitches`（クロックが
  十分遅く、半周期がそんなに短くない所での、クロックの 1 サンプルのパルス）と `ringing`（データの線 → どのビットも
  そんなに短くない遅いクロックでの、3 サンプル未満のパルス）、それを数えた変化の数（`clock_edges`、`data_edges`）と
  そのうちノイズの変化の数（`glitch_edges`、`ringing_edges`）、その割合: `glitch_share`、`ringing_share`（データの線 → 割合）、どれかが 1 % を超えると `noisy`。
  ノイズ（反射、隣の線を拾う、グラウンドが遠い）の印。数は記録の長さで増えるので、判断は割合で。I2C では
  `same_sample` も: SDA が SCL の縁と同じサンプルで変わった回数（セットアップかホールドが 1 サンプル未満。無い START
  や STOP に読めることがある。コントローラの波形か、それに対して低すぎるレート）。
  どのバスにも `line_changes`（線 → `changes`、最後の変化の tick `last`）。起きたときは `stopped`（`line`、最後の変化の
  `tick`、`others_after`: その後にバスのほかの線が 16 回以上変わったのに、黙っている線）と `oscillating`（`line`、`s`、
  `e`、`changes`: 前の変化から 2 サンプル以内の変化が 1000 回以上続く）。どちらも動いているバスではなく、入力が固まったか
  振れているか、線が外れたもの。キャプチャがプローブのフィルタを記録していれば、`glitch_filter_ns` も付く（グリッチ
  フィルタは、遅く雑音の乗った縁で固まるか発振することがある）。
  analyze の主張にも、`unread` を除いて付く。区切って読むと、区切りごとに自分の tick で数えて足す。止まった線は、
  最後の変化より後の区切りで判断する。
- `clock_speeds`（クロックのあるバス）: 読めたフレームから見た、クロックの速さ（フレームごとの `period_ns` と
  `high_ns` は、そのクロックの中央値なので反射のひげは数えない。フレームに付く）。周期が、ある速さのいちばん速いものの
  1.25 倍以内のフレームは、どこにあってもその速さ。速い順に、`period_ns`、`freq_hz`、`high_ns`、`low_ns`（中央値）、
  `frames`、`s` と `e`（最初のフレームの始まり、最後のフレームの終わり）、`rate_ok`: high か low が 3 サンプルより
  短いと false。high が 2 つのサンプルの間に落ち、クロックが抜けることがある（デバッガは途中で速さを変える: WCH-LinkE は
  ゆっくり接続してから速く読む）。
- `undersampled`: レートに対して速すぎる速さ（`rate_ok` が false）があるとき: `clock`、`rate_hz`、`frames`（その速さで
  読めたもの）、`period_ns`、`high_ns`、`low_ns`（いちばん厳しいもの）、`least_rate_hz`（high と low の短い方に
  3 サンプル: 抜けない）と `best_rate_hz`（5 サンプル: 対称なクロックなら 1 周期に約 10）。このレートで測った high と low から、`restored`（抜けたクロックを補って読んだ RVSWD のフレームの数）。クロックが 1 つ足りないフレームは、
  読みが 1 つに決まるときだけ読む（抜けたかもしれない、同じくらいありそうな場所のどれで補っても同じビットになり、
  検査も合う）。ほかは未解釈。`decode` と `analyze` は stderr にも出す。

### 3.2 result

analyze の書き出し（`--mode final`、`depth` まで）の層。刻みは区間の刻み 0 から:

| 方式 | 層 |
| --- | --- |
| `i2c` | `transactions`: `s`、`e`、`addr`、`rw`、`addr_ack`、`bytes`（16 進）、`acks`、`start_cond`（S / Sr）、`end_cond`（P / Sr / null: 区間の終わりで開いたまま）、`scl_low_ticks`、`restored`（補ったクロックの tick: 遅い立ち上がりで 1 サンプルより短く切られたもの。読みが 1 つに決まる所に置く）、`scl_waits`（`[{after_word, bit, ticks, s}]`）。区間の終わりで切れたものは `complete: false` と `pending_bits` |
| 独自のプロトコル（[独自のプロトコル](protocols.ja.md)） | バスの下に、その名前で: `{"messages", "checks_passed", "checks_failed"}`。チェックが通ったときに出て、そのときはバスの最終の読み |
| `spi` | `transfers`: `s`、`e`（最初と最後の標本のビット）と、データのピンごとのバイト（16 進、チャンネルの名前の下）。SD のコマンドを読めたら、横に `sd`（capture-test-guide §4.8）。CS があれば `modes`: SCK の待機のレベルに合う 2 つのモードそれぞれの `readable`（そのサンプルの辺でどのデータ線も変わらない）と、そのモードで読んだ `bytes`（MSB から）。`modes_ok` は読めるもの。読めるモードが 1 つならモードが決まる。読めるモードのバイトが同じなら、見分けなくてよい |
| `uart` | `nmea`、`modbus_rtu`（チェックサム / CRC が通ったもの）、`lines` か `bytes`（`{"s": [...], "hex"}`。9 データビットなら `hex` は文字ごとの下位 8 ビット、`bit8` は 9 ビット目を `"0"` / `"1"` で順に。マルチドロップではアドレスの印）: 最終の読みは、読めたもののうちこの順で最初のもの（GPS の文は、テキストの行ではなく `nmea`） |
| `can` | `frames`: `s`、`id`、`ide`（1: 拡張の識別子）、`dlc`、`data`（16 進。DLC が運ぶバイト。CAN FD は 64 まで）、`ack`、`ok`。CAN FD のフレームは `fd: true`、`brs`、`esi` と、BRS のあるものは `data_ratio`（データ相の速さの名目の速さに対する比）も。FD のフレームのある線は `params.fd`、`bitrate`、`data_bitrate`（フレームが 1 つの速さに切り替わるとき） |
| `dali` | `frames`: `s`、`e`、`kind`（`forward`: コントローラの 16 ビット、`backward`: 機器の 8 ビット）、`data`（コマンドか明るさのバイト、応答）、前向きのフレームは `addr_byte` とその読み `to`（`short` / `group` / `broadcast` / `special`）、`address`、`selector`（`command` / `arc_power`）、`ok`。役割は `dali` |
| `cec` | `messages`: `s`、`e`、`initiator`、`destination`（論理アドレス。15: ブロードキャスト）、あれば `opcode` と `data`（16 進、オペランド）、`acked`（すべてのブロックの ACK: 宛先のあるメッセージは相手が 0、ブロードキャストは 1 のまま）、`complete`（最後のブロックの EOM を見た）。役割は `cec` |
| `lin` | `frames`: `s`（break）、`id`、`data`（16 進）、`ok`（sync、識別子のパリティ、チェックサムが合う） |
| `dmx512` | `packets`: `s`（break）、`start_code`、`data`（16 進。スロット）。線の Run から出た速さではなく規格の 250 kbaud で読んだときは `params.rate_given: "dmx512"`（スロットが 1 つの値ばかりだと速さが出ない）。`params.idle` は線の待機のレベル（1。RS-485 の反転側の線は 0） |
| `swd` | `packets`: `s`、`e`（最初と最後の標本のビット）、`apndp`（`DP` / `AP`）、`rnw`（`W` / `R`）、`a`（A[3:2]）、`ack`（`OK`、`WAIT`、`FAULT`）、`data`（OK のときだけ）、`ok` |
| `mdio` | `frames`: `s`、`e`（最初と最後の標本のビット）、`op`（Clause 22: `W` / `R`。Clause 45: `ADDR` / `W` / `R` / `RINC`）、`phy`、`reg`（Clause 22）か `dev`（Clause 45 の MMD）、`data`、`ok`。Clause 45 の読み書きには `addr` も（レジスタ: そのポートとデバイスの最後の `ADDR`、`RINC` ごとに 1 進む。`ADDR` を見る前は無し） |
| `ps2` | `frames`: `s`、`e`、`data`（バイト）、`ok`（スタート、奇数パリティ、ストップ、ホストからデバイスへは ACK が合う）。役割は `clk` / `data`。`params.sample_edge`（`fall`: デバイスからホスト、`rise`: ホストからデバイス） |
| `sdmmc` | `frames`（SD モードの CMD 線）: `s`、`e`、`dir`（`CMD` ホストからカード、`RSP` 48 ビットの応答、`R3` CRC のない OCR、`R2` 136 ビットの CID / CSD）、`index` と `arg`（CMD / RSP）、`ocr`（R3）、`reg0`〜`reg3`（R2: CRC の前の 120 ビット、32 + 32 + 32 + 24）、`ok`（CRC7 が通った）。役割は `clk` / `cmd` |
| `spdif` | `subframes`: `s`、`e`、`preamble`（`B`: ブロックの最初のフレームでチャネル A、`M`: チャネル A、`W`: チャネル B）、`audio`（24 ビット: 補助 4 ビットと 20 ビットの音声、LSB から）、`v`、`u`、`c`、`ok`（偶数パリティが通った）。役割は `data` |
| `swim` | `transactions`: `s`、`e`、`cmd`（`ROTF` / `WOTF` / `SRST`）、ROTF / WOTF は `count`、`addr`（24 ビット）、`data`（16 進。ROTF はターゲットの、WOTF はホストのバイト）、`ok`（すべてのフレームのパリティが通り ACK された）。役割は `swim` |
| `usb` | `packets`: `s`、`e`（パケットの最初の K と SE0）、`pid`（`SOF`、`IN`、`OUT`、`SETUP`、`DATA0` / `DATA1` / …、`ACK`、`NAK`、`STALL` など）、トークンの `addr` と `endp`、SOF の `frame`、データの `data`（16 進、CRC16 は除く）、`ok`（CRC が通った）、または `error`（`stuff`、`sync`、`pid`、`short`）。役割は `dp` / `dm`、`params.speed`（`low` / `full`）。最終の読みは `transactions`: `s`、`e`、`token`（`IN` / `OUT` / `SETUP` / `PING`、または `SOF` とその `frame`）、`addr`、`endp`、`data_pid` と `data`（データのパケットがあれば）、`handshake`（`ACK` / `NAK` / `STALL` / `NYET`、あれば）。SETUP の 8 バイトは `setup` にも（`bmRequestType`、`bRequest`、`wValue`、`wIndex`、`wLength`、`direction`、`type`、標準の要求なら名前を `request`） |
| `usb_pd` | `packets`: `s`、`sop`（`SOP`、`SOP'`、`SOP''`、`Hard Reset`、`Cable Reset`）、`header`、`type`（`GoodCRC`、`Accept`、`PS_RDY`、`Source_Capabilities`、`Request`、`Vendor_Defined` など）、`id`、`power_role`、`data_role`、`spec_rev`、`objects`（データオブジェクト、32 ビットずつ）、`ok`（CRC32 と EOP が合う）、または `error`。役割は `cc` |
| `wiegand` | `frames`: `s`、`e`、`bits`（フレームの長さ）、`data`（送られた順のビット）、`value`（16 進）。26 と 34 ビットのフレームは `ok`（前半の偶数パリティと後半の奇数パリティ）、26 ビットは `facility` と `card` も。役割は `d0` / `d1` |
| `i2s` | `frames`: `s`、`left`、`right`（符号付き、`params.word_bits` ビット）。`params.format`（`i2s`、`left`、`right`: 語がスロットのどこにあるか。詰め物から決める。語がスロットを埋めて決め手がないときは I2S とし `format_assumed`）、`slot_bits`。WS が最初に変わる前のスロットは読まない。TDM（`params.format: "tdm"`。フレームに 1 回のフレーム同期と 3 つ以上のスロット）: `frames`: `s`、`slots`（フレームのワード、符号付き）。`params.slots_per_frame`、`slot_bits`、`word_bits`、`offset`（1: フレーム同期が有効になった 1 ビット後に最初のスロットの MSB、DSP モード A。0: 同時、モード B）、`fs_active`（フレーム同期の有効なレベル）、パディングが前にあれば `justify: "right"`。パディングでスロットの幅（32 ビット）やオフセット（1）が決まらないときは `format_assumed` |
| `jtag` | `scans`: `s`（Capture の状態）、`type`（`IR` / `DR`）、`bits`、`tdi`、`tdo`（16 進。シフトしたビットで、最初にシフトしたものが最下位）。`params.ir_bits`（IR のスキャンのいつもの長さ）。TMS で TAP の状態が決まる前のスキャンは読まない。2 線の cJTAG（IEEE 1149.7、OScan1）: 役割は `tckc`、`tmsc`、`params.cjtag: "oscan1"`、`scans` は同じ（OScan1 のオンライン化の後のサイクルから。TCKC 3 サイクルで JTAG の 1 サイクル） |
| `dht` | `readings`: `s`（ホストの開始）、`bytes`（16 進、5 バイト）、`checksum_ok`、`rh`（%）、`t`（℃）。`params.model`（`dht11`: 整数の値で 2 バイト目が 0。`dht22`: 0.1 刻み。AM2301/AM2302/AM232x、RHT03 も） |
| `onewire` | `resets`: `s`（リセットの立ち下がり）、`reset_ticks`、`presence`、`data`（16 進。その後のバイト、LSB から。読み出しのスロットは運んだビットとして）。`params.speed`（`standard` / `overdrive`）。標準の速さで Overdrive Skip / Match ROM を送った後は、`overdrive_after_bits`（切り替わる前に読んだビット数）と、その後の標準の長さのリセットまでのリセットに `speed: "overdrive"` |
| `rvswd`、`swio` | WCH のデバッグの伝送路（2 線と 1 線）。その上の層は RISC-V のもので、どちらでも同じ。`dmi`: 運ぶ RISC-V の DMI のアクセスで、`s`、`e`（RVSWD: フレームの最後のサンプルの縁）、`op`（`R` / `W`）、`addr`、`reg`（RISC-V Debug Specification 0.13 のレジスタの名前。仕様の外の WCH のレジスタは `wch_7c`〜`wch_7f`、どちらでもないアドレスは null）、`data`、`ok`（フレームの検査が合った）。RVSWD ではさらに `form`（`short`: WCH の 52 ビットのフレーム、`long`: CH32V103 が受ける 85 クロックのフレーム）と `status`（long 形式の DMI の結果。0 は完了、2 は失敗、3 は busy）、どの target も答えなかった long のフレームには `answered: false`（その `data` と `status` は null。頼んだだけで、行われていない）。RVSWD ではさらに `bits`（フレームのビットを、don't-care のものも含めてサンプルしたまま。フレームどうしをビットで比べる）、`period_ns`（クロックのサンプルの間隔の中央値）、`fields`（フレームの欄を名前で、ビットの文字列で: short は `addr rw header_parity park ctl data data_parity park2 status pad stop` と `header_parity_ok` / `data_parity_ok`、バーストは `header`、`aux`（語ごとの 6 ビット）、`trailer`、long は `host_addr host_data op bit41 target_addr target_data status bit83 last`）、`repaired`（どの形にも無い長さのフレームを、ひげのクロックを抜いて読んだとき、抜いたものの tick）、`restored`（形より 1 クロック足りないフレームを、サンプルの間に落ちたクロックを補って読んだとき、その推定の tick: `undersampled`、§3.1）。SWIO ではさらに `fast`（data0 を続けて読む 33 パルスのフレーム。data0 の読みの後でだけ取る。LinkE が接続の前に送るような、ぽつんとある 33 パルスは未解釈）、`space_ns`（パルスの間のアイドルの時間の中央値。ビットによらず同じ）、`pitch_1_ns` / `pitch_0_ns`（1 の後と 0 の後の、パルスの始まりから次の始まりまでの中央値。パルスの low と間の和なので、0 の方が長い）、`short_median_ns` / `long_median_ns`、フレームのパルスを短い（1）と長い（0）の low の境（`params.split_ns`。線の幅の 2 つのかたまりから決める）と比べたもの: `short_max_ns`（いちばん長い短い low）、`long_min_ns`（いちばん短い長い low）、`rw_low_ns`（R/W のビットの low。長い low が縮んで境を越えると、読みが書き込みになる）、`rw_gap_ns`（R/W のビットの終わりから、最初のデータのパルスまで。target が答える読みの方が長い）。答えのあった long のフレームには `answer_delay`（`median`、`max`）: target が、こだまするビットへ SWDIO を変えた時点を、そのビットの前の立ち上がりからのクロックの周期の数で表したもの（0〜1 は間に合った、1 を超えると読まれる縁より後）。こだま（host の番地と、書き込みなら host のデータ）が変わるビットで測る。`answer_delay_ns` は同じものを ns で、中央値が 1 を超えたら `late: 1`（答えが 1 クロックずれた。読んだビットは target のものではない。CH32V103 がリセットの間にときどきそうする）。`dm`: デバッガが hart に何をしたかを、RISC-V の Debug Module（抽象コマンドと、プログラムバッファで動かしたプログラム。data レジスタのメモリの番地は hartinfo から、無ければプログラムから学ぶ: いちばん多くメモリを読める番地）から読んだもの。`op`（`MEMR` / `MEMW` / `REGR` / `REGW`。data レジスタでないレジスタのバーストは `BURST`。data0 のバーストはその読みで、1 語ずつ `MEMR`）、`addr`（メモリのアドレスか、Access Register のレジスタ番号）、`value`（null は線の上に見えなかった）、`size`（バイト数）、`s`（それを起こした DMI のアクセスの tick: 抽象コマンド、プログラムの実行）、`e`（値を見せた読みの tick） |
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

spec のほかのバスと線を共有するバス（1 つの SPI のバスの、CS ごとの相手）では、ほかのバスが読んだ変化は未解釈に入れず、`shared` に置く: `[{"s", "e", "edges", "bus"}]`（`bus` はそのバスの spec の中の番号、`edges` はそのバスが読んだ変化の数）。`reading` には `shared` と `shared_edges` の数が付き、`unread` は残りだけを数える。ほかのバスの転送が続くあいだに自分の線（CS）が動かないのは、止まった線（`reading.stopped`）にしない。相手を spec に書かなければ、その時間は未解釈のまま。

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
| `claims[]` | 証拠が支えるもの: `protocol`、`roles`（役割 → チャンネル）、`verdict`（`confirmed` / `likely` / `ambiguous` / `unknown-sync`）、`score`、`margin`、`params`。さらに `result`（`--mode final`、`--depth` まで）、`layers` と `evidence`（`--mode all`）、または `--select` で選んだ層、`devices`（`--depth device`）、`runner_up`（`--alternatives`）。CS ごとに相手のいる 1 つの SPI のバスでは、主張は相手ごとで、同じ `bus` を持つ（共有の CLK とデータ線はどの主張にも入る）。刻みは区間の刻み 0 から |
| `relations[]` | 一緒に読む主張の組（コマンドの行とその応答）。`scpi` とその `exchanges`、UART の組の上のほかの人のプロトコル（`uart_pair`、[独自のプロトコル](protocols.ja.md) §3.1）とその `messages`、`checks_passed`、`checks_failed` |
| `analysis_seconds` | 文書の一番上: 解析にかかった秒（実行ごとに変わる） |
| `window`、`segment`、`events` | `--window` / `--segment` のとき（1 つの区間の中の範囲。区間が 2 つ以上なら `--part` で選ぶ） |

試す例は tests/data/examples にあります（WCH-LinkE の SWIO と RVSWD のやりとり、生成した I2C と SPI のバスを並べたもの）。それぞれ、持っているバスを `<名前>.truth.json` に書いてあります: `wireskein analyze tests/data/examples/mixed-0.wireskein`。WireSkein のファイルは区間ごとに解析します。`.sr` と VCD は 1 つの区間です。何をどれだけ確かに
主張するかは版で変わることがあります（評価のコーパスで調整するため）。キーは保ちます。

## 5. 安定性

形式の番号は、版 1 まで 1 のまま（互換の無い変更でも）。読み手は `format` を確かめ、知らないキーは無視する。
