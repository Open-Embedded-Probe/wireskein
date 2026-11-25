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
