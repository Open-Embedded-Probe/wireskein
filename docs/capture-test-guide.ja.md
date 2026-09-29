# キャプチャで確かめるテストの作り方

CH32 向け Arduino コアの動作確認テストに、ロジックアナライザのキャプチャでの検証を加えるためのガイドです。

## 前提

- ESP32-P4 がプローブです。CH32 ボードのできるだけ多くのピンと直結し、次の役割を兼ねます。
  - ピンのキャプチャ
  - RVSWD での書き込み
  - SPI・I2C のデバイス側のスタブ
- PC からのコマンドは、OEP の `target.console`（デバッグ線の上の dmseq）で CH32 に送ります。`PING`/`PONG`、`PWM 64` → `PWM duty=64` のような 1 行ずつのやり取りです。**このやり取りは、ロジックのキャプチャには映りません。**
- キャプチャは、ステップごとの単発取得です（`trace_kit.Capture`: `configure` → `arm` → `wait` → `read_all`。1 サンプル 1 バイトで、ビット k が k 番目のチャンネル）。x035 の PARLIO は 20 MHz まで、約 650 kHz より遅くはできません。記録の長さはおおよそ 64 KiB × 8 ÷ チャンネル数のサンプルです。
- キャプチャを使う今のテストは `tests/manual/oep_*` の単体のスクリプトです（pytest では集めていない）。多くは測定値を記録するだけで、許容誤差での OK/NG の判定はまだありません。

WireSkein は本来、未知の信号から方式を推定する道具です。このガイドの用途はそれとは違い、**意図した信号どおりに出ているかを確かめる**ものです。そのため、次の2点を前提にします。

- **期待をテストが持つ。** 方式・ピン・パラメータ・許容誤差は、テストから解析側に渡します。解析側は推定ではなく照合をします。WireSkein のヒント（`protocols`、`pins`、`baud`）と時間窓（`--window`）が、この用途の入口です。
- **区切りと静かな区間をテストが作る。** どこからどこまでが、どの期待に対応するかを、マーカーで示します。

## 1. どんな不具合をキャプチャで見るか

今のペアテスト（CH32 と P4 で値を受け渡して、OK か NG かを見る）で見つかるのは、主に値の不一致です。キャプチャは、値は合っているのに波形が仕様から外れている不具合を見つけるために使います。

| 分類 | 例 | ペアテストで見えるか |
| --- | --- | --- |
| 後始末 | I2C の後に SDA や SCL が High に戻らない。SPI の CS が High に戻らない。`end()` の後もピンが出力のまま | 見えない（次のテストで初めて壊れる） |
| 始まりの乱れ | `begin()` でピンの機能を切り替えるときのグリッチ。UART の初期化で出るゴミの1文字 | 受け側が捨てれば見えない |
| 時間のパラメータ | PWM の周波数やデューティー比の誤差、ボーレートの誤差、I2C の tLOW・tHIGH、SPI の CS からクロックまでの時間 | 受け側の許容範囲内なら見えない |
| 形式 | SPI のモード（CPOL/CPHA）、ビット順、ストップビットの数、I2C の Repeated Start | 受け側がたまたま同じ設定だと見えない |
| 余計な動き | 別のピンが動く（リマップの誤り）。余計なクロックが出る。更新時に PWM が欠ける | 見えない |
| 受け手のいない送信 | RGB LED（WS2812）、tone()、MCO のクロック出力 | P4 側で解析するのが面倒 |

特に効くのは**「このステップで動いてよいピン以外は、全部静止していること」**という検査です。全ピンを直結しているので、リマップの誤りや、ピンの初期化漏れを一度に見つけられます。

## 2. マーカー

マーカーの書式と区間の規則は、[ワークベンチのモデル](workbench-model.ja.md) の「マーカーで長い記録を実験の文脈に結び付ける」に従います。
この節では、それを CH32 のテストにどう当てはめるかを決めます。区間に分ける処理と照合はプロトタイプに実装済みです（`src/wireskein/_engine/markers.py`、`src/wireskein/runlog.py`、`src/wireskein/verify.py`、`wireskein verify`）。

### 2.1 マーカーは PC 側のログに書く

