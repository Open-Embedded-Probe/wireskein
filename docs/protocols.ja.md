# 独自のプロトコル

[English](protocols.md)

WireSkein は UART、SPI、I2C、CAN、LIN などを自分で復号します。その上に乗るプロトコル（機器の命令のフレーム、センサーの報告、CAN のフレームの中のノードの値、UART の組の上のテキストの命令と応答など）は、コードを書かずに表で宣言します。宣言すると、`wireskein analyze` がそのメッセージをバスの下に出し、試験はほかの検査と同じに確かめられます（§4）。クロックとデータ、または 1 本の線の上で、それ自体がバスであるプロトコルは、拡張点ではありません。WireSkein 自身のそうしたプロトコルは、変わりうる分析のエンジンの一部です（§5）。

## 1. プロトコルの表

表は、定義のディレクトリ（[独自の分析](extending.ja.md) §2）の `protocols/<名前>.toml` に置きます。

```toml
format = "wireskein-protocol/1"
name = "acme.link"          # 持ち主の名前空間と自分の名前（英小文字、数字、_）
over = ["uart", "spi"]      # 乗るバス

[frame]                      # バイトのフレーム（§2）。UART の組の命令と応答なら [lines]（§3）
...
```

| `over` | 表が読むもの（[分析の形式](analysis-format.ja.md) §4 が書き出すバス） |
| --- | --- |
| `"uart"` | バイトの流れ（`bytes`）。フレームを探す |
| `"spi"` | 転送（`transfers`）ごと、データの線ごと。フレームを探す。メッセージに `line` が付く |
| `"i2c"` | トランザクション（`transactions`）ごとのバイト。フレームを探す。`addr` をフィールドやチェックで使える（`@addr`） |
| `"can"`、`"lin"`、`"dmx512"` | フレーム（`frames`、`packets`）ごとのデータを、表のフレーム 1 つとして丸ごと読む。フレームの値（`@id` など）を使える。バス自身が誤ったフレームは除く |
| `"uart_pair"` | 同じ速さの 2 本の UART で、メッセージが交互に続くもの。そのテキストの行を時刻の順に（§3） |

どのメッセージにも、最初の tick が `"s"` に入ります。プロトコルのフレームが通ったチェックと失敗したチェックが、その証拠です: 分析は、通った数が多く失敗が少ないほどバスの確かさを上げ（Modbus RTU と同じ）、チェックが通ったときだけ、そのプロトコルをバスの最終の読みとして出します。チェックが一度も通らないプロトコルは何も出しません: 証拠が無ければ、どのバイトも何にでも読めるからです。

## 2. フレーム: `[frame]`

```toml
[frame]
fields = [
    { name = "sof", equals = 0xA5 },                      # 0xA5 でなければならないバイト
    { name = "len", max = 32 },
    { name = "seq" },
    { name = "cmd", mask = 0x7F },                        # 出す値は mask のビット
    { name = "reply", of = "cmd", mask = 0x80, as = "bool" },   # 前のフィールドからもう 1 つの値
    { name = "payload", size = "len", as = "hex" },       # len の数だけのバイト
    { name = "crc", check = { kind = "crc8", over = ["len", "seq", "cmd", "payload"] } },
]
show = ["seq", "cmd", "reply", "payload"]                 # メッセージのキー（既定はチェック以外の全フィールド）
text = "#{seq} cmd {cmd:#x}"                              # 任意: ビューアが描く文字
```

| フィールドのキー | 意味 |
| --- | --- |
| `name` | 名前（必須） |
| `size` | バイト数: 数（既定 1）、前のフィールドの名前（その値）、`"rest"`（後ろのフィールドの手前まで） |
| `endian` | 2 バイト以上のフィールドの `"big"`（既定）か `"little"` |
| `equals`、`min`、`max` | そこにフレームがあるための値。合わなければフレームは無い（チェックの失敗ではない） |
| `mask` | 出す値は、このビットを下に詰めたもの（1 ビットなら `as = "bool"`） |
| `of` | 新しいバイトでなく、前のフィールドから読む（`mask` と一緒に） |
| `as` | `"hex"`（バイトの列）、`"bool"`。既定は、8 バイト以下なら数、それより長ければ 16 進 |
| `check` | このフィールドは、`over` に並べたフィールド（そのバイトを、その順に）のチェック: `kind` と下のオプション |

