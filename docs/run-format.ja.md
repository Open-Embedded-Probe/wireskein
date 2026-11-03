# 記録の形式 `wireskein-run/1` と照合の結果

[English](run-format.md)

試験の記録と、その照合の結果の仕様です。この仕様に従えば、どの言語で書き読みしてもかまいません（Python では `wireskein.runlog` が書き、`wireskein verify` が読みます）。キャプチャのファイルそのものは [WireSkein のファイル形式](wireskein-format.ja.md)、区間の規則は [マーカー](markers.ja.md) にあります。互換のない変更では `format` を上げ、読み手は知らない値の記録を断ります（[設計の決定](design.ja.md) §1）。

## 1. 記録

記録はディレクトリです。

| 項目 | 中身 |
| --- | --- |
| `run.json` | ログ、キャプチャの一覧、期待（§2） |
| `cNNNN.wireskein` | 1 回ごとのキャプチャ（`c0001` から） |

記録をディレクトリにし、キャプチャを 1 回ごとに別のファイルにするのは、試験の途中で落ちても、それまでのキャプチャが残るようにするためです。

## 2. `run.json`

| キー | 型 | 意味 |
| --- | --- | --- |
| `format` | 文字列 | `"wireskein-run/1"`。読み手は、ほかの値の記録を断ります |
| `meta` | オブジェクト | 記録についての情報（`Recorder(out, **meta)` に渡したもの） |
| `log` | 配列 | ログ: `{"host_ns": 整数, "src": "marker" / "host" / "dut" / "note" / "lost", "text": 文字列}`。`host_ns` はホストの時計 `time.monotonic_ns()`（キャプチャの `meta.probe.clock` がホストを読む時計と同じ）。行は `host_ns` の順。プローブの時計の行は、さらに `probe_ns` などを持つ（§2.4） |
| `captures` | 配列 | キャプチャ: `{"file": 名前, "host_ns": 整数, "channels": [名前...], "path": 区間のパス}`。`host_ns` は arm した時刻 |
| `expect` | オブジェクト | 区間のパスから `{"checks": [検査...]}` へ |

### 2.1 区間とパス

- 見出し（`#` がテスト、`##` がステップ。名前のない見出しで閉じる）を `log` に `src: "marker"` で書き、区間の木を作ります。
- 区間のパスは、見出しの名前を `/` でつないだものです（例 `test_pwm/duty=64`）。
- **同じ親の下で同じ名前が繰り返されたとき、最初のものは名前のまま、2 回目から `[1]`、`[2]`、... を付けます**（例 `duty=64`、`duty=64[1]`）。あとから同じ名前を足しても、前の区間のパスは変わりません。
- 番号を 2 回目から付けるのは、あとから同じ名前を足しても、前の区間のパスと、それに付けた期待のキーが変わらないようにするためです。
- `wireskein analyze --segment` などのマーカーの区間のパスも、同じ規則です。

### 2.2 キャプチャの割り当て

- `captures[].path` は、キャプチャを取ったときに開いていた、いちばん深い区間のパスです。区間の外で取ったときは、キー自体を入れません。
- 照合は、`path` のあるキャプチャを、その区間（とその祖先）のものとして扱います。
- `path` のないキャプチャ（ほかの道具が書いた記録）は、その `host_ns` を含む区間に割り当てます。
- プローブの時計の行で開け閉めした区間は、例外です。その区間を含むキャプチャを、区間の時間だけ切り出して割り当てます（§2.4）。

### 2.3 検査

`checks` の各要素は `{"kind": ..., ...}` のオブジェクトです。`kind` は `square`、`level`、`starts`、`ends`、`only_moving`、`pulses`、`i2c`、`spi`、`uart`、`voltage`、`phase`、`skew`、`dead_time`、`i2c_timing`、`rgb_led`、`onewire`、`ir`、`lin`、`i2s`、`quadrature`、`sd_spi` です。各検査のキーと意味は [キャプチャで試験する](capture-test-guide.ja.md) §4 にあります（Python の `wireskein.runlog` の同名の関数が、このオブジェクトを作ります）。

検査が比べる値の名前（どの検査も同じ規則）:

| 形 | 意味 | 例 |
| --- | --- | --- |
| `<量>_<単位>` | 期待の値 | `freq_hz`、`delay_s`、`level_v`、`phase_deg`。比は単位なし（`duty`） |
| `<量>_rel` | 期待の値に対する相対の許容（`pytest.approx` の `rel` と同じ） | `freq_rel`、`baud_rel`、`period_rel` |
| `<量>_abs_<単位>` | 量の単位での許容（`pytest.approx` の `abs` と同じ）。比は単位なし | `delay_abs_s`、`level_abs_v`、`phase_abs_deg`、`duty_abs` |
| `<量>_min_<単位>`、`<量>_max_<単位>` | 限度。片側だけでもよい | `dead_min_s`、`t0h_max_s`、`reset_min_s`、`break_min_bits`、`ncr_max_bytes`、`errors_max`（数）、`jitter_max`（周期の標準偏差の、中央値に対する比） |

量は、期待の値と同じ語（`freq_hz` → `freq_rel`）。単位は最後（`_s`、`_hz`、`_v`、`_deg`、`_bits`、`_bytes`）。数と比には付けない。