コマンドは console で送り、キャプチャは単発なので、マーカーを波形の中に置く場所がありません。そこで、マーカーはテストのスクリプトが **PC 側のログ** に書きます。これはワークベンチのモデルでいう「合成ソース」で、イベントのログと波形を時刻で合わせる形です。

| ログに書くもの | 例 | 記録器（`runlog.Recorder`）の呼び出し |
| --- | --- | --- |
| 見出しのマーカー | `# test_pwm`、`## duty=64`、`##` | `heading(level, name)`、`section(level, name, expect=...)` |
| 送ったコマンド | `PWM 64` | `command(text)` |
| 受けた応答 | `PWM duty=64`、`PONG 1234` | `reply(text)` |
| キャプチャ | `c0001.bin`、開始した時刻、レート、ビットとピンの対応 | `armed()` の後に `capture(data, rate, bits, t)` |

- 時刻はすべて PC の時計です。キャプチャは、開始（`arm`）した時刻で、どの区間に入るかが決まります。
- **CH32 のファームウェアは変えなくてよい。** マーカーは CH32 に送らないので、`#` の行の扱いは関係ありません。
- 見出しのマーカーでない行（コマンドと応答）は、区間の中のイベントとして時刻付きで残ります。主に GUI 向けで、CLI の照合結果には既定で出しません（`wireskein verify --json 出力 --log` で含める）。

**マーカーを波形の中に置く場合**（今後、長い記録やストリーミングで、コマンドをキャプチャする UART で送る場合）も、書式は同じです（`wireskein segments` で読めます）。そのときは、CH32 のコマンド処理に次の規則が必要です。今の `tests/sketches/testcmd.h` の `tc_ready()` は、`#` の行に `unknown cmd` や `ERR` を返します。

- `#` で始まる行はコメントとして捨て、応答もエコーもしない。
- コマンドの行は `#` で始めない。

### 2.2 書式: 見出し型

```text
# test_pwm                 ← 階層 1: テスト
## duty=64                 ← 階層 2: ステップ（1 つの条件）
PWM 64                     ← コマンド（イベント）
PWM duty=64                ← 応答（イベント）
                           ← ここでキャプチャを開始し、取り出す
##                         ← 名前のない同じ階層: ステップを閉じる
## duty=128
...
##
#                          ← テストを閉じる
```

| 規則 | 内容 |
| --- | --- |
| 階層 | `#` の数。テスト、ステップ、フェーズの 3 段を目安にする（深さに上限はない） |
| 閉じ方 | 同じ階層または上の階層の次の見出しで閉じる。名前のない見出し（`##` だけ）は閉じるだけで、新しい区間を作らない |
| 同じ名前 | 同じ親の下で名前が重なると、`duty=64[0]`、`[1]` のように連番（0 から）を付けて区別する。元の文字列は残す。期待も繰り返しごとに別に持つ（`Recorder` が `close()` のときに、`run.json` の期待のキーを連番付きのパスにする）。重ならない名前には連番を付けない |
| 親と子 | 親の区間を選ぶと、子の区間も含む全範囲を扱う。子の見出しは、親の中のイベントとして見える |
| 壊れた構造 | 階層の飛び（`#` の次に `###`）や、対応する見出しのない閉じは、照合の NG（`markers`）として出す。構造が崩れていると、キャプチャが別の期待で照合されるおそれがあるため |

`Recorder.section()` を `with` で使えば、閉じ忘れは起きません。

### 2.3 キャプチャの窓を決める

単発のキャプチャでは、「前の静かな区間・動作・後の静かな区間」を、1 回のキャプチャの窓の中に作ります。

| 確かめたいこと | キャプチャの開始 | 窓の長さ |
| --- | --- | --- |
| 定常の値（PWM・tone の周波数とデューティー比） | 応答を受けて少し待ってから（今の `periph_trace` と同じ） | 20 周期以上 |
| 通信の中身と後始末（I2C・SPI・UART） | コマンドを送る**前**（今の `i2c_trace` と同じ） | 通信の後に、ドライバの最長のタイムアウトより長い静かな時間が入るまで |
| 始まりの乱れ（`begin()` のグリッチ） | 初期化のコマンドを送る前 | 初期化の後、数 ms |
| 切り替わり（止めずに周波数を変える） | 変更のコマンドの前 | 変更の前後で、それぞれ 10 周期以上 |