| チェックの `kind` | 中身 |
| --- | --- |
| `crc8`、`crc8_maxim`、`crc8_sae_j1850`、`crc16_modbus`、`crc16_ccitt_false`、`crc16_xmodem`、`crc16_kermit`、`crc32` | よく使う名前の CRC。`poly`、`init`、`refin`、`refout`、`xorout` で変えられる |
| `crc` | 自分の CRC: `width`、`poly`、`init`、`refin`、`refout`、`xorout` |
| `sum8`、`sum16`、`xor8` | バイトの和（2⁸、2¹⁶ で割った余り）か xor。`init` から始め、最後に `xorout` で xor |

`init` には、バスのフレームの値を名指せます（`"@id"`: CAN や LIN の識別子、`"@addr"`: I2C のアドレス）。

### 2.1 フレームの見つけ方

流れ（UART のバイト、SPI の転送、I2C のトランザクション）では、どのバイトでもフレームを試します。フィールドの `equals`、`min`、`max` が合わないか、フレームが終わりを越えるなら、そこにフレームは無く、次のバイトを試します。チェックが失敗したフレームはチェックの失敗に数え、次のバイトから探し続けます（同期の取り直し）。CAN、LIN、DMX512 のフレームでは、フレームのデータを丸ごと読みます: ちょうど表のフレーム 1 つでなければなりません。

### 2.2 カウンタ

```toml
sequence = { field = "seq", per = "@id", modulo = 256, note = " (seq skipped)" }
```

1 ずつ増える値（`per` があれば、その値ごとに）: どのメッセージにも `level` が付き、`ok`、または数が飛んだ所（フレームの欠けや二重送り）は `warn` で、`text` の後ろに `note` が付きます。飛びは見せるだけで、チェックの失敗にはしません。

### 2.3 ビューアのための文字

`text` はひな形です: `{name}` は値、`{name:x}` / `{name:#x}` / `{name:02x}` / `{name:d}` は書式付き、`{name|-}` は値が空なら `-`。`{@id}` はバスのフレームの値。

## 3. 命令と応答: `[lines]`

多くの機器は、1 本の UART でテキストの命令を受け、もう 1 本で応答します（AT コマンド、シェル、試験治具のプロトコル）。`[lines]` の表は `"uart_pair"` に乗ります: 分析は、同じ速さでメッセージが交互に続く 2 本の UART を組にし、メッセージを行に分け（行の tick は、メッセージの中に割り振る）、表はそれを時刻の順に読みます。

```toml
[lines]
command = '^#(?P<seq>[0-9a-f]{2}) (?P<verb>[A-Z][A-Z0-9_]*)(?P<args>(?: \S+)*)$'
answer = '^#(?P<seq>[0-9a-f]{2}) (?P<status>OK|ERR)(?P<values>(?: \S+)*)$'
event = '^!(?P<event>[A-Z][A-Z0-9_]*)(?P<values>(?: \S+)*)$'      # 任意: 聞かれずに知らせるもの
pair = "seq"                                  # 任意: この group が同じ応答だけが属する
words = ["args", "values"]                    # 語のリストに分ける group
warn = { status = "ERR" }                     # この値の応答は level warn
answered = "{command} -> {status}{values}"    # 応答の来た命令の文字
```

- `command`、`answer`、`event` は正規表現で、名前付きの group がメッセージのキーになります。命令を送る線がホストで、もう一方の線の行は応答とイベントです。
- **証拠は組です:** すぐ前の命令への応答（`pair` があれば、その group が同じもの）はチェックの成功、どの命令にも属さない応答や、次の命令が来ても応答の無い命令は失敗です。この形の無いテキストはどちらでもないので、ほかのテキストでは何も出しません。
- メッセージは、応答付きの命令（`level` は `ok`、`warn` の値か応答が無ければ `warn`）、イベント（`ok`）、何にも属さない応答（`error`）のどれかです。パートの `relations` の下に出ます（分析の形式 §4）。

