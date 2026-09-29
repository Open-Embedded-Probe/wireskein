# WireSkein — プラグインの記述言語の調査

2026-09-28 作成。WireSkein のコアが型付きストリーム（SyncBits＋Frames、Words、Chars＋ブレーク、PulseSymbols、時刻付きバイト列、トランザクション）を渡し、プラグインが区切り方・解釈を決めて「復号結果＋スコアの指標」を返す、という形（[プロトタイプ README](../research/README.ja.md) の「段構成のエンジンの流れ」、[優先度の調査](protocol-priority.ja.md) 5〜6 節）を前提に、プラグインを**何で書かせるか**を整理する。

言語ごとの速度・メモリ・呼び出し費用の実測は別のベンチマーク（`research/plugin-lang-bench/`、対象は QuickJS・Lua 5.4・LuaJIT・Rhai・Starlark・CPython・WASM）が担う。この文書は既存ツールの先例と候補の見取り図、推奨の組み合わせ、ベンチマークで確かめるべき点を扱う。

表記の約束:

- 一次資料で確かめていないものは **（未確認）**、この文書の解釈は **（解釈）** と明記する。
- 版・日付は 2026-09 時点の調べ。変わりやすいので採用時に再確認する。

## 1. 評価の軸

プラグインは主に AI エージェントが書く。人が書く場合より「書き間違えにくさ」と「誤りの報告の質」の比重が上がる（**解釈**）。

| 軸 | 見る点 |
| --- | --- |
| A 書きやすさ・曖昧さの少なさ | 文法の素直さ、学習データの量（AI が既に知っているか）、落とし穴（1 始まりの添字、整数と浮動小数の混同、暗黙の型変換、ビット演算の有無） |
| B データの受け渡し | 大きな配列の零複製、型付きの 2 進データ（u8/u16/u32/u64 の配列）、ビット演算、**64 bit 整数**（時刻・サンプル番号） |
| C 速さ・メモリ | 1 要素ごとの処理の速さ、ホスト↔スクリプトの呼び出し費用、記録（レコード）を大量に作るときの割り当て |
| D 組み込み・配布 | Windows/macOS/Linux で追加の実行時が要るか、ビルドの手間、ブラウザ（WASM 化した GUI）で動くか |
| E 隔離 | ファイル・ネットワークへの到達を断てるか、無限ループ・メモリ暴走を止められるか（燃料・時間・上限） |
| F デバッグ | 誤りの文言、ファイル名・行・スタック、型検査の事前実行、AI が誤りを読んで自己修正できるか |

## 2. 既存ツールの先例

### 2.1 ロジックアナライザ系

