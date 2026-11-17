# アルゴリズムの測定台

[English](README.md)

推定の各段（線の分類、クロックの周期、ビット時間、ノイズの除去、全体）に、複数のアルゴリズムを並べて同じデータ
で測り、結果をすべて台帳に残します。本命（今の WireSkein のやり方）と試行中の方式を同じ表で比べ、どの方式を
製品に取り込むかを数字で決めるためのものです。package には入らず、`src/` からは使いません。

```sh
uv run python bench/run.py --list                                  # 段、アルゴリズム、スイート
uv run python bench/run.py --stage all --suite smoke               # 全部が動くか（30 秒ほど）
uv run python bench/run.py --stage clock_period --suite noise --note "comb: 候補をヒストグラムから"
uv run python bench/report.py                                      # アルゴリズム × データセットの表
uv run python bench/report.py --stage e2e --delta                  # 本命との差
uv run python bench/report.py --history clock_period/comb          # 1 つの方式の試行の跡
uv run python bench/select.py --stage e2e --suite noise --by noise # 切り替えの検討（1.8）
```

台帳に残す計測は、どの機械でも同じ版で走らせます（`uv run --with-requirements bench/requirements.txt python bench/...`、1.9）。

## 1. 決まり

### 1.1 分ける

- **測定台は製品から完全に分ける。** `src/wireskein` は `bench/` を読み込みません。測定台は `wireskein` を読む
  だけで、書き換えません（モンキーパッチもしない）。
- **製品への取り込みは別の変更にする。** 試行中の方式が勝ったら、製品のコードに入れる変更を別に作り、その根拠
  として台帳の実行 ID を挙げます（1.5）。

### 1.2 段（stage）

- **段が入力と出力と採点を決める。** アルゴリズムは自分を採点しません。同じ段のアルゴリズムは、すべて同じ採点の
  コードで、同じ正解と比べられます（`wsbench/stages.py`）。
- **アルゴリズムは正解を見ない。** 受け取るのはキャプチャだけです（チャンネル名は D0…Dn で、並びも入れ替えて
  あります）。
- **段の採点は変えずに足す。** 指標を足すのは自由です。今ある指標の意味を変えると昔の数字と比べられなくなるので、
  変えるときは新しい名前の指標にします。測定台のコードのハッシュ（`env.bench_sha`）が台帳に残ります。
- **前の段の正解を与えて測る。** `clock_period` はクロックだと分かっている線だけ、`bit_unit` は非同期の線だけ
  を聞かれます。前の段の誤りが混ざらず、その段だけの力が分かります。前の段の誤りの影響は、`e2e` で全体として
  測ります。

| 段 | 受け取るもの → 返すもの | 主な指標 |
| --- | --- | --- |
| `lineclass` | `fn(cap)` → `{線: {"label": ...}}` | `accuracy`（ラベル: clock / data / select / periodic / random / static）、`accuracy_ns`（select と data を同じ組として数える。選択線はほかの線を囲む役割で、線 1 本からではなくグループ分けで決まるため） |
| `clock_period` | `fn(cap, lines)` → `{線: {"period_s", "confidence"}}` | `hit3`（±3% に入った割合）。`octave`（整数倍の取り違え） |
| `bit_unit` | `fn(cap, lines)` → `{線: {"unit_s", "confidence"}}` | `hit3` |
| `grouping` | `fn(cap)` → `[{"clock", "data", "select", "score"}]`（バスの候補） | `found`（正解のクロック・データ線・選択線の組が候補にある割合）、`true_score`（その組の点数。後の段の証拠の強さ）、`groups_per_case` |
| `segments` | `fn(cap, {線: "clock" / "async"})` → `{線: [(始め, 終わり, 単位)]}` | `rate_ok`（正解の区間の中ほどで速さが 5% 以内）、`blocks_err`（速さの区間の数の誤差） |
| `clean` | `fn(cap)` → `Capture` | `f1_rel`: ノイズを入れる前のエッジとの一致。その線の一番短いランの 4 分の 1 以内なら一致とする（跳ね返りを最後にまたいだ所で読むと、エッジは数十 ns ずれ、速い取り込みでは何サンプルにもなる）。`f1` は 2 サンプル以内。ノイズなしの列で劣化しないことも見る |
| `e2e` | `fn(cap)` → claims（`decode.export` の形） | `ok`（確定または有力で正しいバス）、`false_confirmed`（**0 を保つ**） |