## 4. 例と、試験での確かめ方

`examples/` は 3 つの表を持つ定義のディレクトリで、それぞれ、OEP のプローブが ESP-IDF のドライバで送った記録が付いています（正解は送ったもので `*.truth.json` にあり、試験対象の機器の出力ではありません）。

| 表 | 形 | 記録 |
| --- | --- | --- |
| `demo_link.toml` | UART と SPI の上の `0xA5 \| len \| seq \| cmd \| payload[len] \| crc8`。応答は `cmd` の bit 7 を立てる | `demo_link_p4.wireskein`（UART 115200 8N1） |
| `demo_cmd.toml` | `#<seq> <VERB>[ <arg>...]` に `#<seq> OK[ <value>...]` か `#<seq> ERR <code>` で応え、`!<EVENT>` は聞かれずに | `demo_cmd_p4.wireskein`（2 本の UART） |
| `demo_telemetry.toml` | CAN と LIN のフレームの中の `seq \| reading... \| check`。check は `(識別子 + 前のバイト) & 0xFF` を `0x5A` で xor、識別子ごとのカウンタ | `demo_telemetry_p4_can.wireskein`、`_lin.wireskein` |

```
$ wireskein --definitions examples analyze examples/protocols/demo_link_p4.wireskein
"examples.demo_link": {"messages": [{"s": 563052, "seq": 1, "cmd": 16, "reply": false, "payload": "0102"}, ...],
                       "checks_passed": 3, "checks_failed": 0}
```

試験では:

```python
from wireskein.runlog import protocol

with rec.section(2, "ping", expect=[protocol("examples.demo_link", "uart", {"data": "PA2"}, baud=115200,
                                             messages=[{"seq": 1, "cmd": 0x10}, {"seq": 1, "cmd": 0x10, "reply": True}])]):
    ...
```

- `bus` は `"uart"`、`"spi"`、`"i2c"`、`"uart_pair"`、`"can"`、`"lin"`、`roles` は役割ごとのピン（UART、CAN、LIN は `{"data": ...}`、`{"clk", "mosi", "miso", "cs"}`、`{"scl", "sda"}`、`{"a": TX, "b": RX}`）です。組では、組の点数にかかわらず 2 本を一緒に読みます: `protocol("examples.demo_cmd", "uart_pair", {"a": "PA2", "b": "PA3"}, baud=115200, messages=[{"verb": "PING", "status": "OK"}])`。
- `messages` は順に比べ、それぞれ書いたキーだけを比べます（`"s"` など書かないキーは比べません）。`messages` が無ければ、メッセージが 1 つ以上あること。
- チェックの失敗は `checks_failed_max`（既定 0）まで。
- 定義のディレクトリは、試験を検証する所で足します（`wireskein --definitions DIR verify RUN`、または試験の中で `add_definitions`）。
- これで言えないきまりは、試験の判定にします（`rec.result`、[ガイド](capture-test-guide.ja.md) §3.3）。

## 5. 表でないもの

- **線そのものの上のプロトコル**（クロックとデータ、または 1 本の線の上の独自の枠）: WireSkein 自身のそうしたプロトコル（`decl/*.toml`: I²C、SPI、SWD、CAN など）は、分析のエンジンの一部で、そのエンジンの層に合わせて書いてあり、エンジンとともに変わりえます。直下にプロトコルの表がある定義のディレクトリは断ります。WireSkein が読まないバスは、提案してください。
- **表で言えないプロトコル**（フレームをまたぐ状態、分割したものの組み立て、独自の計算）: 分析の出力を自分のコードで読み（`wireskein analyze FILE`、バスの層）、試験では `rec.result` で判定します。
- **機器パックとマーカーの方言**は、同じディレクトリの `devices/` と `markers/` にある別の表です（[機器パック](../src/wireskein/decl/devices/README.md)、[マーカー](markers.ja.md) §4）。
