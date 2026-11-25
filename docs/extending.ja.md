# 独自の分析

[English](extending.md)

WireSkein は、記録を、読み手が要るもの（そこにあるバス、そのメッセージ、試験が通ったか）に変えるので、サンプルそのものを読まずに済みます。組み込まれていないものは、次の 2 つのどちらかで足します。どちらも、自分のコードを WireSkein の中で動かしません。

- **定義のディレクトリの表**: WireSkein が読むバスの上に乗るもの（自分のプロトコル、機器パック、マーカーの方言）。表は分析が書き出すものを読むので、バスの見つけ方に依らず、分析の形式を読むどのプログラムでも同じに動きます。
- **文書化した形式の出入り**: 表で言えないものは、記録のファイルを入れ、分析と試験の判定を JSON で出し、自分のコードがそれを読みます。

どちらでもないもの（WireSkein が読まないバス、線の符号、検査の種類、ファイルの形式）は、WireSkein 本体への要望です。提案してください。

## 1. どれを使うか

| したいこと | 使うもの | 詳しく |
| --- | --- | --- |
| UART、SPI、I2C に乗るフレーム（先頭のバイト、長さ、CRC） | プロトコルの表の `[frame]` | [プロトコル](protocols.ja.md) §2 |
| CAN、LIN、DMX512 のフレームの上の層（ノードの値、カウンタ、チェックのバイト） | `"can"` / `"lin"` / `"dmx512"` に乗る `[frame]` | [プロトコル](protocols.ja.md) §1、§2 |
| 1 本の UART のテキストの命令と、もう 1 本の応答（AT コマンド、シェル） | `"uart_pair"` に乗る `[lines]` | [プロトコル](protocols.ja.md) §3 |
| I2C や SPI の機器のレジスタと命令に名前を付ける | `devices/` の機器パック | [機器パック](../src/wireskein/decl/devices/README.md) |
| 自分の試験の記録のマーカーの行を区間として読む | `markers/` のマーカーの方言 | [マーカー](markers.ja.md) §4 |
| 表で言えないプロトコル（フレームをまたぐ状態、分割の組み立て、独自の計算） | 分析の出力を読む自分のコード: `wireskein analyze FILE`（または `wireskein.decode.analyze_file`）のバスの層 | [分析の形式](analysis-format.ja.md) §4 |
| 1 つの試験だけの判断 | `wireskein.verify.sections(run)`、区間の分析、`rec.result(...)` | [ガイド](capture-test-guide.ja.md) §3.3 |
| WireSkein が読まない形式の記録 | `wireskein.capture`（`logic`、`analog_volts`、`Part`、`Capture`、`write`）で WireSkein のファイルに変換する | [ファイルの形式](wireskein-format.ja.md) |
| 判定でなく、見るためのもの | Python で記録を読み（`wireskein.capture`、`wireskein.decode`）、見つけたものをファイルに入れる（§3） | [ファイルの形式](wireskein-format.ja.md) §5 |
| WireSkein に無い種類の取り込み機材 | WireSkein 本体に足すドライバ（Rust）。差し込みではない | [取り込み機材を足すには](devices.ja.md) |
| WireSkein に無いバス、線の符号、検査の種類 | 拡張点ではない | 提案してください |

これらは重なります: 分析がバスを見つけ、プロトコルの表・機器パック・マーカーの方言がその上を読み、試験の `protocol(...)` がそれを判定し、メッセージは注釈とマーカーとしてファイルに入ります。

## 2. 定義のディレクトリ

```
my_definitions/
  protocols/acme_link.toml        自分のプロトコル（wireskein-protocol/1）
  devices/i2c/acme/sensor.toml    機器パック（wireskein-device/1）
  markers/acme.toml               マーカーの方言（wireskein-marker-dialect/1）
```