### 1.3 アルゴリズム

- **1 つの考え方を 1 つのファイルにする**（`algos/<段>/<名前>.py`）。パラメータ違いは `variants` で並べます
  （`deglitch.k2`、`deglitch.k3`）。
- **状態は 3 つ。** `main`（本命: 今の WireSkein のやり方。段ごとに 1 つ）、`trial`（試行中）、`retired`
  （引退）。
- **消さずに引退させる。** うまくいかなかった方式も `status="retired", reason="..."` にして残します。何を試して、
  なぜやめたかが後から読めるようにするためです。引退した方式は、名前で頼んだときだけ走ります。
- **同じ入力には同じ出力を返す。** 乱数が要るなら、入力から種を作ります。
- **エンジンに差し込める方式には、その形を添える**（`@algorithm(..., engine=関数)`。関数は方式のパラメータを
  受け取り、`wireskein._engine.methods` の段の名前と関数の組を返す）。e2e の `inject` は、それをエンジンに差し込んで
  解析全体を測ります。段ごとの指標で勝っても、解析全体が良くなるとは限らないので、取り込む前にこれで測ります。
- **共通の部品は `wsbench/kit.py` に置く。** アルゴリズムのファイルどうしは読み込み合えません。ファイルと `kit.py`
  のハッシュが台帳に残り（`algo_sha`）、変わると表に `!` が付きます（測り直しが要る印）。

```python
from wsbench import algorithm

@algorithm("clock_period", "comb", status="trial", about="一行の説明",
           variants={"m2": {"max_mult": 2}, "m4": {"max_mult": 4}})
def comb(cap, lines, max_mult):
    return {n: {"period_s": ..., "confidence": ...} for n in lines}
```

### 1.4 データ

- **データセット = 元のデータ × 変換の列。** どちらも文字列で書き、そのまま台帳に残ります。
  - 元: `set:<名前>[a:b]`（`eval/corpus/synth` の固定のセット）、`real`（`eval/corpus/real`）、`dir:<パス>[a:b]`
    （`<id>.wireskein` と `<id>.truth.json` が並ぶ任意のディレクトリ）、`gen:<プロファイル>[/<ストレス>][a:b]@<速度>`
    （`eval/synth.py` のシナリオを、指定の速度で取ったもの。どの速度でも通信は同じなので、`decimate` と組み合わせて
    速度を振れます）、`i2cbench:h417_slow/stream[a:b]`（`tests/data/i2c_bench` の実機の記録。弱いプルアップの I2C
    に「S, 0x13 書き込み, NACK, P」が 50 回。正解は読むときに作る。SCL と SDA は D0 と D1 に付け替える。アドレスの
    ACK / NACK まで比べる（正解の `expect.compare_ack`。eval の生成データは比べない）
  - ほかのリポジトリの記録（コピー）: `wchfix:<サブディレクトリ>[a:b]`（wch-protocols の LinkE の fixture。実機の
    RVSWD / SWIO、正解は参照の復号器の `.dmi.txt`）、`ebdd:<名前>[/tune|/holdout][a:b]`（EmbedBenchDeviceData の記録。実機の I2C、
    正解はファームウェアの出力。`embedbench-device-data` は 2 MS/s 以下で取ったもの、`embedbench-device-data-50m` は
    ENV III を 50 MS/s で、バスは 100 k / 400 k / 1 MHz。`/tune`、`/holdout` はケースの名前で半分を確認用に取り分ける）
  - 測定台のために取った記録: `linke:<ディレクトリ>[/rate]/<tune|holdout>[a:b]`（`bench/data/<ディレクトリ>`。
    wireskein-run/1 に正解を添えたもの。WCH-LinkE の RVSWD を 160 MS/s で、1 回に ch32rv の操作 1 つ、その区間に
    切り出してある。正解は ch32rv が読んだ中身と、USB で送った DMI の操作。遅く取ったもの（`decimate`、`resample`）
    には、それらの操作がすべて見つかる 160 MS/s の読みも正解にする。`/rate` は同じ操作をプローブが遅い速さで
    取ったもの。確認用の半分は、取った時点でシナリオの名前から決めてある）。切り出す前の生の記録は、別の
    リポジトリ（wireskein-bench-raw）に置き、ディレクトリの `SOURCE.json` が指す
