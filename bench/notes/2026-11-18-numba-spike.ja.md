# numba を任意の高速化として試す（2026-11-18、調査）

**numba を入れるかどうかは、まだ決まっていない。** これは入れるかを決めるための調査:
- 任意の extra として入れる形を作った。
- 実際の解析の熱い所を 3 つ移した。
- 出力がバイトまで同じかを確かめ、numba あり・なしの時間を測った。

製品の版・CHANGELOG・docs は変えていない。

関連:
- Rust のコアの試し（ブランチ rust-core-spike の `bench/notes/2026-11-18-rust-spike.ja.md`。main にはない）: 単体のカーネルでは numba は Rust と同じくらい速い
- [グランドデザイン](2026-11-18-grand-design.ja.md)（§3 の「1 つの層に実装は 1 つ」）

## 0. まとめ

- 実際の解析で速くなった分のほとんどは、Python のループを配列の処理に書き換えたことから来ていて、numba は要らない。
  - x_read_ram_20k: 18.3 → 15.9 s（−13 %、静かな時間帯）。
  - 混んだ時間帯に main と交互に回した値では、28.4 → 20.6 s（−28 %、§3.4）。
  - numba 自体の上乗せは、さらに 15.9 → 15.2 s（−4 %）だけ。
- 解析の時間の多くは、Python のオブジェクトを作る所にあり、numba は効かない。
  - 宣言の定義の解釈（`declarative.parse_sequence` など）、`riscv_dm`、`pulsecode.rgb_led`。
- numba が大きく効くのは、numpy でまとめて書いてある所をループに直したとき（estimate_units で 4 倍）。
  - ただしそのループは、numba なしでは numpy の約 60 倍遅い。
  - 「同じソースが numba あり・なしの両方で動く」（1 つの実装）が成り立たない。
- 2 つの道を用意した。
  - **A**（ブランチ `worktree-agent-a600b4bbe6abf699a`）: numba を任意の extra `wireskein[fast]` にして、3 つのカーネルを jit にした。
  - **B**（ブランチ `array-rewrite-no-numba`）: numba は入れず、numba なしで速くなるか同じ書き換えだけを取った。

## 1. numba の入れ方（道 A）

- `pyproject.toml` に `fast = ["numba>=0.61"]` を足した。0.61 は、Python 3.13 と numpy 2.1 に出た最初の numba。
- `src/wireskein/_engine/jit.py` の `njit(fn)` は、最初の呼び出しのときに次のどちらかになる。
  - numba を import できるとき: `numba.njit(cache=True, nogil=True)(fn)` にする。
  - import できないとき、または環境変数 `WIRESKEIN_JIT=0` のとき: `fn` をそのまま使う。
- そのため、同じソースが両方で動く。WIRESKEIN_JIT=0 は、比べるときや、pdb でカーネルの中に入るときに使う。
- numba を import するのは、カーネルを最初に呼んだときだけ。`import wireskein`、runlog、capture の保存では import しない。
- numba が入っていても使えないときは、ImportError になり、黙って素の Python で動く。
  - 例: numba が支えない版の numpy（numba 0.68 は numpy 2.5 まで）。numba は import のときに版を確かめて ImportError を出す。
- コンパイルの失敗は隠さない。そのまま例外になる。

## 2. 移したもの

x_read_ram_20k（RVSWD、30 秒）のプロファイルで、numba に向く数値のループを選んだ。

| 場所 | 前 | 後（道 A） | 道 B |
| --- | --- | --- | --- |
| `typed.chars` | ビットの列を 1 つずつ歩き、文字ごとに numpy の切り出し・シフト・和を作る | 文字の始まりを探す歩きだけを jit のカーネル `_char_starts` に。ビットは全文字をまとめて 1 度の配列の処理で読む | 同じ書き換え。歩きは素の Python |
| `typed.async_segments` の区切りの選択 | アイドルの run のリストを作り、`min(key=lambda)` | jit のカーネル `_fewest_bad` | numpy でまとめて（最小の bad → 最長 → 最初） |
| `kernels.chain` | `tolist()` したリストを歩く | jit のカーネル `_follow`（配列を歩く） | 前のまま |

移さなかったもの:
- `declarative.parse_sequence`、`riscv_dm`: オブジェクトを作る処理。
- `rgb_led`、`frames_startstop`: 主な時間がタプルとリストを作る所。
- `estimate_units`: §4 を参照。

## 3. 測った値

手元の環境: x86_64、4 コア、Python 3.13、numpy 2.5.5、numba 0.68.1。

### 3.1 出力が同じか

