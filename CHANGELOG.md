# Changelog / 変更履歴

## Unreleased
- (EN) First beta. The prototype is now the `wireskein` package on PyPI (`pip install --pre wireskein`, Python 3.13+, numpy only). Public modules are `wireskein.runlog` (recording a test run, standard library only), `wireskein.verify` (checking a recorded run) and `wireskein.analyze` (decoding a capture, beta), with a `wireskein` command (`analyze`, `segments`, `verify`). The `wsproto` import and `prototype/ws.py` are gone; evaluation scripts moved to `research/` and are not packaged.
- (JA) 最初のβ版。プロトタイプを PyPI の `wireskein` package にした（`pip install --pre wireskein`、Python 3.13 以上、依存は numpy だけ）。公開の module は `wireskein.runlog`（テストの実行の記録。標準ライブラリだけ）、`wireskein.verify`（記録の照合）、`wireskein.analyze`（キャプチャの復号。β）。コマンドは `wireskein`（`analyze`、`segments`、`verify`）。`wsproto` の import と `prototype/ws.py` はなくなった。評価のスクリプトは `research/` に移し、package には入れない。
- (EN) Checks: `square`, `level`, `starts`, `ends`, `only_moving`, `pulses`, `i2c`, `spi` and `uart`. The UART check measures the bit rate from the edges, takes the format (`bits`, `parity`, `stop`), does not count characters cut by the window as errors, and with `baud=None` only measures. An I2C transaction cut at the window end is reported with `complete: false` and `pending_bits`. A capture whose meta has `time_base_slipped: true` keeps its verdict, and a failure's reason names the slip.
- (JA) 検査は `square`、`level`、`starts`、`ends`、`only_moving`、`pulses`、`i2c`、`spi`、`uart`。UART はエッジからビットレートを測り、フォーマット（`bits`、`parity`、`stop`）を指定でき、窓で切れた文字をエラーにせず、`baud=None` で測るだけにもできる。窓の終わりで切れた I2C の取引は `complete: false` と `pending_bits` 付きで返す。meta に `time_base_slipped: true` があるキャプチャは判定を変えず、NG の理由にそれを書く。
- (EN) Repeated heading names under one parent keep their own expectations (`name[0]`, `name[1]`). A check whose pins are not captured is unchecked, never NG.
- (JA) 同じ親の下で繰り返した見出しは、それぞれの期待を持つ（`name[0]`、`name[1]`）。ピンがキャプチャにない検査は、NG ではなく未検査。
- (EN) Run format: `wireskein-run/0`, unchanged from the prototype, so runs recorded before this release still verify.
- (JA) 記録の形式は `wireskein-run/0` で、プロトタイプから変えていない。このリリースより前の記録もそのまま照合できる。