- **測定台は、このリポジトリのファイルしか読まない**（Actions でも手元でも同じ。`dir:` でリポジトリの外を指すと
  拒否します）。スイートが使うほかのリポジトリの記録は、要るものだけを `bench/data/extract.py` で `bench/data/` に
  コピーし、元の `LICENSE` と `SOURCE.json`（リポジトリ、コミット、各ファイルの元と SHA-256）を付けます。記録は
  `.wireskein` に変換することがありますが、エッジは同じで、ケースの `input_sha` も変わりません（コピーのときに確認）
  - 変換: `spike:L`（ランダムな 1 刻みのパルス。エッジ 1 つあたり L 個）、`bounce:L`（エッジの L の割合で直後に
    跳ね返り）、`xtalk:L`（隣のチャンネルのエッジの L の割合で 1 刻みのパルス）、`crop:A:B`（A から B の部分）、
    `decimate:K`（K 倍遅く取ったもの）、`resample:HZ`（そのキャプチャの格子から HZ で取ったもの。プローブの
    分数の分周と同じく、サンプルは元のクロックの刻みに乗り、間隔は揃わない。整数の分周なら `decimate` と同じ）。
    `+` でつなぎます（`crop:0:0.5+spike:0.1`）
  - オープンドレインの遅い立ち上がり: `odslow:NS`（I2C の線の立ち上がりが NS ナノ秒ほど遅れ、それより短い High は
    消える。弱いプルアップの実機と同じ、切り詰められたクロック）
  - 時間で決めたノイズ: `pspike:L:NS`（NS ナノ秒のパルス。エッジ 1 つあたり L 個）、`pbounce:L:NS`（エッジの L の
    割合で、直後の NS ナノ秒のうちに跳ね返り）。実際の線と同じく、速く取るほど何サンプルにも見え、遅く取ると
    サンプルが当たらずに写らないことがあります。刻みで決めたノイズ（`spike` など）は、どの速度でも 1 刻みです
  - **取り込みの速度を上げても点数が下がらないこと**を、`rate` スイートで確かめます（同じ通信を 200 MS/s から
    16 分の 1 まで）。情報が増えて悪くなる方式は、閾値をサンプル数で決めているなどの欠陥がある印です
  - ノイズの乱数の種は、ケースの ID と変換の文字列から作ります。いつ、どの機械でも同じデータになります。各ケース
    の入力のハッシュも台帳に残ります（`inputs_sha`）
- **読めないはずのデータは、外さずにラベルを付けて、別に数える**（`wsbench/data.py` の `hardness`）。ラベルは、
  方式の結果を見ずに、正解と物理と記録の事実だけから決めます。`undersampled`（クロックのあるバスで 1 周期 4 サンプル
  未満、非同期で 1 ビット 3 サンプル未満。速度が変わるものは一番速い所で）、`noise_over_bit`（ノイズの幅がビットの
  半分以上）、`defective_capture`（実機の記録で、README に記録された、プローブのフィルタが固まる設定）、
  `aliased`（周波数 × 2.5 が取り込みの速さを超える、おとりの周期的な線）。e2e の指標は、今までのもののほかに、
  ラベルのないバスだけの `ok_readable` / `confirmed_readable`、ラベルのあるバスの `ok_hard` / `confirmed_hard` /
  `decode_hard`（どこまで頑張れるか）、ラベルごとの `ok_<ラベル>`、エイリアスの線を使った誤確定
  `false_confirmed_aliased` を出します。**誤確定は、読めないデータでも合計に数えます**（自信を持った誤りは、
  利用者には実害だからです）。
- **スイートは、結果ができたら変えない**（`suites.toml`）。変えたいときは新しい名前で足します（`noise-v2`）。
- **調整用（tune）と確認用（holdout）を分ける。** holdout のスイート（生成の heldout のシード、実機の記録）を
  見て方式を直すことはしません。
- **正解は WireSkein の外から取る。** 生成器、ほかの確立した復号器、試験のファームウェアが送ったもの、のどれか
  です。WireSkein の出力を正解にはしません。

### 1.5 台帳と取り込み