- snapshot の 27 例: 合成の swd・can・swdcan の各種 0〜7（eval/synth.py の scenario）と、V203 の high/target_info、low/read_flash_4k、high/x_read_ram_20k。
- analyze の export（mode all、alternatives）が、元のコードとバイトまで同じだった。
  - 道 A: numba あり、WIRESKEIN_JIT=0 の両方で同じ。
  - 道 B: 同じ。
- 全試験（MemoryMax=4G の上限付き）:
  - 道 A、numba あり: 509 passed, 13 skipped
  - 道 A、WIRESKEIN_JIT=0: 509 passed, 13 skipped
  - 道 B: 507 passed, 15 skipped（§3.4）

### 3.2 解析の CPU 時間（道 A、process_time、2 回の最良、同じ時間帯に続けて）

| 記録 | 元のコード | 書き換え・numba なし | numba・キャッシュなしの最初の 1 回 | numba・キャッシュあり |
| --- | --- | --- | --- | --- |
| x_read_ram_20k | 18.32 s | 15.91 s | 16.57 s（2 回目から 15.42 s） | 15.24 s |
| read_flash_4k | 4.01 s | 3.59 s | 3.46 s | 3.49 s |
| target_info | 1.54 s | 1.33 s | 1.28 s | 1.30 s |
| 合成 swdcan 3 / can 5 | 0.10 / 0.07 s | 0.10 / 0.07 s | 0.10 / 0.07 s | 0.10 / 0.07 s |

- 最初の 1 回とキャッシュあり・なしの差（約 1 s）は、3 つのカーネルのコンパイルの時間。
- 同じ日の朝、ホストが混んでいた時間帯の 1 回きりの値も残す（x_read_ram_20k）。
  - 元のコード 37.6 s → 書き換え・numba なし 29.9 s → numba あり 28.2 s。
  - 割合は上の表と同じ傾向。
- 合成の例は小さく、どれも 0.1 s 前後で差が出ない。

import の時間（プロセスごと）:

| 場面 | 時間 |
| --- | --- |
| `import wireskein.decode` | 0.11〜0.20 s（numba の有無で変わらない） |
| numba の import（ディスクのキャッシュが温まっているとき） | 0.16〜0.43 s |
| numba の import（冷えた読み取り専用の環境の最初の 1 回、混んだホスト） | 2.7 s |

### 3.3 カーネル単体（numba なしで遅くなる所）

ランダムな入力、5 回の最良:

| カーネル | 元のコード | numba あり | numba なし |
| --- | --- | --- | --- |
| chars（100 万ビット） | 971 ms | 13 ms | 72 ms |
| chain（20 万の始まり） | 37.5 ms | 14.7 ms | **54 ms（1.4 倍遅い）** |
| 区切りの選択（10 万） | 36 ms | 0.8 ms | 37 ms（道 B の numpy 版は §3.4） |

- chain は、numba なしでは numpy の配列を 1 要素ずつ読む分だけ遅くなる。
- chain が numba でも 2.5 倍にしかならないのは、時間の多くが searchsorted（numba の外）にあるから。
- 道 B では chain を前のままにした。

### 3.4 道 B（ブランチ `array-rewrite-no-numba`、main の ec279c7 から）

道 B は、numba を入れずに次の 2 つだけを書き換えた:
- `typed.chars`: 全文字のビットを 1 度に読む。歩きは素の Python で、リストに積む。
- 区切りの選択: numpy でまとめて選ぶ。

jit.py、extra、chain の書き換えは入れていない。

出力:
- snapshot の 27 例で、main とバイトまで同じだった。
- 全試験（上限付き）: 507 passed, 15 skipped。main で増えた試験の 2 つが、環境変数がないため skip になる。

単体（ランダムな入力、5 回の最良、numba なし）:

| 場所 | main | 道 B |
| --- | --- | --- |
| chars（100 万ビット） | 785 ms | 61 ms |
| 区切りの選択（10 万） | 23.4 ms | 0.94 ms（numba 版と同じくらい） |

区切りの選択は、小さな種付きの入力 300 例でも main と同じ答えだった。

解析の CPU 時間の測り方:
- ホストの混み具合が大きく揺れた（別々のプロセスで測ると、main の 2 回が 30.6 s と 24.0 s）。
- そこで、1 つのプロセスの中で main の 2 つの関数（chars、区切りの選択）を差し戻す形にし、main と道 B を交互に 4 回ずつ回した。
- 各回で、export も同じだった。

結果（process_time、最良）:

| 記録 | main | 道 B | 各回の組（main, 道 B） |
| --- | --- | --- | --- |
| x_read_ram_20k | 28.37 s | 20.56 s（−28 %） | (33.9, 27.2)、(34.0, 29.4)、(28.4, 20.6)、(30.9, 26.9) |
| read_flash_4k | 7.23 s | 6.31 s（−13 %） | (7.3, 6.3)、(7.4, 6.7)、(7.6, 6.6)、(7.2, 6.4) |
| target_info | 2.63 s | 2.53 s（−4 %） | (2.6, 2.5)、(2.9, 2.7)、(3.1, 2.8)、(3.1, 2.7) |

