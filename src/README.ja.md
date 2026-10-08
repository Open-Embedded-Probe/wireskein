# src

Python の package `wireskein` です（`pip install wireskein` で入るもの）。

| 場所 | 内容 |
| --- | --- |
| `wireskein/runlog.py` | 試験の記録器と検査の helper。標準ライブラリだけで動く（[記録の形式](../docs/run-format.ja.md)） |
| `wireskein/verify.py` | 記録の照合（`wireskein verify`） |
| `wireskein/fileformat.py` | `.wireskein` の読み書き。標準ライブラリだけで動く（[ファイル形式](../docs/wireskein-format.ja.md)） |
| `wireskein/analyze.py`、`annotate.py` | 復号（`wireskein analyze`）と、復号の注釈の保存 |
| `wireskein/align.py` | アナログやほかのファイルとの時刻の合わせ込み（`wireskein align`） |
| `wireskein/sources/` | 取得元（`oep:`、`sigrok:`）。entry point の `wireskein.sources` で足せる |
| `wireskein/gui.py` | ビューアのサーバー（`wireskein gui`）。ビューアの本体 `wireskein/web/` は git に入れず、`tools/fetch_web.py` で取ってきて wheel に入れる |
| `wireskein/cli.py` | コマンドの入口 |
| `wireskein/decl/` | 方式（`*.toml`）、デバイス（`devices/`）、マーカーの方言（`markers/`）の定義 |
| `wireskein/_engine/` | 内部: 解析のエンジン（段構成、TOML の解釈器、`.sr` と VCD の入出力など）。API として約束しない |

設計の決定と理由は [docs/design.ja.md](../docs/design.ja.md) にあります。
