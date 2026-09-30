# Changelog / 変更履歴

## Unreleased

## 0.0.2
- (EN) Capture files `.wsc` (`wireskein.wsc`, standard library only): each channel at its own sample rate (`step` ticks per sample), so a probe that decimates some channels stores only the samples it took. `wsc.from_interleaved()` splits a probe's sample stream by bits per sample and bit positions. The analysis uses each channel's own sample width wherever it assumed one tick.
- (JA) キャプチャのファイル `.wsc`（`wireskein.wsc`、標準ライブラリだけ）: 各チャンネルを自分のサンプルレート（`step` 刻みに 1 サンプル）で持つ。一部のチャンネルを間引くプローブは、取ったサンプルだけを保存する。`wsc.from_interleaved()` はプローブのサンプルの並びを、1 サンプルのビット数とビット位置で切り分ける。解析は、1 刻みを前提にしていた所で、各チャンネルのサンプルの幅を使う。
- (EN) Attachments and notes in a `.wsc`: `wsc.attach()` stores a named free-form file (text, JSON, bytes; replaceable), `wsc.note()` appends a timestamped entry to an append-only log; both work on an existing file without rewriting the channels, and go along through conversions. Commands `wireskein info`, `note` and `attach`; `Recorder.capture(..., attachments={...})`.
- (JA) `.wsc` の添付とメモ: `wsc.attach()` は名前付きの自由なファイル（テキスト、JSON、bytes。差し替え可）、`wsc.note()` は時刻付きの追記専用のメモ。どちらも既存のファイルにチャンネルを書き直さずに足せ、変換でも持ち運ぶ。コマンドは `wireskein info`、`note`、`attach`。`Recorder.capture(..., attachments={...})`。
- (EN) `wireskein convert IN OUT` converts between `.wsc`, `.sr` and fixture directories (by extension). A `.sr` has one rate, so slow channels are repeated to it; `wireskein.json` inside the `.sr` keeps their real rate, and reading it back restores them. `wireskein.analyze.save()` does the same from Python.
- (JA) `wireskein convert IN OUT` で、`.wsc`、`.sr`、fixture のディレクトリの間を変換する（拡張子で決まる）。`.sr` は 1 つのレートなので、遅いチャンネルは水増しする。本当のレートは `.sr` の中の `wireskein.json` に残し、読み戻すと元に戻る。Python からは `wireskein.analyze.save()`。
- (EN) Breaking: runs store captures as `.wsc` and `run.json` is `wireskein-run/1`; runs of `wireskein-run/0` (`.bin`) are no longer read. `Recorder.capture(armed, tick_hz, *, interleaved=..., names=..., width=8, positions=None, n=None)` or `capture(armed, tick_hz, channels=[wsc.Channel(...)])`, and `Recorder.armed()` returns `time.monotonic()`, so a capture client's own stamp can be passed as is.
- (JA) 互換のない変更: 記録はキャプチャを `.wsc` で持ち、`run.json` は `wireskein-run/1`。`wireskein-run/0`（`.bin`）の記録は読まない。`Recorder.capture(armed, tick_hz, *, interleaved=..., names=..., width=8, positions=None, n=None)` または `capture(armed, tick_hz, channels=[wsc.Channel(...)])`。`Recorder.armed()` は `time.monotonic()` を返すので、キャプチャのクライアントが付けた時刻をそのまま渡せる。

## 0.0.1
- (EN) First beta. The prototype is now the `wireskein` package on PyPI (`pip install --pre wireskein`, Python 3.13+, numpy only). Public modules are `wireskein.runlog` (recording a test run, standard library only), `wireskein.verify` (checking a recorded run) and `wireskein.analyze` (decoding a capture, beta), with a `wireskein` command (`analyze`, `segments`, `verify`). The `wsproto` import and `prototype/ws.py` are gone; evaluation scripts moved to `research/` and are not packaged.
- (JA) 最初のβ版。プロトタイプを PyPI の `wireskein` package にした（`pip install --pre wireskein`、Python 3.13 以上、依存は numpy だけ）。公開の module は `wireskein.runlog`（テストの実行の記録。標準ライブラリだけ）、`wireskein.verify`（記録の照合）、`wireskein.analyze`（キャプチャの復号。β）。コマンドは `wireskein`（`analyze`、`segments`、`verify`）。`wsproto` の import と `prototype/ws.py` はなくなった。評価のスクリプトは `research/` に移し、package には入れない。
- (EN) Checks: `square`, `level`, `starts`, `ends`, `only_moving`, `pulses`, `i2c`, `spi` and `uart`. The UART check measures the bit rate from the edges, takes the format (`bits`, `parity`, `stop`), does not count characters cut by the window as errors, and with `baud=None` only measures. An I2C transaction cut at the window end is reported with `complete: false` and `pending_bits`. A capture whose meta has `time_base_slipped: true` keeps its verdict, and a failure's reason names the slip.
- (JA) 検査は `square`、`level`、`starts`、`ends`、`only_moving`、`pulses`、`i2c`、`spi`、`uart`。UART はエッジからビットレートを測り、フォーマット（`bits`、`parity`、`stop`）を指定でき、窓で切れた文字をエラーにせず、`baud=None` で測るだけにもできる。窓の終わりで切れた I2C の取引は `complete: false` と `pending_bits` 付きで返す。meta に `time_base_slipped: true` があるキャプチャは判定を変えず、NG の理由にそれを書く。
- (EN) Repeated heading names under one parent keep their own expectations (`name[0]`, `name[1]`). A check whose pins are not captured is unchecked, never NG.
- (JA) 同じ親の下で繰り返した見出しは、それぞれの期待を持つ（`name[0]`、`name[1]`）。ピンがキャプチャにない検査は、NG ではなく未検査。
- (EN) Run format: `wireskein-run/0`, unchanged from the prototype, so runs recorded before this release still verify.
- (JA) 記録の形式は `wireskein-run/0` で、プロトタイプから変えていない。このリリースより前の記録もそのまま照合できる。