- 終了状態（`ends`、`i2c` の `released`）は、窓の最後のサンプルで判定します。通信の直後で窓が終わると判定できないので、後ろに余白を取ります。
- 初期状態（`starts`）は、窓の最初のサンプルで判定します。
- `square` は、窓の全体が定常であることを前提にします。動作の後ろに静かな区間を取る窓（`TOGGLE`、`MILLIS` のように、決まった数だけ動いて止まるもの）では、`pulses(pin, count, period_s)` で数と周期を、`ends` で止まった後のレベルを確かめます。
- 動いてよいピン以外の静止（`only_moving`）は、そのキャプチャに入っているピンだけを見ます。**確かめたいピンは、キャプチャの割り当てに加えておきます**（チャンネル数を増やすと、記録できる長さが短くなります）。

### 2.4 期待はステップの見出しに付ける

期待は、見出しのパスをキーにして `run.json` に記録します。見出しの名前は、人が読めて区間を特定できれば十分です（`duty=64` など）。

```python
from wireskein.runlog import Recorder, square, level, only_moving, i2c

with rec.section(2, f"duty={duty}", expect=[square("PA1", 1003.5, duty / 255, tol_freq=0.01, tol_duty=0.01),
                                            only_moving(["PA1"])]):
    ...
```

| 検査 | 内容 |
| --- | --- |
| `square(pin, freq_hz, duty, tol_freq, tol_duty, max_jitter)` | 規則的な方形波。周波数は相対、デューティー比は絶対の許容誤差。サンプルレートによる分解能は自動で足す |
| `level(pin, value)` | 窓の間ずっと一定（duty 0 / 255 など） |
| `starts({pin: v})` / `ends({pin: v})` | 窓の最初・最後のレベル |
| `only_moving([pins])` | ほかのキャプチャ対象のピンが動かない |
| `pulses(pin, count, period_s, tol)` | 立ち上がりの数と周期（`TOGGLE` / `MILLIS`） |
| `i2c(scl, sda, transactions, hz, tol_hz, released)` | トランザクションの列（アドレス、読み書き、バイト、ACK）、SCL の周波数、終了時に両方 High。STOP のないまま窓が終わった最後のトランザクションは `"complete": False` で、最後の完全なバイトの後に打たれたビットを `pending_bits` に持つ。期待に `complete` を書かなければ `True`（完了）として比べ、`pending_bits` は書いたときだけ比べる。アドレスの途中で止まったものは `addr` が `None` |
| `spi(clk, mosi, miso, cs, mode, mosi_bytes, miso_bytes, hz)` | モード、両方向のバイト、SCK の周波数、終了時に CS が High |
| `uart(pin, baud, data, tol_baud, idle, bits, parity, stop, max_errors)` | ボーレート（エッジから測ったビットの幅と `baud` の相対の差が `tol_baud` 以内）、バイト列、アイドルレベル、フレームとパリティのエラー。`baud` には呼び値ではなく、送信側が実際に出すはずの値（CH32 なら F_CPU / BRR）を渡す。フォーマットは `bits`（データのビット数）、`parity`（`"none"` / `"even"` / `"odd"`）、`stop`（1、1.5、2）。窓が文字の途中で始まる・終わるのはエラーにしない（1 文字分以上の idle の後から読み、それより前は `lead_in`、最後に切れた文字は `cut_at_end` として数える）。`max_errors` を与えると、フレームとパリティのエラーの合計がそれを超えたら NG（既定は数えて出すだけ）。測定値は `baud`、`baud_error`、`samples_per_bit`、`chars`、`frame_errors`、`parity_errors`、`data`、`edge_offset_max` / `edge_offset_p99`（文字の中のエッジと、ビットの格子のずれ。ビット単位）、`errors_at`。エラーの文字がどれも、ほかの文字よりはっきりエッジがずれていれば（0.2 ビット以上、かつ p99 の 2 倍以上）、理由に「キャプチャの時間軸を疑う」と出る（ソフトで歩調を取るサンプラーが止まった場合など）。フレームエラーの後は次の 1 文字分の idle まで読み直しを待つので、1 つの乱れは 1 件と数える（その間に飛ばした文字は `resync_skipped`）。ボーレートの測定は期待から独立で、エッジの間隔から候補を求め、期待の値は最も近い候補を選ぶのにだけ使う |

