# tools

開発とリリースに使うスクリプトです。package には入りません。

| ファイル | 内容 |
| --- | --- |
| `fetch_web.py` | `web.json` で固定した版の wireskein-web（ビューア）を npm から取り、ハッシュを確かめて `src/wireskein/web/` に置く。`wireskein gui` を手元で試すときと、CI・リリースで使う |
| `web.json` | 同梱するビューアの package 名、版、integrity（sha512） |
| `prepare_release.py` | リリースの workflow が呼ぶ。`pyproject.toml` と `src/wireskein/__init__.py` の版を書き換え、`CHANGELOG.md` の `## Unreleased` を版の見出しの下へ移す |

リリースの手順はリポジトリの [README](../README.ja.md#リリース) にあります。