- `threshold`（V、または `(low, high)`）を与えると、ロジックの検査（`square`、`level`、`starts`、`ends`、`pulses`、`i2c`、`spi`、`uart`、`phase`、`skew`、`dead_time`、`i2c_timing`、`rgb_led`、`onewire`、`ir`、`lin`、`i2s`、`quadrature`、`sd_spi`）が、アナログのチャンネルをロジックとして読みます。
- `only_moving` は、ロジックのチャンネルだけを見ます。

### 2.4 プローブの時計の行

プローブの上のコードが、自分の時計でログを持つことがあります（oep-probe-arduino の `OepProbeLog.h`、oep-client-python の `oep_client.probe_log`）。行を送らずにためるので、UART に流すより速く、ピンも要りません。`Recorder.probe_lines(rows, lost=...)` が、その行をログに書きます。

| キー | 型 | 意味 |
| --- | --- | --- |
| `probe_ns` | 整数 | 行の時刻。プローブの時計（起動からの ns。OEP core §7.7 の uptime_ns と同じ時計） |
| `boot_id` | 整数（OEP の boot_id） | その時計の起動 |
| `probe` | 文字列（任意） | どのプローブか（例 `"oep://30eda0ea068b"`: プローブの場所。経路を除いた OEP のアドレスで、どの経路で開いても同じ個体は同じ値） |
| `uncertainty_ns` | 数（任意） | 行の時刻そのものの不確かさ（±ns） |

- `host_ns` も必ず入れます（プローブの時計を、ホストの時計に換算した値）。区間の木と行の順番は `host_ns` で作ります。
- 見出しの行（`src: "marker"`）は、ほかの見出しと同じに区間を開け閉めします。ほかの行は `src: "dut"` です。
- **開く行と閉じる行の両方に `probe_ns` がある区間**は、キャプチャの中の位置で決まります。キャプチャの区間の `meta.start_ns` と同じ時計（`boot_id` が同じ。`probe` があればそれも同じ）なら、行の刻みは (`probe_ns` − `start_ns`) × 刻みの速さ です。その区間を含むキャプチャは、記録された `path` に関わらず、その区間に割り当て、区間の時間だけを切り出して検査します。
- 境の不確かさは、キャプチャの区間の `start_uncertainty_ns` と、行の `uncertainty_ns` の和です。検査は、境を両方に動かした 2 つの窓（広い窓と狭い窓）で行い、結果が違えば `unchecked`（境の位置で結果が変わる）にします。黙って `ok` にしません。区間が不確かさより短ければ `unchecked` です。
- 区間がキャプチャの前から始まる（後で終わる）ときは、キャプチャの端で切り、結果の `measured` の `clipped` に示します。
- 同じ時計のキャプチャがなければ、その区間の期待は「キャプチャがない」の `ng` です。黙って時刻の近いキャプチャを当てません。
- キャプチャの起動は、`meta.boot_id`、`meta.probe.boot_id`、または `meta.probe.clock` の `boot_id` から読みます。`Recorder.capture(..., boot_id=...)` で入れられます。
- プローブが行を落としたとき（ためる場所があふれた、印が飛んだ）は、`src: "lost"` の行になり、照合は `probe_log` の `ng` にします。どの区間が正しいか分からないからです。
- `Recorder.close()` は、プローブの行をキャプチャのファイルのマーカーにも写します（`by: "probe-log"`、[ファイル形式](wireskein-format.ja.md) §5.2）。ビューアで波形の上に見えます。
- プローブの行の区間の期待は、`Recorder.expect(パス, [検査...])` で付けます。

## 3. 照合の結果

照合は、検査ごとに 1 つの結果を返します（`wireskein verify` の報告）。

| キー | 意味 |
| --- | --- |
| `path` | 区間のパス |
| `capture` | キャプチャのファイルの名前（キャプチャに当たらなかったものは null） |
| `check` | 検査の種類（`kind`） |
| `status` | 下の 4 つのどれか |
| `ok` | `status` が `ok` なら true、`ng` なら false、それ以外は null（`status` を読むほうが確かです） |
| `expected`、`measured`、`reason` | 期待、測った値、理由 |

状態:

| `status` | 意味 | 既定で失敗か |
| --- | --- | --- |
| `ok` | 期待どおり | いいえ |
| `ng` | 期待と違う | はい |
| `unchecked` | 検査できなかった（ピンがキャプチャにない、電圧への換算がない、区間やキャプチャがない、など） | **はい**（`--allow-unchecked` で、いいえ） |
| `measured` | 測るだけと頼まれた（`uart(baud=None)` など）。測った値を返す | いいえ |

- `unchecked` を既定で失敗にするのは、多くが試験の書き間違いか、配線やキャプチャの抜けだからです。
- 要約（`summary`）は、`ok`、`ng`、`unchecked`、`measured` の数と、区間の数、キャプチャの数です。
- 報告の JSON（`--json`）と JUnit XML（`--junit`）の形は、この表のとおりです。JUnit では、`ng` は failure、`unchecked` は既定で failure（許すときは skipped）、`measured` は passed（system-out に測った値）です。
- テキストの行（`lines()`）は人が読むためのもので、形を約束しません。プログラムは JSON を読みます。
