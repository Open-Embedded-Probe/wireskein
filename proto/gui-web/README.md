# GUI の試作 2: web の技術（使い捨て）

bench/notes/2026-11-25-gui-study.ja.md §7 の試作の 2 つ目（案 E3）。**本番には持って行かない。** 試作 1（proto/gui の egui）と
同じ中身を web の技術で作り、並べて比べるためのもの。分かったことはノートの §10 に書いた。

- 画面: TypeScript + [Solid](https://www.solidjs.com/)（1.9）+ [dockview-core](https://dockview.dev/)（8.9、MIT。ドッキング・
  別の窓・保存）。ビルドは Vite。
- 波形: Canvas の WebGPU（なければ WebGL2）。図形はすべて 1 種類のインスタンス（矩形か太さのある線分、8 個の float）で、
  1 回の draw で描く。文字（注釈・目盛り・線の名前・測定の吹き出し）は上に重ねた 2D の Canvas にブラウザのフォントで描く。
- `wasm/view`（画面のスレッド、152 KB）: proto/gui/view-core を wasm にした薄い口。要約・タイル・注釈の配置から GPU の
  インスタンスの列を作る（`Scene.frame`）。
- `wasm/engine`（Web Worker、2.9 MB）: wireskein-io / wireskein-core / wireskein-analysis（本番の crates を path で読むだけ）を
  wasm にしたもの。ファイルを読む、要約の段 0 を作る、解析（大きな記録は分割読み）、フレームとビットの段（試作 1 と同じく
  クロックから）、検索。
- 本番の workspace（ルートの Cargo.toml）は `proto/` を exclude している。`wasm/` は自分の workspace と Cargo.lock を持つ。

## 作り（どのスレッドで何をするか）

```
画面のスレッド（Solid + dockview + WebGPU）           Web Worker
  view の wasm: Scene                                   読む（load.worker）: ファイル → 線の変化（BigInt64Array を移す）
    線の変化（ファイル）/ 合成の線（その場で作る）         → 線ごとに要約の段 0 を 40 ms ずつ渡す
    要約（段 0 を受け取り、上の段をここで作る）         要約 ×2（load.worker、合成のとき）: 16 本を 2 つで分けて
    注釈の索引（Track::from_parts で受け取る）          解析（analysis.worker）: 解析 → 層ごとに型付き配列で渡す
    frame(): 見えている所 → インスタンスの列             検索もここで（画面は止まらない）
  2D Canvas: 文字、目盛り、マーカー、カーソル
```

画面のスレッドは受け取るだけで、重い処理（ファイル、要約、解析、検索）はすべて Worker。試作 1 の wasm 版のように開くときに
止まることはない（合成のアナログ 400 万サンプルの帯だけは画面のスレッドで作る: 約 0.1 s）。

## 起動の仕方

要るもの: Rust（`rustup target add wasm32-unknown-unknown`）、wasm-bindgen の CLI（Cargo.lock の wasm-bindgen と同じ
0.2.130）、Node.js 20 以上。

```sh
cargo install --locked wasm-bindgen-cli --version 0.2.130
cd proto/gui-web
./build.sh            # wasm 2 つ（cargo -j1、初回 5 分ほど）→ wasm-bindgen → npm ci → vite build（web/dist）
# WASM_BINDGEN=/path/to/wasm-bindgen ./build.sh でも

cd web
node tools/serve.mjs --files ../../../tests/data          # http://127.0.0.1:8080/
# 大きな記録のある所も足すなら: --mount scratch=/path/to/dir （/scratch/... で読める）
```

配信には COOP / COEP の見出し（Cross-Origin-Opener-Policy: same-origin、Cross-Origin-Embedder-Policy: require-corp）を付ける
（`tools/serve.mjs` が付ける。依存なしの node のスクリプト）。今の作りは SharedArrayBuffer を使わないので、見出しがなくても動くが、
メモリの計測（measureUserAgentSpecificMemory）と、将来 wasm のスレッドを使うときに要る。開発中は `npm run dev`（Vite、同じ見出し）。

ブラウザは Chrome / Edge（WebGPU）。Firefox・Safari・WebGPU のない所は WebGL2 で描く（`?gpu=webgl2` で強制）。

### 開くもの（URL の引数）

```
http://127.0.0.1:8080/?file=files/examples/mixed-1.wireskein
http://127.0.0.1:8080/?file=files/p4x-i1/i2c-controller-400k-nack.vcd
http://127.0.0.1:8080/?file=files/rvswd_long_truth/linke_v203_read_flash_4k.sr
http://127.0.0.1:8080/?file=files/i2c_bench/h417_slow/stream/impl-100000000-g0.wireskein     # 3 億 tick
http://127.0.0.1:8080/?synth=1e8                    # 合成の 1 億の変化（16 本、その場で作る）と作り物の取引 50 万行
http://127.0.0.1:8080/?synth=1e9&items=1e6
http://127.0.0.1:8080/?file=scratch/i2c-100k.wireskein   # 合成の I2C 10 万取引（proto/gui の wsk_synth で作る。下）
```

合成の I2C のファイルは試作 1 の道具で作る: `cd proto/gui && cargo run --release -j1 --bin wsk_synth -- /path/to/dir/i2c-100k.wireskein --tx 100000`
（`--mount scratch=/path/to/dir` で配る）。

ほかの引数: `part=K`（区画）、`light=1`、`layout=標準|一覧を大きく|波形 2 つ|保存した名前`、`zoom=Z&at=F`（全体の 1/Z を位置 F に）、
`search=Q`、`select=ROW`、`gpu=webgl2`、`notiles=1`、`popout=1`（一覧を最初からブラウザの別の窓に）、`listwin=1`（独立した一覧の窓）、
`bench=1`（下）。

ファイルは上の「開く」（File API）か、窓にドラッグアンドドロップしても開ける（.wireskein / .sr / VCD）。

### 操作

試作 1 と同じ。

- 波形: ホイールで拡大・縮小（マウスの所を中心に）、ドラッグで移動（離すと慣性）、Shift+ドラッグか右ドラッグで範囲を選んで拡大、
  `F` で全体、`+` / `-`、`←` / `→`、ミニマップ（いちばん上）をクリック・ドラッグで移動、左の線の名前の上のホイールで縦に送る。
- 注釈: クリックで選ぶ（一覧がそこへ）、ダブルクリックでその項目に拡大、マウスを載せると中身と時刻。`N` / `Shift+N` で次 / 前の誤り。
- 測定: デジタルの線にマウスを載せると幅・周期・周波数・デューティ（「測定」パネルにも）。`1` / `2` か右クリックのメニューでマーカー。
- 一覧: クリックで選ぶと「一覧と連動」の波形がそこへ。`↑` / `↓` / PageUp / PageDown。主張・層・列（「列」）を選べる。列の幅は見出しの
  右の端をドラッグ。「波形と連動」を切ると波形での選択に付いて来ない。
  - 「別の窓へ」: そのパネルをブラウザの別の窓に出す（dockview の popout。同じページの状態をそのまま使う。窓を閉じると戻る）。
  - 「独立した窓」: list.html を別の窓で開く。メインの窓とは BroadcastChannel で受け渡す（層のデータを写して受け取り、選択・検索の
    当たり・テーマを送り合う）。メインの窓を読み直しても残り、つなぎ直す。
- 検索: 試作 1 と同じ言葉（`0x48 書き込み`、`addr=0x50 nack`、`エラー`、`疑わしい`…）。解析の Worker で走る。日本語の変換中
  （IME の composition）は検索しない。Enter / Shift+Enter で次 / 前。
- パネル: タブをドラッグで分ける・まとめる・浮かせる（dockview）。「パネル」で同じ種類を足せる（波形 2 つ、一覧 2 つ）。
  「レイアウト」で既定の 3 つの切り替え、名前を付けて保存（localStorage）。
- 上の「ダーク / ライト」（波形の色は GPU のパレットの uniform を差し替えるだけ）。

## 速さを測る（自分の画面で fps を取る）

この機械（作った所）には Chrome が使える GPU がなく、WebGPU は SwiftShader（CPU）で動く。fps は参考にならない。
GPU のある画面では、普通の Chrome で `&bench=1` を付けて開く:

```
http://127.0.0.1:8080/?synth=1e8&bench=1
http://127.0.0.1:8080/?synth=1e9&items=1e6&bench=1
http://127.0.0.1:8080/?file=files/i2c_bench/h417_slow/stream/impl-100000000-g0.wireskein&bench=1
http://127.0.0.1:8080/?file=scratch/i2c-100k.wireskein&bench=1
```

要約と解析が終わるのを待ってから、試作 1 の `--bench` と同じ筋書き（倍率 8 段 × 96 フレーム、1 フレーム 7 px ずつ移動、
それから 1 フレーム 6 % ずつ全体から拡大）を流し、結果を「速さ」パネルの下と console に出す（`window.__bench` にも）。
ブラウザの fps は画面のリフレッシュに縛られる（60 Hz の画面なら 60 が上限）。「waveform CPU」は 1 フレームを作って GPU に
渡すまでの CPU の時間（wasm の分を括弧に）。

ヘッドレスの Chrome で回す道具（puppeteer-core。スクリーンショットとベンチ、Chrome のプロセスのメモリ）:

```sh
cd web
node tools/bench.mjs "http://127.0.0.1:8080/?synth=1e8&bench=1" --bench out.txt --shot out.png
# --size 1600x960、--chrome /path/to/chrome、--headful、--wait-ms N、--type 'SELECTOR=>文字'、--click SELECTOR、--popout out2.png
```

ヘッドレスの Chrome（SwiftShader）はスクリーンショットに WebGPU のキャンバスを写さない（白くなる）。道具は WebGPU の
1 フレームを読み戻してキャンバスの上に重ねてから撮る（`window.__gpuShot()`）。

## Tauri で包む（試作 2b、`tauri/`）

同じ画面（web/dist）を Tauri 2（2.12）の殻に入れたもの。数字と分かったことはノートの §11。Linux は WebKitGTK、Windows は
WebView2（Chromium）で描く。エンジンの動かし方を URL の引数で選べる:

- `engine=wasm`（既定）: 今までどおり wasm の Web Worker。ファイルは殻の `wskfile` の scheme から読む（`--files` / `--mount` の所）。
- `engine=native`: Worker の代わりに殻の Rust の側（ネイティブ）で読む・要約・解析・検索をする。`wasm/engine` と `wasm/view` の
  crate（本番の wireskein-io / -core / -analysis を path で読む）をそのままネイティブでビルドして使う。画面の側は
  `src/tauri.ts` の `NativeWorker` が Worker と同じ口（postMessage / onmessage / terminate）で受け渡す（engine.ts は Worker を
  作る所だけ変えた）。殻の側の「Worker」はキューを持つスレッド。
- `ipc=`（`engine=native` のとき）: 大きなデータの渡し方。`chunk`（既定。下の raw のバイト列を 1 MiB ずつ。§11 で開く間の画面の止まりが
  いちばん短かった）、`raw`（JSON の頭と型付き配列を 1 つのバイト列で、画面は
  コピーせずに型付き配列で見る）、`json`（配列も JSON の数の列）、`fetch`（channel では
  知らせだけ、バイト列は画面が `wskblob` の scheme から fetch）。
- `frameipc=1`（`engine=native` のとき）: 殻の側にもネイティブの Scene を持ち、ベンチの間、同じフレームの図形の列を IPC で
  取り寄せて往復の時間を測る（描くのは今までどおり画面の wasm）。終わりに、何もしていない時に 40 回続けて raw と JSON で測る。
- `report=1`: ページが測った値と console.log を殻が `[report 経過 ms, rss MB] ...` で出す。`exit=1` でベンチの後に終わる。

### Linux で作る・動かす

要るもの: 上の wasm の道具に加えて、WebKitGTK 4.1 の開発用（Debian / Ubuntu: `libwebkit2gtk-4.1-dev libsoup-3.0-dev
libjavascriptcoregtk-4.1-dev libgtk-3-dev librsvg2-dev libssl-dev libxdo-dev`）。Tauri の CLI は要らない（`cargo build` で
画面を埋め込んだ 1 つの実行ファイルができる。`custom-protocol` を既定の feature にした）。

```sh
cd proto/gui-web
./build.sh                                   # wasm と web/dist（殻はこれを埋め込む。画面を変えたら先にこれ）
cd tauri
cargo build --release -j1                    # 初回 -j1 で約 40 分（依存 280 個ほど）、2 回目からは 10 分ほど（thin LTO と埋め込み）
cd ../web                                    # files/ の既定は上の階層の tests/data
../tauri/target/release/wsk-gui-tauri "file=files/examples/mixed-1.wireskein&engine=native"
../tauri/target/release/wsk-gui-tauri --mount scratch=/path/to/dir "file=scratch/i2c-100k.wireskein&engine=native&ipc=raw&bench=1&report=1&exit=1"
```

殻の引数: `--files DIR`、`--mount NAME=DIR`、`--size WxH`（既定 1600x960）、`--log FILE`（報告をファイルにも）、最後にページの
URL の引数（`?` は要らない）。画面のない所では `tauri/bench.sh '引数' out.png`（Xvfb の上で動かし、`SHOT` 秒後に撮る）。

### Windows で作る・動かす（ユーザーの機械で fps を取る）

Windows の Tauri は WebView2（Edge の Chromium）なので WebGPU が使える見込み（Iris Xe）。

1. 要るもの: Rust（MSVC。`rustup default stable-msvc`、`rustup target add wasm32-unknown-unknown`）、Visual Studio Build Tools の
   「C++ によるデスクトップ開発」、Node.js 20 以上、WebView2 ランタイム（Windows 11 は入っている）、
   `cargo install --locked wasm-bindgen-cli --version 0.2.130`。
2. 画面を作る（PowerShell、build.sh と同じこと）:

   ```powershell
   cd proto\gui-web\wasm
   cargo build --release --target wasm32-unknown-unknown
   foreach ($c in 'view','engine') { wasm-bindgen --target web --out-dir ..\web\src\wasm-pkg\$c target\wasm32-unknown-unknown\release\wsk_web_$c.wasm }
   cd ..\web
   npm ci
   npm run build
   ```

3. 殻を作る: `cd ..\tauri; cargo build --release`（`-j1` は要らない。初回は 10〜20 分）。できるのは
   `tauri\target\release\wsk-gui-tauri.exe`（画面を埋め込んだ 1 つの exe）。
4. 流す（`web` の所から。リリースの exe は端末に出さないので `--log` に書く。終わると窓が閉じる）:

   ```powershell
   cd ..\web
   $exe = '..\tauri\target\release\wsk-gui-tauri.exe'
   $B = 'bench=1&report=1&exit=1'
   & $exe --log tauri-bench.txt "file=files/examples/mixed-1.wireskein&$B" | Out-Null
   & $exe --log tauri-bench.txt "file=files/examples/mixed-1.wireskein&engine=native&frameipc=1&$B" | Out-Null
   & $exe --log tauri-bench.txt "synth=1e8&$B" | Out-Null
   & $exe --log tauri-bench.txt "synth=1e8&engine=native&frameipc=1&$B" | Out-Null
   & $exe --log tauri-bench.txt "synth=1e9&items=1e6&engine=native&$B" | Out-Null
   & $exe --log tauri-bench.txt "file=files/i2c_bench/h417_slow/stream/impl-100000000-g0.wireskein&engine=native&$B" | Out-Null
   # 合成の I2C 10 万（proto/gui の wsk_synth で作ったもの）を C:\wsk に置いたなら
   & $exe --log tauri-bench.txt --mount scratch=C:\wsk "file=scratch/i2c-100k.wireskein&engine=native&$B" | Out-Null
   & $exe --log tauri-bench.txt --mount scratch=C:\wsk "file=scratch/i2c-100k.wireskein&engine=native&ipc=json&$B" | Out-Null
   ```

   `tauri-bench.txt` の `bench` の行に、表（倍率ごとの fps・1 フレームの時間・波形の CPU）、IPC（渡した量、画面のスレッドで
   受け取るのにかかった時間、開く間の最も長いフレームの間）、`renderer`（`WebGPU (…)` か `WebGL2`）、メモリが出る。
   Windows のメモリは殻のプロセスだけ（WebView2 のプロセスはタスクマネージャーで見る）。

## WebKitGTK（Tauri の Linux の webview）で開く

殻を作らなくても、同じ WebKitGTK（2.52）の MiniBrowser で同じページを開ける（Tauri はこれと同じ webview を使う）:

```sh
/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1/MiniBrowser --features=+WebGPU "http://127.0.0.1:8080/?file=files/examples/mixed-1.wireskein&report=1&bench=1"
```

`report=1` でページが測った値（描画の種類、最初の絵、ベンチの結果）を `tools/serve.mjs` に送り、配信の側の端末に `[report]` で出る
（console の見えないブラウザのため）。WebKitGTK 2.52 は WebGPU を「unstable」の機能として持ち、`navigator.gpu` はあるが、
この機械ではアダプタが出ず WebGL2 で描いた。

## 書いた量

`wc -l` で数えた（README と lock を除く）。

- wasm/view: 586 行、wasm/engine: 936 行（試作 1 の model.rs の解析の読み方・フレームとビットの段と panels.rs の検索をほぼそのまま移した）。
- web/src の TS / TSX: 3,106 行（波形 733、一覧 362、GPU 329、エンジンとの受け渡し 270、状態 251、パネル 247、上の帯 237、ドック 192、
  Worker 179、ベンチ 110 …）、CSS 402 行。
- web/tools: 249 行（配信、ヘッドレスの道具）。
- view-core に足したのは約 60 行（Track::parts / from_parts、Status::from_u8、AnalogLine::levels / from_parts）。どれも足しただけで、
  試作 1 の使う口は変えていない。
