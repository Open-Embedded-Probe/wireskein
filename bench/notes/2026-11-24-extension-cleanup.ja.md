# 拡張の整理（2026-11-24、案）

ユーザー:「そこの拡張性部分は今の仕様壊してもいいから整理しようか。そんな複雑な拡張性を上に載せる必要ある？」

## 1. 今の口と、使われ方

外のリポジトリ（oep-*、pytest-embedded-wireskein、dev_wch）では、どの `register_*` も、entry point も使われていない。使っているのは WireSkein 自身の試験と examples だけ（2026-11-24 に grep で確かめた）。壊しても困る人はいない。

| 口 | 形 | 使う所 |
| --- | --- | --- |
| 機器パック、マーカーの方言 | TOML（定義のディレクトリ） | 本体の decl、試験 |
| `register_protocol` | Python の `decode` | examples の demo_link / demo_cmd / demo_telemetry、試験 |
| `register_check` | Python | examples の demo_settle、試験 |
| `register_format` | Python | examples の saleae_csv、試験 |
| `register_source` | Python | 本体の oep・sigrok（組み込み）、試験 |
| `register_engine` | Python | 試験だけ |
| entry point（6 つの group）、`--plugin`、`WIRESKEIN_PLUGINS` | 読み込みの仕組み | 試験だけ |

## 2. 整理の形（案）

拡張は 2 つだけにする。

1. **宣言（テキスト）**: バスの上に載るものを、表で書く。Rust、ネイティブ、web、Python のどれでも同じものが動く。
   - 機器パック（今のまま）
   - マーカーの方言（今のまま）
   - 上のプロトコル（新しく作る。今の `register_protocol` の置き換え）
     - 載るバス（UART、SPI、I2C、CAN、LIN、UART の組）
     - 枠: 先頭、長さのフィールド、フィールド、終わり
     - 検査: CRC やチェックサムは名前で選ぶ（本体に表を持つ）。連番も検査になる
     - 要求と応答の組
     - テキストのコマンドと応答（行、プロンプト）
2. **データの出入り（コードは差し込まない）**: 複雑なことは WireSkein の外で、文書化した形式を読み書きして行う。
   - 入り口: 記録のファイル（.wireskein、.sr、VCD）。ほかの形式は、外で変換する（Python の `capture` で書ける）。
   - 出口: 分析の形式（JSON）と記録の形式。状態を持つプロトコル、組み立て、独自の計算は、出口を読む外のコードで行う。
   - 試験: 1 つの試験だけの判断は、今の `rec.result(...)` で（docs/capture-test-guide.md §3.3）。

なくすもの:

- `register_protocol`、`register_check`、`register_format`、`register_engine`
- 外から足す `register_source`。oep と sigrok は組み込みのまま残す。
- entry point、`--plugin`、`WIRESKEIN_PLUGINS`
- 名前の名前空間（`owner.name`）は、宣言のファイルの名前にだけ残す。

要望が来たものは、本体に足す。対象は、線の符号、新しいバス、よく使う検査の種類、ファイル形式。

メモ「OEP は拡張性優先」との関係: 拡張点はなくさず、形を変える。コードの差し込みをやめ、宣言とデータの出入りにする。言語を選ばず、Rust だけの形でもそのまま残る。

## 3. 順番

1. **上のプロトコルの宣言を作り、書き切れるか試す。**
   - examples の 3 つ（demo_link、demo_cmd、demo_telemetry）を宣言で書き直す。
   - 本体の上の段（NMEA、Modbus RTU、SCPI）も、宣言で書けるかを数える。書けないものは、本体のコードのまま残す。
   - 同じ記録と正解（examples の *.truth.json）で、読みが同等以上かを比べる。
2. **口をなくす。** `plugins.py`、`_engines` の足したエンジン、`verify` の check kind、`formats` の足した形式、entry point、`--plugin`。それぞれの試験と examples も片づける。
3. **文書を直す。** docs/extending.md と protocols.md を、宣言とデータの出入りの 2 つで書き直す。CHANGELOG に破壊的変更として書く。

## 4. 試すと分かること

- 宣言で書けない上のプロトコルが、どれだけあるか（本体の上の段と examples で数える）。
- 検査の種類（demo_settle のような時間の規則）を、宣言で持つ必要があるか。それとも試験のコードで足りるか。

## 5. 結果（2026-11-24）

### 5.1 宣言で書き切れたか

- examples の 3 つは、すべて宣言で書けた（`[frame]` 2 つ、`[lines]` 1 つ）。
  - プローブの記録 4 つ（demo_link、demo_cmd、demo_telemetry の CAN と LIN）で、メッセージ・チェックの数・text・level が、Python 版と完全に同じ。
  - 試験も、同じ期待のまま通った。
- 宣言の形（`wireskein-protocol/1`、`src/wireskein/_declared.py`）
  - `[frame]`
    - フィールド: `size` は数・前のフィールド・`rest`。`equals` / `min` / `max`、`mask`、`of`、`as`、`endian`
    - チェック: 名前で選ぶ CRC 8 種、自前の `crc`、`sum8` / `sum16` / `xor8`。`init` に `@id` を使える
    - カウンタ（`sequence`）、`text` のひな形
    - 流れでは探す、フレームの中では丸ごと読む
  - `[lines]`
    - 命令・応答・イベントの正規表現（名前付き group）
    - `pair`、`words`、`warn`、`answered`
- 本体の上の段を、この形で書けるか（数えただけ。本体のコードのまま）

| 上の段 | 書けるか | 足りないもの |
| --- | --- | --- |
| NMEA | もう少しで書ける | 区切り文字までのフィールド（`until = "*"`）と、チェックを 16 進の文字で比べること |
| Modbus RTU | 書けない | 3.5 文字の無音でフレームを区切り、CRC が合う所で終わりを探し、後ろのごみ（ドライバの切り替え）を許す。時間と探索が要る |
| SCPI（UART の組） | 一部 | 応答はパターンでなく「次の行」。証拠はチェックでなく文法に合う割合 |
| テキストの行 | 対象外 | 本体の層（プロトコルではない） |
| マーカーの文法 | すでに表 | マーカーの方言 |

書けないものは、本体のコードに残す（要望が来たら本体に足す、の扱い）。NMEA の 2 つの足りないものは、一般的なので、要る例が来たら宣言の形に足す候補。

### 5.2 なくしたもの

- `register_protocol` / `register_check` / `register_format` / `register_source` / `register_engine`
- すべての entry point、`--plugin`、`WIRESKEIN_PLUGINS`
- 外から足したエンジンを主張に混ぜる口（`_engines.merge`、decode の足したエンジンのバス、`protocol(NAME, None)` の足したエンジンへの振り分け）
- examples の demo_settle（検査の種類）と saleae_csv（ファイルの形式）
- 試験の test_engines.py、test_check_plugins.py。test_formats.py は .sr と切り出しの試験だけ残した。
- 宣言の誤り（名前、バス、チェックの種類、指すフィールド、二重の宣言）は、`add_definitions` のときに断る。
