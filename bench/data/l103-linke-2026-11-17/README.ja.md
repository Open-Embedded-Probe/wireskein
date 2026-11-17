# l103-linke-2026-11-17

WCH-LinkE → CH32L103C8T6 の RVSWD を、OEP のプローブで LinkE の側のピンから取り込んだ記録です。測定台の
`linke:` の元（`bench/wsbench/data.py`）が読みます。切り出す前の生の記録は、別のリポジトリ（wireskein-captures の
`l103-linke-2026-11-17/`、`SOURCE.json` にコミット）にあります。

## 取り方

| 項目 | 値 |
|---|---|
| target | CH32L103C8T6（UID `3a6dabcda282bc48`、chip ID `0x10310710`） |
| デバッガ | WCH-LinkE `0E028F0692F1`（ファーム 2.23）、ch32rv 0.31.2 |
| 取り込み | OEP ESP32-P4X `e8f60ae84dd7`（ファーム 0.0.56+dea9e3c）、GPIO12 = SWCLK、GPIO15 = SWDIO、LinkE の側で分岐、流し続け、グリッチフィルタなし |
| 速さ | 160 MS/s（`runs/rate/` は 160 MHz の整数分周と分数分周） |

1 回に ch32rv の操作 1 つ（wireskein-captures の `l103-linke-2026-11-17/tools/capture.py`）。取り込みを始めて 3 秒待ってから ch32rv を走らせ、取り込みの時計の
記録で操作が収まったことを確かめ、収まらなかった回と欠けた回は撮り直しています。

## 中身

- `runs/{high,medium,low}/<操作>`: LinkE の速さ（`--speed`）3 段で同じ 14 操作（`target_info`、`read_ram_256`、
  `read_flash_4k`、`flash_pattern4k`、`verify_pattern4k`、`read_pattern4k`、`dbg_halt`、`dbg_regs`、`dbg_step`、
  `dbg_resume`、`reset`、`erase_all`、`read_erased_1k`、`flash_sketch`）。
- `runs/high/x_*`: 追加の 61 操作（繰り返し、64 KiB と 20 KiB の読み出し、RAM への書き込みと読み戻し、レジスタの
  読み書き、step の連続、DMI の直接の読み書き、reset の種類、release-dm、connect-under-reset、エラーになる操作、
  オプションバイトの往復、クロックの設定を変えたイメージでの操作）。
- `runs/rate/<MHz>/<操作>`: 4 操作（`target_info`、`read_flash_4k`、`flash_pattern4k`、`low_read_ram_256`）を、
  プローブ自身が 160、133.3、50、10、8、5 MHz で取ったもの。

各 run は wireskein-run/1（`run.json`、`c0001.wireskein`）に次を添えています。

| ファイル | 中身 |
|---|---|
| `c0001.wireskein` | ch32rv が走った 20 ms 前から 100 ms 後までの切り出し（wireskein-captures の `tools/publish.py`、`meta.excerpt`） |
| `truth.json` | 正解（WireSkein の外）: `ch32rv.ndjson` の DmiOp（1 つが線上の DMI の 1 フレーム）の列 `dmi_ops`、`read.bin` の中身 `read_bytes`、160 MS/s の読みがそれと合うか `agreement_160M` |
| `reading.json` | その版の WireSkein の読み（正解ではない）: フレーム、ノイズ、クロックの速さ、DMI の列 `dmi`（eval の数え方） |
| `ch32rv.ndjson` / `ch32rv.json` / `ch32rv.stderr` | ch32rv の USB の往復（`--capture`）、出力（`--json`）、エラー出力 |
| `read.bin` | ch32rv が読んだ中身（読み出しの操作） |
| `take.json` | 取り方: 引数、終了コード、時刻、試行の回数、確認用か、切り出した区間 |

取り方の道具（`capture.py`）、正解（`truth.py`）、切り出し（`publish.py`）、速さごとの表（`rate_report.py`）、本物と間引きの比べ（`rate_compare.py`）と書き込んだイメージは、生の記録と一緒に wireskein-captures の `l103-linke-2026-11-17/tools/` にあります。

確認用（holdout）の半分は、取った時点でシナリオの名前のハッシュで決めてあります（`run.json` の `meta.holdout`。
`rate/` は操作の名前で、同じ操作はどの速さでも同じ側）。

## 正解の使い方

- 160 MS/s のままの記録: `read_bytes`（256 バイト以上の読み出し）と `dmi_ops`。
- 遅く取ったもの（`decimate`、`resample`）: それに加えて、同じ回の 160 MS/s の読み（`reading.json` の `dmi`）。
  ch32rv の DmiOp がすべて順に見つかる回（`agreement_160M.usable`）だけ。
- `rate/` の本物の遅い取り込み: `read_bytes` と `dmi_ops` だけ（その記録の読みは正解にしない）。
