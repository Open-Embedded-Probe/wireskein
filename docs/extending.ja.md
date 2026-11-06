# 独自の分析

[English](extending.md)

WireSkein は、キャプチャを読み手に要るもの（載っているバス、そのメッセージ、試験が通ったか）に変えます。サンプルそのものを読まずに済ませるためです。要るものが組み込みに無いときは、それが属する層に足します。このガイドは、何にどの方法が合うか、どの追加も守ること、結果をビューアに出す方法を書きます。

## 1. どの方法か

| したいこと | 使うもの | 詳しく |
| --- | --- | --- |
| UART、SPI、I2C に乗るプロトコル（命令のフレーム、報告、パケット）を分析で読む | Python のプロトコルのオブジェクト（`wireskein.protocols`） | [独自のプロトコル](protocols.ja.md) §1〜§4 |
| UART の 1 本のテキストの命令と、もう 1 本の応答（AT コマンド、シェル） | `"uart_pair"` に乗るプロトコルのオブジェクト | [独自のプロトコル](protocols.ja.md) §3.1 |
| それ自体がバスのプロトコル（クロックとデータ、または 1 本の NRZ の線の上の独自のフレーム） | 定義のディレクトリの TOML の定義 | [独自のプロトコル](protocols.ja.md) §5 |
| I2C や SPI の機器のレジスタや命令に名前を付ける | 定義のディレクトリの `devices/` のデバイスの定義 | [デバイスの定義](../src/wireskein/decl/devices/README.ja.md) |
| 自分の試験のログのマーカーの行を区間として読む | 定義のディレクトリの `markers/` のマーカーの方言 | [マーカー](markers.ja.md) §4 |
| どの検査も持たないきまりを試験に使い、`wireskein verify`、pytest、ビューアで同じに判定する | 検査の種類（`wireskein.checks`） | [ガイド](capture-test-guide.ja.md) §4.10 |
| 1 つの試験だけがする判定 | `wireskein.verify.sections(run)`、区間の分析、`rec.result(...)` | [ガイド](capture-test-guide.ja.md) §3.3 |
| 判定ではなく、見るためのもの | Python でキャプチャを読み（`wireskein.capture`、`wireskein.states`、`wireskein.decode`）、見つけたものをファイルに入れる（§3） | [ファイル形式](wireskein-format.ja.md) §5 |
| WireSkein が読まない線の符号や物理層（マンチェスターなど） | 拡張の口ではありません。WireSkein の中にビットの層が要ります | 提案してください |

方法は積み重なります: 定義が分析にプロトコルを与え、プロトコルのオブジェクトがその上に乗るものを読み、検査が試験でそれを判定し、そのメッセージと注意は、ファイルの注釈と印になります。

## 2. どの追加も守ること

- **書かれた形を読む。** プロトコルは [分析の形式](analysis-format.ja.md) が書き出す形のバスを、検査は区間の窓（ガイド §4.10）を、スクリプトは [ファイル形式](wireskein-format.ja.md) と [記録の形式](run-format.ja.md) のファイルを受け取ります。形式の番号（`/1`）の中でこれらの形は変わらないので、その上に作った追加は動き続けます。WireSkein の内部の型は変わることがあり、使いません。
- **証拠のあるものだけを出す。** プロトコルは、フレームが通ったチェックと失敗したチェック（CRC、長さ、順番の番号）を数え、分析は偶然を超えるときだけそれを信じます。チェックが無ければ何も出しません。検査は、判定にかかわらず測ったものを言い、プロトコルとしては許されるが見ておきたいものは、失敗ではなく、その tick の注意にします（ガイド §4.9）。
- **正解は、試験されるもの以外から取る。** 追加は、それを読むコードとは別に作った信号で確かめます: 試験の中の自分の符号器か、既存のライブラリで出すプローブ（OEP のプローブの ESP-IDF の UART ドライバなど）。試験される機器の出力は、決して正解にしません。それこそが誤っているかもしれないからです。
- **名前。** プロトコルや検査の名前は、英数字と `_` で、WireSkein の持つ名前とは別のものにします。同じ名前は断ります。
- **失敗は見えるままに。** プロトコルの `decode` の例外は、そのバスと一緒に出て、分析は続きます。だれも足していない種類の検査は `ng` で、飛ばしません。
- **パッケージ。** パッケージは追加を entry point で宣言し、入れたときにいつも使えるようにします。

| entry point の group | 足すもの | 値 |
| --- | --- | --- |
| `wireskein.protocols` | UART、SPI、I2C に乗るプロトコル | `name`、`over`、`decode` を持つオブジェクト |
| `wireskein.definitions` | 定義のディレクトリ（プロトコル、デバイスの定義、マーカーの方言） | それを持つディレクトリの module、またはそのパスを返す callable |
| `wireskein.checks` | 検査の種類 | 検査の関数、または `.check`（と `.notices`）を持つオブジェクト |
| `wireskein.sources` | `wireskein capture` の取得元 | `capture(target, request)` を持つ module |