- 1 回目と 3 回目は main を先に、2 回目と 4 回目は道 B を先に回した。
- どの回でも道 B が速かった。
- x_read_ram_20k の差（約 7 s）は、道 A の §3.2（元のコードと書き換え・numba なしの差 2.4 s）より大きく出た。
  - 混んだホストでは、文字ごとの numpy の小さな呼び出しの固定費が膨らむためとみられる（未確認）。
  - 割合は、静かな時間帯に測った §3.2 の −13 % を下限の目安とする。

## 4. 1 つの実装の決まりとの相性

numba に向くのは、素直なループ。それが numba なしでも許せる速さかどうかで、2 つに分かれる。

- **もともと Python のループで書かれている所**（chars の歩き、chain、deglitch、区切りの選択）:
  - 同じソースを jit にしても、numba なしで前と同じくらいの速さ。
  - 1 つの実装のまま両方で動く。
  - ただし実際の解析での numba の上乗せは小さい（§3.2）。
  - chars で効いたのは、文字ごとの numpy の呼び出し（固定費）をまとめたことで、numba ではない。
- **numpy でまとめて書いてある所**（estimate_units、collapse など）:
  - ループにすると numba で速くなるが、numba なしでは何十倍も遅い。
  - estimate_units で試した（コミットしていない）。
    - x_read_ram_20k の 20 回の呼び出し: numpy 5.46 s → numba 1.37 s（4 倍）。
    - 同じループを素の Python で 3 万 runs: 4.19 s。numpy では 0.068 s で、約 60 倍遅い。実際の 30 万 runs × 20 回では、10 分を超える見込み。
  - 浮動小数の和の順が numpy（pairwise の和）と違うため、平均の残差（`resid`）が 20 回中 16 回、末の桁でずれた。
    - u2・fit・support は全部同じだった（整数の和なので順に依らない）。
    - `resid` は並べ替えの比べにしか使わないが、バイトまで同じとは保証できない。
  - ここを numba で速くするには、numba あり・なしで別の書き方を持つことになる。二重の実装になり、グランドデザインの決まりに反する。
- 残りの時間の多くは、numba が効かないオブジェクトの処理にある（書き換え後のプロファイル、x_read_ram_20k、プロファイラ込みで計 98 s）:
  - parse_sequence: 21 s（累計）
  - riscv_dm.feed: 7.4 s
  - estimate_units: 6.0 s
  - rgb_led: 5.8 s
  - 宣言の定義の `_run_words`: 11 s

## 5. キャッシュと読み取り専用の環境

`cache=True` のキャッシュ（`.nbi`/`.nbc`）が置かれる場所:

| 入れ方 | キャッシュの場所 | 確かめ |
| --- | --- | --- |
| 編集可能な形（`uv sync`） | モジュールの横の `__pycache__` | 確かめた |
| wheel、site-packages が読み取り専用 | ユーザーのキャッシュ（`$XDG_CACHE_HOME/numba` か `~/.cache/numba`）。numba が自動で移す | wheel を別の venv に入れ、`chmod a-w` で確かめた。2 回目のプロセスはコンパイルしない |
| `NUMBA_CACHE_DIR` を置いたとき | そこ | 未確認（numba の文書による） |
| どこにも書けない（読み取り専用のコンテナで、ホームも書けない） | なし | numba は最初の呼び出しで `RuntimeError: cannot cache function` を出し、解析が止まっていた。jit.py でキャッシュなしのコンパイルに落とすよう直した。そのときは、プロセスごとに毎回コンパイルする（3 つで約 2 s、混んだホスト） |

注意:
- numba のキャッシュは、ソースのファイルの時刻と大きさで無効になる。版を上げると、最初の 1 回でコンパイルし直す。
- 別の jit 関数を呼ぶカーネルでは、呼ばれる側を変えても呼ぶ側のキャッシュが無効にならないことがある（numba の既知の制約）。
  - 今の 3 つは他を呼ばない。
- 入力の型（int8 か int64 か）が変わると、型ごとに別にコンパイルし、キャッシュする。

## 6. Python と OS の支え

numba 0.68.1（2026-10-27）の wheel:

| | Linux x86_64 | Linux aarch64 | macOS arm64 | macOS x86_64 | Windows x64 | Windows arm64 |
| --- | --- | --- | --- | --- | --- | --- |
| cp311 | あり | あり | あり | **なし** | あり | なし |
| cp312〜cp315 | あり | あり | あり | **なし** | あり | あり |

