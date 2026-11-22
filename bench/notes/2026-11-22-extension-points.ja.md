# 利用者の拡張点の見直し（2026-11-22、v1 で固める前）

v1 の B7 で公開を `capture`、`runlog`、`verify`、`decode`、`sources` と決めた。その面から、利用者が足せるものを洗い出した。
直すのは計画が決まってから（v1 の決定の進め方）。

## 今ある拡張点

| 足すもの | 仕組み | B7 の公開面から | 文書 | EP のテスト |
|---|---|---|---|---|
| バスに乗るプロトコル（Python） | `protocols.register`、EP `wireskein.protocols` | 届かない | protocols.md §1-4 | なし |
| 定義ディレクトリ（TOML のプロトコル、機器パック、マーカーの方言） | `protocols.add_definitions`、EP `wireskein.definitions` | 届かない | protocols.md §5（例だけ） | なし |
| 検査の種類 | `verify.register_check`、EP `wireskein.checks` | 届く | guide §4.10 | あり |
| ファイル形式 | `formats.register`、EP `wireskein.formats` | 届かない（`decode.load` は使う） | extending.md §2 | なし |
| 取り込み元 | EP `wireskein.sources`（register はない） | 届く | docstring だけ | なし |
| 結果を書き込む（マーカー、注記、添付） | `capture.add_markers` ほか | 届く | extending.md §3 | あり |

解析の段の差し替え（`_engine.methods`）は bench 用で、公開しないと明記されている。これはそのままでよい。

## 固める前に直すこと（重い順）

1. **公開面と拡張点が合っていない。** extending.md は、B7 では非公開の `wireskein.protocols`、`formats`、`states`、`annotate` を
   利用者の道として案内している。
2. **内部の型が利用者のコードに渡る。**
   - 検査の関数は `_engine.model.Capture` を受け取る。extending.md は「内部の型は使わない」と書いていて、矛盾している。
   - 取り込み元の `Result` は `_fileformat.Channel` と `_fileformat.Part` を使う。`Part` は公開の `capture.Part` と別のクラスで、
     引数の順も違う。
3. **公開の名前がぶつかる。** `capture.Channel`（サンプルで持つ）と `decode.Channel`（変化で持つ）、`capture.Capture` と
   `decode.Capture` が別物になっている。命名の規則の「同じ種類は同じ形」に反する。
4. **登録の仕組みがそろっていない。**
   - register と unregister が、あるものとないものがある（検査は unregister なし、取り込み元は register なし）。
   - EP の名前とオブジェクトの `name` のどちらを先に使うかが、種類ごとに違う。
   - 組み込みの名前の予約に `uart`、`spi`、`i2c` などが入っていない（`protocols._RESERVED`）。文書は拒むと書いている。
   - 組み込みと同じ名前の取り込み元の EP は、知らせずに隠される。
5. **単位の接尾辞がない。** `sources.Request` の `rate`、`analog_rate`、`timeout`、`duration` と、検査が受け取る `cap.rate`、
   `cap.duration`。同じクラスの `glitch_filter_ns` と `tick_hz` には付いている。
6. **形式の番号がない。** 宣言型の TOML、機器パック、マーカーの方言に番号がなく、TOML は未知のキーも通す。
7. **組み込みが自分の仕組みを使っていない。**
   - 組み込みの Python の解析は `PLUGINS` と `UPPER` で動き、`protocols.register` を通らない。
   - 組み込みの TOML だけが、`ADAPTERS` で出力を整えてもらえる。
   - 外の TOML で `reframe` や `[score] terms` が使えることが、文書にない。
8. Python のプロトコルを、analyze の hints で選んだり外したりできない（読みから推した。未確認）。
9. CLI だけでは、定義ディレクトリも自前のプロトコルも足せない（パッケージにするしかない）。

## 提案

- **A. 拡張は `wireskein.plugins` の 1 つの口に集める。**
  - 関数: `register_protocol`、`register_format`、`register_check`、`register_source`、`add_definitions`、それぞれの
    `unregister_*`。
  - EP の群: `wireskein.protocols`、`wireskein.definitions`、`wireskein.formats`、`wireskein.checks`、`wireskein.sources`
    をそのまま読む。
  - 名前の決まりはどれも同じにする: EP の名前とオブジェクトの名前がずれたら断る。組み込みの名前は、プロトコルも形式も
    取り込み元も断る。
  - `verify.register_check` と `protocols.register` は移したあとは置かない（別名は置かない方針）。
- **B. 取り込んだものの型を 1 つにする。** 利用者に渡すのは `wireskein.capture.Capture`（とその `Part`）だけにする。
  - 検査には、そこから作った変化で持つ見方（区間の窓、`rate_hz`、`level_at`、線ごとの変化）を渡す。
  - `decode.Capture` と `decode.Channel` は外に出さない。
  - 取り込み元の `Result` は `capture.Part` を返す。
- **C. 単位の接尾辞を付ける。** `Request.rate_hz`、`analog_rate_hz`、`timeout_s`、`duration_s`。
- **D. 定義ファイルに形式の番号を付ける。** 先頭に `format = "wireskein-protocol/1"`（機器パックとマーカーの方言も同じ形）。
  未知のキーは断る。
- **E. 組み込みも同じ口を通す。** 段階的に進める。まず ADAPTERS と reframe、terms を、外の定義からも名前で使える形として
  文書にする。
- **F. CLI に `--definitions DIR`、`--plugin MODULE` を足す。**
- **G. EP の読み込みのテストを、群ごとに 1 つ足す。**

## 決まったこと（2026-11-22、ユーザー）

- A: 拡張は `wireskein.plugins` に集める。公開面（B7）は capture、runlog、verify、decode、sources、plugins になる。
- B: 利用者が触る型は `wireskein.capture` の Capture、Part、Channel だけにする。検査には、そこから作った変化で持つ見方を
  渡す。`decode.Capture` と `decode.Channel` は外に出さない。
- C〜G は提案のまま進める。
