# GUI の試作 1: egui（使い捨て）

bench/notes/2026-11-25-gui-study.ja.md §7 の試作の 1 つ目。**本番には持って行かない。** 方向を比べるための試作で、
次は同じものを web の技術（TS + WebGPU + wasm）で作って並べる。分かったことはノートの §9 に書いた。

- `view-core/`: 画面に依らない描画の部品（ノート §3）。依存なしの普通の Rust で、wasm にもビルドできる。次の web の試作でも使う。
- `egui/`: その部品を使う egui（eframe 0.37、wgpu、egui_dock 0.22）の試作。

本番の workspace（ルートの Cargo.toml）は `proto/` を exclude している。ここは自分の workspace と Cargo.lock を持ち、
本番の `cargo test --workspace` や CI には入らない。本番の部品（crates/wireskein-io、wireskein-analysis、wireskein-core）は
path の依存で読むだけ。

## 起動の仕方

`proto/gui` で（初回は GUI の依存のビルドに時間がかかる。-j1 で 30〜40 分）:

```sh
cd proto/gui
cargo build --release -j1

# 例のファイル（.wireskein / .sr / VCD）
cargo run --release -j1 --bin wsk-egui -- ../../tests/data/examples/mixed-1.wireskein
cargo run --release -j1 --bin wsk-egui -- ../../tests/data/p4x-i1/i2c-controller-400k-nack.vcd
cargo run --release -j1 --bin wsk-egui -- ../../tests/data/rvswd_long_truth/linke_v203_read_flash_4k.sr

# リポジトリの大きな記録（h417_slow の stream: 3 億 tick、bits で持つ 2 本）
cargo run --release -j1 --bin wsk-egui -- ../../tests/data/i2c_bench/h417_slow/stream/impl-100000000-g0.wireskein

# 合成の記録: 1 億 / 10 億の変化を 16 本に（変化はその場で作る。要約だけメモリに）と、作り物の取引 50 万行
cargo run --release -j1 --bin wsk-egui -- --synth 1e8
cargo run --release -j1 --bin wsk-egui -- --synth 1e9 --items 1e6

# 合成の I2C（10 万取引 + UART + PWM、25 MHz、約 42 秒、1270 万の変化）のファイルを作って開く（本物のエンジンが読む）
cargo run --release -j1 --bin wsk_synth -- /tmp/i2c-100k.wireskein --tx 100000
cargo run --release -j1 --bin wsk-egui -- /tmp/i2c-100k.wireskein
```

ほかの引数: `--part K`（区画）、`--light`、`--layout 標準|一覧を大きく|波形 2 つ`、`--zoom Z --at F`（全体の 1/Z を位置 F に）、
`--search 'Q'`、`--select ROW`、`--detach-list`（一覧を最初から別の窓に）、`--no-analysis`、`--size WxH`、
`--bench OUT.txt`（倍率ごとに 96 フレームずつ流して fps を測り、結果を書いて終わる。vsync を切る）、`--shot OUT.png`（画面の写真を撮って終わる）。

### 操作

- 波形: ホイールで拡大・縮小（マウスの所を中心に）、ドラッグで移動（離すと慣性）、Shift+ドラッグか右ドラッグで範囲を選んで拡大、
  `F` で全体、`+` / `-`、`←` / `→`、ミニマップ（いちばん上）をクリック・ドラッグで移動。
- 注釈: クリックで選ぶ（一覧がそこへ）、ダブルクリックでその項目に拡大、マウスを載せると中身と時刻。`N` / `Shift+N` で次 / 前の誤り。
- 測定: デジタルの線にマウスを載せると、その所のパルスの幅・周期・周波数・デューティ（「測定」パネルにも）。`1` / `2` か右クリックでマーカー。
- 一覧: クリックで選ぶと「一覧と連動」の波形がそこへ動く。主張・層・列を選べる。「波形と連動」を切ると波形での選択に付いて来ない。
  「別の窓へ」で OS の別の窓に（閉じるとドックに戻る）。
- 検索: 言葉はすべて合うもの。`0x48 書き込み`、`addr=0x50 nack`、`エラー`、`疑わしい`、ほかは項目の文字から。合ったものは一覧の文字色、
  波形の枠、ミニマップの印。Enter / 「次へ」で順に。
- パネル: タブをドラッグで分ける・まとめる・別の egui の窓に出す（egui_dock）。「パネル」で同じ種類を足せる（波形 2 つ、一覧 2 つ）。
  「レイアウト」で既定の 3 つの切り替え、名前を付けて保存（eframe の保存先に残る）。
- 上の「ダーク / ライト」。

### web（wasm）

```sh
rustup target add wasm32-unknown-unknown
cargo build --release -j1 --target wasm32-unknown-unknown --bin wsk-egui
cargo install --locked wasm-bindgen-cli --version 0.2.130   # Cargo.lock の wasm-bindgen と同じ版
wasm-bindgen --target web --no-typescript --out-dir web-out target/wasm32-unknown-unknown/release/wsk-egui.wasm
cp egui/web/index.html web-out/ && cd web-out && python3 -m http.server 8000
# http://localhost:8000/ （例 mixed-1 を中に持つ） / ?synth=1e8 / ?light=1
```

web ではファイルを開けない（例を 1 つ中に持つ）。スレッドがないので要約と解析は開くときにその場で作る（1e8 で数秒止まる）。

## 測った数字（2026-11-25、Intel N100 系 4 コア、Xvfb）

