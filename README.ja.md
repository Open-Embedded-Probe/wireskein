# WireSkein

![WireSkein: オープンソースのロジックアナライザのライブラリ](https://raw.githubusercontent.com/Open-Embedded-Probe/wireskein/main/docs/images/wireskein-top.jpg)

[English README](https://github.com/Open-Embedded-Probe/wireskein/blob/main/README.md)

WireSkein は、ロジックアナライザのキャプチャを読むツールです。用途は 2 つあります。

- **実機のテストの記録を照合する。** テストは、送ったコマンド、キャプチャ、各ステップの線の上であるべき姿（1 kHz の方形波、0x42 への I2C の書き込み、F_CPU / BRR の UART など）を記録します。`wireskein verify` は、その期待とキャプチャを照らし合わせます。結果は、測定値付きの OK / NG として、テキスト、JSON、JUnit XML で出します。
- **中身の分からないキャプチャを復号する。** `wireskein analyze` は、どのピンが I2C、SPI、UART、RVSWD / SWIO、SWD、CAN かを見つけて復号します。その上の層（NMEA、Modbus、既知の I2C / SPI デバイス）も試します。

状態: **β 版**です。互換のない変更が入ることがあります（[安定性](#安定性)を参照）。

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
            rec.capture(t, rate, interleaved=data, names=["PA1", "PA0"], start_us=segment_start_us)
rec.close()
```

照合は次のコマンドで行います。

```sh
wireskein verify out/run1 --junit out/run1/report.xml --json out/run1/report.json
```

キャプチャは `.wsc`（後述）で保存されます。NG があれば、終了コードは 1 です。検査に使うピンがキャプチャにないときは、未検査（`--`）になります。未検査は、実行を失敗にしません。

- 見出し（`#` がテスト、`##` がステップ、名前のない `##` で閉じる）で、区間の木を作ります。
- キャプチャは、開始した時刻を含む区間に割り当てます。
- 期待は、区間のパス（例 `test_pwm/duty=64`）に付けます。同じ親の下で名前が重なると、`duty=64[0]`、`duty=64[1]` のように連番を付け、それぞれが自分の期待を持ちます。

検査の一覧、窓と許容誤差の決め方、実機の例は [docs/capture-test-guide.ja.md](docs/capture-test-guide.ja.md) にあります。

pytest からは、[pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein) を使います。test ごとに記録器 `ws_run` を渡し、test の後に照合します。

## キャプチャを取る

`wireskein capture` で、機器からロジックのチャンネルを取り、`.wsc` に保存します。

```sh
pip install "wireskein[oep]"      # OEP のプローブを使うとき（oep-client-python）。sigrok は sigrok-cli が PATH に要る
wireskein capture --source oep:/dev/ttyACM0 --channels SDA=47,SCL=48 --rate 20M --samples 200k -o i2c.wsc
wireskein capture --source sigrok:fx2lafw --channels SDA=D0,SCL=D1 --rate 12M --samples 1M \
                  --trigger SCL:fall --pretrigger 1k --note "リフローの後" -o i2c.wsc
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
- ファイルには、機器が実際に使ったレートを入れます。取得元が知っている情報（プローブの `start_us`、`time_base_slipped` の印、トリガーの位置）も入れます。
- Python からは `wireskein.sources.capture(取得元, Request(...), 出力先)` です。
- 別の package から、entry point の `wireskein.sources` で取得元を足せます。

## キャプチャのファイル（.wsc）と変換

`.wsc` は、各チャンネルを自分のサンプルレートのまま持ちます。帯域に収めるために一部のチャンネルを間引くプローブ（例: 32 サンプルに 1 つ）は、取ったサンプルだけを `step=32` で保存します。間を埋める水増しはしないので、ビューアは実際にあるサンプルだけを見せられます。読み書きは、標準ライブラリだけで動く `wireskein.wsc` で行います。

```python
from wireskein import wsc

wsc.write("c.wsc", 100_000_000, [
    wsc.Channel("PA5", wsc.pack(pa5_samples), n),                 # samples: 1 サンプル 1 バイト（0 か 1）
    wsc.Channel("PB0", wsc.pack(pb0_samples), n // 32, step=32),  # 1/32 のレートで取ったチャンネル
], start_us=segment_start_us)
channels = wsc.from_interleaved(data, ["PA5", "PA7"], width=8)    # プローブの並び: 1 サンプル width ビット、ビット k がチャンネル k
```

アナログのチャンネルも、同じファイルに入れられます。

- 各チャンネルが、自分のレートと最初のサンプルの時刻（`t0_ticks`）を持ちます。ADC の実際のレートがロジックの刻みの整数倍でなくても、本当の時刻のまま持てます。
- ADC の値は、生のまま、1 次式の換算（`zero`、`scale_nv`）と一緒に保存します。
- 取ったときの情報（ピン、減衰、基準電圧、プローブの出荷時の補正値）も、分析で使うかにかかわらず保存します。

```python
wsc.write("m.wsc", 20_000_000, [
    wsc.Channel("CLK", wsc.pack(clk_samples), n),
    wsc.analog_raw("VBUS", raw_values, Fraction(80_000_000, 1667), width=16, value_bits=12, zero=0, scale_nv=805_860,
                   pin=22, attenuation_db=12, reference={"source": "vdd", "mv": 3300}),
    wsc.analog_volts("SINE", volts, 1_000_000),                     # 電圧（sigrok や Saleae から）
], probe={"chip": "ESP32-P4", "calibration": {"scheme": "curve-fitting-v1", "raw": "..."}})
```

`wireskein capture --source sigrok:<ドライバ>` で、アナログのチャンネルも取れます（`--channels CLK=D0,VBUS=A0`）。解析は、今までどおりロジックのチャンネルを読みます。アナログの検査は、これから作ります。形式の仕様は `docs/wsc-format.ja.md` にあります。

テストの記録では、同じものを `rec.capture(t, tick_hz, channels=[...])` に渡します。

`.wsc` には、キャプチャについてのほかの情報も入れられます（取得の設定、配線のメモ、分析の結果など）。

- **添付**: 名前付きのファイルです（テキスト、JSON、bytes）。後から差し替えられます。
- **メモ**: 追記専用の記録です。1 回の呼び出しで 1 件、時刻付きで足します。

どちらも、既存のファイルに、チャンネルのデータを書き直さずに足せます。

```python
wsc.attach("c.wsc", "probe.json", {"fw": "1.2", "plan": plan})   # dict / list は JSON、str はテキスト、bytes はそのまま
wsc.note("c.wsc", "PA5 がうるさい。次は線を短く")
wsc.note("c.wsc", {"i2c": transactions}, kind="analysis")
wsc.attachments("c.wsc"), wsc.notes("c.wsc")
```

```sh
wireskein info c.wsc                                  # チャンネルとレート、メタ情報、添付、メモ
wireskein note c.wsc "リフローの後に取り直し"
wireskein attach c.wsc setup.txt --text "SDA/SCL に 10k のプルアップ"
wireskein attach c.wsc scope.png scope.png            # どんなファイルでも
```

テストの記録では、`rec.capture(..., attachments={...})` でキャプチャと一緒に添付を保存します。添付とメモは、`.sr` への変換と、`.sr` からの戻しでも持ち運ばれます。

形式の変換は、コマンドで行います（形式は拡張子で決まります）。

```sh
wireskein convert c0001.wsc c0001.sr     # PulseView 用: 1 つのレート、遅いチャンネルは水増し
wireskein convert c0001.sr c0001.wsc     # 戻す: チャンネルは本当のレートに戻る
wireskein convert corpus/fixtures/real/<id> capture.sr
```

`.sr` は全チャンネルで 1 つのレートなので、遅いチャンネルは最も速いレートに水増しします。各チャンネルの本当のレートは、`.sr` の中の `wireskein.json` に残します。sigrok はこのファイルを無視し、WireSkein は読み戻します。PulseView では、水増しした値も取った値と同じに見えます。

## キャプチャを復号する

```sh
wireskein analyze capture.wsc                             # .wsc、sigrok の .sr、または fixture のディレクトリ
wireskein analyze capture.sr --hint '{"protocols": ["i2c"]}'
wireskein segments capture.sr --results                   # UART にマーカーの行を流したキャプチャ
```

```python
from wireskein.analyze import load, save, analyze, export

cap = load("capture.wsc")                                     # .sr や fixture のディレクトリも
doc = export(analyze(cap, {"protocols": ["spi"]}), cap)
```

## 安定性

| 部分 | β の間の約束 |
| --- | --- |
| `wireskein.runlog`（`Recorder` と検査の helper の名前、引数、意味） | 変えない。足す引数は、既定値で今の意味を保つ |
| 記録の形式（`run.json` と `.wsc` のキャプチャ、`FORMAT = "wireskein-run/1"`）とキャプチャの形式（`.wsc`、`wireskein-capture/0`） | 変えない。互換のない変更をするときは `FORMAT` を上げ、`verify` は古い形式をはっきりしたエラーで断る |
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
