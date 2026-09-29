# WireSkein

[English README](https://github.com/Open-Embedded-Probe/wireskein/blob/main/README.md)

WireSkein は、ロジックアナライザのキャプチャを読むツールです。用途は 2 つあります。

- **実機のテストの記録を照合する。** テストは、送ったコマンド、キャプチャ、各ステップの線の上であるべき姿（1 kHz の方形波、0x42 への I2C の書き込み、F_CPU / BRR の UART など）を記録します。`wireskein verify` は、その期待とキャプチャを照らし合わせます。結果は、測定値付きの OK / NG として、テキスト、JSON、JUnit XML で出します。
- **中身の分からないキャプチャを復号する。** `wireskein analyze` は、どのピンが I2C、SPI、UART、RVSWD / SWIO、SWD、CAN かを見つけて復号します。その上の層（NMEA、Modbus、既知の I2C / SPI デバイス）も試します。

状態: **β 版**（`0.1.0b1`）です。互換のない変更が入ることがあります（[安定性](#安定性)を参照）。

## 入れ方

```sh
pip install --pre wireskein        # または uv add --prerelease=allow wireskein
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
            t = rec.armed()                     # キャプチャを開始した直後
            data = read_capture()               # bytes。1 サンプル 1 バイト、ビット k がピン k
            rec.capture(data, rate, ["PA1", "PA0"], t, start_us=segment_start_us)
rec.close()
```

照合は次のコマンドで行います。

```sh
wireskein verify out/run1 --junit out/run1/report.xml --json out/run1/report.json
```

NG があれば、終了コードは 1 です。検査に使うピンがキャプチャにないときは、未検査（`--`）になります。未検査は、実行を失敗にしません。

- 見出し（`#` がテスト、`##` がステップ、名前のない `##` で閉じる）で、区間の木を作ります。
- キャプチャは、開始した時刻を含む区間に割り当てます。
- 期待は、区間のパス（例 `test_pwm/duty=64`）に付けます。同じ親の下で名前が重なると、`duty=64[0]`、`duty=64[1]` のように連番を付け、それぞれが自分の期待を持ちます。

検査の一覧、窓と許容誤差の決め方、実機の例は [docs/capture-test-guide.ja.md](docs/capture-test-guide.ja.md) にあります。

pytest からは、[pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein) を使います。test ごとに記録器 `ws_run` を渡し、test の後に照合します。

## キャプチャを復号する

```sh
wireskein analyze capture.sr                              # sigrok の .sr、または fixture のディレクトリ
wireskein analyze capture.sr --hint '{"protocols": ["i2c"]}'
wireskein segments capture.sr --results                   # UART にマーカーの行を流したキャプチャ
```

```python
from wireskein.analyze import load, analyze, export

cap = load("capture.sr")
doc = export(analyze(cap, {"protocols": ["spi"]}), cap)
```

## 安定性

| 部分 | β の間の約束 |
| --- | --- |
| `wireskein.runlog`（`Recorder` と検査の helper の名前、引数、意味） | 変えない。足す引数は、既定値で今の意味を保つ |
| 記録の形式（`run.json`、`FORMAT = "wireskein-run/0"`） | 変えない。互換のない変更をするときは `FORMAT` を上げ、`verify` は古い形式をはっきりしたエラーで断る |
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
2. `Release` の workflow を手で実行し、版（例 `0.1.0b1`）を入れます。
3. 残りは workflow が行います。
   - `pyproject.toml` と `src/wireskein/__init__.py` の版を書き換える
   - 変更履歴を新しい版の見出しの下へ移す
   - テストとビルドを行い、コミットして `v<版>` のタグを付ける
   - GitHub Release を作り、PyPI に出す

PyPI への公開は Trusted Publishing で行います。

## ライセンス

MIT