| ツール | 記述手段 | 積み重ね | うまくいった点 | 問題点 |
| --- | --- | --- | --- | --- |
| sigrok / PulseView | Python 3 の解析器（`pd.py`）。`inputs`/`outputs`・`annotations`・`annotation_rows` をクラス属性で宣言。API v3 で `self.wait({0: 'r'}, {1: 'e'})` のような**条件待ち**を導入し、標本の走査は C 側で行う（[Queries](https://sigrok.org/wiki/Protocol_decoder_API/Queries)） | 任意（`inputs` の型名で接続）。実際の深さは 2〜3（[優先度 5.1 節](protocol-priority.ja.md)） | 宣言部（メタデータ）と処理部の分離。条件待ちで「1 標本ずつ Python」を避けた。出力を注釈・Python オブジェクト・2 進・メタの 4 種に分けた | Python の版の制約と組み込みの配布が重い（対応版は長く 3.2〜3.5 に留まった。[libsigrokdecode](https://github.com/sigrokproject/libsigrokdecode)。現行の上限は（未確認））。`OUTPUT_PYTHON` の型の約束が docstring だけ |
| Saleae Logic 2 | LLA は C++ SDK（平台ごとにビルドした共有ライブラリ）、HLA は同梱の Python 3.8（[HLA](https://www.saleae.com/support/extensions-api/extensions/high-level-analyzer-extensions)）。`extension.json`＋`readme.md`＋Python ファイル | 固定 2 段（LLA → HLA）。HLA を積めない | HLA は `decode(frame) -> AnalyzerFrame` の 1 関数で、書き始めが易しい。FrameV2（型名＋キーと値）がそのまま表の列になる。拡張の市場（共有 HLA）が育った | LLA の C++ は 3 平台分ビルドが要り、共有がほぼ HLA に偏った（**解釈**）。HLA は 1 入力で状態機械を手書きする。Python 3.8 は既に保守終了 |

### 2.2 パケット・2 進形式の記述

| ツール | 記述手段 | うまくいった点 | 問題点・教訓 |
| --- | --- | --- | --- |
| Wireshark | C の dissector と Lua dissector。4.4 系から Lua 5.3/5.4 に移り 5.1/5.2 を削除、配布物は Lua 5.4.6 同梱（[4.4 リリースノート](https://www.wireshark.org/docs/relnotes/wireshark-4.4.6.html)）。欄は `ProtoField` で事前登録し、dissector table・heuristic で接続 | **欄の事前登録**により表示フィルタ・列・色分けが全プラグインで共通に効く。Lua は配布が 1 ファイルで済む | Lua は C より数倍〜数十倍遅い（読み込み時間が約 50 倍の報告と LuaJIT での改善例: [dev ML](https://lists.wireshark.org/archives/wireshark-dev/201904/msg00013.html)、[LuaJIT 化の記事](https://prontog.wordpress.com/2018/12/28/boost-your-lua-wireshark-dissector/)）。Lua の版替えで既存プラグインが壊れた（5.1→5.4 の整数・ビット演算の差）（**解釈**）。宣言的な記述の要望は昔からある（[dev ML 2017](https://www.wireshark.org/lists/wireshark-dev/201704/msg00102.html)）。ASN.1・pidl からの C 生成は本体で実用 |
| Kaitai Struct | YAML の `.ksy` → C++・C#・Go・Java・JS・Lua・Nim・Perl・PHP・Python・Ruby・Rust の読み取り器を生成。v0.11（2025-09）で Rust 対応、Java/Python で直列化（[v0.11](https://kaitai.io/news/2025/09/07/kaitai-struct-v0.11-released.html)） | 1 つの記述から多言語。Web IDE で部分木と誤り位置が見える。`.ksy` → Wireshark Lua の変換器もある（[kaitai-to-wireshark](https://github.com/joushx/kaitai-to-wireshark)） | バイト列前提で、ビット単位の欄（`b9` 等）は書けるが**時刻付きのストリームや区切りの推定**は範囲外。式言語が独自で、複雑な条件は書きにくい |
| 010 Editor Binary Templates | C に似た構文の「実行される宣言」。`struct` の中で `if`・`while` が書け、宣言した変数が即座にファイルの欄になる | 宣言と手続きの混在が自然で、書き手に易しい。欄が自動で木・色になる | 独自処理系で他ツールに持ち出せない。速度は重視されない |
| Construct（Python） | `Struct("addr" / BitsInteger(7), "rw" / Flag)` のような組み合わせ子。解析と生成の両方向 | 小さな部品の組み合わせで双方向。ビット単位の構造（`BitStruct`）が書ける | Python の速度。誤りの文言が組み合わせ子の内部を指しがち（**解釈**） |
| Scapy | Python クラスの `fields_desc` に欄を並べ、`bind_layers` で積む | 欄の宣言＋層の結合という形が簡潔。表示・生成・照合が同じ宣言から出る | 実行時の速度。宣言で書けない部分は `guess_payload_class` 等の上書きになり、規則が散る |
| Zeek / Spicy | 専用 DSL の `unit`（型付きの欄、`&size`・`&until`・`&convert`、正規表現、`bitfield(8)`、フック `on %done`）→ C++ 生成。`spicyc`・`spicy-driver`・独自ホストへの組み込み（[Spicy](https://docs.zeek.org/projects/spicy)、[host applications](https://docs.zeek.org/projects/spicy/en/latest/host-applications.html)） | 「欄の宣言＋必要な所だけ手続き」をひとつの言語に入れ、**安全（境界検査）で速い**。入力が途中で切れても再開できる（増分解析）。誤りを捨てず回復（`&synchronize`） | C++ コンパイラが実行時に要り、配布が重い。GUI 付き製品への組み込み例は少ない（**解釈**） |
| P4 | パーサを**状態機械**（`state parse_eth { extract(hdr); transition select(hdr.type) {...} }`）で書き、ハードウェア・ソフトウェアの対象へ生成 | 区切り（フレーミング）を状態と遷移で書くと、ループのない有限の記述になり検証しやすい | パケット先頭からの固定的な解析向け。推定・曖昧さは扱わない |
| protobuf・DFDL 系 | スキーマから読み書きを生成（DFDL は XML Schema に区切り・長さの注記を付ける） | スキーマが「次の段への型の約束」になる | 物理層の区切りには合わない。**段と段の間の型の約束**の表現として参考になる（**解釈**） |

### 2.3 解析ツールのプラグイン

| ツール | 記述手段 | 教訓 |
| --- | --- | --- |
| IDA Pro | IDC（独自）、IDAPython、C++ SDK | 独自言語（IDC）は廃れ、利用者は Python に集まった。C++ SDK は版ごとの ABI 追随が負担 |
| Binary Ninja | Python・C++・Rust の API（同じ C ABI の上の束縛）。ヘッドレス実行可 | **C ABI を 1 つ定め、言語束縛をその上に作る**と、多言語でも API がずれない（**解釈**）。Kaitai の組み込み例もある（[Vector35/kaitai](https://github.com/Vector35/kaitai)） |

### 2.4 先例から引き出す教訓

1. **宣言（メタデータ）と処理を分ける。** sigrok の `inputs`/`outputs`/`annotations`、Wireshark の `ProtoField`、Saleae の `extension.json`。コアは処理を走らせる前に、入力の型・出力の欄・注釈行を知る必要がある。WireSkein でも欄の役割（Header、Address、CRC …）を宣言させる（[優先度 6 節](protocol-priority.ja.md)）。
2. **1 標本ずつのループをスクリプトに書かせない。** sigrok API v3 の `wait()` が示すとおり、走査はコアのカーネルに置き、プラグインは SyncBits・Words・Chars のような**圧縮済みの型**を受ける。WireSkein の段構成はこれを既に満たしている。
3. **欄の宣言＋例外だけ手続き**が最も書きやすい。010 Editor、Spicy、Scapy の共通点。純宣言（Kaitai、P4）は定型に強いが、I²C の 10 bit アドレスや SPI の D/C# 線のような例外で詰まる。
4. **配布が重い方式は共有が育たない。** Saleae の LLA（C++、平台別ビルド）と HLA（Python 1 ファイル）の差、Spicy の C++ コンパイラ依存。1 ファイルで全平台に配れる形が要る。
5. **言語の版はコアが固定して同梱する。** sigrok の Python 版の制約、Wireshark の Lua 5.1→5.4 の移行、Saleae の Python 3.8 の据え置き。版上げは既存プラグインを壊すので、API に版番号を付ける。
6. **型の約束を文書でなく機械可読にする。** sigrok の `OUTPUT_PYTHON` は docstring 頼み、Saleae の FrameV2 はキーと値のみ。AI が書く前提では、入出力の型定義（WIT、スキーマ）が AI への仕様書を兼ねる（**解釈**）。

## 3. 候補の言語・方式

### 3.1 ベンチマーク対象外の候補

| 候補 | 成熟度・保守 | Rust からの組み込み | 2 進データ・64 bit | 隔離 | 誤りの報告 | 平台 | AI が書く適性 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Luau | Roblox が開発、MIT。週次に近い頻度で公開（[GitHub](https://github.com/luau-lang/luau)） | C++ 製。Rust からは mlua の `luau` 機能で使える | `buffer` 型（固定長バイト列、`readu32` 等）あり。数値は倍精度のみで **64 bit 整数が無い**（未確認：整数型の追加提案の状況） | `luaL_sandbox`・`safeenv`、割り込みコールバックで停止（[sandbox](https://luau.org/sandbox/)） | 行番号付き。段階的な型検査（`--!strict`）を事前に回せる | 全平台、WASM 化の実績あり | 良。Lua の知識が流用でき、型注釈で誤りを事前に拾える。64 bit 整数の欠如は時刻の扱いで致命的になりうる |
| TypeScript（型を剥がして QuickJS 等で実行） | QuickJS 自体はベンチマーク対象。型の除去は oxc・swc（[oxc transformer](https://oxc.rs/docs/guide/usage/transformer)）。QuickJS＋oxc の組み合わせ例あり（[tack_runtime_quickjs](https://docs.rs/tack-runtime-quickjs/latest/tack_runtime_quickjs/)） | rquickjs＋oxc を Rust に同梱。`tsc` 相当の型検査は別途（Node か tsgo が要る）（**解釈**） | `Uint8Array`/`Uint16Array`/`DataView`、`BigInt`（64 bit、ただし遅い） | 実行時は QuickJS の制限（メモリ上限、割り込み） | ソースマップで行を戻す手間がある | 全平台・ブラウザ | 最良級。学習量が最も多い。型定義（`.d.ts`）を API 仕様として AI に渡せる |
| Rune | 0.13 系、rune-rs で継続的に更新（[rune 0.13](https://rune-rs.github.io/posts/rune-0-13-0/)） | Rust ネイティブ（純 Rust） | `Vec<u8>`・`i64` あり。型付き配列の零複製は（未確認） | 命令数の上限等（未確認） | 行・範囲付きの診断（Rust 風） | Rust が動く所 | 中。「型のない Rust」で学習量が少なく、AI が Rust の構文を混ぜる恐れ（**解釈**） |
| Koto | 0.16.1（2026-01）（[docs.rs](https://docs.rs/crate/koto/latest)） | 純 Rust | 整数・浮動小数を区別。2 進配列は（未確認） | 限定的（未確認） | 行付き | Rust が動く所 | 低〜中。学習量が少ない |
| Roto | NLnet Labs、v0.11.0（2026-05）。静的型・Cranelift で JIT（[GitHub](https://github.com/NLnetLabs/roto)、[1 年の記事](https://blog.nlnetlabs.nl/one-year-of-roto-the-compiled-scripting-language-for-rust/)） | 純 Rust。Rust の型をそのまま登録 | 型付きで速い。可変長配列・ループの表現力は発展途上（未確認） | 言語が小さく外界に触れない | 事前の型検査 | Cranelift の対象（x86-64・aarch64）。ブラウザ不可 | 中。フィルタ用の小言語で、状態機械を書く汎用性は不足（**解釈**） |
| Gluon | 0.18.4 が最新（日付は未確認、更新は少ない）（[releases](https://github.com/gluon-lang/gluon/releases)） | 純 Rust | 静的型 | — | — | — | 低。保守が細い |
| Mun | v0.5.0（プレリリース、12 月 28 日、年は未確認）（[releases](https://github.com/mun-lang/mun/releases)） | LLVM に依存、ホットリロード | 静的型 | 無し（ネイティブ実行） | — | LLVM の配布が重い | 低 |
| Nickel / Dhall / CUE | 設定言語。CUE は Go、Nickel は Rust、Dhall は Haskell/Rust | 評価器として組み込める | 処理の記述には向かない | 全域的（停止する）言語が多い | 型・制約の誤りは丁寧 | — | **3.3 節のスキーマの検証**にだけ使える。処理本体には不適 |
| WASM Component Model＋WIT | WASI 0.3 が安定目標、ツール群（wit-bindgen、wasmtime、jco）が追随中（[road to 1.0](https://bytecodealliance.org/articles/the-road-to-component-model-1-0)） | wasmtime（ベンチ対象）。Rust・C/C++・Go・Python（componentize-py）・JS（StarlingMonkey／QuickJS 系の componentize-qjs）から作れる | WIT の `list<u8>` 等は**呼び出し境界で複製**される（canonical ABI）。零複製には共有メモリか資源（resource）越しの読み出しが要る（**解釈**） | 最強。能力を渡さなければ何も触れない。燃料・epoch で停止 | 言語ごとの差が大きい。Rust 製ならスタックは出るが行情報は DWARF 次第 | 全平台・ブラウザ（jco で ES モジュールへ） | 型定義（WIT）が仕様書を兼ねる点は最良。ビルド手順が 1 段増える |
| Extism | WASM の上のプラグイン枠組み。Rust・Python・JS 等のホスト SDK と PDK（[extism](https://github.com/extism/extism)） | Rust ホスト SDK | バイト列の入出力が基本（型は自前で直列化） | 上限・時間制限付き | — | 全平台・ブラウザ | 中。Component Model より簡単だが型が弱い |
| Spicy 風 DSL（自作） | — | 自作の解釈器か、Rust への生成 | 必要な型だけ持てる | 全域的に作れる | 自作次第 | 全平台 | 学習量がゼロなので、**小さく・例を豊富に**しない限り AI は誤る（**解釈**） |

### 3.2 ベンチマーク対象の言語の位置づけ（定性のみ）

| 言語 | 定性的な見込み | ベンチマークで確かめる点 |
| --- | --- | --- |
| QuickJS（rquickjs） | 学習量が最多。型付き配列で零複製の見込み。64 bit は BigInt で遅い | 外部 ArrayBuffer の零複製、`Uint32Array` 経由の時刻（u64 を 2 語に割る必要）、レコード生成の費用 |
| Lua 5.4 | 64 bit 整数・ビット演算あり。1 始まりの添字が落とし穴 | userdata 経由の走査費用、スタックトレースの質 |
| LuaJIT | 最速級。ただし Lua 5.1 系で 64 bit 整数は FFI の `int64_t` 頼み。上流の公開版が長く出ていない（未確認） | FFI の零複製と、FFI を使うと隔離が破れる点 |
| Rhai | 純 Rust で組み込みが最も楽。遅い見込み | 走査・レコード生成の速さ、誤りの文言 |
| Starlark | 全域的（再帰・無限ループ無し）で隔離は強い。ビット演算・2 進配列が弱い | 大きな配列を渡す方法、ループ制限下で I²C が書けるか |
| CPython（pyo3） | AI の適性は最良。配布（Python 同梱）と隔離が弱い。numpy の memoryview で零複製 | 起動・メモリ、GIL、配布物の大きさ |
| WASM（wasmtime） | 速さと隔離は最良。書き手はビルドを要する | 線形メモリへの複製費用、境界呼び出しの費用 |

### 3.3 WireSkein 専用の宣言的なフレーミング記述

段構成のエンジンでは、多くのプラグインの仕事は「Frames の中を n bit の語に切り、欄に名前を付け、規則に合うかを数える」ことである（I²C の 9n+1、SPI の 8n、RVSWD の 53/85 等）。これを**コード無し**で書ける記述を用意する。

~~~toml
[plugin]
name = "i2c"
api = 1
input = "sync.frames"          # SyncBits + Frames（START..STOP）

[word]
bits = 9
order = "msb"
fields = [ { name = "byte", bits = "8:1" }, { name = "ack", bits = "0", role = "ack", expect = 0 } ]

[frame]
first = { word = 0, fields = [ { name = "addr", bits = "8:2", role = "address" }, { name = "rw", bits = "1", enum = { 0 = "write", 1 = "read" } } ] }
rest  = { name = "data", from = "byte", role = "data" }
length = "9n+1"                # 端数 1 bit は STOP 直前の標本

[score]
support = ["ack == expect", "length matches"]
refute  = ["length mismatch"]
~~~

設計の要点（**解釈**）:

- 対象は「語の幅・ビット順・欄の切り出し・固定値・長さの規則・列挙・欄の役割・スコアの支持/反証」まで。**条件分岐と状態は持たせない**（Kaitai・P4 の純宣言の限界を越えようとしない）。越える場合は同じ宣言を持つスクリプトへ昇格させ、宣言部はそのまま流用する。
- 宣言の検証はスキーマ（JSON Schema か CUE）で行い、誤りを「`word.fields[1].bits`: 範囲 9..0 は語の幅 9 を超える」のように**欄の経路つき**で返す。AI の自己修正に最も効くのはこの形である。
- 宣言からコアが Rust の高速経路を作れるので、定型プロトコルでは速度の問題が消える。

## 4. 推奨

### 4.1 組み合わせ

| 層 | 方式 | 担当する範囲 | 理由 |
| --- | --- | --- | --- |
| 1. 宣言 | WireSkein 専用のフレーミング記述（TOML/JSON、3.3 節） | 語の切り出し・欄・長さの規則・スコアの支持/反証。全プラグインのメタデータ（入出力の型、欄の役割、注釈行） | コード無しで大半の定型を覆う。検証が機械的で誤りが明確。速度はコア側で決まる |
| 2. 汎用スクリプト | **TypeScript（型除去）→ QuickJS** を第一候補、ベンチマークの結果次第で **Lua 5.4** | 状態機械、上位の解釈（Modbus、SCPI、DMI → DM）、機器パックの計算部分 | AI の学習量が最多、`.d.ts` を API 仕様として渡せる、型付き配列、ブラウザでも同じ処理系。Lua 5.4 は 64 bit 整数と軽さで優る |
| 3. 重い・信頼できない処理 | **WASM（wasmtime、WIT で API を定義）** | 未知の同期シリアルの総当たり、CRC 探索、大きな上位（USB、CAN の上位群）、第三者の配布物 | 隔離と速度が最良。Rust で書けばコアのカーネルと同じコードを共有できる。WIT が型の約束を兼ねる |

補足:

- **API は 1 つの型定義から 3 層へ出す。** 型付きストリームとレコードの型を WIT（または独自の IDL）で定め、そこから TypeScript の `.d.ts`、Lua の型注記（文書）、WASM の束縛、宣言記述のスキーマを生成する（Binary Ninja の「C ABI を 1 つ」に相当）（**解釈**）。
- **大きな配列はコアが持ち、プラグインへは読み取り専用の窓で渡す。** スクリプトには型付き配列（QuickJS の外部 ArrayBuffer、Lua の userdata）、WASM には「範囲を読むホスト関数」か、線形メモリへの必要分だけの複製。1 標本ずつのコールバックは作らない。
- **64 bit の時刻はスクリプトへ直接渡さない方がよい。** 語・文字の時刻は「区間内の相対 u32＋区間の基点」で渡すと、JS（倍精度・BigInt）でも Luau でも誤らない（**解釈**）。
- **CPython は内蔵しない。** 試作（`src/wireskein/_engine`）と外部プロセスの開発用の口（標準入出力で JSON/Arrow を往復）に留める。配布と隔離の費用が効果を上回る（**解釈**）。
- Rhai・Starlark・Rune・Koto・Roto は、学習量か 2 進データの扱いで上の 2 候補に劣る見込み。Starlark は「宣言記述の中の式」だけに使う選択肢はある（**解釈**）。
- Spicy 風の独自 DSL は作らない。宣言記述（条件無し）とスクリプトの 2 段で足り、第 3 の言語を AI に覚えさせる費用を避ける（**解釈**）。

### 4.2 AI が書くための付帯物

| 付帯物 | 内容 |
| --- | --- |
| 型定義 | `.d.ts`／WIT／スキーマ。入力の型と出力のレコード・欄の役割を網羅 |
| 雛形と例 | I²C・UART 上位・PulseSymbols の 3 例を各層で。宣言 → スクリプトへの昇格例 |
| 検査コマンド | `wsk plugin check`: スキーマ検証、型検査（TS）、フィクスチャ（`corpus/fixtures`）での実行と期待値の比較 |
| 誤りの形 | `ファイル:行:列`、スタック、入力の位置（フレーム番号・標本番号）を必ず含める。スクリプトの誤りと「入力が規則に合わない」（スコアの反証）を区別する |
| 上限 | 1 呼び出しあたりの時間・メモリの上限と、超えたときの誤り文言 |

### 4.3 導入の順序

| 段階 | 作るもの | 終わりの条件 |
| --- | --- | --- |
| 1 | 型定義（IDL）と宣言記述のスキーマ。I²C・SPI・RVSWD を宣言だけで再現 | `src/wireskein/_engine/staged.py` の同期系プラグインと同じ結果・同じスコア |
| 2 | 層 2 の処理系を 1 つ（ベンチマーク結果で決定）。UART 上位（行・NMEA・Modbus RTU）と SCPI を移植 | AI に型定義と雛形だけを渡して、新しい上位（例: LIN の診断フレーム）を書かせ、`wsk plugin check` を通るまでの往復回数を記録 |
| 3 | 層 3（WASM）の口。未知の同期シリアルの探索を移す | 同じ入力でネイティブ比の時間と、打ち切り（燃料）の動作を確認 |
| 4 | 配布形式（1 ファイルのパッケージ: 宣言＋スクリプトまたは `.wasm`＋例＋期待値） | Windows・macOS・Linux・ブラウザで同じパッケージが動く |

段階 2 の「AI の往復回数」は、ベンチマークでは測れない軸 A・F の実測になる。言語を 2 つに絞れない場合は、同じ課題を両方で書かせて比べる（**解釈**）。

## 5. ベンチマーク結果との照合項目

別途のベンチマーク（`research/plugin-lang-bench/SPEC.md` の t1〜t5）の結果は、次の基準で読む。基準値は目安であり、結果を見て改める（**解釈**）。

| 項目 | SPEC の欄 | 判定の目安 | 推奨への影響 |
| --- | --- | --- | --- |
| 大配列の零複製 | `t1_zero_copy`, `t1_ms` | 零複製できること（1 引き渡し 1 ms 未満） | 零複製不可の言語は層 2 から外す |
| 走査の速さ | `t2_ms`（200 万標本） | ネイティブ比 50 倍以内なら許容。ただし走査はコアで行う前提なので**重みは低い** | 層 3（WASM）が必要な処理の範囲を決める |
| 典型プラグイン（I²C） | `t3_ms`, `t3_handoff_ms`, `t3_ok` | 正しさ（`t3_ok`）が必須。レコード生成込みで捕獲の読み込み時間の 1 割以内 | 層 2 の第一候補の決定に最も効く |
| 呼び出し費用 | `t4_host_call_us`, `t4_script_call_us` | 1 µs 未満。フレームごとに呼ぶ設計なら重要 | 大きいなら「区間ごとに一括で渡す」API にする |
| 誤りの報告 | `t5_error` | ファイル名・行・関数名の連鎖が出るか。未定義変数の名前が出るか | AI の自己修正の可否。行が出ない言語は外す |
| メモリ | `peak_rss_kb` | 処理系の基礎分が数 MB 以内 | ブラウザ・複数インスタンスの並列に効く |
| 64 bit | `notes` | u64 の時刻を落とさず扱えたか | JS は相対 u32 の設計で回避できるか確認 |
| 隔離 | `notes` | 零複製の手段が隔離を破らないか（LuaJIT FFI、`unsafe` の外部 ArrayBuffer） | 速さと隔離の両立しない言語は、信頼済みの内蔵プラグイン専用にする |

ベンチマークで測らない点（別途確かめる）: Windows/macOS でのビルドと配布物の大きさ、ブラウザ（wasm32）で同じ処理系が動くか、実行の打ち切り（燃料・割り込み）の効き方、TypeScript の型除去と行番号の対応。

## ベンチマーク結果（2026-09-28、`research/plugin-lang-bench`）

Rust のホストから各処理系を組み込み、同じ入力（実記録の SHT30＝I²C＋UART、1,000 万サンプル／L103 の書き込み時＝RVSWD、3,000 万サンプル）で同じ課題を行った。サンプルは 16 bit の密なファイルを copy-on-write でメモリマップし（`Arc<Mmap>`）、エッジ列は `Arc<[u32]>`、I²C の入力（ビット列・時刻・区切り）は Rust のカーネルで作ってから渡した。全処理系で T2・T3 の結果が参照と一致した。課題の定義は `plugin-lang-bench/SPEC.md`。数値は各処理系を順に走らせた一斉の測定（負荷の低い状態）で、各エージェントが並行ビルド中に測った値は最大 10 倍ぶれたため使わない。Linux（WSL2、x86-64）のみで測定。

| 処理系 | 大きなデータの見せ方（T1） | T1 コピーした場合 | T2 走査 200 万サンプル | T3 I²C（SHT30 / L103 44 万 bit） | 呼び出し ホスト→スクリプト / 逆 | ピーク RSS（SHT30 / L103） | 実行ファイル |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ネイティブ（Rust、基準） | スライス | — | 0.8 ms | 0.00 / 0.82 ms | — | 10 / 63 MB | 0.4 MB |
| QuickJS（rquickjs） | 外部メモリの ArrayBuffer（零複製） | 8〜27 ms | 38〜42 ms | 0.23 / 25 ms | 0.08 / 0.06 µs | 41 / 121 MB | 1.5 MB |
| Lua 5.4（mlua） | userdata の `__index`（零複製、1 回ごとに C 呼び出し） | 23〜27 ms | 50〜57 ms | 0.06 / 24 ms | 0.04 / 0.03 µs | 42 / 94 MB | 1.0 MB |
| LuaJIT（mlua、FFI） | ポインタを `ffi.cast`（零複製） | 17 ms | 2.1〜2.9 ms | 2.6 / 9.4 ms | 0.05〜0.07 / 0.04〜0.06 µs | 27 / 82 MB | 1.2 MB |
| Rhai | 独自型の添字（零複製） | 15〜23 ms | 1,010〜1,170 ms | 0.87 / 315 ms | 0.29〜0.31 / 0.27 µs | 40 / 79 MB | 3.6 MB |
| Starlark（starlark-rust） | 独自の StarlarkValue（零複製） | 15〜17 ms | 64〜67 ms | 0.10 / 23 ms | 0.04 / 0.02 µs | 36 / 87 MB | 8.1 MB |
| CPython 3.13（pyo3） | バッファプロトコルの memoryview（零複製）。numpy も零複製で使える | 9〜44 ms | 116〜135 ms（numpy 4 ms） | 0.25 / 63 ms | 0.08 / 0.07 µs | 77 / 162 MB | 0.6 MB＋libpython 24 MB＋標準ライブラリ 約 12 MB |
| WASM（wasmtime、Rust で作成） | 線形メモリの一部にファイルを copy-on-write でマップ（零複製、Linux の mmap のみ） | 10〜32 ms | 1.4〜1.8 ms | 0.01 / 1.05 ms | 0.03 / 0.005 µs | 71 / 195 MB | 12.2 MB＋.wasm 21〜28 KB（行情報付き 1.6 MB） |

### エラー表示（T5: 2 段の呼び出しの奥、4 行目の誤り）

| 処理系 | 出る情報 | 質 |
| --- | --- | --- |
| QuickJS | `missingVariable is not defined`、`at inner (error.js:4:19)` 以下の呼び出し履歴 | 良。ファイル・行・列・関数名 |
| Lua 5.4 / LuaJIT | `error.lua:4: attempt to index a nil value (field 'missing')` と traceback | 良。末尾呼び出し（`return f()`）では呼び出し元のフレームが消える。先頭に `[C]` の雑音 |
| Rhai | `Variable not found: missing_variable (line 4, position 21)` と呼び出し連鎖 | 良。ファイル名とソースの引用はホストが補う |
| Starlark | Python 風の traceback に、rustc 風の該当箇所の印。未定義の変数は実行前の読み込み時に検出 | 最良 |
| CPython | traceback と、3.13 の該当箇所の印（`^^^^`） | 最良 |
| WASM（Rust） | panic の文言（`src/error.rs:4:19`）と、関数名・行付きの wasm の backtrace。先頭に std の内部フレーム約 10 個。ブラウザでは名前のみ | 良。行情報を残すと .wasm が 1.6 MB。フレームの除去はホストでできる |

### 環境の作りやすさ・配布

| 処理系 | ビルド・配布 | 複数 OS・ブラウザ（Linux 以外は資料による、**未確認**） | 隔離 |
| --- | --- | --- | --- |
| QuickJS | C コンパイラ。1 分程度。依存は libc のみ | Windows・macOS・Linux・モバイル。ブラウザは JS そのもの | メモリ上限・割り込み |
| Lua 5.4 | C コンパイラ。約 70 秒。libc のみ | 全平台（ANSI C）。ブラウザは emscripten 経由 | 標準ライブラリを絞れば強い |
| LuaJIT | C コンパイラ＋make。クロスコンパイルが面倒 | iOS は JIT 不可で遅くなる。ブラウザは不可 | **FFI を使うと任意のメモリに触れ、隔離がなくなる** |
| Rhai | 純 Rust。約 3 分 | 全平台・ブラウザ（公式の playground あり） | 深さ・操作数の上限 |
| Starlark | 純 Rust だが依存 187 クレート、約 7 分。0.x で API が変わりやすい | 主要 OS。ブラウザは未確認 | 決定的で強い（`while` なし、再帰は実装では可） |
| CPython | `PYO3_PYTHON`、実行時の `LD_LIBRARY_PATH`・`PYTHONHOME` が要り、欠けると理由の分からない致命的エラー | 主要 OS は可（同梱物が重い）。ブラウザは Pyodide（別物、数 MB） | ほぼ不可（os・ctypes に触れる） |
| WASM | ホストは wasmtime で重い（初回約 10 分）。プラグインは各言語のツールチェーンが要る | 主要 OS。iOS は事前コンパイルかインタープリタ。同じ .wasm がブラウザでも動く（Node/V8 で確認） | 最強（能力・燃料・epoch・メモリ上限） |

### 判定

- **生サンプルを舐める処理はスクリプトに書かせない。** 零複製で見せても、JIT のない処理系は走査がネイティブの 50〜1,400 倍遅い（QuickJS 50 倍、Lua 5.4 70 倍、Starlark 80 倍、CPython 150 倍、Rhai 1,400 倍）。LuaJIT（FFI）と WASM だけがネイティブ並み。1 節の「サンプル単位のループはスクリプトに書かせない」を数値で裏付けた。
- **型付きストリームを受け取るプラグイン（T3）なら、どの処理系でも実用の範囲。** 44 万ビットの I²C で 23〜63 ms（Rhai だけ 315 ms）。段構成のエンジンでは、1 記録あたりのプラグイン呼び出しが数百〜数千回なので、呼び出しの費用（0.02〜0.3 µs）は問題にならない。
- **零複製は全処理系でできた。** ただし仕組みの安全性が違う。QuickJS・Starlark・Rhai・Lua 5.4・CPython は、読み取り専用の窓を安全に作れる。LuaJIT の FFI は生ポインタなので隔離が破れる。WASM は線形メモリへのマップで、OS ごとに別の実装が要り、ブラウザでは使えない（コピーになる）。
- **総合（解釈）:**
  - 汎用スクリプトの第一候補は **QuickJS（TypeScript を型除去して実行）**。速さは中位だが、零複製が安全、実行ファイルが小さく依存がない、エラー表示が良い、ブラウザで同じ処理系が使える、AI の学習量が最多、の条件がそろっている。
  - **Lua 5.4** は、ほぼ同等の性能で最も軽い対抗案。64 bit 整数を持つ点が優れ、1 始まりの添字と末尾呼び出しの traceback が弱点。
  - 重い処理と信頼できない配布物は **WASM**。
  - Starlark はエラー表示と隔離が最良だが、依存が重く `while` がないため、状態機械を書くのには向かない。宣言記述の中の式言語としてなら候補になる。
  - Rhai は遅すぎる。CPython は配布と隔離の費用が大きい。LuaJIT は隔離と iOS・ブラウザの制約から、主候補から外す。
- **次に確かめること:** QuickJS と Lua 5.4 で、AI にプラグインを書かせたときの往復回数（4.3 節の段階 2）。WASM のマップの Windows 版。TypeScript の型除去（oxc）を同梱したときの大きさと時間。

## 6. 未確認のまま残した点

- libsigrokdecode の現行版が対応する Python の上限。
- Luau の 64 bit 整数（型の追加提案）の状況。
- LuaJIT の上流の公開版の状況（rolling release の扱い）。
- Gluon・Mun の最終公開日（年）と保守の実態。
- Rune・Koto で外部のバイト列を零複製で見せる手段と、命令数上限による停止。
- WASM Component Model で大きな `list<u8>` を複製せずに渡す標準的な手段（WASI 0.3 の stream／資源での読み出し）の成熟度。
- Saleae HLA で pip の外部パッケージが使えるかの範囲。
- rquickjs の外部 ArrayBuffer（ホストのメモリを参照）を安全に使う条件（寿命・解放の約束）。
- Luau の `buffer` をホストのメモリの上に作れるか（零複製の可否）。
- oxc の型除去で元の行・列が保たれるか（空白での置き換えか、ソースマップが要るか）。
- Extism・Component Model のブラウザでの実行時の大きさと起動時間。