- **実行はすべて台帳に追記する**（git に入れる）。`results/ledger.jsonl` は 1 行が（実行、段、アルゴリズム、
  データセット）の 1 つで、要約、コスト、アルゴリズムのハッシュとパラメータ、データセット、入力のハッシュを
  持ちます。`results/runs.jsonl` は 1 行が 1 回の実行で、その実行に共通のもの（コミット、作業ツリーが dirty
  だったか、`src/wireskein` のハッシュ、機械と版、`--note` の理由、コマンド）を持ちます。読むツールは両方を
  つないで読みます。行は消しません。点数は 6 桁、コストは 4 桁に丸めます。項目ごとの結果は
  `results/runs/<実行>.jsonl` に出ます（git には入れません）。
- **残す実行は、コミットしてから走らせる。** 書きかけの試行は `WSBENCH_RESULTS=<ディレクトリ>` で別の台帳に
  出します。
- **trial を main にする条件**（製品に取り込む条件）:
  1. 調整用のスイートで main より良く、holdout のスイートでも悪くならない
  2. ノイズなしの列（`clean`）で main より悪くならない
  3. `e2e` の `false_confirmed` が増えない（0 のまま）
  4. かかる時間（`seconds_median`）が許せる
  取り込んだら、製品の変更の中で `current` がその方式になり、試行のファイルは `retired`（reason: 取り込んだ版）
  にします。

### 1.6 外から足す

- **アルゴリズム**: `--algos-dir DIR`（何度でも）か、環境変数 `WSBENCH_ALGOS`（`os.pathsep` 区切り）。中身は
  `algos/` と同じ書き方で、リポジトリの外に置けます。台帳には、そのファイルのパスとハッシュが残ります。
- **データ**: 自分の記録を、eval の正解の形（`<id>.truth.json`）を添えてディレクトリに置き、`dir:<パス>` で
  スイートに入れます。

### 1.7 コスト

`report.py --cost` は、方式ごとの点数とコストを並べ、パレート最適（ほかのどれにも「点数が上で、かつ安い」で負けない）
に `P` を付けます。時間はほかの負荷（別のビルドなど）で揺れるので、揺れにくいものから順に見ます
（`wsbench/cost.py`）:

| 指標 | 意味 | 負荷への強さ |
| --- | --- | --- |
| `mem kB`（`--memory`） | 呼び出しの間に確保したメモリのピーク（tracemalloc） | 負荷に関係なく同じ値 |
| `scaling` | エッジ数に対する CPU 時間の伸び（1 なら比例、2 なら 2 乗） | 定数倍の揺れでは動かない |
| `vs main` | 同じケースを同じワーカーで続けて測った、本命との CPU 時間の比（幾何平均） | 全コアが埋まった状態で ±15〜30% |
| `ref` | ケースごとに横で測る基準作業との比。機械が違っても比べられる | 重い段は ±10%、軽い段は 2 倍ほど揺れる |
| `cpu ms` | CPU 時間（短い呼び出しは最大 5 回繰り返して最小を取る） | 同じ物理コアを分け合うと伸びる |

- **方式を選ぶときは `vs main` と `scaling` で決める。** 10 倍遅い、データの 2 乗で遅くなる、といった違いは負荷が
  あっても見えます。10% の差を論じるときは、空いた機械で測り直します。
- 実行前の負荷（loadavg）は台帳に残り（`env.load_before`）、混んでいた実行は表に注意が出ます。
- **命令数**（`M instr`）: ハードウェアのカウンタがある機械（ネイティブの Linux）では、呼び出しごとに実行した命令の
  数を自動で記録し、本命比と伸びも命令数で出します。負荷やクロックの変化でほとんど動かないので、取れるときは
  これで判断します。WSL2 や多くのクラウドの VM（GitHub のランナーも）には PMU がなく、記録しません。
  `python bench/wsbench/perfcount.py` で、その機械で取れるか（取れないなら理由）が分かります。

### 1.8 切り替えの検討（`select.py`）

どんなデータにも一番の方式があるとは限りません。`select.py` は、実行の明細から（走らせ直さずに）切り替えの
やり方を比べます。

```sh
uv run python bench/select.py --stage e2e --suite noise --by noise
```

| やり方 | 意味 |
| --- | --- |
| `fixed <方式>` | どのデータにも 1 つの方式 |
| `oracle` | 項目ごとに、正解を知って最良を選んだもの。どんな切り替えでも、ここまでしか届かない |
| `confidence` | 項目ごとに、方式が出す自信度（`confidence`）の一番高いもの |
| `dispatch:<キー>` | プロトコル、線の種類、ノイズ、速度などの組ごとに、その組で一番の方式。組ごとの選択はケースの半分で決め、残りの半分で採点する（入れ替えて 2 回）ので、採点するケースに合わせた分は得点にならない |