**注意: fps は Xvfb の上（DRI3 がなく、wgpu は llvmpipe のソフトウェア描画）で測った。** 画面全体（1600×960）を CPU で
塗るのでどれも 10〜20 fps に張り付き、GPU のある画面の数字ではない。波形を作る CPU の時間（下の「波形 CPU」）が本当の上限の目安。
実機の画面では `--bench` を自分で流すこと。

| 記録 | 変化 | 最初の絵 | 要約を作る | 解析 | 波形 CPU（16 本、1 フレーム） | メモリ（RSS） |
|---|---|---|---|---|---|---|
| mixed-1（例） | 1.4e5 | 35 ms | – | 1.1 s | 0.4 ms | 190 MB |
| h417_slow impl-100000000-g0（3 億 tick） | 1430 | 361 ms（bits を読むのに 201 ms） | 0 s | 0.34 s | 0.2 ms | 179 MB |
| 合成 I2C 10 万取引（ファイル、42 s） | 1.27e7 | 398 ms（読む 367 ms） | 0.12 s | 136 s（分割読み） | 1〜4 ms | 1188 MB |
| 合成 1e8（その場で作る） | 1.0e8 | 225 ms | 1.4 s（2 本並列） | – | 1.3〜2.8 ms | 869 MB |
| 合成 1e9（その場で作る） | 1.0e9 | 235 ms | 15.6 s（2 本並列） | – | 1.4〜3.3 ms | 1372 MB |

- 最初の絵: 要約ができる前から描く（ファイルの記録は変化を二分探索で 1 ピクセルずつ、合成は作れた所まで。残りは「要約 n%」）。
- 合成 I2C の解析: analyze_file を丸ごとで 545 s・3.5 GB（final でも）。分割読み（split の既定）で 122〜136 s・0.5 GB。
  GUI は 200 万の変化を超えたら分割読みにした。それでも 2 分は一覧を待つ。
- view-core のベンチ（`cargo run --release --bin view-bench -- 1e8 1e6`、画面なし）:
  - 要約: 1.3e8 の変化を 1.6 s（1 スレッド、82 M/s）、45 MB（0.35 B / 変化。Vec の伸び代で実際は約 2 倍）。
  - 16 本を 1920 px に読んで図形にする: 全体 1.6 ms、倍率を上げて 0.8 ms → 変化そのものの所は 0.01〜0.1 ms。
  - タイル（256 px、2 の冪の倍率）で 200 フレームの移動: 1 フレーム平均 16 µs（新しいタイルの所で最悪 0.9 ms）。
  - メモリの上の 1300 万の変化: 要約から 1 本の全体を 71 µs、要約なしで 1 ピクセルずつ二分探索だと 1.3 ms。
  - 注釈 100 万項目: 索引 0.8 s、59 MB（文字を 1 本の String にまとめた形。項目ごとの String では 107 MB）。
    全体の配置 1.3 ms、拡大すると数 µs。
  - アナログ 2000 万サンプル: 帯（4 の冪）を 0.45 s、160 MB。読み 0.08 ms。

## 書いた量

- view-core: 約 1,400 行（ベンチ 180 行を含む）。
- egui: 約 2,600 行（app 845、wave 756、model 620、panels 338、list 213、theme・png・合成・解析の道具）。
- 作った時間: 約 2 時間（依存のビルド待ちを除く。依存の初回のビルドは -j1 で約 40 分、wasm は約 20 分）。

## view-core の口（次の web の試作で使う）

```rust
// 見ている範囲
Viewport { t0: f64 /*左端の tick*/, tpp: f64 /*1 px の tick*/, width: f32 }   // fit / zoomed / clamped / x_of / t_of
// デジタルの線
trait EdgeSource { n_ticks, initial, edges_in(t0, t1, cap, out) -> bool, level_before(t) -> Option<u8>, columns_direct, approx_edges }
VecEdges::new(initial, n_ticks, edges)              // メモリの変化
synth::synth_lines(total, n_lines) -> (n, Vec<SynthEdges>)   // その場で作る変化
digital::shift_for(n_ticks, n_edges, per_bucket) -> u32      // 要約の段 0 の幅（2^shift tick）
digital::build_lod(src, shift0) -> Lod               // まとめて作る
digital::build_lod_shared(src, shift0, &RwLock<Lod>, &AtomicBool)  // 少しずつ作り、作れた所から読める
digital::read_view(src, &lod, &vp, max_exact, &mut LineView)  // 1 px ごとの列（変化の数、始めと終わりの水準）か変化そのもの
shapes::digital_segs(&LineView, &vp, &mut Vec<Seg>)  // Run / Edge / Busy{density} / Unknown
digital::around(src, &lod, t) -> (level, prev, next, next2)   // カーソルの測定
// タイル
tiles::tiles_for(&vp, n) -> Vec<TileKey>; tile_viewport(key); placement(key, &vp) -> (offset, scale); TileCache
// アナログ
AnalogLine::new(name, unit, t0, dt, values); read_view(&vp) -> Points | Bands(min, max, mean)
// 注釈
ann::Track::new(name, Vec<(Item{s, e, status, id}, full, short)>)
track.range(t0, t1) / hit(t) / index_at(t) / next_bad / prev_bad / layout(&vp, char_px, &mut Vec<AnnShape>)
AnnShape::Box{x0, x1, idx, fit: Full|Short|None, status} | Busy{x0, x1, n, status}
```

すべて px は view の左端から。色や文字の描き方は持たない（画面の側）。wasm には `cargo build --target wasm32-unknown-unknown -p view-core` で通る。
