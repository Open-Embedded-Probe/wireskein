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
        want = ([square("PA1", 1000, duty / 255, freq_rel=0.02, duty_abs=0.01)] if 0 < duty < 255
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
- 期待とキャプチャの `names` は、DUT のピン名（`PA1` など）で書きます。プローブの線と DUT のピン名の対応は、試験の側で 1 か所にまとめておきます。
- 区間と欠落ごとまとまって来るキャプチャ（ストリーム、repeat）は、そのまま加えます: `rec.add(t, cap)`。`cap` は `wireskein.capture.Capture`（OEP のプローブなら、試験が持つセッションで `wireskein.sources.oep.capture_of(acquire.capture(hst, req))`）か、書いた `.wireskein` のパス。
- `rec.close()` が記録を書いた後（pytest-embedded-wireskein は試験の本体が終わったときに閉じる）は、何を足しても例外になります。記録から黙って落ちるものはありません。

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

### 3.1 1 本のキャプチャで複数の条件を通す

パラメーターが複数ある試験は、すべての条件を 1 つのファイルに残せます。試験が条件を順に進めるあいだ 1 本のキャプチャを取り続け、条件ごとにその区間にします。検査が失敗したら、区間のパスでどの条件かが分かり、ファイルではすべての条件を 1 本の時間の軸で見られます。

```python
from oep_client import acquire
from wireskein.runlog import becomes, level, only_moving
from wireskein.sources.oep import capture_of

def ask(wait, text, reply, timeout_s=2.0):
    """コンソールで命令を送り、短い wait のあいだに応答を探す（そのあいだも stream を受け取り続ける）。"""
    rec.command(text)
    send(text)
    for _ in range(int(timeout_s / 0.01)):
        wait(0.01)
        line = read_reply(reply)                   # 受け取り済みの応答の行、または None
        if line:
            rec.reply(line)
            return line
    raise TimeoutError(f"{text!r}: no reply")

def steps(wait):
    rec.use_clock(lambda: hst.clock_best(2), probe=hst.address.place)
    with rec.section(1, "servo PB6", expect=[only_moving(["PB6"])]):
        with rec.section(2, "rest", expect=[level("PB6", 0)]):
            wait(0.05)
        for us in (544, 1000, 1500, 2000, 2400):
            with rec.section(2, f"us={us}", expect=[becomes("PB6", period_s=0.02, high_s=us * 1e-6, high_abs_s=20e-6)]):
                ask(wait, f"servo 6 {us}", "SERVO")
                wait(0.5)                          # 25 周期
        with rec.section(2, "detach", expect=[becomes("PB6", level=0)]):
            ask(wait, "servooff", "SERVOOFF")
            wait(0.05)
    rec.use_clock(None)

t = rec.armed()
lines = [("PB6", "x035@pb6"), ("PB7", "x035@pb7"), ("PB8", "x035@pb8")]   # 対象のピンと、only_moving のための隣のピン
req = acquire.Request(lines, 1_000_000, 0, mode="stream", during=steps)
rec.add(t, capture_of(acquire.capture(hst, req)))
```

- **キャプチャ**は、試験が持つセッションでの stream です。`during=steps` を渡すと `steps(wait)` が戻るまで取り続け、`wait(秒)` は試験が待つあいだに stream を受け取ります（oep-client-python 0.0.47 以降）。steps の中でプローブに送るもの（コンソールの命令、治具の操作）は、同じセッションを通ります。
- **`wait` をこまめに呼びます。** wait と wait のあいだは、プローブの stream のバッファだけがデータを持ちます。steps の中でほかに止まる処理（応答を待つなど）は短くするか、短い wait のあいだに応答を探します。
- **見出しをプローブの時計に置きます。** `use_clock` は見出しごとにプローブの時計を読むので、区間ごとにキャプチャから切り出せます（[記録の形式](run-format.ja.md) §2.4）。`probe` は、キャプチャの `meta.probe.address` と同じプローブのアドレスです（oep-client-python では `hst.address.place`）。`use_clock` が無いと、キャプチャ全体が arm したときに開いていた区間のものになり、条件を分けられません。
- **条件ごとに 1 つの区間にし、その命令から次の命令までとします。** 区間は変化とその後を持ち、期待はその両方を言います（§3.2）。既定は `becomes` で、変化と行き着く状態を判定します。変化を切り落として定常を見る（`trim`）のは、変化を問わないと決めた試験だけです。判定した範囲は結果（`trimmed`）に出ます。期待の無い区間は残しません。
- **まわりのピン。** `only_moving` は、キャプチャに入っているピンしか見ません。対象のピンの隣（と、試験が触ってはいけないピン）をキャプチャのチャンネルに入れます。
- **前後に静かな区間を置きます。** 最初の条件の前と、最後の条件の後（出力を止めた）の区間が、試験の外での線の状態を持ちます。
- **離したピン。** ピンを入力に戻すと、線はボードが引く先の値になります。プルが無ければ値は決まらず、プローブからは「Low のまま浮いている」と「Low を出している」を見分けられません。離したことをレベルで確かめるのは、ボードかプローブがプルしている線だけにします。それ以外では、離す区間には `only_moving`（ほかが動かないこと）を付け、レベルは問いません。
- **ファイル**は 1 本のキャプチャ `c0001.wireskein` で、見出しがそのマーカーになり、ビューアで波形の上に見えます。`wireskein verify` は、区間ごとに、判定した範囲（`c0001.wireskein@A:B`）を付けて報告します。線が何をしたかは `wireskein states` が並べます（§3.3）。
- **欠損:** `on_loss="stop"`（既定）では、欠損があると `wait` から例外が出て、stream はそこで終わります。取れた分は残り、記録は失敗になります。
- stream にはプローブからの通知が要ります。購読を断る中継（OEP core §10.3）を通すと stream は取れないので、条件ごとに単発のキャプチャ（§2.2）を取ります。

### 3.2 信号ごとの検査の選び方

条件の区間は、変化から始まります。試験がそこに問うことは、次の 2 つのどちらかです。

- **どう変わったか**: 線が前の状態から求める状態へ 1 度だけ移ったこと、そして変化のところで何が起きたか。`becomes(pin, ...)` は、前の状態を区間の直前のキャプチャから読み、変化を見つけ、その後のすべての周期を判定します。変化のところでは、どちらの状態でもないパルスを `irregular_max` 個まで許します（既定 1: 周期の途中で切れたタイマーの出力。出力を始めるときと止めるときは、多くのマイコンで避けられません）。`irregular_min` で、あることを求めることもできます。区間の始めから変化までの時間（`change_s`。上限は `within_s`）と、その半端なパルスそのもの（`irregular_pulses`）を測ります。
- **定常だけ**: 変化は問わず、落ち着いた後の値だけを見ます。どの検査も `trim(検査, head_s=..., tail_s=...)` または `trim(検査, head_periods=...)` で、区間の両端を除いて判定します。

| 信号 | 問うこと | 検査 |
| --- | --- | --- |
| 設定の変わる連続した波形（PWM のデューティー比、tone の周波数、サーボのパルス幅、クロックの出力） | 新しい値、その全周期、きれいな変化 | 周波数か周期と、デューティー比か high の時間を渡した `becomes`。周期の終わりで切り替わるハードウェア（プリロード付きのタイマー）では `irregular_max=0` |
| 同じもので、値だけが問題のとき | 落ち着いた後の値 | `trim(square(...), head_periods=...)` |
| 始まる、止まる出力（PWM の on / off、`tone` / `noTone`、`attach` / `detach`） | 最初と最後のパルス、止めた後のレベル | 止めるときは `becomes(pin, level=...)`、始めるときは静かな区間から `becomes(...)` |
| レベル（GPIO の書き込み、0 % や 100 % の PWM） | そのレベルが続くこと | 区間の中で変わるなら `becomes(pin, level=v)`、変わらないなら `level(pin, v)` |
| 決まった数のパルス（トグル、バースト） | 数と周期、その後のレベル | `pulses` と `ends` |
| やりとりのバス（I2C、SPI、UART、1-Wire、LIN） | 中身と順番、タイミング、バスが静かに戻ること | バスの検査（`i2c`、`spi`、`uart` など）を区間に。区間の境の静かな状態は `starts` / `ends`。切り落とさない（やりとりはひとまとまり） |
| 2 本の線の関係（ハーフブリッジ、位相、並列の出力） | 関係 | `dead_time`、`phase`、`skew` |
| アナログのレベル | 平均、範囲、リプル | `voltage`。区間が段差で始まるなら `trim` を付ける |
| ほかの動いてはいけないもの | 静かなこと | 外側の区間に `only_moving` |

連続した波形とやりとりのバスでは、問いのありかが違います。波形は周期ごとに判定するので、変化は 2 つの状態のあいだの点で、その近くが見どころです。やりとりは始めからバスが静かに戻るまでのひとまとまりで判定するので、区間はそのやりとりとその後の時間（ドライバの最長のタイムアウト）で、切り落としません。

### 3.3 要約、詳細、判定

WireSkein は、キャプチャを読み手に要るものに変えます。サンプルそのものを読まずに済ませるためです。どれだけ出すかは、問いで決めます。

| 欲しいもの | 使うもの | 出てくるもの |
| --- | --- | --- |
| 線で何が起きたか、手短に | `wireskein states FILE [--pin P]` | 区切りごと（レベル、または波形: 周波数、high の時間、デューティー比、周期の数）と、そのあいだの半端なパルスを、1 行ずつ |
| 同じものを、試験の記録の区間ごとに、期待なしで | `wireskein states RUN --section 'パス'`（Python では `wireskein.verify.sections(run)` が、区間ごとにキャプチャから切り出したものを返す。どの分析にも使える） | パスの合う区間ごとの区切り |
| バスが何を運んだか | `wireskein analyze` / `decode` | 復号したやりとり |
| 試験が通ったか、通らなかったのはどこか | `wireskein verify` | 検査ごとに 1 つの結果と、測った値 |
| ある範囲のすべての変化 | `wireskein gui`、または Python でファイルを読む（`wireskein.capture`） | すべて |

- まず要約から始め、何かが見えたところだけ詳細へ進みます。`states` が半端なパルスの時刻を言い、ビューアでそこを見ます。
- 検査は許容を先に受け取り（`freq_rel`、`duty_abs`、`irregular_max` など）、判定にかかわらず測った値を出します。測るだけで判定しない検査は、そう書けます（`uart(baud=None)`）。許容を決めても、失う情報はありません。
- `states` には自分の許容があります（`--tol`、`--min-periods`）。その中でそろったパルスを 1 つの波形とします。許容を狭くすると、区切りが増えます。
- どの検査も持たないきまりは、試験が自分で判定します: 区間の分析（`wireskein.verify.sections(run)`、それに `states`、`decode` など）を読んで判定し、`rec.result(パス, 名前, 状態, measured=..., reason=..., notices=[{"kind", "tick"}])` で記録します。照合はそれを自分の検査と一緒に報告して数えるので、1 つの記録と 1 つの報告に、すべての判定が残ります。

### 3.4 秒か、周期の数か

| 量 | 単位 | 理由 |
| --- | --- | --- |
| 区間が波形を持つ長さ | 周期の数（周波数とデューティー比なら 20 以上。`becomes` の `min_periods` 以上） | 検査は周期を判定する。同じ数なら、どの周波数でも同じ確かさになる |
| 波形の区間から切り落とす長さ | 周期の数（`head_periods`） | 変化は、周波数によらず、信号の 1〜2 周期続く |
| 応答やバスが静かになるのを待つ長さ | 秒 | ファームウェアとドライバ（タイムアウト）で決まり、信号では決まらない |
| 変化がいつまでに起きるべきか | 秒（`within_s`） | DUT が命令に応じるまでの時間 |
| 試験の前後の静かな区間 | 秒。まわりの波形のいちばん遅い周期より長く | 1 周期より長く続いたときだけ、波形の Low の半分とレベルを見分けられる |

## 4. 検査

| 検査 | 内容 |
| --- | --- |
| `square(pin, freq_hz, duty, freq_rel, duty_abs, jitter_max, outliers_max)` | 窓全体で規則的な方形波。すべての周期が `freq_rel`（相対）に、その high の時間が `duty_abs`（周期に対する絶対）に入ること。外れてよい周期は `outliers_max` 個まで（既定 0）。`jitter_max` は周期のばらつきの上限（相対）。サンプルレートによる分解能は自動で足す |
| `becomes(pin, level=v)` / `becomes(pin, freq_hz= か period_s=, duty= か high_s=, high_abs_s か high_rel, irregular_max, irregular_min, min_periods, within_s)` | 線が窓の終わりにある状態（レベル、または波形）にあり、窓の前の状態から 1 度だけの変化でそこへ移ったこと（§3.2）。`change_s`、`irregular_pulses` を測る |
| `trim(検査, head_s, tail_s, head_periods=, tail_periods=)` | どの検査も、窓の両端を除いて判定する（§3.2） |
| `level(pin, value)` | 窓の間ずっと一定 |
| `starts({pin: v})` / `ends({pin: v})` | 窓の最初・最後のレベル |
| `only_moving([pins])` | ほかのキャプチャ対象のピンが動かない。ロジックのチャンネルだけを見る |
| `pulses(pin, count, period_s, period_rel)` | 立ち上がりの数と周期。`period_rel` は相対 |
| `i2c(scl, sda, transactions, freq_hz, freq_rel, released)` | トランザクションの列（アドレス、読み書き、バイト、ACK）、SCL の周波数、終了時に両方 High。トランザクションごとに `start`（`S`、`Sr`）と `end`（`P`、`Sr`）を書けば、それも比べる |
| `spi(clk, mosi, miso, cs, mode, mosi_bytes, miso_bytes, freq_hz, freq_rel, bit_order, cs_active, bits, notices)` | モード、両方向のバイト、SCK の周波数、最後に CS が High。モードは線から読む（§4.5）。`cs` があれば、CS の窓ごとのクロックの数（`frame_bits`。`bits` で比べる）と、窓の中で SCK のレベルが中央値の半分より短くないこと（グリッチ）。CS が非アクティブのあいだの SCK の動きと、クロックの無い窓は注意（§4.9） |
| `uart(pin, baud, data, baud_rel, idle, bits, parity, stop, errors_max)` | ボーレート、バイト列、アイドルレベル、フレームとパリティのエラー（下記） |
| `voltage(pin, level_v, level_abs_v, level_min_v, level_max_v, ripple_max_v)` | アナログのチャンネルの電圧。平均が `level_v` ± `level_abs_v`、全サンプルが `level_min_v`〜`level_max_v`、ピークからピークが `ripple_max_v` 以下。与えたものだけを見る |
| `phase(a, b, phase_deg, phase_abs_deg, freq_hz, freq_rel, edge)` | 2 本の周期的な線の位相。a のエッジから、同じ向きの b の次のエッジまでを、a の周期に対する角度（0〜360 度）にする。どの組も `phase_deg` ± `phase_abs_deg`（度。0 の近くは回り込む）、両方の周波数が `freq_hz` の `freq_rel`（相対）の内 |
| `skew(a, b, delay_abs_s, delay_s, edge)` | 一緒に変わる 2 本の線（パターン、並列の出力）のずれ。a のどのエッジにも、同じ向きの b のエッジが `delay_s` ± `delay_abs_s` にある。相手の無いエッジは NG。`edge` が `"rise"` / `"fall"` ならその向きだけ（1 つのタイマの、デューティの違う 2 本の出力） |
| `dead_time(hi, lo, dead_min_s, dead_max_s, active, allow_overlap)` | ハーフブリッジの 2 本の出力（`active` で on）。片方が off になってから他方が on になるまでが、毎回 `dead_min_s`〜`dead_max_s`。両方が on（貫通）は、`allow_overlap` が無ければ NG |
| `i2c_timing(scl, sda, mode, limits)` | UM10204 の個々の時間（§4.4） |
| `rgb_led(pin, data, t0h_min_s, t0h_max_s, t1h_min_s, t1h_max_s, period_min_s, period_max_s, reset_min_s)` | WS2812 の類の線（§4.6）。リセットで区切ったフレームごとのバイト（送った順の 16 進）、T0H、T1H、ビットの周期、リセットの長さ |
| `onewire(pin, data, presence, speed)` | 1-Wire の線（§4.6）。リセットごとの後のバイト、presence の有無、リセット、presence、スロット、low の時間 |
| `ir(pin, frames, carrier_hz, carrier_rel, timing_rel, protocol)` | IR のリモコンの線（§4.6）。NEC のフレーム（アドレス、コマンド、リピート）、コマンドとその反転、搬送波の周波数、マークとスペースの長さ |
| `lin(pin, baud, frames, baud_rel, break_min_bits, checksum)` | LIN のフレーム（§4.7）。識別子とデータ、sync、識別子のパリティ、チェックサム、break と区切りの長さ、同期の欄から測ったボーレート |
| `i2s(bclk, ws, sd, format, bits, fs_hz, fs_rel, samples, slot_bits, left_ws)` | I2S の類の音声（§4.7）。形式、サンプルの速さ、1 チャンネルの BCLK の数、続いて現れるサンプルの列 |
| `quadrature(a, b, steps, cycles, direction, errors_max)` | 2 相のエンコーダ（§4.7）。数（ステップか周期）、向き、両方の線が同時に変わった回数 |
| `sd_spi(clk, mosi, miso, cs, commands, mode, cs_active, ncr_max_bytes)` | SD カードの SPI のモード（§4.8）。コマンドの列（番号、引数、R1、R7 / R3 / R2、データのブロック）、NCR のうちの応答、CMD0 と CMD8 の CRC7、カードのブロックの CRC16 |

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
- `errors_max` を与えると、フレームとパリティのエラーの合計がそれを超えたら NG です（与えなければ数えて出すだけ）。
- ボーレートは、1 文字の中の同じ向きのエッジどうしの間隔からビットの幅を最小二乗で求めます。立ち上がりと立ち下がりの遅れの差は、この測り方で打ち消されます。期待の値は、エッジから求めた候補を選ぶのにだけ使います。1 ビットに 8 サンプル以上あるときに付けます。
- **測るだけ**なら `baud` に `None` を渡します。ほかに期待（`data`、`errors_max`）がなければ、結果は `measured` で、測ったボーレート、文字数、エラー数が出ます。範囲外の設定で DUT が何を出すかを残すのに使います。
- エラーの文字がどれも、ほかの文字よりはっきりエッジがずれていれば、NG の理由に「キャプチャの時間軸を疑う」と出ます（ソフトで歩調を取るサンプラーが止まった場合など）。プローブ自身が遅れを報告したキャプチャ（`time_base_slipped`）も、NG の理由にそう添えます。

```python
uart("PA2", 8_000_000 / 9, baud_rel=0.015, errors_max=0)                  # 8N1
uart("PA2", 8_000_000 / 9, baud_rel=0.015, parity="even", errors_max=0)   # 8E1
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

`i2c_timing` は、UM10204（rev. 7）の表 10 の限度を `mode`（`"standard"`、`"fast"`、`"fast-plus"`）で選びます。`limits` で、名前ごとに限度の片側を置き換えます: `t_low_min_s`、`t_vd_dat_max_s`、`f_scl_max_hz`（`mode=None` なら `limits` だけ）。

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
i2c_timing("PC2", "PC1", "standard", limits={"t_su_dat_min_s": 300e-9})
```

### 4.5 SPI のモード

`spi` のモードは、線そのものから読みます。CPOL は CS が有効になったときの SCK の level です（CS が無ければ、いちばん長い休みの level）。CPHA は、MOSI（無ければ MISO）の変化の前の SCK のエッジで決めます。前が先のエッジ（休みの level から離れるエッジ）なら、そこで出して後のエッジで取る CPHA 1、後のエッジなら CPHA 0 です。CS が有効になってから最初のエッジまでの変化は、ほかに変化が無いときだけ使います（どちらのモードでも、CS で最初のビットを出す相手がある）。復号器が選んだモードは `decoded_mode` に残ります。CS が active high なら `cs_active=1` です。

### 4.6 1 本の線の方式

- **`rgb_led`**: 線は low で休み、1 ビットは T0H か T1H の high とその後の low、MSB から。ビットの周期の 5 倍より長い low でフレームを区切ります。0 と 1 は high の幅のいちばん大きい切れ目で分けます（全部同じ幅なら、周期の半分より短い high が 0）。限度の既定は WS2812B のデータシートの T0H 0.4 µs、T1H 0.8 µs（± 150 ns）、周期 1.25 µs（± 600 ns）、リセット 50 µs 以上。ほかの LED（SK6812、WS2811 の遅いモード、新しい WS2812B のリセット 280 µs）は、その値を渡します。最後のフレームの後で、リセットの長さを待たずに窓が終わると、LED に届いたかが決まらないので未検査です。
- **`onewire`**: 線は high で休み、どのパルスも low。`speed` の `reset_detect`（standard 288 µs、overdrive 48 µs）以上の low がリセット、その後の presence の窓の中で始まる low が presence、ほかの low がスロットです。スロットは low が `split`（standard 15 µs、overdrive 2 µs）より短ければ 1、長ければ 0、LSB から。読み出しのスロットも、運んだビットとして読みます（誰が線を下げたかは、線からは分かりません）。限度は Maxim の AN126 と DS18B20 のもの: standard ではリセット 480 µs 以上、presence の待ち 15〜60 µs、presence 60〜240 µs、スロット（low の始まりから次の始まり）61 µs 以上、スロットの間の high 1 µs 以上、0 の low 120 µs 以下、1 の low 1〜15 µs。
- **`ir`**: LED を駆動する線（搬送波付き。low で休む）も、受光モジュールの出力（high で休み、マークが low）も読みます。アクティブなパルスがほとんど 100 µs より短ければ搬送波付きとし、150 µs より近いパルスを 1 つのマークにまとめて、搬送波の周波数とデューティを測ります。NEC は 9 ms のマークの後、4.5 ms でデータ（32 ビット、LSB から: アドレス、反転したアドレスかアドレスの上位、コマンド、反転したコマンド）、2.25 ms でリピートです。アドレスとその反転が合わなければ、16 ビットの拡張アドレスとして `addr` に入れます。コマンドとその反転が合わないフレームは NG。`timing_rel`（相対、既定 0.1）で、どのマークとスペースも NEC の長さと比べます（受光モジュールはマークの幅を変えるので、その出力では広くします）。

```python
rgb_led("PA7", ["ff0000", "00ff00"])                              # 2 回の書き込み、1 個ずつ
onewire("PD2", ["cc44", "ccbe"])                                  # Skip ROM + Convert T、Skip ROM + Read Scratchpad
ir("PB1", [{"addr": 0x04, "cmd": 0x08}, {"repeat": True}], carrier_hz=38e3)
```

### 4.7 LIN、I2S、2 相のエンコーダ

- **`lin`**: 線は high で休みます。`baud` で 11 ビット以上の low を break とし、その後の high を区切りとします。次の break か、2 文字より長い休みまでの文字（8N1）が、sync、保護された識別子、データ、チェックサム（最後の文字）です。各フレームのビットの長さは、スレーブと同じく sync の欄（0x55 の 5 つの立ち下がりが 8 ビット）から測り、その長さで文字を読みます。break は `break_min_bits`（既定 13、LIN 2.x のマスタの下限）以上、区切りは 1 ビット以上です。チェックサムは `checksum="auto"` なら、診断の 0x3C / 0x3D は classic、ほかは enhanced です。
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

### 4.9 注意（notices）

プロトコルとしては許されるが、見ておきたい動きがあります。試験はそれを失敗にしたいことも、何回かは許したいこともあります。検査はそれを見つけて場所を言いますが、それだけでは何も失敗にしません。プロトコルとして許されないもの（フレーミングやパリティの誤り、CS の窓の中の SCK のグリッチ、途中で切れたバイト）は、注意ではなく `ng` です。

- 注意は 1 件が 1 つの場所です: `kind`、キャプチャの中の `tick` と `part`、種類ごとの値。いつも結果（`notices`）に入り、要約で数え、`wireskein verify -v` で 1 件ずつ出し、`--mark` でキャプチャのマーカーにも書きます。ビューアで、波形の上に 1 件ずつ見えます。
- 期待は、自分の区間の中の注意を制限します: `notices={"sck_outside_cs": 1}` は 1 回まで、`notices={"sck_outside_cs": "ng"}` は 0 回。その検査が見つけない種類の名前は期待の誤り（`ng`）で、打ち間違いが黙って通ることはありません。

| 検査 | 注意 | 何か |
| --- | --- | --- |
| `spi`（`cs` あり） | `sck_outside_cs` | CS が非アクティブのあいだに SCK が動いた（その間の最初の変化の所。`count`: 変化の数）。ほかの機器と共有するバスでは正しい動き。設定し直しの余分な変化もここに出る |
| `spi`（`cs` あり） | `empty_cs_window` | クロックの無い、CS がアクティブの窓（CS がアクティブになった所。`length_ticks`） |
| `uart` | `baud_near_limit` | ボーレートが `baud_rel` の中だが、その半分より大きくずれている（線の最初の変化の所。`value`: ずれ、`limit`） |



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

I2C の tLOW と tHIGH は `i2c_timing`（§4.4）、WS2812 のビットの幅は `rgb_led`（§4.6）で検査します。SPI の CS のタイミング（CS から最初のクロック、最後のクロックから CS）の検査はありません。

## 7. 時刻の精度

PC の時計とキャプチャの開始時刻の対応は、ms の精度です（開始の要求がプローブに届くまでの遅れがある）。区間の割り当てには十分ですが、コマンドとキャプチャの中の変化点のずれを µs で測る用途には使えません。プローブの時計に置いた見出し（§3.1）はキャプチャの時計の上で正確ですが、命令から効くまでの時間には DUT の側の遅れも入ります。そのため試験は、変化の瞬間ではなく、空白の後の steady の区間で、変化の後の状態を確かめます。