`dispatch` の組のうち、実行時に分かるのは `rate`（取り込みの速度）と、キャプチャを測って得られる量
（`wsbench/observe.py`）で分けた `obs_noise`（その線のビット時間の 5 分の 1 より短いランの割合）、`obs_os`
（一番細かい線の 1 ビットあたりのサンプル数）、`obs`（その両方）だけです。`(truth)` の付いた組（プロトコル、
線の種類、データセット、ノイズの種類）は測定台だけが知っている正解の情報で、上限の目安にしかなりません。

e2e では、各方式の出力そのもの（確定・有力・拮抗の数、実行時に分かる）で切り替える案も比べます: `cascade A->B`
（まず A、A に迷いがあれば B）、`most_confirmed`（確定の一番多い出力）、`least_unsure`（迷いの一番少ない出力）。

`oracle` が一番の `fixed` より大きく上で、`dispatch` か `confidence` がそれに近いなら、切り替えを作る価値が
あります。そうでなければ、1 つの方式で足ります。`--by <キー>` で、組ごとにどの方式が勝つかの表も出ます。

### 1.9 どこで測るか

点数は決定的（同じケース、同じ種、同じ方式なら同じ値）なので、どこで測っても同じです。コストは機械で変わります。

| 何を | どこで | どう |
| --- | --- | --- |
| 全スイートの点数（本計測） | GitHub Actions | Actions の「Bench」を手動で起動（段、スイート、方式、並列数、メモ、コミットするか）。スイートの既定は `all`（全部のスイート）、確認用を除くなら `tune`。ケースを並列のジョブに分け（1 つのケースの方式はすべて同じジョブ）、最後に集計して、台帳の新しい行を起動したブランチにコミットする（push の直前にブランチの最新を取り直し、その上に追記し直す。先に別の push があればやり直す）。明細は artifact `bench-<実行 ID>` に置き、手元へは `uv run python bench/merge.py --import <展開したディレクトリ>` |
| コスト | 命令数を数えられる機械（ネイティブの Ubuntu） | `cost` スイートを `--memory` 付きで。並列数はコア数より 1 少なく |
| 素早い試行 | 手元 | `smoke`、`rate-smoke` など。`WSBENCH_RESULTS` で別の台帳に |

- 命令数を数える機械でのコストの計測:

  ```sh
  git pull                                       # 作業ツリーはきれいなまま（台帳の行に dirty と残らないように）
  python3 bench/wsbench/perfcount.py             # 2 行目が「counts itself: ok」なら、ワーカーも自分を数えられる
  uv run --with-requirements bench/requirements.txt python bench/run.py --stage all --suite cost --memory -j 3 \
      --note "cost on <機械>"
  uv run python bench/report.py --suite cost --cost
  git add bench/results && git commit -m "bench: cost run on <機械>" && git push
  ```

- 分けて走らせるときは `run.py --shard I/N --run-id ID` を N 回、そのあと `merge.py ID` です（Actions はこれを
  しています）。分けても分けなくても、点数と入力のハッシュは同じになります。
- 依存の版は `bench/requirements.txt` で固定します。変えると測るものが変わるので、変えた後の最初の実行の `--note`
  に書きます。Python の版は台帳に残ります。
- Ubuntu の既定（`kernel.perf_event_paranoid=4`）では、ふつうの利用者は命令数を数えられません。
  `sudo sysctl kernel.perf_event_paranoid=2` で、自分のプロセスのユーザー空間だけを数えられるようになります。

調査の記録は `bench/notes/` に置きます（例: [2026-11-16 判定のゼロベースの見直し](notes/2026-11-16-detection-review.ja.md)）。

## 2. 段を足す

`wsbench/stages.py` に `Stage` を継いだクラスを書き、`STAGES` に加えます。決めること: アルゴリズムの呼び方
（`run`）、項目ごとの採点（`score`）、まとめ（`summarize`）、表の主な指標（`headline`）、低いほど良い指標
（`LOWER`）。そのうえで、今の WireSkein のやり方を `main` として `algos/<段>/current.py` に包みます。比べる
相手がないと、試行の良し悪しが分からないからです。

まだない段で、次に足すとよいもの: 速度の区切り（可変クロック・ボーレートの区間）、クロックとデータの組（グループ）、
フレームの区切り。
