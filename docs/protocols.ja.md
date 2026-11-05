# 独自のプロトコル

[English](protocols.md)

WireSkein は UART、SPI、I2C を自分で復号します。その上に乗るプロトコル（機器の命令のフレーム、センサーの報告、ブートローダーのパケットなど）は、WireSkein に手を入れずに分析に足せます。足すと、`wireskein analyze` がそのメッセージをバスの下に出し、試験はほかの検査と同じに確かめられます。

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

その試験（`tests/test_protocols.py`）は、WireSkein の生成器で信号を作ります。正解を、試験される機器から取らないためです。実機では、プローブが既存のライブラリで出した信号（OEP のプローブの UART や SPI など）を正解にします。試験されるファームウェアの出力ではありません。

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
