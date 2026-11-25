# GUI の試作 4: Makepad（使い捨て）

bench/notes/2026-11-25-gui-study.ja.md §7 の試作の 4 つ目（案 E2: 宣言型の Rust の GUI のうち、§12.1 で「次に試すなら」とした Makepad）。
**本番には持って行かない。** 試作 1（proto/gui の egui）、試作 2・2b（proto/gui-web の web と Tauri）、試作 3（proto/gui-iced の iced）と
同じ中身を [Makepad](https://github.com/makepad/makepad) 1.0（crates.io の makepad-widgets 1.0.0、MIT / Apache-2.0）で作り、並べて比べるためのもの。
分かったことはノートの §13 に書いた。

- `app/src/`:
  - `wave.rs`: 波形の widget。図形の列は試作 3 の `scene.rs`（そのまま写した）が作るインスタンス（8 個の float）。それを **Makepad の
    シェーダ言語で書いた `DrawSeg`**（DrawQuad を継ぐ。頂点シェーダが矩形か太さのある線分を置く）に `begin_many_instances` で 1 つの
    draw call にまとめて渡す。文字は Makepad の DrawText（注釈・目盛り・線の名前）。吹き出しは別の draw call（角丸は SDF）。
  - `list.rs`: 結果の一覧。Makepad の **PortalList**（仮想の行、慣性つきのスクロール）に、セルを DrawText で描く自前の行（`ListRow`）。
    主張・層は DropDown、列は CheckBox、連動の入り切り。
  - `panels.rs`: 検索（TextInput と、当たりの PortalList）、測定・主張・速さ（自前の文字の widget と Button / CheckBox）。
  - `app.rs`: Root・Window・上の帯・**Dock**（パネルの種類をテンプレートに）・状態の行。レイアウト（既定 3 つと保存した JSON）を Dock の
    `load_state` に入れる。どのパネルも別の OS の窓へ（Window をテンプレートから作る。閉じると Dock に戻る）。ダーク / ライト。
  - `state.rs`: 状態（記録・解析・選択・マーカー・検索・パネルごとの状態）。widget には Makepad の `Scope` で渡す。
  - `model.rs` / `scene.rs`: 試作 3 の写し（view-core と本番の crates を path で読むだけ）。`bench.rs`: `--bench`。
- 本番の workspace（ルートの Cargo.toml）は `proto/` を exclude している。ここは自分の workspace と Cargo.lock を持つ。

## 起動の仕方

`proto/gui-makepad` で（初回は依存のビルドに -j1 で 15〜20 分ほど。Makepad は依存が少なく、Cargo.lock で 190 個ほど）:

```sh
cd proto/gui-makepad
cargo build --release -j1

cargo run --release -j1 --bin wsk-makepad -- ../../tests/data/examples/mixed-1.wireskein
cargo run --release -j1 --bin wsk-makepad -- ../../tests/data/p4x-i1/i2c-controller-400k-nack.vcd
cargo run --release -j1 --bin wsk-makepad -- ../../tests/data/i2c_bench/h417_slow/stream/impl-100000000-g0.wireskein   # 3 億 tick
cargo run --release -j1 --bin wsk-makepad -- --synth 1e8
cargo run --release -j1 --bin wsk-makepad -- --synth 1e9 --items 1e6

# 合成の I2C（10 万取引 + UART + PWM、1270 万の変化）: 試作 1 と同じ道具（同じソースをこの workspace でもビルドする）
cargo run --release -j1 --bin wsk_synth -- /path/to/i2c-100k.wireskein --tx 100000
cargo run --release -j1 --bin wsk-makepad -- /path/to/i2c-100k.wireskein
```

Windows（PowerShell / cmd）でも同じコマンド（`\` でも `/` でもよい）。描画は Makepad が OS ごとに持つもの（Linux は OpenGL（X11、EGL）、
Windows は Direct3D 11、macOS は Metal）。wgpu は使わない。

**注意（Makepad 1.0 の作り）:** フォントなどの資源（`crate://…`）は実行時にソースの置き場（cargo の registry の makepad-widgets など）から
読む。`cargo run` で動かす分には問題ないが、exe だけを別の機械に持って行くと文字が出ない（配るには `cargo makepad` の包む道が要る）。
また Makepad は既定でソースの `.rs` を 100 ms ごとに読み直す（live design の書き換えを拾う）スレッドを動かす。

ほかの引数（試作 1・3 と同じもの）: `--part K`、`--light`、`--layout 標準|一覧を大きく|波形 2 つ|保存した名前`、`--zoom Z --at F`、
`--search 'Q'`、`--select ROW`、`--detach-list`（一覧を最初から別の窓に）、`--no-analysis`、`--size WxH`、`--bench OUT.txt`、
`--exit-after S`（S 秒で終わる。撮るとき用）。`--shot` はない（Linux では `run.sh` が Xvfb の画面を xwd で撮る: `XSHOT=out.png SHOT=秒`）。

### 操作

- 波形: ホイールで拡大・縮小（マウスの所を中心に）、Shift+ホイールで横に、ドラッグで移動（離すと慣性）、Shift+ドラッグか右ドラッグで
  範囲を選んで拡大、右クリックでマーカー（M1・M2 を交互に）、`F` で全体、`+`（`=`）/ `-`、`←` / `→`、`1` / `2` でマーカー
  （キーは波形を一度クリックしてから）、ミニマップ（いちばん上）をクリック・ドラッグで移動、左の線の名前の上のホイールで縦に送る。
  上の帯のボタンも同じ。
- 注釈: クリックで選ぶ（一覧がそこへ）、ダブルクリックでその項目に拡大、マウスを載せると中身と時刻。`N` / `Shift+N` で次 / 前の誤り。
- 測定: デジタルの線にマウスを載せると、その所のパルスの幅・周期・周波数・デューティ（吹き出しと「測定」パネル）。マーカー 2 本の差と、
  その間の変化の数。
- 一覧: クリックで選ぶと「一覧と連動」の波形がそこへ（ダブルクリックで拡大）。主張・層（DropDown）・列（CheckBox）を選べる。
  「波形と連動」を切ると波形での選択に付いて来ない。スクロールは PortalList の慣性つき。
- 検索: 試作 1 と同じ言葉（`0x48 書き込み`、`addr=0x50 nack`、`エラー`、`疑わしい`）。打つたびに検索し、Enter / 「次」で順に。
  当たりは一覧の文字色、波形の枠、ミニマップの印。
- パネル: Makepad の Dock。タブをドラッグして別のグループの縁（分ける）か真ん中・タブの帯（まとめる）へ。境目をドラッグで大きさ。
  タブの × で閉じる。**Linux（X11）では Makepad 1.0 にドラッグアンドドロップがない**ので、タブを掴んで動かし、離した所に移す
  （落とす先の印は出ない）。上の帯の「パネルを足す」で同じ種類を足せる（波形 2 つ、一覧 2 つ）、「別の窓へ」でその種類のパネルを
  OS の別の窓に（波形のパネルは帯の「別の窓へ」でも）。別の窓を閉じると Dock に戻る。「レイアウト」で切り替え、名前を入れて「保存」
  （Linux は `~/.config/wsk-makepad/layouts.json`、Windows は `%APPDATA%\wsk-makepad\layouts.json`）。
- 上の「ライトに / ダークに」。Makepad の部品のテーマも替わる（DSL を読み直す）。

## --bench（試作 1・3 と同じ形）

`--bench OUT.txt` は試作 1・3 と同じ筋書き（倍率 8 段 × 最大 96 フレーム、移動しながら。その後 6 % ずつの拡大の掃引）で流し、
2 つの表を書いて終わる。

1. 倍率ごとの fps（中央値）、1 フレーム p50 / p95、波形 CPU（scene: 図形の列を作る時間）。頭に描画の API と窓の大きさ。
2. **1 フレームの内訳**（ms / フレームの平均）。Makepad には iced の beacon や egui の profiling のような区間がないので、アプリが自分で測る:
   - `main CPU`: メインスレッドの CPU 時間（試作 1 の `proto/gui/egui/src/meter.rs` を同じに使う: Linux は `clock_gettime`、Windows は
     `QueryThreadCycleTime` を起動時に測った速さで時間に直す。RSS も同じ所で、Windows は working set）。頭の `# meter:` に取り方と
     この exe を作った時刻、終わりに流しの全体のメインスレッドの CPU を 2 つの時計（Windows は `GetThreadTimes` と並べる）で出す。
   - `events`: アプリが 1 フレームの間に扱ったイベントの合計（下の 3 つとマウスなど）。`frame loop`（NextFrame: 解析を見に行く・慣性・ベンチ）、
     `draw`（Event::Draw: すべての widget が draw list を作る）、その中の `wave draw`（波形の widget 全体）、`scene`（図形の列）、
     `instances push`（図形を DrawSeg のインスタンスに詰める）、`text`（文字を積む）、`list draw`（一覧のパネル）。
   - `platform CPU` = main CPU − events: Makepad の下の層がメインスレッドで使う CPU（draw list を GPU の API に渡す、swap、X11）。
   - `gap` = dt − events: platform CPU に加えて GPU の待ち・次のフレームまでの待ち。
   - 1 フレームを数える: `instances`（DrawSeg のインスタンス）、`labels`（注釈の文字の数）、`shapes`（view-core の図形）。
   - 垂直同期: Makepad 1.0 は Windows（D3D11）では常に切っている（`Present(0, 0)`）。Linux の EGL は設定していない（ドライバの既定）。
     切り替える引数はない。結果の頭の `# present:` にこれを書く。

### 利用者の機械で流すコマンド（Windows の PowerShell / cmd でそのまま。`proto\gui-makepad` で）

```sh
cd proto\gui-makepad
cargo run --release -j1 --bin wsk-makepad -- --synth 1e8 --bench makepad-synth1e8.txt
cargo run --release -j1 --bin wsk-makepad -- --synth 1e9 --items 1e6 --bench makepad-synth1e9.txt
cargo run --release -j1 --bin wsk-makepad -- ../../tests/data/examples/mixed-1.wireskein --bench makepad-mixed1.txt
cargo run --release -j1 --bin wsk-makepad -- ../../tests/data/p4x-i1/i2c-controller-400k-nack.vcd --bench makepad-vcd.txt
cargo run --release -j1 --bin wsk-makepad -- ../../tests/data/i2c_bench/h417_slow/stream/impl-100000000-g0.wireskein --bench makepad-h417.txt
```

- 1〜5 行目が試作 1・3 の表と並べるもの。2026-11-25 の 2 回目（ノート §14）から Windows でも `main CPU`・`platform CPU`・rss が出る
  （その前の exe では NaN）。終わりの「main thread CPU over the run」で 2 つの時計が数 % 以内で合うことを見る。
- 窓は流している間そのままにしておく（隠れたり最小化されたりすると描かない）。
- Windows では MSVC のツールチェーン（Visual Studio の C++ Build Tools）が要る（Makepad の Windows の層が `windows` crate を使う）。

## 測った数字（2026-11-25、この機械: 4 コア、Xvfb + Mesa llvmpipe の OpenGL。fps は参考にならない）

詳しくはノートの §13.3。混んでいなかった回の値:

| 記録 | 最初の絵 | 要約 | 波形 CPU（scene の CPU 時間） | 1 フレームの CPU（メインスレッド） | うち Makepad の draw / 下の層 | RSS |
|---|---|---|---|---|---|---|
| mixed-1 | 50 ms | 0.00 s | 0.04〜0.07 ms | 4.1〜6.2 ms | 0.8〜1.0 / 3.1〜5.0 ms | 0.58 GB |
| VCD（28 変化） | 43 ms | 0 | 0.03 ms | 3.8〜4.2 ms | 0.7〜0.8 / 2.7〜3.0 ms | 0.57 GB |
| h417_slow（3 億 tick） | 258 ms（読む 200 ms） | 0 | 0.03〜0.05 ms | 4.5〜5.6 ms | 1.0〜1.3 / 1.1〜4.0 ms | 0.58 GB |
| 合成 1e8 | 465 ms | 5.5 s | 1.1〜2.5 ms | 6.6〜25 ms | 2.3〜4.3 / 1〜16 ms | 1.22 GB |
| 合成 1e9 | 316 ms | 14.4 s | 1.4〜2.8 ms | 6.4〜25 ms | 2.5〜4.3 / 3〜14 ms | 2.23 GB |
| 合成 I2C 10 万（混んだ回） | 2.1 s（読む 1.6 s） | 0.25 s | – | 5.3〜10.7 ms | – | 1.56 GB（解析 289 s、370 万行） |

- 「下の層」= メインスレッドの CPU − アプリのイベント処理（draw list を GL に渡す・swap・X11）。llvmpipe ではインスタンスの数（2 万前後）に
  比例して大きい。GPU のある機械では別物なので、利用者の機械の `--bench` で決める。
- 小さな記録でも RSS 0.57 GB の大半（415 MB）は名前のない対応づけ（llvmpipe の GL のテクスチャ・バッファと見ている）。
