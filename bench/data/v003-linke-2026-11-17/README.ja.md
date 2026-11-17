# v003-linke-2026-11-17

WCH-LinkE → CH32V003F4P6 の SWIO を、OEP のプローブで LinkE の側のピンから 160 MS/s で取り込んだ記録です。測定台の
`linke:` の元（`bench/wsbench/data.py`）が読みます。切り出す前の生の記録と、取り方・正解・切り出しの道具は、
wireskein-captures の `v003-linke-2026-11-17/` にあります（`SOURCE.json` にコミット）。

## 取り方

| 項目 | 値 |
|---|---|
| target | CH32V003F4P6（UID `ef17abcd5957be73`、chip ID `0x00300510`） |
| デバッガ | WCH-LinkE `FBC18F0680B0`（ファーム 2.23）、ch32rv 0.31.2 |
| 取り込み | OEP ESP32-P4X `e8f60ae84dd7`、GPIO16 = SWIO（LinkE の SWDIO）だけ、LinkE の側で分岐、流し続け、グリッチフィルタなし。プローブのファームは run により 0.0.56+645fbad-diag か 0.0.56+958a231（`meta.probe.firmware`） |
| 速さ | 160 MS/s（`runs/rate/` は 133.3、50、10、5 MHz も） |

LinkE が接続のときに 2 線で問い合わせる所（SWCLK も動く約 100 ms）は、この写しには入れていない（生の記録の
`runs/attach2/` にある）。

## 中身

- `runs/{high,medium,low}/<操作>`、`runs/high/x_*`: L103（`l103-linke-2026-11-17`）と同じ組の操作を、V003 の大きさに
  合わせたもの（flash 16 KiB、RAM 2 KiB）。
- `runs/rate/<MHz>/<操作>`: 4 操作を、プローブ自身が 160、133.3、50、10、5 MHz で取ったもの。

各 run のファイル（`c0001.wireskein`、`truth.json`、`reading.json`、`ch32rv.*`、`read.bin`、`take.json`）と、確認用
（holdout）の決め方、正解の使い方は `l103-linke-2026-11-17` と同じです。

## 読みについて

ch32rv の DMI の操作が 160 MS/s の読みの中に順にすべて見つかるのは 123 本中 101 本（`truth.json` の
`agreement_160M`）。見つからない 22 本は短い操作で、接続のあとのフレームが「読めなかった区間」になります（LinkE の
ファーム 2.23 は SWIO のフレームの間を詰めて送り、WireSkein の SWIO はそれを区切れない: bench/notes/2026-11-17-test-plan の
穴 b）。その 22 本の正解は ch32rv の DMI の操作と読み出しの結果だけで、遅く取ったものにも 160 MS/s の読みを正解にしません。
