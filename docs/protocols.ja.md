# 独自のプロトコル

[English](protocols.md)

WireSkein は UART、SPI、I2C を自分で復号します。その上に乗るプロトコル（機器の命令のフレーム、センサーの報告、ブートローダーのパケットなど）は、WireSkein に手を入れずに分析に足せます。足すと、`wireskein analyze` がそのメッセージをバスの下に出し、試験はほかの検査と同じに確かめられます（§1〜§4）。クロックとデータ、または 1 本の線の上で、それ自体がバスであるプロトコルは、宣言の定義として足します（§5）。

## 1. プロトコル

プロトコルは、次を持つ Python のオブジェクトです。

| 属性 | 意味 |
| --- | --- |
| `name` | 出すときの名前。英数字と `_`。WireSkein の層の名前とは別のもの |
| `over` | 乗るバス。`"uart"`、`"spi"`、`"i2c"` のタプル |
| `decode(bus)` | 復号したバス 1 本を読み、`{"messages": [...], "checks_passed": n, "checks_failed": m}` を返す |

`bus` は、分析が書き出す形のバスです（[分析の形式](analysis-format.ja.md) §4）: `roles`（役割ごとのピン）、`tick_hz`、その層。UART なら `bytes`（`{"s": [tick...], "hex": "..."}`）、`chars`（それぞれ `{"s", "value", "ok"}`）、`breaks`。SPI なら `transfers`（それぞれ `{"s", "e", <ピン>: 16 進}`）。I2C なら `transactions`。プロトコルは WireSkein の内部の型を見ないので、分析の形式が変わらない限り動き続けます。

- **メッセージ**は、ただの JSON の値です。それぞれの最初の tick を `"s"` に入れると、読み手とビューアがそこを見つけられます。
- **チェック**が証拠です: チェックサム、合う長さ、数えていく順番の番号。でたらめのバイトでは偶然にしか通らないチェックが、そのバスがこのプロトコルを運んでいることを言います。分析は、通った数が多く失敗が少ないほど、バスの確かさを上げ（Modbus RTU と同じ）、チェックが通ったときだけ、そのプロトコルをバスの最終の読みとして出します。チェックの無いプロトコルは何も出しません: 証拠が無ければ、どのバイトも何にでも読めるからです。
- `decode` の例外は、バスと一緒に出ます（`error`）。分析は続きます。

## 2. 足し方

- スクリプトや試験の中で: `wireskein.protocols.register(protocol)`。
- パッケージから: entry point の `wireskein.protocols` に宣言すると、パッケージを入れたときにいつも使えます。

```toml
[project.entry-points."wireskein.protocols"]
demo_link = "demo_link:PROTOCOL"
```

## 3. 見本: demo_link

`examples/protocols/demo_link.py` は、このガイドのために作ったプロトコルです。多くの機器のプロトコルと同じ形をしています。

```
0xA5 | len | seq | cmd | payload[len] | crc8
```

- `0xA5` がフレームの始まりです。途中から読み始めた読み手は、フレームが合う次の `0xA5` を探します（読み直し）。
- `len` はペイロードの長さ（0〜32）、`seq` はフレームを数え、`cmd` は命令です。応答は `cmd` のビット 7 を立て、要求の seq を持ちます。
- `crc8` は、`len`、`seq`、`cmd` とペイロードの CRC-8（多項式 0x07、初期値 0）で、これがチェックです。
- UART ではフレームがバイトの流れの中に続き、SPI では CS の窓ごとに、MOSI に 1 つ（要求）、MISO に 1 つ（応答）が乗ります。1 つの `decode` が両方を読むので、`over = ("uart", "spi")` です。

`wireskein analyze` では、バスの `result` の下に次のように出ます。

```json
"demo_link": {"messages": [{"s": 1210, "seq": 1, "cmd": 16, "reply": false, "payload": "0102"}, ...],
              "checks_passed": 4, "checks_failed": 0}
```

その試験（`tests/test_protocols.py`）は、WireSkein の生成器で信号を作ります。正解を、試験される機器から取らないためです。実機では、プローブが既存のライブラリで出した信号を正解にします。試験されるファームウェアの出力ではありません: `examples/protocols/demo_link_p4.wireskein` は、OEP のプローブ（ESP32-P4）が ESP-IDF の UART のドライバ（115200 8N1）でフレームを出し、そのピンを取り込んだもので、送ったものは `demo_link_p4.truth.json` にあります。demo_link が無いと、分析はその UART を `likely` と読み、あると `confirmed` で、3 つのフレームとその CRC を読みます。同じキャプチャには SPI のフレームもありますが、プローブ自身がピンを動かす SPI の controller で出したもので、既存のライブラリではないので、正解にはしません。

