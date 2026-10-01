# 凍結の前に決めること（wireskein / wireskein-web / pytest-embedded-wireskein）

**まだ凍結していません。** ここで決めたことは、凍結に向けて先に入れるものです。凍結の時期は、利用者が決めます。それまでは、ほかにも互換のない変更がありえます。

作成 2026-09-30。ArduinoCore-CH32（現 [ArduinoCore-CH32RV](https://github.com/ch32-riscv-ug/ArduinoCore-CH32RV)）のセッションが洗い出した 10 項目を、コードで確かめて、案を付けたものです。

**決定（2026-09-30）: A〜E はすべて案のとおり。** A: 凍結で `wireskein/1`（`/0` は読まない）。B: `capture.json` に `id`。C: `tol_period`、`tol_v` に改名。D: 未検査は既定で失敗（`measured` は失敗にしない）。E: Python 3.11 以上。印のない項目も案のとおりに進めます。**決めてから文書（仕様）に書き、そのあと実装します。** 「要判断」の印の項目は、利用者が選びます。印のないものは、案のまま進めてよいと考えるものです。

OEP と連動する項目（10）は、dev_oep の決定を待ちます。

## 1. 形式の版

**現状**
- 読み手は `wireskein/0` の完全一致だけを読みます（`sniff` は `wireskein/` で始まれば WireSkein と見なし、`read_header` が版の違いを断る）。
- 仕様 §7 は「`/0` の中では、足すだけ」と決めています。版の中の小さい番号（minor）や、「読むのに要る最低の版」はありません。
- `.sr` の中の付加情報も `wireskein.json` という同じ名前で、`format` の接頭辞（`wireskein-sr-extra/0`）だけで区別しています。

**案**
- 版の番号は、互換のない変更のときだけ上げる 1 つの数のままにします。足す変更（新しいキー、新しい `encoding`、新しい部分）は版を変えず、読み手は知らないものを無視する（今の §2.2、§3.2 のとおり）。これで足りるので、minor は作りません。
- **要判断 A: 凍結のときに `wireskein/1` にするか。**
  - 案: 凍結の時点で `wireskein/1` にし、それを互換を守る最初の版にします。β の間の `/0` のファイルとは、はっきり区別できます。過去の形式は読まなくてよい方針なので、`/0` は読みません。
  - もう 1 つの道: `/0` のまま凍結する。β の間のファイルも読めますが、β の間に中身が少しずつ変わったものも `/0` になります。
- `.sr` の中の付加情報の名前を、`wireskein/sr-extra.json` に変えます（生の値を入れている `wireskein/` の下にそろえる）。WireSkein のファイルの `wireskein.json` と名前が重ならなくなります。

## 2. meta の時刻のキー

**現状**
- `trigger_index` は、`meta`（ロジックの区画の位置）と、アナログのチャンネルの `acquisition` に書いていますが、仕様の表にありません。
- `start_us`（OEP の古い区画）が、仕様、runlog の説明、README の例、fileformat の説明に残っています。OEP の今のクライアントは `start_ns` と `start_uncertainty_ns` を返します。

**案**
- `trigger_index` を仕様の表に足します。meta のものは「ロジックのサンプルの番号」、`acquisition` のものは「そのチャンネルのサンプルの番号」です。
- `start_us` はやめます。仕様の表と説明から消し、例を `start_ns` と `start_uncertainty_ns` に直します。書き手は、`start_ns` を返さない古いプローブのときも、`start_us × 1000` を `start_ns` として書きます。

## 3. capture.json

**現状**
- `tick_hz` の分母の上限が、書くときは 10⁹、`_fraction` は 10¹² で、そろっていません。
- アナログの `width` は 8、16、32 だけです。`unit` は `analog-f32` にしかありません。
- エッジの列の `encoding` は予約のまま、パラレル ADC の `acquisition` のキーは予定のままです。

**案**
- 分数（`tick_hz`、`rate_hz`、`t0_ticks`）の分子と分母は、1 から 2⁵³ − 1 までにします。JavaScript の数で正確に扱える範囲です。
- 浮動小数点から分数にするときの分母の上限は、10⁹ にそろえます。整数や分数で渡されたものは、そのまま使います。
- `width` は 8、16、32 のままです（ADC は 32 ビットで足ります）。
- `analog` にも任意の `unit` を認めます。`(値 − zero) × scale_nv × 10⁻⁹` の単位で、既定は V です。電流のセンサーなどで A と書けます。足す変更なので、今入れても後から入れても互換は保てますが、今決めておきます。
- エッジの列とパラレル ADC は、足す変更として後から入れられるので、今は決めません。仕様にもそう書きます。

## 4. 添付（alignment.json）

**現状**
- 別のファイルへの合わせ込みは、基準のファイルを `capture.json` の生のバイト列の SHA-256 で見分けています。そのため、`convert` や書き直しで `capture.json` の並びが変わると、中身が同じでも無効になります。
- 仕様 §8 は「0.0.12 から」と書いていますが、これは 0.0.12 のリリース（2026-09-30）で正しくなりました。

**案**
- **要判断 B: キャプチャに ID を持たせるか。**
  - 案: `capture.json` に、取ったときに作る `id`（128 ビットの乱数、16 進）を入れます。`convert`（`.sr` や VCD との往復も）で持ち越し、合わせ込みは `capture_id` で基準を見分けます。
  - もう 1 つの道: 生のバイト列ではなく、正規化した中身（キーを並べ替えた JSON）の SHA-256 にする。ID は要りませんが、メタ情報を 1 つ足すだけで無効になります。

## 5. 記録（wireskein-run/2）

**現状**
- 期待のキーは `a/b[k]` です。`[k]` は、同じ名前が 2 回以上出たときだけ付きます。そのため、あとから同じ名前の区間を 1 つ足すと、前の区間のキーが `a/b` から `a/b[0]` に変わります。
- `section(..., **rules)` は保存されますが、`verify` は読みません。
- キャプチャは、開始の時刻（µs）だけで区間に割り当てています。

**案**
- キーの付け方: 最初の区間は名前のまま（`duty=64`）、2 回目から `[1]`、`[2]` と付けます。あとから足しても、前のキーは変わりません。
- `**rules` はやめます（引数を消し、渡されたら誤りにする）。使っている所はありません（ArduinoCore-CH32 の tracekit.py も使っていない）。
- キャプチャを取ったときに開いている区間のパスを、`run.json` のキャプチャに記録します（`"path"`）。`verify` はそれを使い、ない記録（ほかの道具が書いたもの）だけ時刻で割り当てます。
- これらで記録の形が変わるので、`wireskein-run/3` に上げます。

## 6. verify の API

**現状**
- 許容誤差の意味がそろっていません。
  - 相対: `tol_freq`、`tol_baud`、`pulses` の `tol`
  - 絶対: `tol_duty`、`voltage` の `tol`
- `threshold` は、`only_moving` と `voltage` にはありません。
- 未検査（`ok=None`）は、失敗になりません。

**案**
- 名前で意味が分かるようにします（**要判断 C: 呼び方が変わる**。tracekit.py は `pulses` と `voltage` の `tol` を使っていないので、影響は小さい見込み）。
  - 相対は `tol_freq`、`tol_baud`、`tol_period`（`pulses` の `tol` を改名）。
  - 絶対は `tol_duty`（デューティの差）、`tol_v`（`voltage` の `tol` を改名、ボルト）。
- `only_moving` は、ロジックのチャンネルだけを見る、と仕様に書きます（アナログは対象外）。`voltage` はアナログそのものなので、`threshold` は要りません。
- **要判断 D: 未検査の扱い。**
  - 案: 結果の状態を 4 つに分けます: `ok`、`ng`、`unchecked`（検査できなかった: ピンがない、換算がないなど）、`measured`（`baud=None` のように、測るだけと頼まれたもの）。
    - `unchecked` は、既定で失敗にします。多くはテストの書き間違いか、配線の抜けだからです。`--allow-unchecked`（CLI）と、pytest の `wireskein_unchecked = pass` で、今の動きに戻せます。
    - `measured` は失敗にしません。
  - もう 1 つの道: 今のまま、未検査は失敗にしない（`--strict` で失敗にする）。

## 7. CLI

**現状**
- `analyze --window FROM TO`（秒の 2 つの値）と、`align --window 300us`（1 つの値、単位付き）で、同じ名前が違う意味です。
- 数の読み取り（`20M`、`250k`）で、小文字の `m` を mega と読んでいます。
- cli が `sources.Request` を 9 個の位置引数で組み立てています。
- エラー文に、古い名前の `ws.py segments` が残っています。

**案**
- 時間を受ける引数は、どれも単位付きの値（`300us`、`1.5ms`、単位なしは秒）を受けるようにします。
- `align --window` は、意味どおりに `--max-offset` に改名します。`analyze --window FROM TO` は範囲のまま残し、単位を受けるようにします。
- 数の単位は `k`/`K`、`M`、`G` だけを受けます。小文字の `m` は「M のことですか」と誤りにします。
- `Request` は、キーワード引数で組み立てます（内部の直し）。
- `ws.py` の文は直します（誤り）。

## 8. wireskein-web

**現状**
- `package.json` の説明が `.wsc` のままです。
- `exports` の `./src/*` で、すべてのモジュールが公開されています。
- JS の名前は camelCase（`tickHz`）で、Python と仕様（`tick_hz`）とは違います。

**案**
- 説明は直します（誤り）。
- `exports` は `.`（index.js）だけにし、公開の API をそこで出すものに限ります。
- JS の名前は camelCase のままにします。JS の習慣に合わせ、ファイルの中のキーとの対応を README に表で書きます。

## 9. pytest-embedded-wireskein

**現状**
- `ws_run` は `Recorder` そのものです（Recorder の API が、プラグインの API になっている）。
- プラグインが内部の `rec.doc["expect"]` を読み、`report["summary"]["ng"]` と、`lines()` の `"NG"` という接頭辞に頼っています。
- Python 3.13 以上を求めています。

**案**
- `ws_run` は `Recorder` のまま、と決めます。公開の API は、`Recorder` の説明にあるメソッド（`section`、`command`、`reply`、`armed`、`capture`、`close`）です。
- 内部を読まないように、`Recorder` に `is_empty` を足します。報告は、状態の数（`summary`）と結果の一覧（`results` の `status`）を、文書にした安定の API として使います。`lines()` の文字列には頼りません。
- **要判断 E: Python の下限。** 案は 3.11 以上です。3.12 以下で動かない理由は `PurePath.full_match`（3.13）の 1 か所だけで、置き換えで済みます（3.11 と 3.12 で試して確かめた）。wireskein と pytest-embedded-wireskein の両方を下げます。

## 10. OEP と連動するもの（dev_oep の決定を待つ）

- 区画の 33 バイトと `start_ns` の形、末尾の読み飛ばしの規則: oep-client の範囲です。WireSkein は、クライアントが返す値を使うだけです。
- 補正の方式の名前（`scheme`、例 `com.espressif.esp32.two-point`）: OEP の仕様が決め、WireSkein はファイルにそのまま入れます。名前を変えるなら、凍結の前に OEP 側で変えてもらいます。
- `oep-client-python` の下限: `>=0.0.4` は実態に合っていません。アナログと組の取得に要る `>=0.0.10` にします（誤り）。

## 進め方

1. 要判断 A〜E を決める。
2. 決めたことを、仕様（wireskein-format、記録の形式、verify の説明）に書く。
3. 実装する（wireskein、wireskein-web、pytest-embedded-wireskein）。互換のない変更は、はっきり壊れる形（古い名前は誤りにする）で入れる。
4. ArduinoCore-CH32 の tracekit.py と dev_oep に、変わる呼び方を知らせる。

## 実装の状況（2026-09-30）

| 項目 | 状態 |
| --- | --- |
| 1. 形式の版（`wireskein/1`、`/0` を断る、`wireskein/sr-extra.json`） | 実装済み（wireskein 6acf678、wireskein-web 35f28c1） |
| 2. `trigger_index` を仕様に、`start_us` をやめる | 実装済み（6acf678。古いプローブの µs は `start_ns` にして入れる） |
| 3. 分数の範囲（2⁵³ − 1、浮動小数点は分母 10⁹）、`analog` の `unit` | 実装済み（6acf678） |
| 4. `capture.json` の `id`、合わせ込みは `capture_id` | 実装済み（6acf678、wireskein-web 35f28c1） |
| 5. `wireskein-run/3`（キーの付け方、キャプチャの `path`、`**rules` をやめる） | 実装済み（84e4f14。仕様は [記録の形式](run-format.ja.md)） |
| 6. `tol_period`、`tol_v`、結果の `status`、未検査は既定で失敗 | 実装済み（84e4f14、d2593fa、wireskein-web 3bbc45c） |
| 7. CLI（単位付きの時間、`--max-offset`、数の単位、`Request` をキーワードで、`ws.py`） | 実装済み（a46f32f） |
| 8. wireskein-web（説明、`exports` は入口だけ、名前の対応表） | 実装済み（35f28c1） |
| 9. pytest-embedded-wireskein（公開の API だけを使う、`--wireskein-unchecked`、Python 3.11） | 実装済み（ba744d2）。wireskein 0.0.13 が要る |
| E. Python 3.11 以上 | 実装済み（a46f32f。3.11、3.12、3.13 で全テストを確かめた。CI も 3 つの版で試す） |
| 10. OEP と連動するもの | `oep-client-python>=0.0.10` は実装済み（a46f32f）。OEP の凍結（`oep.fixture.logic` への改名など）は、oep-client の新しい版が出てから追随する |

リリースの順番: wireskein-web → wireskein（同梱のビューアを上げてから）→ pytest-embedded-wireskein。版の番号は、いつもどおり次の番号です（wireskein 0.0.13、wireskein-web 0.0.6、pytest-embedded-wireskein 0.0.4 の見込み）。凍結したときの番号は、そのときに決めます。
