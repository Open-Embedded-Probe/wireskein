# 引き継ぎ: ArduinoCore-CH32 のテストに `ws verify` を組み込む

> **2026-09-29 追記:** この文書は、package 化より前のものです。文中の `prototype/`、`wsproto`、`ws.py`、`PYTHONPATH=.` は、今は次のとおりです。`pip install wireskein` で入れ、`from wireskein.runlog import ...` で記録し、`wireskein verify` で照合します。合成の例は `tests/demo_run.py` にあります。引き継ぎの手順は、[pytest-embedded-wireskein](https://github.com/Open-Embedded-Probe/pytest-embedded-wireskein) に置き換わります。

作成 2026-09-28（wireskein のコミット `9c2a534` の時点）。**ArduinoCore-CH32 は、その後 ArduinoCore-CH32RV に改名されました（https://github.com/ch32-riscv-ug/ArduinoCore-CH32RV、FQBN は `ch32-riscv-ug:ch32rv:<board>`）。** この文書の名前は、当時のまま残します。実機を接続したマシンの新しいセッションで、ArduinoCore-CH32 の修正を依頼するためのメモです。読み手は、この会話の履歴を持たない作業者（Claude Code のセッションを含む）です。

## 1. 目的

CH32 向け Arduino コアの動作確認テストで、ESP32-P4 のプローブが取ったキャプチャから波形を照合し、ステップごとに OK/NG を出せるようにします。値の受け渡し（ペアテスト）では見えない不具合を見つけるのが狙いです。

- 戻し忘れ（I2C の SDA が Low のまま、CS が High に戻らない）
- 周波数やデューティー比のずれ
- 別のピンが動く（リマップの誤り）

照合する側（WireSkein のプロトタイプ）は実装済みで、合成データでだけ確かめてあります。**実機の記録で確かめるのは、今回が初めてです。**

## 2. 読むもの

| 何 | 場所 |
| --- | --- |
| テストの作り方のガイド（マーカー、キャプチャの窓、検査の一覧、組み込み例） | `wireskein/docs/capture-test-guide.ja.md` |
| マーカーの仕様（見出し型、区間の規則） | `wireskein/docs/workbench-model.ja.md`「マーカーで長い記録を実験の文脈に結び付ける」 |
| 記録器（テスト側が使う。標準ライブラリだけ） | `wireskein/prototype/wsproto/runlog.py` |
| 照合の本体 | `wireskein/prototype/wsproto/verify.py`、CLI は `wireskein/prototype/ws.py verify` |
| 合成した記録の例（x035 の条件、不具合入り） | `wireskein/prototype/verify_demo.py` |
| 実験の記録（照合の結果を含む） | `wireskein/prototype/findings.ja.md` の最後の節 |

## 3. 準備

```sh
git clone https://github.com/Open-Embedded-Probe/wireskein ~/dev_oep/wireskein   # 置き場所は任意。以下はこの前提
cd ~/dev_oep/wireskein/prototype
uv sync                                   # Python 3.12 以上、numpy
PYTHONPATH=. uv run python verify_demo.py /tmp/ws-demo
PYTHONPATH=. uv run python ws.py verify /tmp/ws-demo      # 13 ok, 4 ng（仕込んだ 4 件）、終了コード 1 になれば準備完了
```

ArduinoCore-CH32 のテストは、兄弟ディレクトリの `~/dev_oep/oep-client-python` を使います（`oep_smoke.DEFAULT_CLIENT`、`--oep-client` で変更可）。

## 4. ArduinoCore-CH32 の今のテスト（`cc08ae5` の時点で調べた内容。作業前に今の HEAD と照らすこと）

- キャプチャを使うテストは `tests/manual/oep_*/*.py` の単体のスクリプトです（PEP 723 の見出しと `main()` を持ち、`uv run` で動かす）。pytest は `manual/` を集めません（`tests/pyproject.toml` の `norecursedirs`、`tests/conftest.py` の 11〜19 行）。
- 共通部品は `tests/manual/oep_smoke/trace_kit.py`（`Session`、`Capture`）です。
  - `Capture.assignments(*channels)` で並べた順が、キャプチャのビットの順になります。
  - `configure(rate, samples)` → `arm()` → `wait()` → `read_all()` の流れです。
  - 返り値は `bytes`（1 サンプル 1 バイト、ビット k = k 番目のチャンネル）です。
  - クライアントの `Segment` の `start_us` は捨てています。
- **コマンドは OEP の `target.console`（dmseq）で送ります。** `PING`/`PONG`、`PWM 64` → `PWM duty=64` のような 1 行ずつのやり取りで、ロジックのキャプチャには映りません。行の処理は `tests/sketches/testcmd.h` の `tc_ready()` で、`#` で始まる行には `unknown cmd` や `ERR` を返します（コメントの扱いはない）。
- ピンの対応は `tests/manual/oep_smoke/targets.py` の `TARGETS` です。
  - x035: P4、PARLIO で 20 MHz まで。約 650 kHz より遅くできない。
  - v003: クラシック ESP32。0.4〜2 MHz、窓は最長 164 ms 程度。
  - l103: キャプチャなし。
  - `periph_trace`、`i2c_trace`、`uart_trace`、`reset_trace` は、スクリプトの先頭にも x035 の定数を直に書いています。
- 判定は、ほとんどが測定値をログに出すだけです（PWM と tone は許容誤差なし）。
  - `--result-json` に出すのは復号結果だけです。
  - サンプルを保存するのは `oep_i2c_trace.py --raw-dir` だけです。
- キャプチャの開始位置:
  - PWM と tone は、応答を受けて 50 ms 待ってから開始します。
  - TOGGLE、MILLIS、SPI、I2C は、コマンドを送る前に開始します。

## 5. 決まっている方針

- **マーカーは PC 側のログに書きます。** CH32 には送りません。そのため、ファームウェアは変えなくてよい。見出しの `#` の数が階層で、名前のない見出し（`##`）はその階層を閉じます。キャプチャは、開始した時刻で区間に割り当てます。
- **期待は区間のパスに付けます。** `Recorder.section(level, name, expect=[...])` を使います。
- **照合は推定ではありません。** ピンと役割を与えて復号し、指定した役割の復号結果と比べます。
- **コマンドと応答はイベントです。** `run.json` の `log` に時刻付きで残します。主に GUI 向けで、CLI の結果には既定で出しません。
- **WireSkein は当面 Python で進めます。** Rust で作るかは未定で、作るなら別リポジトリ（例: `wireskein-rust`）にします。

## 6. やること（順番に）

1. **trace_kit に記録器を組み込む。**
   - `wireskein/prototype/wsproto/runlog.py` を読み込めるようにします。`--wireskein PATH`（既定 `~/dev_oep/wireskein/prototype`）で `sys.path` に足す形が、今の `--oep-client` とそろいます。runlog は標準ライブラリだけなので、テスト側の依存は増えません。
   - `Session` か `Capture` に、オプションで `Recorder` を持たせます。
   - コマンドの送信（`link.send`）、応答（`link.wait` で得た行）、キャプチャ（`arm()` の直後に `rec.armed()`、`read_all()` の後に `rec.capture(data, rate, bits, t)`）を記録します。
   - `bits` には CH32 のピン名（`PA1` など）を、キャプチャのビットの順に並べます。
   - `Segment.start_us` も `capture(..., start_us=...)` の追加情報として残します（将来の時刻合わせ用）。
2. **P4 の GPIO 番号を CH32 のピン名に引く関数を作る。** `TARGETS` の逆引きです。スクリプトの先頭に直に書いた x035 の定数は、`TARGETS` から取るようにそろえます。
3. **`oep_periph_trace.py` と `oep_i2c_trace.py` に `--run-dir DIR` を足す。**
   - 見出しを `# test_pwm` / `## duty=64` のように付けます。
   - 期待を付けます。
     - PWM: `square("PA1", 1000, duty/255, tol_freq=0.02, tol_duty=0.01)`。duty 0 と 255 は `level`。
     - tone: `square(pin, hz, 0.5, tol_freq=...)`。
     - I2C: `i2c(scl, sda, transactions, hz=...)`。終了時に解放されていること。
     - SPI: `spi(...)`。
   - 最後に `ws.py verify DIR --junit DIR/report.xml` を実行し、その終了コードをスクリプトの終了コードに反映します。実行は `uv run --project <wireskein>/prototype` などで、wireskein 側の環境を使います。
   - `ws.py` は `PYTHONPATH=<wireskein>/prototype` を前提にしています。
4. **（任意）`testcmd.h` で `#` の行を無視する。** 将来、コマンドをキャプチャできる UART で送る場合のためです。`sync_testcmd.py --check` で、各スケッチの複製をそろえる仕組みがあるので、それに従います。

## 7. 実機で確かめること（受け入れ条件）

| 確かめること | 条件 |
| --- | --- |
| x035 の `periph_trace --only pwm --only tone --only spi --run-dir ...` | 正しく動いている今のコアで、NG が 0 |
| x035 の `i2c_trace --run-dir ...`（既定の経路） | 0x42 の書き込みが OK。0x43 はアドレスの NACK として OK。終了時に SCL と SDA が High |
| x035 の `i2c_trace --stuck` | SDA を張り付かせたステップで `i2c` の検査が NG になる（意図どおりの検出） |
| わざと誤った期待（duty の期待を 10% ずらす、など） | その項目だけ NG になり、理由と測定値が出る |
| v003 で同じもの | サンプルレート（2 MHz まで）で測れる範囲で、NG が 0。測れないもの（SPI が 4 MHz を超える、など）は検査を外す |
| 記録の大きさ | 1 回の実行で `run.json` と `.bin` の合計がどれくらいかを記録する（リポジトリに入れるかの判断材料） |

NG が出たときは、先に「照合する側の誤り」か「コアの不具合」かを切り分けます。

- `.bin` は oep_client の `LogicCapture.to_sr(path, data, samples, names)` で `.sr` にして、PulseView で見られます。
- 照合する側の誤りと判断した場合は、その記録（`run.json` と `.bin`）を残し、wireskein の作業に戻します。wireskein 側では `corpus/raw/`（git の対象外）に置いてから、正解を付けて `corpus/fixtures/real/` に変換します。

## 8. 分かっている限界

- PC の時計とキャプチャの開始時刻の対応は ms の精度です（区間の割り当てには十分ですが、µs のずれは測れません）。キャプチャの時間軸の印は、OEP の `oep.if.capture` にありません。
- `only_moving` は、そのキャプチャに入っているピンだけを見ます。確かめたいピンはキャプチャに加えます（チャンネルを増やすと窓が短くなります）。
- 方形波の検査は、窓の全体が定常であることを前提にしています。立ち上がりや切り替わりを含む窓では NG になります（定常の窓で測るか、`pulses` を使う）。
- SPI の復号には、1 クロックに 5 サンプル程度が要ります（20 MHz のキャプチャでは、SCK は 4 MHz まで）。
- I2C の tLOW と tHIGH、SPI の CS のタイミング、WS2812 のビット幅の検査は、まだありません。

## 9. 作業の決まり

- 返答は日本語で書きます。
- コミットとプッシュは、依頼されたときだけ行います（main へのコミットは利用者が行う）。
- `~/dev/I2CDeviceDB` と `~/dev_wch/wch-protocols` は読むだけで、変更しません。
- 書式や規則を新しく作る前に、既存の仕様（wireskein の `docs/`、`oep-spec/docs/`）を探します。マーカーの書式を仕様を見ずに作り、やり直した経緯があります。
- 照合する側（wireskein）を直す必要が出たら、その内容と記録を持ち帰り、wireskein のセッションで直します。ArduinoCore-CH32 のセッションでは、wireskein は読むだけにします。