## 4. 試験で確かめる

```python
from wireskein.runlog import protocol

with rec.section(2, "ping", expect=[protocol("demo_link", "uart", {"data": "PA2"}, baud=115200,
                                             messages=[{"seq": 1, "cmd": 0x10}, {"seq": 1, "cmd": 0x10, "reply": True}])]):
    ...
```

- `bus` は `"uart"`、`"spi"`、`"i2c"`、`roles` は役割ごとのピン（`{"data": ...}`、`{"clk", "mosi", "miso", "cs"}`、`{"scl", "sda"}`）です。
- `messages` は順に比べ、それぞれ書いたキーだけを比べます（`"s"` など書かないキーは比べません）。`messages` が無ければ、メッセージが 1 つ以上あること。
- チェックの失敗は `checks_failed_max`（既定 0）まで。
- これで言えないきまりは、自分の検査（[ガイド](capture-test-guide.ja.md) §4.10）か、試験の判定（`rec.result`、ガイド §3.3）にします。

## 5. 線そのものの上のプロトコル: 定義

自分がバスであるプロトコル（クロックとデータの上、または 1 本の NRZ の線の上の、独自のフレーム）は、WireSkein の I²C、SPI、SWD、CAN と同じく TOML で宣言的に書き（[設計](design.ja.md) §3.2）、定義のディレクトリとして外から足します。

- **定義のディレクトリ**は、WireSkein の `decl/` と同じ形です: 直下の `*.toml` がプロトコル、`devices/<バス>/...` がデバイスの定義、`markers/*.toml` がマーカーの方言。どれも無くてかまいません。
- **足し方:** スクリプトや試験では `wireskein.protocols.add_definitions(パス)`、パッケージからは entry point の `wireskein.definitions`（`NAME = "module"`: その module のディレクトリに定義がある。またはパスを返す callable）。
- **ビットの層**は WireSkein が持つものを、定義の `input` で名前で選びます: `"nrzbits"`（1 本の線。ビットの時間を線から読み、10 ビット以上の待機の後をフレームとする）、または `"frames.select"`、`"frames.gap"`、`"frames.startstop"`（クロックとデータ。選択の線、隙間、START / STOP で区切る）。独自の線の符号（マンチェスターなど）は、WireSkein の中にビットの層が要るので、定義では書けません。
- **分析**は、それを独自のプロトコルとして出します。フレームは `result.frames` の下（`format = "hex"` の欄はバイトの 16 進）で、チェック（決まった値、CRC）がその証拠です。
- **試験**は、`protocol(NAME, None, {役割: ピン}, messages=[...])` でフレームを確かめます（`bus` が None: そのプロトコル自身がバス）。
- WireSkein のプロトコルの名前は断ります。

### 5.1 見本: demo_wire

`examples/definitions/demo_wire.toml` は、このガイドのために作った 1 本線のプロトコルです。

```
待機 1 | 開始 0 | addr 7 | cmd 4 | len 4 | data 8 x len | crc8 | 終わり 1      （最上位ビットから）
```

- CRC-8（多項式 0x07、初期値 0）は、アドレスからデータまでに取ります。
- 送り手は、同じ値が 5 ビット続いたら反対の値を 1 ビット入れます（ビットスタッフィング）。開始ビットから CRC の終わりまでで、CAN と同じく、どのデータでも受け手がビットの時間を保てるようにするためです。
- フレームとフレームのあいだは、10 ビット分以上の待機です。

```toml
[plugin]
name = "demo_wire"
input = "nrzbits"

[roles]
data = "data"

[[sequence]]
order = "msb"
stuffing = { after = 5, from = "sof", to = "crc" }
fields = [
  { name = "sof",  width = 1, expect = 0, hide = true },
  { name = "addr", width = 7 },
  { name = "cmd",  width = 4 },
  { name = "len",  width = 4 },
  { name = "data", width = "8 * len", format = "hex" },
  { name = "crc",  width = 8, hide = true },
  { name = "eof",  width = 1, expect = 1, hide = true },
]
checks = [
  { kind = "crc", field = "crc", from = "addr", to = "data", poly = 0x07, width = 8 },
]
```

その試験は、読む定義とは別に、自分の符号器で線を 1 ビットずつ書きます（`tests/test_protocols.py`）。
