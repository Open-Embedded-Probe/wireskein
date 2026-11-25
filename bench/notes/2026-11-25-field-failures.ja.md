# 現場の失敗の記録を受け取る（2026-11-25〜）

ユーザー（2026-11-25）: コアなどで失敗した波形を、都度もらう。流し直して通ったときも、流し直す前のデータが本当は欲しい。

## 頼んだこと（[core]、[ch32rv]、[EmbedBench]、[oep] に）

- wireskein の verify が NG か unchecked で、DUT は正しそうに見えるとき（wireskein の読み違いが疑わしいとき）に、run ディレクトリのパスと一行の説明を送ってもらう。
- 間欠の失敗で、流し直して通ったときも、**流し直す前の失敗した回の run を消さずに残して**、そのパスを送ってもらう。通った回のパスもあれば並べる。
- [EmbedBench] は、失敗した回の作業ディレクトリを録り直しのときに消さず、collect/failed/ に時刻付きで移すようにした（2026-11-25）。
- 送ってもらうのはパスと一行だけ。記録を整えたり、正解を別に作ったりは頼まない（[data-repos-stay-simple]）。

## こちらですること

- 受け取った run を写して置く（向こうの `work/` は消えたり上書きされたりするため）。置き場所と、ベンチの組（例: `field:`）にする形は、最初の数件を受け取ってから決める。
- 正解は、試験の期待値（送った内容が分かっているもの: shiftOut のバイト、書いたパターンなど）から取る。DUT が自分で読んだ値は正解にしない（truth-not-from-dut）。
- holdout は、受け取った時点で名前のハッシュで分け、中身を見ない。
- 直したら、頼んだセッションに知らせる。

## 受け取ったもの

| 日付 | 送り元 | run | 一行 | 直したか |
| --- | --- | --- | --- | --- |
| 2026-11-24 | [core] | v103r8t6-evt/softspi test_shift（11-24_11-42-05） | spi、CS なしで mode 3 vs 0（CPOL の読み違い、間欠） | 直した（verify の CPOL、v0.0.38） |
| 2026-11-25 | [core] | v203c8t6/signals test_pwm_sequence（v203sig-1125-1） | convert で VCD に書くと TypeError（60 MS/s）。道具の不具合で、読みの問題ではない | 直した（vcdio、v0.0.38） |
| 2026-11-25 | [core] | v006k8u6/softspi test_shift（softspi-ws038-2、11-25_04-29-39。通った回は softspi-ws038-1） | spi、CS なしで「mosi None vs f0fac0ffffff」（解析の読みが変化の少ない MOSI を外した、間欠） | 直した（verify が名指しの線を標本の辺で読む、次の版） |
| 2026-11-20 | [EmbedBench] | DeviceData の sht30-modes 5 件（下） | 待ちきれずに止めたストレッチの後の 1 バイトが 61 / b0 / 60 と揺れる | 直した（読まない印 clock_lost、次の版） |

**正しく NG を出した例**（読みが疑わしい側ではない。「NG が正解」の組に使える）:

| 日付 | 送り元 | run | 中身 |
| --- | --- | --- | --- |
| 2026-11-25 | [core] | v203c8t6/signals test_pwm_sequence（v203sig-1125-1） | DUT の不具合（CH32RVPwmSequence の play() がデータの前に余分な 1 周期を出す。PA8 は 49 パルス、ArduinoCore-CH32RV cfba6c60 で直した）。期待値 102030a5c30f に対して NG が正解 |

## sht30-modes: 待ちきれずに止めたストレッチの後の 1 バイト（2026-11-25）

[EmbedBench] DeviceData の 5 件（EmbedBenchDeviceData の runs/ の下の sht30-modes__i2c-*、run e6e40c83・051aec57・dd2f2a02（50 MS/s、100k / 400k / 1M）、70985ea6・77c69e5b（2 MS/s、100k））。記録は読むだけ（写していない）。e6e40c83・051aec57・dd2f2a02 は ebdd-50m-holdout に入っている（sht30-modes は名前のハッシュで holdout の側）。70985ea6・77c69e5b はベンチのデータに入っていない: 直しの形は合成で決め、5 件は確かめにだけ使った。今後この 5 件はきれいな確かめにならない（見て書いたため）。

### 線で起きていること（5 件とも同じ形）

