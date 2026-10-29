# キャプチャで試験する

[English](capture-test-guide.md)

実機の試験に、ロジックアナライザのキャプチャでの照合を加えるためのガイドです。例は、CH32 向けの Arduino コア（[ArduinoCore-CH32RV](https://github.com/ch32-riscv-ug/ArduinoCore-CH32RV)）を、ESP32-P4 などの OEP のプローブで試す場合です。pytest からは [pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein) を使います。

この用途は、未知の信号から方式を推定することではなく、**意図した信号どおりに出ているかを確かめる**ことです。そのため、次の 2 点を前提にします。

- **期待を試験が持つ。** 方式・ピン・パラメータ・許容誤差は、試験から照合する側に渡します。
- **区切りと静かな区間を試験が作る。** どこからどこまでが、どの期待に対応するかを、マーカーで示します。

## 1. どんな不具合をキャプチャで見るか

値を受け渡して OK / NG を見るペアテストで見つかるのは、主に値の不一致です。キャプチャは、値は合っているのに波形が仕様から外れている不具合を見つけるために使います。

| 分類 | 例 | ペアテストで見えるか |
| --- | --- | --- |
| 後始末 | I2C の後に SDA や SCL が High に戻らない。SPI の CS が High に戻らない。`end()` の後もピンが出力のまま | 見えない（次のテストで初めて壊れる） |
| 始まりの乱れ | `begin()` でピンの機能を切り替えるときのグリッチ。UART の初期化で出るゴミの 1 文字 | 受け側が捨てれば見えない |
| 時間のパラメータ | PWM の周波数やデューティー比の誤差、ボーレートの誤差、I2C の tLOW・tHIGH、SPI の CS からクロックまでの時間 | 受け側の許容範囲内なら見えない |
| 形式 | SPI のモード（CPOL/CPHA）、ビット順、ストップビットの数、I2C の Repeated Start | 受け側がたまたま同じ設定だと見えない |
| 余計な動き | 別のピンが動く（リマップの誤り）。余計なクロックが出る。更新時に PWM が欠ける | 見えない |
| 受け手のいない送信 | RGB LED（WS2812）、tone()、クロックの出力 | 受け側で解析するのが面倒 |

特に効くのは **「このステップで動いてよいピン以外は、全部静止していること」**（`only_moving`）という検査です。DUT のピンをできるだけ多くプローブにつないでおけば、リマップの誤りやピンの初期化漏れを一度に見つけられます。

## 2. 記録の作り方

### 2.1 記録器

試験のスクリプトが、`wireskein.runlog.Recorder` で 1 つのディレクトリに記録します。記録の形式は [記録の形式](run-format.ja.md) にあります。

| 記録するもの | 例 | 呼び出し |
| --- | --- | --- |
| 見出しのマーカー | `# test_pwm`、`## duty=64` | `section(level, name, expect=[...])`（`with` で使う）、`heading(level, name)` |
| 送ったコマンド | `PWM 64` | `command(text)` |
| 受けた応答 | `PWM duty=64` | `reply(text)` |
| メモ | `capture incomplete` | `note(text)` |
| キャプチャ | `c0001.wireskein` | `t = armed()` の後に `capture(t, rate, interleaved=data, names=[...])`、または `capture(t, tick_hz, channels=[...])` |

- マーカーは PC 側のログに書きます。DUT には送らないので、DUT のファームウェアは変えずに済みます（[マーカー](markers.ja.md) §3.1）。
- キャプチャは、取ったときに開いている区間に割り当てます。
- コマンドと応答は、区間の中のイベントとして残ります（`wireskein verify --json 出力 --log` で報告に含める）。
- プローブが返す開始時刻とその不確かさも渡しておくと、ファイルに残ります: `capture(..., start_ns=seg.start_ns, start_uncertainty_ns=seg.start_uncertainty_ns)`。

### 2.2 1 つの条件を 1 ステップにする

```python
from wireskein.runlog import Recorder, square, level, only_moving

rec = Recorder("out/run1", target="x035")
with rec.section(1, "test_pwm"):
    for duty in (64, 128, 192, 255, 0):
        want = ([square("PA1", 1000, duty / 255, tol_freq=0.02, tol_duty=0.01)] if 0 < duty < 255
                else [level("PA1", 1 if duty == 255 else 0)]) + [only_moving(["PA1"])]
        with rec.section(2, f"duty={duty}", expect=want):
            rec.command(f"PWM {duty}")
            rec.reply(send_and_wait(f"PWM {duty}"))
            t = rec.armed()                        # キャプチャを開始した直後
            data = read_capture()                  # 1 サンプル width ビット、ビット k がチャンネル k
            rec.capture(t, rate, interleaved=data, names=["PA1", "PA0", "PC16", "PC17"])
rec.close()
```

- 周波数とデューティー比を複数試すときは、組み合わせごとにステップを作ります。試験全体を 1 つの区間にすると、どの条件で外れたかが分かりません。
- 複数の周辺機能を同時に動かすステップでは、1 ステップの期待に全部を書き、`only_moving` には動いてよいピンを全部並べます。
- 期待とキャプチャの `names` は、DUT のピン名（`PA1` など）で書きます。プローブのチャンネル番号と DUT のピン名の対応は、試験の側で 1 か所にまとめておきます。

### 2.3 照合

```sh
wireskein verify out/run1 --junit out/run1/report.xml --json out/run1/report.json
```

区間の木を作り、区間ごとのキャプチャを期待と照らし合わせ、項目ごとの結果（`ok`、`ng`、`unchecked`、`measured`）と測定値を出します。`ng` か `unchecked` が 1 つでもあれば終了コードは 1 です（`--allow-unchecked` で `unchecked` を失敗にしない）。NG のキャプチャは `.wireskein` のまま残るので、`wireskein gui` で見られます。

`tests/demo_run.py` は、この形の記録を合成し、不具合を入れて照合する例です。

## 3. キャプチャの窓を決める

単発のキャプチャでは、「前の静かな区間・動作・後の静かな区間」を、1 回のキャプチャの窓の中に作ります。

| 確かめたいこと | キャプチャの開始 | 窓の長さ |
| --- | --- | --- |
| 定常の値（PWM・tone の周波数とデューティー比） | 応答を受けて少し待ってから | 20 周期以上 |
| 通信の中身と後始末（I2C・SPI・UART） | コマンドを送る**前** | 通信の後に、ドライバの最長のタイムアウトより長い静かな時間が入るまで |
| 始まりの乱れ（`begin()` のグリッチ） | 初期化のコマンドを送る前 | 初期化の後、数 ms |
| 切り替わり（止めずに周波数を変える） | 変更のコマンドの前 | 変更の前後で、それぞれ 10 周期以上 |

- 終了状態（`ends`、`i2c` の `released`）は窓の最後のサンプルで、初期状態（`starts`）は最初のサンプルで判定します。通信の直後で窓が終わると判定できないので、後ろに余白を取ります。
- `square` は、窓の全体が定常であることを前提にします。決まった数だけ動いて止まるもの（トグル、millis）は、`pulses` で数と周期を、`ends` で止まった後のレベルを確かめます。
- `only_moving` は、そのキャプチャに入っているピンだけを見ます。**確かめたいピンはキャプチャに加えておきます**（チャンネルを増やすと、取れる長さやレートが下がるプローブもあります）。

## 4. 検査

| 検査 | 内容 |
| --- | --- |
| `square(pin, freq_hz, duty, tol_freq, tol_duty, max_jitter)` | 規則的な方形波。周波数は相対、デューティー比は絶対の許容誤差。`max_jitter` は周期のばらつきの上限（相対）。サンプルレートによる分解能は自動で足す |
| `level(pin, value)` | 窓の間ずっと一定 |
| `starts({pin: v})` / `ends({pin: v})` | 窓の最初・最後のレベル |
| `only_moving([pins])` | ほかのキャプチャ対象のピンが動かない。ロジックのチャンネルだけを見る |
| `pulses(pin, count, period_s, tol_period)` | 立ち上がりの数と周期。`tol_period` は相対 |
| `i2c(scl, sda, transactions, hz, tol_hz, released)` | トランザクションの列（アドレス、読み書き、バイト、ACK）、SCL の周波数、終了時に両方 High |
| `spi(clk, mosi, miso, cs, mode, mosi_bytes, miso_bytes, hz, tol_hz, bit_order, cs_active)` | モード、両方向のバイト、SCK の周波数、終了時に CS が High。モードは線から読む（§4.5） |
| `uart(pin, baud, data, tol_baud, idle, bits, parity, stop, max_errors)` | ボーレート、バイト列、アイドルレベル、フレームとパリティのエラー（下記） |
| `voltage(pin, volts, tol_v, min_v, max_v, ripple)` | アナログのチャンネルの電圧。平均が `volts` ± `tol_v`（V）、全サンプルが `min_v`〜`max_v`、ピークからピークが `ripple` 以下。与えたものだけを見る |
| `phase(a, b, phase_deg, tol_deg, freq_hz, tol_freq, edge)` | 2 本の周期的な線の位相。a のエッジから、同じ向きの b の次のエッジまでを、a の周期に対する角度（0〜360 度）にする。どの組も `phase_deg` ± `tol_deg`（度。0 の近くは回り込む）、両方の周波数が `freq_hz` の `tol_freq`（相対）の内 |
| `skew(a, b, tol_s, delay_s, edge)` | 一緒に変わる 2 本の線（パターン、並列の出力）のずれ。a のどのエッジにも、同じ向きの b のエッジが `delay_s` ± `tol_s` にある。相手の無いエッジは NG。`edge` が `"rise"` / `"fall"` ならその向きだけ（1 つのタイマの、デューティの違う 2 本の出力） |
| `dead_time(hi, lo, min_s, max_s, active, allow_overlap)` | ハーフブリッジの 2 本の出力（`active` で on）。片方が off になってから他方が on になるまでが、毎回 `min_s`〜`max_s`。両方が on（貫通）は、`allow_overlap` が無ければ NG |
| `i2c_timing(scl, sda, mode, limits)` | UM10204 の個々の時間（§4.4） |
| `rgb_led(pin, data, t0h, t1h, period, reset_min_s)` | WS2812 の類の線（§4.6）。リセットで区切ったフレームごとのバイト（送った順の 16 進）、T0H、T1H、ビットの周期、リセットの長さ |
| `onewire(pin, data, presence, speed)` | 1-Wire の線（§4.6）。リセットごとの後のバイト、presence の有無、リセット、presence、スロット、low の時間 |
| `ir(pin, frames, carrier_hz, tol_carrier, tol_time, protocol)` | IR のリモコンの線（§4.6）。NEC のフレーム（アドレス、コマンド、リピート）、コマンドとその反転、搬送波の周波数、マークとスペースの長さ |
| `lin(pin, baud, frames, tol_baud, min_break_bits, checksum)` | LIN のフレーム（§4.7）。識別子とデータ、sync、識別子のパリティ、チェックサム、break と区切りの長さ、同期の欄から測ったボーレート |
| `i2s(bclk, ws, sd, format, bits, fs_hz, tol_fs, samples, slot_bits, left_ws)` | I2S の類の音声（§4.7）。形式、サンプルの速さ、1 チャンネルの BCLK の数、続いて現れるサンプルの列 |
| `quadrature(a, b, steps, cycles, direction, max_errors)` | 2 相のエンコーダ（§4.7）。数（ステップか周期）、向き、両方の線が同時に変わった回数 |
| `sd_spi(clk, mosi, miso, cs, commands, mode, cs_active, max_ncr)` | SD カードの SPI のモード（§4.8）。コマンドの列（番号、引数、R1、R7 / R3 / R2、データのブロック）、NCR のうちの応答、CMD0 と CMD8 の CRC7、カードのブロックの CRC16 |

検査に使うピンがキャプチャにないなど、検査できなかったものは未検査（`unchecked`）です。

### 4.1 I2C の途中で止まったトランザクション

STOP のないまま窓が終わった最後のトランザクションは `"complete": False` で、最後の完全なバイトの後に打たれたビットを `pending_bits` に持ちます。期待に `complete` を書かなければ完了として比べ、`pending_bits` は書いたときだけ比べます。アドレスの途中で止まったものは `addr` が `None` です。障害の再現試験で「どこで止まったか」を確かめるためのものです。

```python
# 0x42 から 1 バイト読み、ACK の後に target が SDA を握ったまま
i2c("PC16", "PC17", [{"addr": 0x42, "rw": "read", "bytes": [0x00], "complete": False}], released=False)
```

### 4.2 UART

- `baud` には呼び値ではなく、送信側が実際に出すはずの値（CH32 なら F_CPU / BRR）を渡します。
- フォーマットは `bits`（データのビット数）、`parity`（`"none"` / `"even"` / `"odd"`）、`stop`（1、1.5、2）。
- 窓が文字の途中で始まる・終わるのはエラーにしません（1 文字分以上の idle の後から読み、それより前は `lead_in`、最後に切れた文字は `cut_at_end` として数える）。
- `max_errors` を与えると、フレームとパリティのエラーの合計がそれを超えたら NG です（与えなければ数えて出すだけ）。
- ボーレートは、1 文字の中の同じ向きのエッジどうしの間隔からビットの幅を最小二乗で求めます。立ち上がりと立ち下がりの遅れの差は、この測り方で打ち消されます。期待の値は、エッジから求めた候補を選ぶのにだけ使います。1 ビットに 8 サンプル以上あるときに付けます。
- **測るだけ**なら `baud` に `None` を渡します。ほかに期待（`data`、`max_errors`）がなければ、結果は `measured` で、測ったボーレート、文字数、エラー数が出ます。範囲外の設定で DUT が何を出すかを残すのに使います。
- エラーの文字がどれも、ほかの文字よりはっきりエッジがずれていれば、NG の理由に「キャプチャの時間軸を疑う」と出ます（ソフトで歩調を取るサンプラーが止まった場合など）。プローブ自身が遅れを報告したキャプチャ（`time_base_slipped`）も、NG の理由にそう添えます。

```python
uart("PA2", 8_000_000 / 9, tol_baud=0.015, max_errors=0)                  # 8N1
uart("PA2", 8_000_000 / 9, tol_baud=0.015, parity="even", max_errors=0)   # 8E1
uart("PB0", None)                                                         # 測るだけ
```

### 4.3 アナログの線にロジックの検査をかける（`threshold=`）

ロジックの検査（`square`、`level`、`starts` / `ends`、`pulses`、`i2c`、`spi`、`uart`、`phase`、`skew`、`dead_time`、`i2c_timing`、`rgb_led`、`onewire`、`ir`、`lin`、`i2s`、`quadrature`、`sd_spi`）は、`threshold=` を与えると、アナログのチャンネルをその電圧でロジックとして読みます。

- 1 つの値（例 `threshold=1.65`）か、ヒステリシスの組（例 `threshold=(1.0, 2.3)`: 2.3 V に達したら high、1.0 V まで下がったら low）。ノイズの乗った遅いエッジは、組にすると 1 本のエッジになります。
- `threshold=` のないロジックの検査にアナログのチャンネルを渡すと、未検査になります。電圧への換算がファイルにないチャンネルも未検査です。

### 4.4 時間の限度と分解能

`phase`、`skew`、`dead_time`、`i2c_timing`、`rgb_led`、`onewire`、`ir`、`lin` は、エッジの間の時間を限度と比べます。エッジは、その刻みの前の 1 サンプル（チャンネルのステップ）のどこかで変わったので、2 本の線のエッジの間の時間は、2 本のステップの大きい方（分解能）だけ不確かです。

- 測った値 ± 分解能が、すべて限度の内なら OK、すべて外なら NG、限度をまたげば未検査（理由に `near`）。未検査になったら、速いサンプルレートで取り直します。
- 限度はどの回にもかけ、いちばん悪い回で決めます。理由に、その値と時刻を書きます。
- 期待を何も与えなければ、測るだけ（`measured`）です。

`i2c_timing` は、UM10204（rev. 7）の表 10 の限度を `mode`（`"standard"`、`"fast"`、`"fast-plus"`）で選びます。`limits` で名前ごとに置き換えます（値 1 つは下限、組は `(下限, 上限)`。`mode=None` なら `limits` だけ）。

| 名前 | 測るもの | standard / fast / fast-plus |
| --- | --- | --- |
| `f_scl` | SCL の周波数（転送の中の立ち上がりから立ち上がり。いちばん短い周期） | ≤ 100 / 400 / 1000 kHz |
| `t_low` | SCL の low | ≥ 4.7 / 1.3 / 0.5 µs |
| `t_high` | SCL の high（START と STOP を含まないもの） | ≥ 4.0 / 0.6 / 0.26 µs |
| `t_hd_sta` | START（と repeated START）から SCL が下がるまで | ≥ 4.0 / 0.6 / 0.26 µs |
| `t_su_sta` | repeated START の前、SCL が上がってから | ≥ 4.7 / 0.6 / 0.26 µs |
| `t_hd_dat` | SCL が下がってから、SDA の最初の変化まで | ≥ 0 |
| `t_vd_dat` | SCL が下がってから、その low の SDA の最後の変化まで（データのビットだけ。START か STOP を含む high の手前の low は、条件の準備なので数えない） | ≤ 3.45 / 0.9 / 0.45 µs |
| `t_su_dat` | SDA の最後の変化から、SCL が上がるまで | ≥ 250 / 100 / 50 ns |
| `t_su_sto` | STOP の前、SCL が上がってから | ≥ 4.0 / 0.6 / 0.26 µs |
| `t_buf` | STOP から次の START まで | ≥ 4.7 / 1.3 / 0.5 µs |

- 線のしきい値で測るので、立ち上がりの遅い線の時間は、規格の 30 % と 70 % の点での時間と少し違います。立ち上がりと立ち下がりの時間（tr、tf）は測りません（アナログのチャンネルで見る）。
- SCL が high の両側で SDA が変われば START か STOP、どちらかの側で SCL が low なら、データの変化として数えます。
- ACK を target が出すときも、`t_vd_dat` は同じ限度（tVD;ACK）です。

```python
i2c_timing("PC2", "PC1", "fast")
i2c_timing("PC2", "PC1", "standard", limits={"t_su_dat": 300e-9})
```

### 4.5 SPI のモード

`spi` のモードは、線そのものから読みます。CPOL は CS が有効になったときの SCK の level です（CS が無ければ、いちばん長い休みの level）。CPHA は、MOSI（無ければ MISO）の変化の前の SCK のエッジで決めます。前が先のエッジ（休みの level から離れるエッジ）なら、そこで出して後のエッジで取る CPHA 1、後のエッジなら CPHA 0 です。CS が有効になってから最初のエッジまでの変化は、ほかに変化が無いときだけ使います（どちらのモードでも、CS で最初のビットを出す相手がある）。復号器が選んだモードは `decoded_mode` に残ります。CS が active high なら `cs_active=1` です。

### 4.6 1 本の線の方式

- **`rgb_led`**: 線は low で休み、1 ビットは T0H か T1H の high とその後の low、MSB から。ビットの周期の 5 倍より長い low でフレームを区切ります。0 と 1 は high の幅のいちばん大きい切れ目で分けます（全部同じ幅なら、周期の半分より短い high が 0）。限度の既定は WS2812B のデータシートの T0H 0.4 µs、T1H 0.8 µs（± 150 ns）、周期 1.25 µs（± 600 ns）、リセット 50 µs 以上。ほかの LED（SK6812、WS2811 の遅いモード、新しい WS2812B のリセット 280 µs）は、その値を渡します。最後のフレームの後で、リセットの長さを待たずに窓が終わると、LED に届いたかが決まらないので未検査です。
- **`onewire`**: 線は high で休み、どのパルスも low。`speed` の `reset_detect`（standard 288 µs、overdrive 48 µs）以上の low がリセット、その後の presence の窓の中で始まる low が presence、ほかの low がスロットです。スロットは low が `split`（standard 15 µs、overdrive 2 µs）より短ければ 1、長ければ 0、LSB から。読み出しのスロットも、運んだビットとして読みます（誰が線を下げたかは、線からは分かりません）。限度は Maxim の AN126 と DS18B20 のもの: standard ではリセット 480 µs 以上、presence の待ち 15〜60 µs、presence 60〜240 µs、スロット（low の始まりから次の始まり）61 µs 以上、スロットの間の high 1 µs 以上、0 の low 120 µs 以下、1 の low 1〜15 µs。
- **`ir`**: LED を駆動する線（搬送波付き。low で休む）も、受光モジュールの出力（high で休み、マークが low）も読みます。アクティブなパルスがほとんど 100 µs より短ければ搬送波付きとし、150 µs より近いパルスを 1 つのマークにまとめて、搬送波の周波数とデューティを測ります。NEC は 9 ms のマークの後、4.5 ms でデータ（32 ビット、LSB から: アドレス、反転したアドレスかアドレスの上位、コマンド、反転したコマンド）、2.25 ms でリピートです。アドレスとその反転が合わなければ、16 ビットの拡張アドレスとして `addr` に入れます。コマンドとその反転が合わないフレームは NG。`tol_time`（相対、既定 0.1）で、どのマークとスペースも NEC の長さと比べます（受光モジュールはマークの幅を変えるので、その出力では広くします）。

```python
rgb_led("PA7", ["ff0000", "00ff00"])                              # 2 回の書き込み、1 個ずつ
onewire("PD2", ["cc44", "ccbe"])                                  # Skip ROM + Convert T、Skip ROM + Read Scratchpad
ir("PB1", [{"addr": 0x04, "cmd": 0x08}, {"repeat": True}], carrier_hz=38e3)
```

### 4.7 LIN、I2S、2 相のエンコーダ

- **`lin`**: 線は high で休みます。`baud` で 11 ビット以上の low を break とし、その後の high を区切りとします。次の break か、2 文字より長い休みまでの文字（8N1）が、sync、保護された識別子、データ、チェックサム（最後の文字）です。各フレームのビットの長さは、スレーブと同じく sync の欄（0x55 の 5 つの立ち下がりが 8 ビット）から測り、その長さで文字を読みます。break は `min_break_bits`（既定 13、LIN 2.x のマスタの下限）以上、区切りは 1 ビット以上です。チェックサムは `checksum="auto"` なら、診断の 0x3C / 0x3D は classic、ほかは enhanced です。
- **`i2s`**: SD と WS を BCLK の立ち上がりで読みます。WS が変わるまでの立ち上がりが 1 チャンネル（スロット）です。`format` の `"i2s"`（Philips）は WS が変わった 1 BCLK 後に MSB、`"left"` は変わったところに MSB、`"right"` はスロットの最後のビットで語が終わります（`bits` が要る）。語は 2 の補数、MSB から。左のチャンネルは `"i2s"` で WS が low、ほかは high（違う相手は `left_ws`）。`samples` は、読んだフレームの中に続けて現れれば OK です（窓は音の途中で始まる）。WS と SD が BCLK の low のうちに変わった割合も出します（判定はしません）。
- **`quadrature`**: A が B より先に変わる向きを +1 とし、エッジごとに数えます（`steps`。`cycles` は 4 で割ったもの）。両方の線が同じ刻みで変わったら、状態を 1 つ飛ばしたとして誤りに数えます（エンコーダに対してサンプルが遅いか、故障）。

```python
lin("PA2", 19200, [{"id": 0x10, "data": "01020304"}])
i2s("PB13", "PB12", "PB15", "i2s", 16, fs_hz=48000, samples=[(0, 0), (1000, -1000)])
quadrature("PA0", "PA1", cycles=24, direction="forward")
```

### 4.8 SD カードの SPI のモード

`sd_spi` は、CS の窓の中の SPI のバイト（既定はモード 0。`mode=None` なら線から読む）を、SD のホスト（DUT）とカードのやりとりとして読みます。

- コマンドは MOSI の 6 バイト（`01` と番号、32 ビットの引数、CRC7 と終わりのビット）、R1 はその後 8 バイト（NCR）のうちに MISO に来る、ビット 7 が 0 の最初のバイトです。CMD8 と CMD58 は R1 の後に 4 バイト（R7、R3）、CMD13 は 1 バイト（R2）、CMD12 / 28 / 29 / 38 は busy（0x00）が続きます。CMD55 の次は ACMD です。
- 読み出し（CMD17、18、9、10、ACMD13、22、51）の後は、カードのブロック（トークン 0xFE、データ、CRC16）を読みます。CMD18 は CMD12 まで続けます。長さは CMD16 で決めたもの（既定 512）か、コマンドの決まった長さです。書き込み（CMD24、25）の後は、ホストのブロック（0xFE / 0xFC、データ、CRC16）、カードのデータの応答（0x05 で受けた）と busy、0xFD の後の busy を読みます。
- `commands` は順に、与えたキーだけを比べます（`cmd` か `acmd`、`arg`、`r1`、`resp`: R7 / R3 / R2 の 16 進、`data`: ブロックの 16 進の並び）。
- どのコマンドも NCR のうちに R1 が来て、終わりのビットが 1、CMD0 と CMD8 は CRC7 が正しく（SPI のモードで CRC が要るのはこの 2 つ）、カードのブロックの CRC16 が正しいことを見ます。ホストが書くブロックの CRC16 は、SPI のモードでは使わないことが多いので、出すだけです。

```python
sd_spi("PA5", "PA7", "PA6", "PA4", [{"cmd": 0, "r1": 0x01}, {"cmd": 8, "arg": 0x1AA, "resp": "000001aa"},
                                     {"cmd": 55}, {"acmd": 41, "r1": 0x00}, {"cmd": 17, "r1": 0x00}])
```

`wireskein analyze` も、SPI の上に SD のコマンドを探します（MOSI と MISO の向きを両方試す。コマンドの前後で線が 0xFF で待つことを証拠にする）。見つかれば、書き出しの SPI の `sd` に出します。

## 5. 許容誤差の決め方

- **発振源の誤差を含める。** 内蔵の RC 発振（CH32 の HSI など）で動かすと、周波数は ±1〜2 % ずれます。水晶で動かす試験と分けるか、許容誤差に含めます。
- **サンプルレートから決まる分解能を含める。** 1 周期が N サンプルなら、デューティー比の分解能は 1/N です。デューティー比を 1 % で見るなら、1 周期に 100 サンプル以上必要です。周波数は、多くの周期の平均で測れば分解能が上がります。
- **目安:** 時間のパラメータを測るなら、信号の周波数をサンプルレートの 1/10 以下にします。SPI の復号には、1 クロックに 5 サンプル程度が要ります。
- **キャプチャ系の自己試験を最初に入れる。** プローブ自身が既知の信号（決まった周波数の方形波など）を出し、それを測ります。サンプルレートや配線の誤りを、DUT の不具合と取り違えないためです。

## 6. 周辺機能ごとの検査項目

| 周辺機能 | 定常の区間で測る | 静かな区間・境界で確かめる |
| --- | --- | --- |
| GPIO | レベル、トグルの時間 | ほかのピンが静止していること |
| PWM | 周波数、デューティー比、周期ごとの揺らぎ、極性 | 最初のパルスが欠けないこと、更新で欠けたり伸びたりしないこと、停止後のレベル |
| tone() | 周波数、持続時間 | 終了後のレベル |
| UART | ボーレートの誤差、データビット・パリティ・ストップビット、内容 | 初期化でゴミが出ないこと、アイドルレベル、`end()` の後のピンの状態 |
| I2C | SCL の周波数、START・STOP・Repeated Start、ACK と NACK、内容 | 終了後に SDA と SCL が両方 High、SDA が Low に張り付かないこと |
| SPI | モード（CPOL/CPHA）、ビット順、クロック周波数、内容 | 取引の間に CS が High、余計なクロックがないこと |
| 電源・アナログ | 電圧の平均、範囲、リップル（`voltage`） | 切り替えの前後の電圧 |

WS2812 と tone() は受け手がいないので、キャプチャでの解析を主な判定にします。I2C と SPI は、プローブ側のスタブでの値の照合（ペアテスト）と、キャプチャでの波形の照合の両方で判定します。

I2C の tLOW と tHIGH、SPI の CS のタイミング、WS2812 のビットの幅の検査は、まだありません。

## 7. 時刻の精度

PC の時計とキャプチャの開始時刻の対応は、ms の精度です（開始の要求がプローブに届くまでの遅れがある）。区間の割り当てには十分ですが、コマンドとキャプチャの中の変化点のずれを µs で測る用途には使えません。精度が要る場合は、DUT かプローブが目印のピンを動かし、それをキャプチャに入れます。