検査に使うピンがキャプチャにないときは、どの検査も NG ではなく未検査（`--`、JUnit では skipped）になります。

例: 途中で止まった読み出し（0x42 から 1 バイト読み、ACK の後に target が SDA を握ったまま）

```python
i2c("PC16", "PC17", [{"addr": 0x42, "rw": "read", "bytes": [0x00], "complete": False}], released=False)
```

例: ボーレートのずれを測る（F_CPU 8 MHz、921600 bps、BRR = 9。1 ビットに 8 サンプル以上あるときだけ付ける）

```python
uart("PA2", 8_000_000 / 9, tol_baud=0.015, max_errors=0)                  # 8N1
uart("PA2", 8_000_000 / 9, tol_baud=0.015, parity="even", max_errors=0)   # SERIAL_8E1
```

測るだけ（期待のボーレートがない、範囲外の設定で何が出るかを残す）なら、`baud` に `None` を渡します。ビットの幅はエッジから推定し、その幅で読み直して求めます。ほかに期待（`data`、`max_errors`）がなければ、結果は OK / NG ではなく未検査（`--`、JUnit では skipped）で、理由の欄に測った baud、文字数、エラー数が出ます（`--json` には全部の測定値）。フォーマットの誤りは、フレームエラーやパリティエラーの数に表れます。

```python
uart("PB0", None)                        # 測るだけ（8N1 として読む）
```

ボーレートは、1 文字の中の同じ向きのエッジどうしの間隔から、ビットの幅を最小二乗で求めます。立ち上がりと立ち下がりの遅れの差は、この測り方で打ち消されます。1 ビットに 8 サンプルあれば、数百文字で 0.01 % 程度の精度になります（合成データで確認）。

### 2.4.1 `runlog` の呼び方は固定する

テスト側は `wireskein.runlog` を直接 import します。このため、次の呼び方と意味は変えません。互換のない変更をするときは `FORMAT`（今は `wireskein-run/0`）を上げ、`wireskein verify` は古い形式をはっきりしたエラーで断ります。

- 標準ライブラリだけで動くこと
- `Recorder(out, **meta)`、`heading`、`section(level, name, expect=None, **rules)`、`command`、`reply`、`note`、`armed`、`capture(data, rate, bits, armed_at, **meta)`、`close`
- 検査の helper（`square`、`level`、`starts`、`ends`、`only_moving`、`pulses`、`i2c`、`spi`、`uart`）の引数。引数やトランザクションのキーの追加は、既定値で今の意味を保つ形でだけ行う

### 2.5 時刻の精度

PC の時計と、キャプチャの開始時刻の対応は ms の精度です（`arm` の要求が P4 に届くまでの遅れがある）。区間の割り当てには十分ですが、コマンドとキャプチャの中の変化点のずれを µs で測る用途には使えません。精度が要る場合は、次のどちらかにします。

- **P4 のキャプチャの印を使う。** OEP には、UART や console のストリームに付ける `mark` がありますが、キャプチャの時間軸に付ける印は今の `oep.if.capture` にありません。付けるなら OEP の仕様への追加提案になります。クライアントの `Segment` が持つ `start_us` を記録に残すことが、その第一歩です。キャプチャの追加情報の名前は `start_us`（プローブの時計で最初のサンプルの時刻、µs、整数）に決めます: `rec.capture(data, rate, bits, t, start_us=seg.start_us)`。時刻合わせを実装するときも、この名前を読みます。
- **CH32 か P4 が、目印のピンを動かす。** 今の `reset_trace` の GPIO マーカーと同じやり方です。

## 3. テストの組み方

### 3.1 1つの条件を1ステップにする

PWM で周波数とデューティー比を複数試すときは、組み合わせごとに `##` の見出しでステップを作り、`##` で閉じます。テスト全体を1つの区間にすると、どの条件で外れたかが分かりません。

複数の周辺機能を同時に動かすテストでは、1ステップの期待に全部を書きます。`only_moving` には、そのステップで動いてよいピンを全部並べます。解析側は、検査が名前で指すピンを含むキャプチャごとに照合します。

