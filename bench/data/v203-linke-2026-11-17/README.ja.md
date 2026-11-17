# v203-linke-2026-11-17

WCH-LinkE → CH32V203C8T6 の RVSWD を、OEP のプローブで LinkE の側のピンから 160 MS/s で取り込んだ記録です。測定台の
`linke:` の元（`bench/wsbench/data.py`）が読みます。切り出す前の生の記録と、取り方・正解・切り出しの道具は、
wireskein-captures の `v203-linke-2026-11-17/` にあります（`SOURCE.json` にコミット）。

## 取り方

| 項目 | 値 |
|---|---|
| target | CH32V203C8T6（UID `dbb1abcd43e8bc6a`、chip ID `0x20310500`） |
| デバッガ | WCH-LinkE `FBC18F0680B0`（ファーム 2.23）、ch32rv 0.31.2 |
| 取り込み | OEP ESP32-P4X `e8f60ae84dd7`（ファーム 0.0.56+b9b46df）、GPIO21 = SWCLK、GPIO16 = SWDIO、LinkE の側で分岐、流し続け、グリッチフィルタなし |
| 速さ | 160 MS/s |

LinkE の速さ high では、SWCLK の High が周期 100 ns のうち約 19 ns で、160 MS/s で 3 サンプルです。この組を取った理由は
そこにあります（9 月の fixture の 50 MS/s では、DMI のアクセスの約半分を取りこぼしていた）。

## 中身

- `runs/{high,low}/<操作>`: L103（`l103-linke-2026-11-17`）と同じ 14 操作を、LinkE の速さ high と low で。
- `runs/high/x_*`: 64 KiB の読み出し、20 KiB の RAM の読み出し、step、`target_info` の繰り返し。

配線が不安定だったので、medium、ほかの操作、ほかの取り込みの速さは取っていません（遅い速さは 160 MS/s から作る）。
各 run のファイルと、確認用（holdout）の決め方、正解の使い方は `l103-linke-2026-11-17` と同じです。

## 読みについて

37 本すべてで、ch32rv の DMI の操作が 160 MS/s の読みの中に順にすべて見つかり（`truth.json` の `agreement_160M`）、
読み出しは `read.bin` と一致します。
