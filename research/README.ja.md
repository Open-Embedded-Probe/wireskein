# 推定パイプラインのプロトタイプ（使い捨て）

[プロトタイプ計画](../docs/prototype-plan.ja.md) に沿って、未知のデジタル記録から方式を推定する試作。**本番へ移植しない前提**で、測定できることを優先して書いている。結果は [findings.ja.md](findings.ja.md) に記録する。

エンジンは2つある。

- **段構成のエンジン（`src/wireskein/_engine/staged.py`、現在の主）**: 共通の下調べ → ピン分類 → 型付きストリーム → 型で受け取るプロトコルのプラグイン → スコア → 組み合わせ → 判定。
- **総当たりのエンジン（`src/wireskein/_engine/pipeline.py`、比較用）**: 全チャンネル×全パラメータの仮説をエッジから直接復号し、早期打ち切りと確実な除外で絞る。

## 実行

~~~sh
cd research
uv sync --group research        # リポジトリの package（wireskein）と、ベンチ用の mini-racer
uv run python evaluate.py --synth 200 --engine staged --tag s6   # 段構成（実記録8件＋生成200件）→ ../corpus/work/eval-s6.json
uv run python evaluate.py --synth 200 --probe --exclude --tag v6 # 総当たり（比較用）
uv run python stage_eval.py 60 [--stress freqhop|baudhop]       # 段ごとの情報量と正しさ
uv run python plugin_reuse_eval.py 60                            # UART・LIN・DMX512 が共通段を共有できるか
uv run python scpi_eval.py 60                                    # TX/RX の組と SCPI
uv run python upper_eval.py 80                                   # 上位層（NMEA・Modbus）の支持の効果
uv run python baud_segment_eval.py 60 --stress baudhop           # UART の途中切り替え
uv run python large_eval.py                                      # 100 万エッジ級の実記録
uv run python exclusion_audit.py 200 [--stress NAME]             # 確実な除外の規則が正解を落とさないか
uv run python m2_eval.py 300                                     # 単線の特徴（L1〜L3）
uv run python boundary_probe.py --probe                          # 段ごとの呼び出し回数・データ量・時間（M7）
~~~

事前に [corpus](../corpus/README.ja.md) の `fixtures/real/` が必要（Git に含まれる）。生成データのストレス条件は `glitch`、`midstart`、`lowrate`、`jitter`、`freqhop`、`baudhop`。プロファイルは `mixed`（UART/I²C/SPI）、`uartlike`（UART/LIN/DMX512）、`duplex`（SCPI）、`upper`（NMEA/Modbus/テキスト/バイナリ）。

Rust の境界ベンチマーク（M7）。入力は実記録の UART 線から作る。

~~~sh
uv run python export_bench_input.py
cd boundary-bench
cargo build --release -p plugin_wasm --target wasm32-unknown-unknown
cargo build --release -p plugin_cdylib -p bench
./target/release/bench ../../corpus/work/uart_edges.i64 ../../corpus/work/uart_edges.json
~~~

## 段構成のエンジンの流れ

~~~text
エッジ列
 └ 共通の下調べ（survey.py）: アイドル、ビット時間（アクティブ側のラン）、文字長、クロック（局所周期）、
                               バースト、ピン同士の相関（相対位相・2山対応）、CS 様の境界
    └ T1 ピン分類（taxonomy.py）: 静止は除外／クロック／データ／まばら／パルス
       └ T2 まとまり: クロック＋相関の高いピン（部分集合も）＋選択線 ／ 単独ピン
          ├ 同期: SyncBits（サンプリング端・ひげ除去の兄弟）＋RateSegments（注釈）
          │   └ Frames（CS / START・STOP / 間隔 3 倍・1.8 倍の兄弟）→ Words
          │       ├ I²C（9n+1、立ち上がりのみ）  ├ SPI（8n）  ├ RVSWD（53/54/585）→ DMI → RISC-V DM
          │       └ 未知の同期シリアル（既知の区切りで説明できない分だけ）
          ├ 非同期: RateBlocks（ビット時間ごとのブロック）→ ブロックごとの文字長 → Chars（ブレーク付き）
          │   ├ UART → 行・NMEA・Modbus RTU・マーカー（上位の支持を下へ返す）
          │   ├ LIN  ├ DMX512
          │   └ 非同期の組（TX/RX、応答の形）→ SCPI（支持を両方の UART へ返す）
          └ パルス: PulseSymbols（幅の2つの山で短／長、空きで区切る）→ SWIO → DMI → RISC-V DM（RVSWD と共用）
~~~

## 構成

| モジュール | 内容 |
| --- | --- |
| `src/wireskein/_engine/model.py`, `srio.py`, `fixture.py` | エッジ列モデル、`.sr` 読み込み、匿名化フィクスチャ |
| `src/wireskein/_engine/gen.py`, `synth.py` | 連続時間で波形を作り量子化する生成器。シードで決まる評価シナリオとストレス条件 |
| `src/wireskein/_engine/features.py`, `survey.py` | 単線の特徴と、共通の下調べ（周波数・ビット数・ピンの相関） |
| `src/wireskein/_engine/taxonomy.py` | T1 ピン分類、T2 まとまり（T3/T4 の旧版の段も残す） |
| `src/wireskein/_engine/typed.py` | 型付きストリーム（SyncBits、Frames、Words、RateBlocks、Chars、PulseSymbols …）とその構築 |
| `src/wireskein/_engine/staged.py` | 段構成のエンジンと、同期系・非同期系のプラグイン |
| `src/wireskein/_engine/rvswd.py` | RVSWD・SWIO → DMI、RISC-V Debug Module |
| `src/wireskein/_engine/plugins_uartlike.py`, `duplex.py` | LIN・DMX512、TX/RX の組と SCPI |
| `src/wireskein/_engine/kernels.py` | コア候補の一括数値処理（呼び出しごとに計測） |
| `src/wireskein/_engine/exclude.py` | 確実な除外の規則（総当たりのエンジン用） |
| `src/wireskein/_engine/stack.py`, `pipeline.py`, `scoring.py`, `analyzers/` | 総当たりのエンジン、組み合わせ探索、スコア、上位解析器 |
| `boundary-bench/` | コアとプラグインの境界方式の Rust ベンチマーク |