### 3.2 ピンの対応表は1か所に置く

CH32 のピン名、P4 のキャプチャのチャンネル、P4 の役割（スタブの SPI など）の対応は、今の `tests/manual/oep_smoke/targets.py` の `TARGETS` にあります。テストと期待値は、CH32 のピン名（`PA1`）で書き、`capture(data, rate, bits=[...])` の `bits` にも CH32 のピン名を並べます。スクリプトの先頭に x035 の定数を直に書いている箇所（`periph_trace`、`i2c_trace`、`uart_trace`、`reset_trace`）は、`TARGETS` から取るようにそろえます。

### 3.3 最初にキャプチャ系の自己テストを入れる

セッションの最初に、P4 自身が既知の信号（決まった周波数の方形波など）を出し、それをキャプチャで測ります。サンプルレートや配線の誤りを、CH32 の不具合と取り違えないためです。

### 3.4 許容誤差の決め方

- **発振源の誤差を含める。** CH32 を HSI で動かすと、周波数は ±1〜2% ずれます。HSE（水晶）を使うテストと分けるか、許容誤差に含めます。
- **サンプルレートから決まる分解能を含める。** 1 周期が N サンプルなら、デューティー比の分解能は 1/N です。デューティー比を 1% で見るなら、1 周期に 100 サンプル以上必要です。周波数は、多くの周期の平均で測れば分解能が上がります。
- **目安**: 測る信号の周波数を、サンプルレートの 1/10 以下にします（時間のパラメータを測る場合）。P4 のキャプチャの最大サンプルレートと、同時に取れるピン数を先に確認しておくと、試せる上限が決まります。

## 4. 周辺機能ごとの検査項目

| 周辺機能 | 定常の区間で測る | 静かな区間・境界で確かめる |
| --- | --- | --- |
| GPIO | レベル、トグルの時間（`digitalWrite` の連続など） | ほかのピンが静止していること |
| PWM（analogWrite、TIM） | 周波数、デューティー比、周期ごとの揺らぎ、複数チャンネルの位相、相補出力のデッドタイム、極性 | 最初のパルスが欠けないこと、更新で欠けたり伸びたりしないこと、停止後のレベル |
| tone() | 周波数、持続時間 | 終了後のレベル |
| UART | ボーレートの誤差、データビット・パリティ・ストップビット、文字間の間隔、内容 | 初期化でゴミが出ないこと、アイドルレベル、`end()` の後のピンの状態 |
| I2C（Wire、P4 がスタブ） | SCL の周波数、tLOW と tHIGH、START・STOP・Repeated Start、ACK と NACK、クロックストレッチへの追従、内容 | 終了後に SDA と SCL が両方 High（戻し忘れ）、SDA が Low に張り付かないこと |
| SPI（P4 がスタブ） | モード（CPOL/CPHA）、ビット順、クロック周波数、内容、CS から最初のクロックまでと最後のクロックから CS までの時間 | 取引の間に CS が High、余計なクロックがないこと、アイドル時の MOSI |
| WS2812（RGB LED） | T0H、T1H、1 ビットの周期、バイトの順序（GRB）、LED の数 | リセットの Low（50 µs または 280 µs 以上）、送信後のレベル |
| 割り込み・pulseIn | P4 が入力を出し、CH32 が応答のピンを動かす。その遅れと揺らぎ | 応答のピン以外が静止していること |
| スリープ | スリープ中のピンの状態 | 復帰後のピンの状態 |

WS2812 と tone() は受け手がいないので、キャプチャでの解析を主な判定にします。I2C と SPI は、P4 のスタブでの値の照合（ペアテスト）と、キャプチャでの波形の照合の両方で判定します。

## 5. 判定の流れ

```
テストのスクリプト（今の oep_*_trace、将来は pytest）
  ├─ Recorder を開く（出力のディレクトリ）
  ├─ ステップごとに: section("## 条件", expect=[...]) の中で
  │     コマンドを送る → 応答を待つ → キャプチャを arm → armed() → wait → read_all → capture(...)
  ├─ close() → run.json と c0001.bin …
  └─ wireskein verify <出力> [--junit report.xml] [--json report.json]
        区間の木 → 区間ごとのキャプチャ → 検査 → 項目ごとの OK/NG と測定値。NG が 1 つでもあれば終了コード 1
NG のキャプチャは .bin のまま残る（PulseView で見るなら oep_client の to_sr で .sr にする）
```