- **足し方:** コマンドでは `wireskein --definitions DIR <コマンド> ...`（繰り返せる）、スクリプトや試験では `wireskein.plugins.add_definitions(DIR)`（外すのは `remove_definitions(DIR)`）、環境では `WIRESKEIN_DEFINITIONS`（パスの区切りで並べる。コマンドがこれを設定するので、分割して読む子のプロセスにも渡る）。
- **足すときに確かめる:** 誤りのある表（形式、名前、バス、チェックの種類、指すフィールド）は、記録を読むときでなく、足すときに断ります。直下に表がある（`protocols/` と並んで `*.toml` がある）ディレクトリも断ります: 線そのものの上のプロトコルは、WireSkein の分析のエンジンの一部です（[プロトコル](protocols.ja.md) §5）。
- **名前:** プロトコルの名前は、持ち主の名前空間と自分の名前の `持ち主.名前`（`acme.link`: 英小文字、数字、`_`）。WireSkein 自身の名前は名前空間を持たない（1 つの台帳 `src/wireskein/registry.toml`）ので、自分の名前は、今もこの先も WireSkein の名前とぶつかりません。1 つの名前は、足したディレクトリ全体で 1 回だけ宣言できます。
- **文書化した形を読む。** 表は、[分析の形式](analysis-format.ja.md)が書き出すバスを読みます。形式の番号（`/1`）の中では変わらないので、表は動き続けます。
- **証拠のあるものだけを出す。** プロトコルは、フレームが通ったチェックと失敗したチェック（CRC、和、命令に属する応答）を数えます。分析は偶然を超えるときだけそれを信じ、通ったチェックが無ければ何も出しません。
- **正解は、試すもの以外から取る。** 表は、それと別に作った信号で試します: 試験の中の自分の符号化、または確かなライブラリで送るプローブ（OEP のプローブの ESP-IDF の UART ドライバなど）。試験対象の機器の出力は、正解にしません。

## 3. ビューアでの結果

- **注釈:** `wireskein annotate FILE --save`（または `wireskein.decode.annotations_file` と `wireskein.capture.write_annotations`）は、分析を行として書きます。自分のプロトコルのメッセージもその中に入ります。メッセージの `text` はビューアが描く文字（無ければフィールド）、`level`（`ok`、`warn`、`error`）は色、`s` と `e` は範囲です。
- **マーカー:** `wireskein.capture.add_markers(path, [{"t": tick, "label": ..., "end"?: tick, "part"?: k}], by="my_tool")` は、自分のマーカーをファイルに入れます（自分の道具が前に入れたものと入れ替え、ほかのものは残す）。`wireskein.capture.markers(path)` で全部を読めます。検証も、同じやり方で notice と変化を書きます（`by: "verify"`）。
- **メモと添付:** `wireskein.capture.note(path, ...)` と `attach(path, name, data)` は、チャネルを書き直さずに、ほかのものを記録と一緒に置きます。

## 4. 知らない記録から自分の分析まで

1. `wireskein analyze FILE` が見つけたものを言います: たとえば UART が `likely` で、バイトはあるがその上は無い。
2. `wireskein states FILE` とバイトから形が分かります: 先頭のバイト、長さ、チェックサム。
3. 表を書きます（[プロトコル](protocols.ja.md) §2）: フィールドを順に、その一部の上のチェック。
4. それと別に作った信号で試します: 試験の中の自分の符号化でフレームを線に乗せ（UART のフレームは、スタートビット、下位から並ぶデータのビット、ストップビット）、`wireskein.capture` で記録として書く。またはプローブの UART ドライバで送る。
5. `wireskein --definitions DIR analyze FILE` が、UART を `confirmed` と読み、`result` の下にフレームを出します。
6. 試験では、`protocol(名前, "uart", {"data": ピン}, messages=[...])` が区間のフレームを確かめます。

自分のチェックの無いプロトコル（1 本の線の素のテキストなど）は、分析に何も出ません: チェックを与えるか（合う応答、数えるフィールド、偶然には起きないほど厳しい決まった形）、試験で線を読んで（`wireskein.verify.sections`、分析の `lines`）そこで判定します。

## 5. リポジトリの例

`examples/` は定義のディレクトリです: `wireskein --definitions examples analyze examples/protocols/demo_link_p4.wireskein`。

| 例 | 見せるもの |
| --- | --- |
| `examples/protocols/demo_link.toml` | UART と SPI の上のフレーム: 先頭のバイト、長さ、バイトの中のフラグ、証拠としての CRC-8 |
| `examples/protocols/demo_cmd.toml` | UART の組の上のテキストの命令と応答: 証拠としての組、イベント、ビューアのための level |
| `examples/protocols/demo_telemetry.toml` | CAN と LIN のフレームの中のフレーム: 識別子を初期値にするチェックのバイト、識別子ごとのカウンタ、ビューアのための text |
| `examples/protocols/*_p4.wireskein` | それぞれ、OEP のプローブが ESP-IDF のドライバ（UART、TWAI）で送って記録したもの。送ったものは `*.truth.json` |
| `tests/test_protocols.py`、`tests/test_demo_cmd.py`、`tests/test_demo_telemetry.py` | 例と別に作った信号での試験、試験の中のプロトコルの検査、注釈とマーカー |
| `tests/test_plugins.py` | 断る名前、コマンドと環境からの定義のディレクトリ |