- llvmlite 0.50.0 も同じで、macOS x86_64 の wheel がない。
  - そこでは `pip install wireskein[fast]` が llvmlite のソースからのビルド（LLVM が要る）で失敗するはず（未確認）。
  - 素の `pip install wireskein` には影響しない。
- 3.14 のフリースレッド版（cp314t）の wheel もある。
- numba 0.68 の制約は `numpy>=1.22,<2.6`。
  - `[fast]` を入れた環境では、numba が新しい numpy に出るまで numpy を上げられない。
  - extra を入れていなければ制約はかからない。
- WireSkein の対象（3.11 以上）は、今の numba で全部覆える。
- numba のない環境（Pyodide、32 ビットの ARM、musl）でも、素の Python で同じ出力になる。

依存の大きさ（Linux x86_64、cp313）:

| | wheel | 入れた後 |
| --- | --- | --- |
| llvmlite | 59.7 MB | 172 MB |
| numba | 3.9 MB | 17 MB |

## 7. 試していないこと

- Python 3.11、3.12、3.14 での動作（手元は 3.13 だけ）。macOS、Windows、aarch64 での動作。
- macOS x86_64 での `[fast]` の導入の失敗の様子。
- numba を抜いた環境での全試験。抜いた venv では、wheel の import とカーネルが素の Python で動くことだけを確かめた。WIRESKEIN_JIT=0 の全試験はそれに近い。
- 複数のプロセスが同時にキャッシュを書くとき（decode の `workers`、pytest-xdist）。
- `NUMBA_CACHE_DIR`。
- 3.14t（GIL なし）。`nogil=True` を付けたが、スレッドから呼ぶ試しはしていない。
- test.yml に `[fast]` の組を足したときの CI の時間。
- 長い記録での verify（短い区間を何度も読む使い方）の時間。小さな配列の呼び出しの固定費は numba で 1〜2 µs になるが、numpy の固定費が効く所は測っていない。
- L103 の長い記録（frames_startstop が重いもの）。

## 8. 決める材料

### 8.1 道 A（numba を任意の extra に）

得るもの:
- 移したカーネル単体で 3〜70 倍。
- ループのまま書いて速い道ができる。これから Python のループで書くカーネル（ビットの読み、宣言の解釈器の一部）を、numba 向けの形で書ける。

費用とリスク:
- 実際の解析での上乗せは、今は 4 % ほどしかない。
- 依存が約 60 MB（入れると 190 MB）。
- 最初の呼び出しのコンパイル（3 つで約 1 s）と、numba の import（0.2〜0.4 s、冷えた環境では数秒）。
- `numpy<2.6` の上限。macOS x86_64 は使えない。
- 読み取り専用の環境では、毎回コンパイルする。
- numba の型の推論のエラーは読みにくい。素の Python で通るのに numba で通らない書き方（オブジェクト、dict、例外の一部）を、カーネルの中では避ける約束が要る。
- 試験を 2 通り（numba あり・なし）流す費用が CI に増える。

1 つの実装との相性:
- 同じソースが両方で動く所だけを対象にすれば、守れる。
- 守ると、numba が大きく効く所（numpy でまとめてある所）には使えない。

Rust に移るとき:
- jit のカーネルは「型の決まった配列とスカラーだけのループ」なので、Rust にそのまま書き写せる。仕様の下書きになる。
- Rust を採ったら numba と jit.py は消す。消しても、素の Python の道があるので出力は変わらない。
- 二重にはならないが、移すまでの間 numba の追従（Python・numpy の新しい版）を抱える。

### 8.2 道 B（配列への書き換えだけ、numba なし）

- 依存も配布も今のまま。1 つの実装のまま。
- 速くなる分の大部分（x_read_ram_20k で −13 〜 −28 %）は、こちらで取れる。
- 区切りの選択は、numpy でまとめた方が numba 版と同じ速さで、numba なしでも速い。
- numba が効く余地（estimate_units の 4 倍など）は取れない。そこは、いつか Rust を採るなら Rust で取る。

### 8.3 問い

1. 道 B だけを先に取り込むか。道 A の上乗せ（4 %）は、依存と配布の費用に見合わないように見える。
2. numba を入れるなら、対象を「もともと Python のループの所」に限るか。numpy でまとめてある所は、二重になるので対象外のままにするか。
3. 道 A の chain（numba なしで 1.4 倍遅い）は、道 A を採るなら前のままに戻すか。
4. numba が入っているのに使えなかったとき（ImportError で素の Python に落ちたとき）に、警告を 1 度出すか。
5. 次に速くするなら、オブジェクトの処理（宣言の解釈器 `parse_sequence`、`riscv_dm`、`rgb_led` の出力の作り方）を配列で持つ形に変える方が、numba よりずっと効く。これをベンチの計画に入れるか。
