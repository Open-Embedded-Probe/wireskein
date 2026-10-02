# WireSkein

![WireSkein: オープンソースのロジックアナライザのライブラリ](https://raw.githubusercontent.com/Open-Embedded-Probe/wireskein/main/docs/images/wireskein-top.jpg)

[English README](https://github.com/Open-Embedded-Probe/wireskein/blob/main/README.md)

WireSkein は、ロジックアナライザのキャプチャを**テストと分析に使うため**のツールです（MIT ライセンス）。波形を人が眺めて終わりにせず、「期待どおりか」をコードで照合し、「何が流れたか」を後工程のスクリプトや AI が読める形で取り出します。

- **実機のテストの記録を照合する。** テストは、送ったコマンド、キャプチャ、各ステップの線の上であるべき姿（1 kHz の方形波、0x42 への I2C の書き込み、F_CPU / BRR の UART、3.3 V ± 0.1 V の電源など）を記録します。`wireskein verify` は、その期待とキャプチャを照らし合わせます。結果は、測定値付きの OK / NG として、テキスト、JSON、JUnit XML で出します。pytest からは [pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein) で使えます。
- **中身の分からないキャプチャを復号する。** `wireskein analyze` は、どのピンが I2C、SPI、UART、RVSWD / SWIO、SWD、CAN かを見つけて復号します。その上の層（NMEA、Modbus、既知の I2C / SPI デバイス）も試します。
- **取って、保存して、見る。** `wireskein capture` でプローブから取り、`.wireskein` に保存し、`wireskein gui` でブラウザに表示します。

状態: **β 版**です。互換のない変更が入ることがあります（[安定性](#安定性)を参照）。

## なぜ WireSkein か

組み込みの開発で、ロジックアナライザは欠かせません。けれども、今までの道具は「人が画面で波形を見る」ことが中心で、次のことが難しいままでした。

| 困っていたこと | WireSkein での解決 |
| --- | --- |
| **テストにならない。** 波形を見て「たぶん合っている」で終わる。キャプチャを期待と照らし合わせて OK / NG を出し、CI に載せる口がない | テストの中で期待をコードとして書き（`square`、`uart`、`i2c`、`voltage` など）、`wireskein verify` が測定値付きで照合する。JUnit XML と pytest のプラグインで、そのまま CI に載る |
| **機材に縛られる。** 使いやすいアプリほど専用の機材が前提で、手元の安いロジアナや自作のプローブと組み合わせにくい。ライセンスが組み込みや改変に合わないこともある | 取得元を選ばない。sigrok の `.sr` や VCD をそのまま読み、sigrok 対応の機器や、ESP32 などで作る OEP のプローブから直接取れる。MIT ライセンス |
| **間引いた・遅いチャンネルが偽のサンプルで埋まる。** 既存の形式は全チャンネルが 1 つのレートなので、帯域のために一部を間引くと、取っていないサンプルが水増しされる | `.wireskein` は各チャンネルを自分のレートのまま持ち、取ったサンプルだけを保存する。ビューアも、実際のサンプルとエッジの不確かさの幅を示す |
| **ロジックとアナログが別々。** 別の記録を別の道具で見て、時刻を手で合わせる | ロジックとアナログを 1 つのファイルに持ち、`wireskein align` が同じ信号のエッジから時刻のずれと倍率を求める。アナログにもロジックの検査（`threshold=`）と電圧の検査をかけられる |
| **情報が多すぎる。** 全波形や全イベントを人や AI が毎回読むのは重い。一方で、要約の途中で大事な信号を落とすこともある（RVSWD の調査で、途中で速くなった通信をノイズと見なした例がある） | 必要な層だけを構造化して出す（コマンド列だけ、区間だけ、詳細まで）。未解釈の区間、候補ごとの根拠、取得の品質は隠さずに残す |
| **出力を使うたびに加工が要る。** 表示用の文字列を読み直したり、ツールごとに変換スクリプトを書いたりする | 出力は安定した構造の JSON。時刻は刻みの整数で、元のキャプチャへ戻れる |

### AI フレンドリー

- **CLI と JSON が基本です。** 画面がなくても、すべての機能を使えます。AI のエージェントがコマンドを実行し、結果を読んで次の手を決められます。
- **必要な分だけ出せます。** `analyze --select`（層の選択）、`--window`（時間の範囲）、`--segment`（マーカーの区間）で、問いに要る部分だけを渡し、トークンを節約できます。
- **判断の材料を返します。** 復号の候補、一致の度合い、合わない位置、未解釈の区間を返すので、AI は「読めたこと」と「読めていないこと」を区別できます。
- **検査は測定値付きです。** NG の理由と実際の値（周波数、デューティ、ボーレートのずれ、電圧など）が出るので、原因の見当を付けやすくなります。
- **ファイルに所見を残せます。** メモ、添付、マーカー、復号の注釈を、キャプチャと同じファイルに入れられます。調べた経緯を、次の人や次の AI に渡せます。

### テストフレンドリー

- **期待はコードです。** `wireskein.runlog` は標準ライブラリだけで動くので、試験のスクリプトにそのまま入れられます。
- **試験の構造をそのまま記録します。** 見出し（`#` がテスト、`##` がステップ）で区間の木を作り、キャプチャを区間に割り当てます。どのステップのどの期待が NG かが分かります。
- **ピンと役割を渡して照合します。** 自動判定に頼らないので、結果が安定します。窓の取り方や許容誤差の決め方は、[docs/capture-test-guide.ja.md](docs/capture-test-guide.ja.md) にあります。
- **CI にそのまま載ります。** JUnit XML で出し、pytest からは [pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein) で、試験ごとに記録して照合します。NG のキャプチャはファイルとして残るので、あとからビューアで調べられます。

### 取得元: 既存の形式も、OEP のプローブも

- **既存の形式をそのまま使えます。** sigrok / PulseView の `.sr`、VCD（GTKWave、Saleae、DSView、シミュレーターなど）を、名前ではなく中身で見分けて読みます。書き出しもできます。sigrok 対応の機器からは、`wireskein capture --source sigrok:<ドライバ>` で直接取れます。
- **[Open Embedded Probe（OEP）](https://github.com/Open-Embedded-Probe/oep-spec)** は、開発中のチップにつなぐ小さなボード（プローブ）と、PC のソフトの間のオープンなプロトコルです。1 つのプローブが、デバッガ（WCH の CH32 の RVSWD / SWIO、ARM の SWD）、ターゲットのコンソール、試験の治具（GPIO、UART、SPI / I2C のデバイス、ロジックとアナログの取得）を兼ねます。ファームウェアは [oep-probe-arduino](https://github.com/Open-Embedded-Probe/oep-probe-arduino)、PC 側は [oep-client-python](https://github.com/Open-Embedded-Probe/oep-client-python) です。
  - **ESP32-P4** をプローブにすると、ロジックを **16 ch で 20 Msps から、2 ch で 160 Msps まで**取れます（8 ch なら 40 Msps）。どのピンを取るかは試験ごとに選べ、配線もファームウェアも変えずに済みます。
  - classic ESP32 などでも、速さは落ちますが取れます。プローブが自分の能力を宣言するので、WireSkein はその範囲で使います。ADC のチャンネルがあれば、アナログも一緒に取れます。

## 入れ方

```sh
pip install wireskein              # または uv add wireskein
```

Python 3.13 以上が要ります。依存は numpy だけです。

## テストの記録を照合する

テストは、`wireskein.runlog` で記録します。このモジュールは、標準ライブラリだけで動きます。

```python
from wireskein.runlog import Recorder, square, level, only_moving

rec = Recorder("out/run1", target="x035")
with rec.section(1, "test_pwm"):
    for duty in (64, 128, 0):
        want = [square("PA1", 1000, duty / 255), only_moving(["PA1"])] if duty else [level("PA1", 0)]
        with rec.section(2, f"duty={duty}", expect=want):
            rec.command(f"PWM {duty}")          # PC が送ったもの
            rec.reply(reply_line)               # デバイスの応答
            t = rec.armed()                     # キャプチャを開始した直後の time.monotonic()
            data = read_capture()               # プローブのサンプル。1 サンプル 1 バイト、ビット k がピン k
            rec.capture(t, rate, interleaved=data, names=["PA1", "PA0"], start_ns=seg.start_ns, start_uncertainty_ns=seg.start_uncertainty_ns)
rec.close()
```

照合は次のコマンドで行います。

```sh
wireskein verify out/run1 --junit out/run1/report.xml --json out/run1/report.json
```

キャプチャは `.wireskein`（後述）で保存されます。NG があれば、終了コードは 1 です。検査に使うピンがキャプチャにないなど、検査できなかったものは未検査（`--`）で、**既定で失敗**にします（多くは試験の書き間違いか配線の抜けだからです。`--allow-unchecked` で失敗にしません）。測るだけと頼んだもの（`uart(baud=None)` など）は measured（`ME`）で、失敗にしません。記録の形式と結果の意味は [docs/run-format.ja.md](docs/run-format.ja.md) にあります。

- 見出し（`#` がテスト、`##` がステップ、名前のない `##` で閉じる）で、区間の木を作ります。
- キャプチャは、開始した時刻を含む区間に割り当てます。
- 期待は、区間のパス（例 `test_pwm/duty=64`）に付けます。同じ親の下で名前が重なると、最初のものは名前のまま、2 回目から `duty=64[1]`、`duty=64[2]` のように番号を付け、それぞれが自分の期待を持ちます。あとから同じ名前を足しても、前のキーは変わりません。

検査の一覧、窓と許容誤差の決め方、実機の例は [docs/capture-test-guide.ja.md](docs/capture-test-guide.ja.md) にあります。

pytest からは、[pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein) を使います。test ごとに記録器 `ws_run` を渡し、test の後に照合します。

## キャプチャを取る

`wireskein capture` で、機器からロジックのチャンネルを取り、`.wireskein` に保存します。

```sh
pip install "wireskein[oep]"      # OEP のプローブを使うとき（oep-client-python）。sigrok は sigrok-cli が PATH に要る
wireskein capture --source oep:/dev/ttyACM0 --channels SDA=47,SCL=48 --rate 20M --samples 200k -o i2c.wireskein
wireskein capture --source sigrok:fx2lafw --channels SDA=D0,SCL=D1 --rate 12M --samples 1M \
                  --trigger SCL:fall --pretrigger 1k --note "リフローの後" -o i2c.wireskein
```

| 取得元 | 機器 | チャンネルの番号 |
| --- | --- | --- |
| `oep:<接続先>` | oep-client-python を使う OEP のプローブ。接続先は、シリアルポート、`tcp://HOST:PORT`（ブローカー）、`usb[:VID:PID[:SERIAL]]` | プローブのチャンネル番号 |
| `sigrok:<ドライバ>` | sigrok が対応する機器（`fx2lafw`、`dreamsourcelab-dslogic`、`demo` など）。`sigrok-cli` を使う | sigrok のチャンネル名（`D0` など） |

- `--channels` は、ロジックの各チャンネルに名前を付けます（`名前=番号`）。
- `--analog 名前=番号[@入力範囲],...` で、アナログのチャンネルを足します（`--analog-rate`、`--analog-samples`。サンプル数の既定は、ロジックと同じ時間）。
  - OEP のプローブでは、ロジックとアナログを組（capture-group）で一緒に始めます。
  - アナログの各チャンネルは、自分のレートと開始時刻（プローブの推定値と不確かさ）、生の値、入力範囲、基準電圧、プローブの出荷時の較正を持ちます。
  - sigrok では、アナログはデバイスの 1 つのレートで取ります。
- ファイルには、機器が実際に使ったレートを入れます。取得元が知っている情報（プローブの時計での最初のサンプルの時刻 `start_ns` とその不確かさ `start_uncertainty_ns`、`time_base_slipped` の印（プローブが、サンプルを取るのが遅れたと知っている。バッファの上限などで起きる。その辺りの時刻が伸びている可能性がある。`wireskein info` がそう説明し、NG の検査は理由にそれを添える）、トリガの位置 `trigger_index`）も入れます。
- UART のプローブ（classic ESP32 の治具など）では、キャプチャの読み出しのためにリンクを速い速度に上げます。1.5 Mbaud、921600、500000 の順に試し、両方向を同時に流しても壊れない最初のものを使います（oep-client-python 0.0.27 以降）。
  - 壊れる速度はクライアントが断り、使っている間にフレームが続けて壊れたら起動時の速度に戻るので、起動時の速度より遅くはなりません。
  - 試すのは 1 回の取得で 2 つまでです（1 つに最大 1 秒ほどかかるため）。確かめるのは読み出しの向き（プローブ → ホスト）だけです。口とプローブ（`unit_id`）ごとに通った速度と通らなかった速度を、oep-client-python の記録（`~/.cache/oep-client/link-speed.json`。通らなかったものは 30 日飛ばす）で使います。速い速度が通らない口は、毎回試す代わりに、回を追って遅い方へ移ります。`?fast=921600` のようにはっきり指定したときは、覚えたものに関わらず試します。
  - `?fast=0` で起動時の速度のまま、`?fast=921600`（並べることもできる）でその速度だけを試します。
- USB のあるプローブ（ESP32-P4 など）は、`oep:usb:VID:PID[:SERIAL]` で開くのがよいです。vendor bulk のインターフェースで、毎秒数 MB を落とさずに読み出せます。USB シリアル（CDC）の口は遅く、負荷が高いと応答を落とすことがあります。
  - `capture` は、リンクがどうなったか（速度、試した結果、読み出しの時間）を表示し、`meta.probe.link` に残します。
- Python からは `wireskein.sources.capture(取得元, Request(...), 出力先)` です。
- 別の package から、entry point の `wireskein.sources` で取得元を足せます。

## ブラウザでキャプチャを見る

```sh
wireskein gui capture.wireskein    # そのキャプチャをブラウザで開く（.sr と .vcd はその場で変換）
wireskein gui runs/                # runs/ の下のキャプチャと記録の一覧のページ
```

ビューアは [wireskein-web](https://github.com/Open-Embedded-Probe/wireskein-web) で、wheel に同梱しています（ブラウザでファイルを開くだけなら [GitHub Pages](https://open-embedded-probe.github.io/wireskein-web/) でも使えます）。次のものを表示します。

- 時間軸つきのロジックとアナログの行。マウスを重ねると、パルスの幅、周期、周波数、デューティを示します（アナログは値）。Shift + ホイールで時間方向に移動します。
- プローブが実際に取ったサンプル（間引いたチャンネルは自分のサンプルだけ）、トリガの位置。時刻の合わせ込みがファイルにあれば、アナログを合わせた時刻で描きます。
- 復号の注釈（I2C の取引、UART の文字など）を、データの線の下に示します。ファイルに入っていればそれを（`wireskein annotate --save`）、なければその場で復号し、ファイルに入れるボタンも出します。
- マーカー: M でマウスの位置に付け、ファイルに保存できます。
- 記録した run の中のキャプチャなら、その照合の結果（OK / NG と理由）。
- メタ情報、取得の設定、添付、メモ。メモを追記する欄もあります。

サーバーは 127.0.0.1 にだけ開き、起動ごとのトークン付きの URL を表示します。

- トークンのない要求や、別のホスト名への要求は断ります。
- ベンチのマシンで動かすときは、ポートを転送し（`ssh -L ポート:127.0.0.1:ポート ベンチ`）、表示された URL を開きます。
- 返すのは、ビューアと、指定したディレクトリの下のキャプチャのファイルだけです。

## WireSkein のファイル（.wireskein）と変換

WireSkein のファイル（`.wireskein`、中身は zip）は、キャプチャと、それに付くもの（今は添付とメモ、将来はマーカーや復号・照合の結果）を 1 つに持ちます。道具は、名前ではなく中身で見分けます。キャプチャは、各チャンネルを自分のサンプルレートのまま持ちます。帯域に収めるために一部のチャンネルを間引くプローブ（例: 32 サンプルに 1 つ）は、取ったサンプルだけを `step=32` で保存します。間を埋める水増しはしないので、ビューアは実際にあるサンプルだけを見せられます。読み書きは、標準ライブラリだけで動く `wireskein.fileformat` で行います。

```python
from wireskein import fileformat as wf

wf.write("c.wireskein", 100_000_000, [
    wf.Channel("PA5", wf.pack(pa5_samples), n),                 # samples: 1 サンプル 1 バイト（0 か 1）
    wf.Channel("PB0", wf.pack(pb0_samples), n // 32, step=32),  # 1/32 のレートで取ったチャンネル
], start_ns=seg.start_ns, start_uncertainty_ns=seg.start_uncertainty_ns)
channels = wf.from_interleaved(data, ["PA5", "PA7"], width=8)    # プローブの並び: 1 サンプル width ビット、ビット k がチャンネル k
```

アナログのチャンネルも、同じファイルに入れられます。

- 各チャンネルが、自分のレートと最初のサンプルの時刻（`t0_ticks`）を持ちます。ADC の実際のレートがロジックの刻みの整数倍でなくても、本当の時刻のまま持てます。
- ADC の値は、生のまま、1 次式の換算（`zero`、`scale_nv`）と一緒に保存します。
- 取ったときの情報（ピン、減衰、基準電圧、プローブの出荷時の補正値）も、分析で使うかにかかわらず保存します。

```python
wf.write("m.wireskein", 20_000_000, [
    wf.Channel("CLK", wf.pack(clk_samples), n),
    wf.analog_raw("VBUS", raw_values, Fraction(80_000_000, 1667), width=16, value_bits=12, zero=0, scale_nv=805_860,
                   pin=22, attenuation_db=12, reference={"source": "vdd", "mv": 3300}),
    wf.analog_volts("SINE", volts, 1_000_000),                     # 電圧（sigrok や Saleae から）
], probe={"chip": "ESP32-P4", "calibration": {"scheme": "curve-fitting-v1", "raw": "..."}})
```

`wireskein capture --source sigrok:<ドライバ>` で、アナログのチャンネルも取れます（`--channels CLK=D0,VBUS=A0`）。解析は、ロジックのチャンネルを読みます。照合は、アナログのチャンネルも読みます（`voltage()` と、ロジックの検査の `threshold=`）。形式の仕様は `docs/wireskein-format.ja.md` にあります。

プローブが返すアナログの開始時刻は、推定値です（ADC は数百 µs ずれて始まったり、少し速く・遅く動いたりします）。同じ信号をロジックとアナログの両方で取っておけば（同じネットを両方につなぐ、または目印のパルス）、`wireskein align` がそのエッジからオフセットと時間の倍率を求め、`attach/alignment.json` に入れます。サンプルと記録した時刻は変えません。

```sh
wireskein align m.wireskein --reference SYNC --via SYNC_A --threshold 1.0,2.3 --save
# SYNC_A against SYNC: start +197.890 us, scale +1490.7 ppm, 166/166 edges matched, residual 5.665 us
```

Python からは `wireskein.align.find()` / `apply()` / `save()` / `load()` です。周期的な信号は、開始のずれがその周期より大きくなりうると、答えが 1 つに決まりません。`--max-offset` を与えるか、不規則な目印のパルスで合わせます。

2 つのプローブで同時に取ったキャプチャも、同じ方法で合わせられます。両方が見た信号（それぞれのピンにつなぐ）を使います。

```sh
wireskein align B.wireskein --to A.wireskein --reference SYNC --via SYNC --save
# B.wireskein onto A.wireskein: tick 0 at +3699.999 us, clock -79.98 +- 0.02 ppm, 264/264 edges matched, residual 0.031 us
```

結果は、B の `alignment.json` に（A を見分ける情報と一緒に）入ります。どちらのファイルのサンプルも変えません。ビューアの「Add another probe's file」で、B のチャンネルを A の時間軸に並べて表示できます（Python からは `align.between()`）。

テストの記録では、同じものを `rec.capture(t, tick_hz, channels=[...])` に渡します。

WireSkein のファイルには、キャプチャについてのほかの情報も入れられます（取得の設定、配線のメモ、分析の結果など）。

- **添付**: 名前付きのファイルです（テキスト、JSON、bytes）。後から差し替えられます。
- **メモ**: 追記専用の記録です。1 回の呼び出しで 1 件、時刻付きで足します。

どちらも、既存のファイルに、チャンネルのデータを書き直さずに足せます。

```python
wf.attach("c.wireskein", "probe.json", {"fw": "1.2", "plan": plan})   # dict / list は JSON、str はテキスト、bytes はそのまま
wf.note("c.wireskein", "PA5 がうるさい。次は線を短く")
wf.note("c.wireskein", {"i2c": transactions}, kind="analysis")
wf.attachments("c.wireskein"), wf.notes("c.wireskein")
```

```sh
wireskein info c.wireskein                                  # チャンネルとレート、メタ情報、添付、メモ
wireskein note c.wireskein "リフローの後に取り直し"
wireskein attach c.wireskein setup.txt --text "SDA/SCL に 10k のプルアップ"
wireskein attach c.wireskein scope.png scope.png            # どんなファイルでも
```

テストの記録では、`rec.capture(..., attachments={...})` でキャプチャと一緒に添付を保存します。添付とメモは、`.sr` への変換と、`.sr` からの戻しでも持ち運ばれます。

形式の変換は、コマンドで行います（形式は拡張子で決まります）。

```sh
wireskein convert c0001.wireskein c0001.sr     # PulseView 用: 1 つのレート、遅いチャンネルは水増し
wireskein convert c0001.sr c0001.wireskein     # 戻す: チャンネルは本当のレートに戻る
wireskein convert c0001.wireskein c0001.vcd    # GTKWave など: 変化点だけ。各チャンネルは自分のレートのまま
wireskein convert saleae.vcd s.wireskein       # ほかの道具の VCD: 刻みは変化の時刻の最大公約数
wireskein convert corpus/fixtures/real/<id> capture.sr
```

`.sr` は全チャンネルで 1 つのレートなので、遅いチャンネルは最も速いレートに水増しします。各チャンネルの本当のレートは、`.sr` の中の `wireskein.json` に残します。sigrok はこのファイルを無視し、WireSkein は読み戻します。PulseView では、水増しした値も取った値と同じに見えます。

## キャプチャを復号する

```sh
wireskein analyze capture.wireskein          # WireSkein のファイル、sigrok の .sr、VCD、fixture のディレクトリ
wireskein analyze capture.sr --hint '{"protocols": ["i2c"]}'
wireskein analyze m.wireskein --threshold TX=1.0,2.3        # アナログのチャンネルも、ロジックとして読んで復号する
wireskein segments capture.sr --results                   # UART にマーカーの行を流したキャプチャ
```

```python
from wireskein.analyze import load, save, analyze, export

cap = load("capture.wireskein")                                     # .sr や fixture のディレクトリも
doc = export(analyze(cap, {"protocols": ["spi"]}), cap)
```

## 安定性

| 部分 | β の間の約束 |
| --- | --- |
| `wireskein.runlog`（`Recorder` と検査の helper の名前、引数、意味） | 変えない。足す引数は、既定値で今の意味を保つ |
| 記録の形式（`run.json` と `.wireskein` のキャプチャ、`FORMAT = "wireskein-run/2"`）とキャプチャの形式（`.wireskein`、`wireskein/0`） | 変えない。互換のない変更をするときは `FORMAT` を上げ、`verify` は古い形式をはっきりしたエラーで断る |
| `wireskein.verify.verify` / `junit`、`wireskein verify` | 変えない。報告の項目は増えることがある |
| `wireskein.analyze`、`wireskein analyze` / `segments` の出力 | 変わることがある |
| `wireskein._engine` | 内部 |

## リポジトリの構成

```text
src/wireskein/          package（runlog、verify、analyze、cli、_engine、decl/ のデータ）
tests/                  pytest
research/               評価のスクリプト、ベンチ、実験の記録（package に入れない）
corpus/                 research/ が使う実記録と合成の fixture
docs/                   設計のメモ
```

## 開発

```sh
uv run pytest
uv build
```

## リリース

pytest-embedded-arduino-cli と同じく、GitHub Actions で出します。

1. `CHANGELOG.md` の `## Unreleased` を更新します。
2. `Release` の workflow を手で実行し、版（例 `0.0.2`）を入れます。
3. 残りは workflow が行います。
   - `pyproject.toml` と `src/wireskein/__init__.py` の版を書き換える
   - 変更履歴を新しい版の見出しの下へ移す
   - テストとビルドを行い、コミットして `v<版>` のタグを付ける
   - GitHub Release を作り、PyPI に出す

PyPI への公開は Trusted Publishing で行います。

## ライセンス

MIT
