# GUI の試作 3: iced（使い捨て）

bench/notes/2026-11-25-gui-study.ja.md §7 の試作の 3 つ目（案 E2: 宣言型の Rust の GUI）。**本番には持って行かない。**
試作 1（proto/gui の egui）、試作 2・2b（proto/gui-web の web と Tauri）と同じ中身を [iced](https://iced.rs/) 0.14（MIT）で作り、
並べて比べるためのもの。分かったことはノートの §12 に書いた（候補の比べと iced を選んだ理由もそこ）。

- `app/`: iced 0.14（wgpu）の試作。Elm 型（状態・メッセージ・update・view）。
  - 波形: 自前の iced の widget 1 つ（`wave.rs`）。図形は iced の **shader の primitive**（`gpu.rs`。wgpu のパイプラインを自前で持つ）に
    試作 2 と同じインスタンス（8 個の float: 矩形か太さのある線分、色はパレットの番号）とシェーダ（WGSL）で 1 回の draw。
    図形の列は試作 2 の `Scene.frame` を試作 1 のモデルの上に移したもの（`scene.rs`）。文字は iced の文字（cosmic-text）で上に重ねる。
  - パネル: iced の `pane_grid`（分ける・ドラッグで並べ替え・大きさ・最大化）に、タブを自前で足した（`dock.rs`。パネルを別のパネルの
    真ん中に落とすとタブになる、「分ける」でタブを別のパネルに）。どのパネルも「別の窓へ」で OS の別の窓に（iced の daemon の複数の窓。
    閉じると戻る）。レイアウトは名前を付けて保存（JSON）・切り替え、既定 3 つ。
  - 結果の一覧（`list.rs`）: 仮想の行（見えている行だけ作る、ホイールと縦のスライダーで場所）。列の選択、主張・層の切り替え、連動の入り切り。
  - 解析・ファイル・要約は試作 1 と同じ（`model.rs` は proto/gui/egui/src/model.rs の写し。view-core と本番の crates を path で読むだけ）。
- 本番の workspace（ルートの Cargo.toml）は `proto/` を exclude している。ここは自分の workspace と Cargo.lock を持つ。

## 起動の仕方

`proto/gui-iced` で（初回は依存のビルドに時間がかかる。-j1 で 30〜40 分）:

```sh
cd proto/gui-iced
cargo build --release -j1

cargo run --release -j1 --bin wsk-iced -- ../../tests/data/examples/mixed-1.wireskein
cargo run --release -j1 --bin wsk-iced -- ../../tests/data/p4x-i1/i2c-controller-400k-nack.vcd
cargo run --release -j1 --bin wsk-iced -- ../../tests/data/i2c_bench/h417_slow/stream/impl-100000000-g0.wireskein   # 3 億 tick
cargo run --release -j1 --bin wsk-iced -- --synth 1e8
cargo run --release -j1 --bin wsk-iced -- --synth 1e9 --items 1e6

# 合成の I2C（10 万取引 + UART + PWM、1270 万の変化）: 試作 1 と同じ道具（同じソースをこの workspace でもビルドする）
cargo run --release -j1 --bin wsk_synth -- /path/to/i2c-100k.wireskein --tx 100000
cargo run --release -j1 --bin wsk-iced -- /path/to/i2c-100k.wireskein
```

Windows（PowerShell / cmd）でも同じコマンド（`\` でも `/` でもよい）。描画の API は環境変数 `WGPU_BACKEND`（`dx12` / `vulkan` / `gl`）。

ほかの引数（試作 1 と同じ）: `--part K`、`--light`、`--layout 標準|一覧を大きく|波形 2 つ|保存した名前`、`--zoom Z --at F`、`--search 'Q'`、
`--select ROW`、`--detach-list`（一覧を最初から別の窓に）、`--no-analysis`、`--size WxH`、`--bench OUT.txt`、`--shot OUT.png`、
`--present vsync|no_vsync|immediate|fifo|mailbox`（iced の `ICED_PRESENT_MODE` に渡す）、`--vsync`（ベンチでも垂直同期を切らない）、
`--backend dx12|vulkan|gl`（環境変数 `WGPU_BACKEND` と同じ）。

### 操作

- 波形: ホイールで拡大・縮小（マウスの所を中心に）、Shift+ホイールで横に、ドラッグで移動（離すと慣性）、Shift+ドラッグか右ドラッグで
  範囲を選んで拡大、右クリックでマーカー（M1・M2 を交互に）、`F` で全体、`+` / `-`、`←` / `→`、`1` / `2` でマーカー、
  ミニマップ（いちばん上）をクリック・ドラッグで移動、左の線の名前の上のホイールで縦に送る。上の帯のボタンも同じ。
- 注釈: クリックで選ぶ（一覧がそこへ）、ダブルクリックでその項目に拡大、マウスを載せると中身と時刻。`N` / `Shift+N` で次 / 前の誤り。
- 測定: デジタルの線にマウスを載せると、その所のパルスの幅・周期・周波数・デューティ（吹き出しと「測定」パネル）。マーカー 2 本の差と、
  その間の変化の数。
- 一覧: クリックで選ぶと「一覧と連動」の波形がそこへ。主張・層・列を選べる。「波形と連動」を切ると波形での選択に付いて来ない。
- 検索: 試作 1 と同じ言葉（`0x48 書き込み`、`addr=0x50 nack`、`エラー`、`疑わしい`）。打つたびに検索し、Enter / 「次へ」で順に。
  当たりは一覧の文字色、波形の枠、ミニマップの印。
- パネル: タイトルの帯（⠿ の所）をドラッグして別のパネルの縁へ（分ける）か真ん中へ（タブにまとめる）。境目をドラッグで大きさ。
  「分ける」「別の窓へ」「最大」「×」。上の帯の「パネル」で同じ種類を足せる（波形 2 つ、一覧 2 つ）。「レイアウト」で切り替え、
  名前を入れて「保存」（Linux は `~/.config/wsk-iced/layouts.json`、Windows は `%APPDATA%\wsk-iced\layouts.json`）。
- 上の「ライトに / ダークに」。

## --bench（試作 1 と同じ形）

`--bench OUT.txt` は試作 1 と同じ筋書き（倍率 8 段 × 最大 96 フレーム、移動しながら。その後 6 % ずつの拡大の掃引）で流し、3 つの表を書いて終わる。
垂直同期は切る（`--vsync` で残す）。

1. 倍率ごとの fps（中央値）、1 フレーム p50 / p95、波形 CPU（scene: 図形の列を作る時間）。頭に GPU（アダプタと API）、`# present:`
   （頼んだ表示の仕方〔`--present` か環境の `ICED_PRESENT_MODE`、なければ `--vsync` の有無で vsync / no_vsync〕→ 面が実際に受けるもの、
   垂直同期で止まるか、面の持つ形の一覧。最初の絵の後に同じ窓に 2 つ目の wgpu の面を作って問い合わせる〔`gpu.rs` の `probe`〕。GL では問わない）、
   `# meter:`（メモリと CPU 時間の取り方、この exe を作った時刻）。GPU の名前は iced の system information を最初の絵の後にもう一度頼む
   （起動時の頼みは窓ができる前だと捨てられて `?` になっていた）。それでも来なければ probe のアダプタの名前。
   最初の絵（`first paint`）は「波形が記録を描いたフレームの次のフレームの合図」（そのフレームが表示された後。記録を開いてから、窓と wgpu の
   立ち上がりを含む）。2 行目に「主の窓の最初の view」（窓・wgpu の device・面ができた所）とそこからの最初の絵（試作 1 の最初の絵と同じ区間）、
   iced の窓が開いた知らせの時刻を出す（知らせは最初のフレームより後に来ることがあり、前の版の「窓が開くまで」は負の値になっていた）。
2. **1 フレームの内訳**（ms / フレームの平均）:
   - iced の段（iced が自分のデバッガ comet のために出す区間を、この試作が comet の代わりに受けて足す）: `update`、`view`（view 関数と
     widget の木を作る）、`layout`、`interact`（イベントを widget に配る）、`draw`（widget が描く命令を積む。波形の図形の列もここで作る）、
     `present`（GPU へ: 下の prepare / render と提出・表示を含む）。`prep quad / tri / shader / text`（GPU に載せる準備。text は文字の
     字形の組みとアトラス）、`rend …`（描画の命令）。
   - wgpu の区間（試作 1 と同じ prof.rs）: `acquire`（次の画面を貰う。垂直同期の待ちはここか present）、`submit`、`wgpu present`、`maintain`。
   - アプリが自分で測る所: `main CPU`（メインスレッドの CPU 時間。試作 1 の `proto/gui/egui/src/meter.rs` を同じに使う: Linux は
     `clock_gettime`、Windows は `QueryThreadCycleTime`。RSS も同じ所で、Windows は working set）、`App::update`、`App::view`、`wave draw`（波形の widget の
     draw 全体）、`scene`（その中の図形の列）、`text`（その中の文字を積む所）、`list view`（一覧の行を作る）、`gap`（dt からiced の段を引いた残り。
     イベントループと待ち）。
   - 1 フレームを数える: `instances`（波形の GPU のインスタンス）、`labels`（波形の文字の数）、`shapes`（view-core の図形）。
3. 流しの間の iced の段の合計と、すべての profiling の区間。

### 利用者の機械で流すコマンド（Windows の PowerShell / cmd でそのまま。`proto\gui-iced` で）

2026-11-25 の 2 回目（ノート §14）。試作 1・4 と同じ記録を、API を変えて並べる。

```sh
cd proto\gui-iced
cargo run --release -j1 --bin wsk-iced -- --synth 1e8 --backend vulkan --bench iced-synth1e8-vulkan.txt
cargo run --release -j1 --bin wsk-iced -- --synth 1e8 --backend dx12 --bench iced-synth1e8-dx12.txt
cargo run --release -j1 --bin wsk-iced -- --synth 1e8 --backend vulkan --vsync --bench iced-synth1e8-vulkan-vsync.txt
cargo run --release -j1 --bin wsk-iced -- --synth 1e9 --backend vulkan --bench iced-synth1e9-vulkan.txt
cargo run --release -j1 --bin wsk-iced -- ../../tests/data/examples/mixed-1.wireskein --backend vulkan --bench iced-mixed1-vulkan.txt
cargo run --release -j1 --bin wsk-iced -- ../../tests/data/p4x-i1/i2c-controller-400k-nack.vcd --backend vulkan --bench iced-vcd-vulkan.txt
```

- 1・2 行目は同じものを Vulkan と DX12 で（試作 1 の egui は既定で DX12 を選び 60 fps で頭打ちだった。API の違いかを見る）。3 行目は
  垂直同期のまま（頭の `# present:` が vsync ON になり、画面の Hz で頭打ちになるのが正しい）。
- 結果の頭の `# gpu:` が `?` でないこと、`memory:` の rss と内訳の `main CPU` が数字であること（Windows でも取るようにした）、
  終わりの「main thread CPU over the run」で 2 つの時計が数 % 以内で合うことを見る。
- `--backend` は環境変数 `WGPU_BACKEND` と同じ。

1 回目（2026-11-25、ノート §12.9）のコマンド: `--synth 1e8`、`--synth 1e9`、mixed-1、VCD、h417_slow、`--present immediate`、`--vsync`（API は既定）。
- 窓は流している間そのままにしておく（隠れたり最小化されたりすると描かない）。
- ベンチの間、iced の区間を受けるために 127.0.0.1 の空いたポートを 1 つ開く（外には出ない）。Windows のファイアウォールが尋ねてきたら
  「キャンセル」でよい（同じ機械の中の接続は通る）。