- `2c06` を書き（高精度・クロック引き延ばしの測定）、続けて 0x44 の read。アドレスの 9 クロック目（ターゲットの ACK）の立ち下がりの後、ターゲット（SHT30）が SCL を Low に保つ: 50 MS/s で 606 900〜612 038 tick（約 12.1〜12.2 ms）、2 MS/s で 24 283〜24 294 tick（約 12.1 ms）。
- 待ちの終わり: SDA が先に下がり（ターゲットが自分の最初のビット 0 を置く）、約 6 µs 後に SCL が上がる。その最初の High だけが短い（100 kHz で 2.8 µs: いつもは 5 µs、400 kHz で 0.94 / 1.36 µs、1 MHz で 0.58 / 0.62 µs）。そこからいつもの周期で 8 回上がり、9 回目の立ち上がりで SCL は High のまま、約 20 µs 後に SDA が下がる（Sr）。STOP はない。続く Sr の後は 0x44 write `240b`（P）。
- 待ちの後の立ち上がりは 9 回で、その取引は 18 ビット（9n）: STOP / Sr の前の立ち上がりを含む普通の取引の 9n + 1 より 1 つ少ない。コントローラ（ESP32-P4）は待ちきれずに諦めた所で、SCL が保たれたまま 1 クロック（自分の D7）を進め、それは線に写らない。ターゲットは見えた立ち上がりで D7 から数えるので、両者は 1 ビットずれる: ターゲットの数え方なら 9 回が D7..D0 と ACK で `61` NACK（2 MS/s では SDA が SCL と同じサンプルで変わる所があり `60`）、コントローラの数え方なら写らない D7 に 7 回の D6..D0、8 回目が自分の NACK、9 回目が Sr の立ち上がり。待ちの中のどこで D7 を取ったかは線から分からない（SDA は待ちのほとんどで 1、最後の 6 µs だけ 0）。
- 前の読み: 0.0.27 の記録の復号（c0001.i2c.json）は 9 回をそのまま読んで `61` NACK、今の Python（HEAD）は 1 クロックを補い（timing.i2c_restore: 9n で長い Low が 1 つ → そこに補う）`b0` NACK（50 MS/s の 3 件、`restored` 付き）、2 MS/s の 2 件は補わず `61` / `60`（0.0.28 の記録は `b0`、77c69e5b では ACK）。Rust のコア（HEAD）は補わず、9 ビットをそのまま `61`（2 MS/s の 77c69e5b は `60`）と読む。どれも「確かな値」として出していた。ファームウェアはそこで読み出しを止める（タイムアウトのエラー）ので、正しいのは「決まらない」。

### 直したこと

- 補わない: i2c_restore は、長い Low がクロックの並びの Low 2 つ分（`RESTORE_LOW_MAX` = 2 × CLIPPED_LOW_MAX = いつもの Low の 8 倍）までのときだけ補う。それより長いのは待ちで、遅い立ち上がりで切られたクロック（Low・一瞬の High・Low）ではない。Python の timing と Rust の verify の両方。
- 読まない印: 閉じた（P / Sr）取引が 9n ビットで、中に SCL の待ち（その取引の Low の中央値の 4 倍超、`scl_waits` と同じ）があれば、最初の待ちが入る語（または待ちの直後の語）から後を読まない。既にある「区間の終わりで切れた取引」と同じ形にした: `complete: false`、そこから打たれたビットをサンプルしたまま `pending_bits`、新しく `clock_lost`（待ちの始まりの tick）。`end_cond` はそのまま（Sr）。アドレスの中なら `addr: null`。Python（typed._lost_in_wait）、Rust のコア（i2c.rs）、Rust の verify（decoded.rs）。キーは analysis-format の i2c の行に足した（registry は層までなので変えない）。
- verify の i2c: 待ちの後のバイトは一致とも不一致とも言わない。期待に `complete: False` が書いてなければ、切れる前（アドレス、読めたバイトと ACK、start / end）だけ比べ、違えば今までどおり `ng`、合えば `unchecked`（理由に「clock was lost in an SCL wait」と両側）。通知 `clock_lost`（`tick`、`bits`）を足した。コントローラに諦めさせる試験は `complete: False` と切れる前のバイトを書けば `ok`。Python と Rust（WIRESKEIN_RUST=verify）で同じ。

### 確かめ

- 合成（tests/test_i2c_lost_clock.py、正解は合成の側）: 書き込み・待ちのある read・Sr・書き込み。諦めたコントローラ（待ちの後 9 回、最初の High が短い）と待ちきったコントローラ（10 回）を 2 / 10 / 50 MS/s で。諦めた方は 1 バイト目でも 2 バイト目でも、Python・WIRESKEIN_ENGINE=rust・WIRESKEIN_RUST=decode で `complete: false`、`pending_bits` がターゲットの 8 ビットと 1、`clock_lost` が待ちの始まり。待ちきった方は今までどおり `5a61`。verify は unchecked / 前の違いは ng / `complete: False` を期待すれば ok / 待ちきった方は ok を、Python・ENGINE=rust・RUST=verify で。
- 5 件（確かめだけ）: Python と Rust（WIRESKEIN_ENGINE=rust）で 5 件とも、その取引だけが `bytes: ""`、`complete: false`、`clock_lost`、`pending_bits` が `0 1 1 0 0 0 0 1 1`（77c69e5b は `0 1 1 0 0 0 0 0 1`）、`end_cond: Sr` に変わった。ほかの取引（45〜50 本）は前と同じで、Python と Rust で同じ。
- ベンチ: e2e、-j 1 で 1 組ずつ。直す前（HEAD dd40095 の src とそこから作った wireskein_core）と後を、同じ機械で wireskein（Python）と engine.rust の両方。組は real-i2c（h417_slow/stream、61）、real-i2c-tune（h417_slow/noise、326）、real-ebdd（35）、ebdd-50m（15 × 5）、ebdd-50m-holdout（30 × 5）。ok、decode の和、各例の行（outcomes・false_*）がすべて同じ。違ったのは Python の real-i2c の fexp2-50000000-g75 の 1 件だけで、直した後の回で 60 s の時間切れ（前は missed）。その 1 件を単独で前と後で流すと、どちらも CPU 約 27 s・主張なしで同じ: 同時に走らせた Rust のビルドで遅れたもの。台帳には入れていない（WSBENCH_RESULTS を scratch に）。