今の `oep_periph_trace.py` の `run()` に当てはめると、次のようになります。

```python
rec = Recorder(out_dir, target=args.target)

def run(cap_lines, rate, samples, command, reply, settle=0.05):
    capture.configure(rate, samples)
    link.drain(0.05)
    rec.command(command)
    link.send(command + "\n")
    got = link.wait(reply, 5)
    line = next((l for l in link.text.splitlines()[::-1] if reply in l), "")
    rec.reply(line.strip())
    time.sleep(settle)
    capture.arm()
    t = rec.armed()
    st = capture.wait(3.0)
    data = capture.read_all(st.samples) if st.flags & FixtureCapture.COMPLETE else b""
    rec.capture(data, rate, [pin_name(ch) for ch in cap_lines], t)   # pin_name: P4 の GPIO → CH32 のピン名（TARGETS の逆引き、要追加）
    return data, line.strip(), got

with rec.section(1, "test_pwm"):
    for duty in (64, 128, 192, 255, 0):
        want = [square("PA1", 1000, duty / 255, tol_freq=0.02, tol_duty=0.01) if 0 < duty < 255
                else level("PA1", 1 if duty == 255 else 0)]
        with rec.section(2, f"duty={duty}", expect=want):
            run((PWM_GPIO,), 2_000_000, 40_000, f"PWM {duty}", "PWM duty=")
rec.close()
```

`tests/demo_run.py` は、この形の記録を合成して `wireskein verify` にかける例です。x035 の条件（PWM は 2 MHz で 4 万サンプル、I2C は 1 MHz で 6 万サンプル、SPI は 5 MHz で 4 チャンネル）で、わざと入れた次の 4 件がすべて NG になり、ほかの 13 項目は OK でした。

- duty 192 が 70% になっていた
- tone のステップで別のピンが動いた
- I2C の書き込みの後に SDA が Low のまま残った
- マーカーの階層が飛んでいた

## 6. WireSkein 側の対応状況

| 機能 | 状態 |
| --- | --- |
| 見出し型のマーカーで区間の木を作る。連番、名前のない見出しでの閉じ、階層の飛びなどの問題の記録 | 実装済み（`src/wireskein/_engine/markers.py`） |
| 記録器（ログ、キャプチャ、期待を 1 つのディレクトリに書く） | 実装済み（`src/wireskein/runlog.py`、標準ライブラリだけ） |
| 期待と照合して、項目ごとの OK/NG と測定値を出す | 実装済み（`wireskein verify`、JUnit XML、JSON） |
| プローブが報告する時間軸の乱れ（キャプチャの meta の `time_base_slipped: true`、OEP の区画の flags bit2） | 実装済み。判定は変えず、測定値に `time_base_slipped` を入れ、NG のときは理由に「the probe reported a time base slip in this capture」を足す |
| 検査: 方形波、一定のレベル、始まりと終わりのレベル、動いてよいピン以外の静止、パルスの数と周期、I2C、SPI、UART | 実装済み（x035 の実機の記録で、PWM、tone、TOGGLE、MILLIS、SPI、I2C を確認。v003 では SPI 以外を確認） |
| I2C の途中で止まったトランザクション（`complete`、`pending_bits`） | 実装済み |
| 取りこぼしたキャプチャの途中までのデータ（`capture(..., incomplete=True)` の meta で印を付ける） | 未実装（OEP v1 の one-shot では、完了しないと segment が報告されない。取りこぼしが問題になった時点で、テスト側で記録し、照合での扱いを決める。それまでは `note("capture incomplete")` だけで、その区間は「no capture inside the segment」の NG） |
| 1 本の長い記録の中のマーカー（UART の文字列）で区間に分ける | 実装済み（`wireskein segments`、`analyze --segment`） |
| I2C の tLOW と tHIGH、SPI の CS のタイミング、WS2812 のビットの幅などの時間の測定 | 未実装 |
| キャプチャの時間軸の印（P4 側）、`start_us` を使った時刻合わせ | 未実装（OEP のキャプチャの仕様に印がない） |