```toml
[project.entry-points."wireskein.protocols"]
acme_link = "acme_wireskein:PROTOCOL"

[project.entry-points."wireskein.definitions"]
acme = "acme_wireskein.definitions"
```

## 3. 結果をビューアに出す

- **注釈:** `wireskein annotate FILE --save`（または `wireskein.annotate.build_file` と `save`）は、分析を行として書きます。入れたプロトコルのメッセージもその中に入ります。メッセージの `text` がビューアで描かれる文字（無ければ欄の値）、`level`（`ok`、`warn`、`error`）が色、`s` と `e` が範囲です。
- **印:** `wireskein.capture.add_markers(path, [{"t": tick, "label": ..., "end"?: tick, "part"?: k}], by="my_tool")` は、自分の印をファイルに入れます。自分の道具が前に入れた印は置き換え、ほかの印は残します。`wireskein.capture.markers(path)` ですべてを読みます。照合も、注意と変化点を同じやり方で書きます（`by: "verify"`）。
- **メモと添付:** `wireskein.capture.note(path, ...)` と `attach(path, name, data)` は、チャンネルを書き直さずに、ほかのものをキャプチャと一緒に残します。

## 4. 未知のキャプチャから独自の分析まで

1. `wireskein analyze FILE` が見つけたものを言います。たとえば UART が `likely` で、バイトはあるがその上には何も無い。
2. `wireskein states FILE` とバイトから、形が分かります: 始まりのバイト、長さ、チェックサム。
3. プロトコルを書きます（§1）: `decode` がバイトの中のフレームを見つけ、チェックサムを数えます。
4. それとは別に作った信号で確かめます: 試験の中の自分の符号器でフレームを符号化して線に乗せ（UART の 1 文字は、開始ビット、下位からのデータのビット、停止ビット）、`wireskein.capture` でキャプチャとして書くか、プローブにその UART ドライバで出させます（WireSkein 自身の試験は内部の生成器を使います。これは公開の口ではありません）。
5. `register` する（または入れる）と、`wireskein analyze` はその UART を `confirmed` と読み、フレームを `result` の下に出します。
6. 試験では、`protocol(NAME, "uart", {"data": PIN}, messages=[...])` で区間のフレームを確かめます。

## 4.1 どの形か

| 線の上にあるもの | 書くもの | その証拠 | 見本 |
| --- | --- | --- | --- |
| UART 1 本の上のバイナリのフレーム（始まりのバイト、長さ、CRC） | `"uart"` に乗るプロトコル。`bytes` を読む | CRC、長さ | `demo_link` |
| 同じフレームを SPI の窓で。要求は MOSI、応答は MISO | 同じプロトコルで `over = ("uart", "spi")`。`transfers` を読む | CRC、応答の seq | `demo_link` |
| 1 本の線のテキストの命令と、もう 1 本の応答（AT コマンド、シェル） | `"uart_pair"` に乗るプロトコル。両方の線の `messages` を読む | 命令に当たる応答 | `demo_cmd` |
| I2C や SPI のチップのレジスタ | デバイスの定義 | 正しく読める識別のレジスタ | [デバイスの定義](../src/wireskein/decl/devices/README.ja.md) |
| 1 本の線、またはクロックとデータの上の独自のフレーム | TOML の定義 | 決まった値の欄、CRC | `demo_wire` |
| 試験が使う、どの線にもあてはまるきまり（落ち着く線、出来事の順番） | 検査の種類 | 測った値、その tick の注意 | `demo_settle` |

自分のチェックを持たないプロトコル（1 本の線のただのテキストなど）は、分析には何も出ません。チェックを持たせるか（合う応答、数えていく欄、偶然には起きないほど厳しい形）、試験の中で行を読んで（`wireskein.verify.sections`、分析の `lines`）そこで判定します。

## 5. リポジトリの中の見本

| 見本 | 見せるもの |
| --- | --- |
| `examples/protocols/demo_link.py` | UART と SPI に乗るプロトコル: フレームの区切り、読み直し、証拠としての CRC-8 |
| `examples/protocols/demo_link_p4.wireskein` | OEP のプローブが ESP-IDF の UART ドライバで出して取り込んだ demo_link と、正解としての送ったもの |
| `examples/definitions/demo_wire.toml` | 1 本の NRZ の線の上のプロトコル: ビットスタッフィング、変わる長さ、CRC-8 |
| `tests/test_protocols.py` | 両方の見本を、別に作った信号で確かめる試験。プロトコルの検査、注釈、印 |
| `examples/protocols/demo_cmd.py` | UART の組の上のテキストの命令と応答: 証拠としての組、出来事、ビューアのための level |
| `examples/checks/demo_settle.py` | 検査の種類: 落ち着かなければならない線、注意としてのチャタリング、期待のレベルが無ければ測るだけ |
| `tests/test_demo_cmd.py` | 分析と、試験のプロトコルの検査での demo_cmd |
| `tests/test_check_plugins.py` | 外から足した検査の種類と、その注意。チャタリングする接点での demo_settle |
