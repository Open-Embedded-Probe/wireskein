# 取り込み機材を足すには

[English](devices.md)

WireSkein は、機材の種類ごとの **ドライバ** を通して取り込みます。取り込み元は先頭に種類を書きます: `wireskein capture --source oep:oep://30eda0ea068b` は `oep` のドライバ、`--source demo:counter` は試しの機材のドライバを使います。ドライバは WireSkein 本体のものです: 新しい種類の機材は、WireSkein にドライバを足して扱います（外から差し込みません）。ドライバは Rust で、1 つの口 `crates/wireskein-capture` に合わせて書きます。機材より後ろ - 受けたバイトが待つ上限、組み立て、データ落ち、ファイル - は共通なので、ドライバは機材と話すことだけを受け持ちます。

この文書は、口の形（§1）、試しの機材を手本にしたドライバの足し方（§2）、機材のメモリに貯める型と接続のたびにファームウェアを送る型の違い（§3）、WireSkein が受け入れられないライセンスのコードを読まずに、機材の通信を観察してプロトコルを書く手順（§4）を書きます。

## 1. 口の形

`crates/wireskein-capture`（`driver` モジュール）:

| 部品 | 中身 |
| --- | --- |
| `Driver` | 機材の種類: `kind()`（取り込み元の頭の名前）、`about()`、`list()`（いま見える機材。それぞれ取り込み元付きの `Found`）、`open(address, &Open)` |
| `Device` | 開いた機材。`close()` まで保持する: `facts()`（型番、シリアル、ファームウェア: 取り込みの `meta.probe`）、`hold()`、`caps()`、`capture(&Request, &Ctl)` |
| `Hold` | ほかの利用者に対する排他の形: `Lease`（OEP: プローブが持ち、取り込み中は更新し、host が死ねば切れる）、`Lock`（USB のインターフェースの claim、tty のロック、ロックファイル）、`None` |
| `Caps` | できること: 線、受ける速さ、`stream` と `memory`（どちらか、または両方）とその上限（`RateLimit`: 何本までなら毎秒何サンプルまで。メモリのバイト数。トリガの前を残せるか）、モード、トリガの種類、しきい値、縮約、送る `firmware`。`Caps::check(&Request)` は超える要求を理由付きで断る |
| `Request` | 何を取るか。どの機材でも同じ（Python の `wireskein.sources.Request`）: 名前と機材の id の組の線、速さ、サンプル数か時間、トリガと pretrigger、モード（`OneShot`、`Repeat`、`Stream`）、損失で止めるか続けるか、ストリームのファイルの置き場、しきい値 |
| `Ctl` | 呼び手が取り込みを握る口: `stop()`（別のスレッドから）と、`Progress` を聞くこと（取り中か読み出し中か、受けたバイトとその総数、未処理、損失） |
| `Acquired` | 組み立てた取り込み: 区画（中に隙間のない ticks の塊）、各区画の線、落ちたもの、meta |
| `Registry` | プログラムが知る種類。`crates/wireskein-sources::builtin()` が WireSkein のもの（`demo`、`oep`） |

共通の流れ（`flow`、`stream`、`spool`、`packed` モジュール）:

- **受信の側は host を待たない**（`stream::Inlet`）。受けたものは、受けてまだ書き出していないもの全体に対する **バイト数の上限 1 本** の中でメモリで待つ（既定 512 MiB、`WIRESKEIN_CAPTURE_FLOW=budget=MIB`）。止まったディスクや混んだ host は未処理を増やすだけ。上限を超えると、流し続ける機材は自分のバッファに入る分だけ持ち、残りを落とす（`Inlet::try_put`）。それはストリームの位置の飛びとして見える。host の速さで読み出す機材は待つ（`Inlet::put`）。
- **隙間は損失**（`spool::Spool`）。バイトはストリームの位置付きで来る。位置が飛んだら区画を閉じ、損失として記録する。損失があれば取り込みは失敗で、取れた所は保存する（`--on-loss stop` が既定で最初の損失で止まる、`continue` は最後まで）。1 つのファイルに複数の区画を持てる。
- **詰めたサンプルを線に**（`packed`）。多くのロジアナは N 本の線を 1 サンプル 1・2・4 バイトに詰めて送る。`Format::Packed { width, positions }` が各線のビットを取り出し（8×8 のビットの転置。幅はいくつでもよく、バイト単位が速い）、spool が来るそばからディスクに書く。独自の並びの機材（OEP の形式 1）は `spool::BlockLayout` を渡す。
- **書き出し** は wireskein-io（`save::to_capture`、`save::save`）。流れの数字は `meta.probe.link.flow`。

## 2. ドライバの足し方

手本は試しの機材（`crates/wireskein-capture/src/demo.rs`）: 実機なしで、カウンタ（線 Dk はサンプル番号のビット k）を詰めたサンプルで送る機材。選択肢で共通の流れの振る舞いを見せられる: `type=stream|memory`、`pace=real|fast`、`drop=K`（K 番目の塊を落とす: 損失）、`stall=MS`（spool が止まる: 上限の中で待つ）、`fail=K`（機材が誤りで止まる）、`budget=MIB`、`firmware=FILE`。

```
wireskein-rs capture --source 'demo:counter?lines=16' --channels A=D0,B=D5 --rate 1M --samples 100k -o demo.wireskein
wireskein-rs capture --source 'demo:?lines=8&drop=3' --channels A=D0 --rate 1M --samples 100k --on-loss continue -o lost.wireskein
wireskein-rs capture --list
```

種類を足す手順:

1. **専用のクレート**（`crates/wireskein-<kind>`）。`wireskein-capture` に依存する（USB は `nusb`: 純 Rust、libusb なし）。workspace と `wireskein-sources::builtin()` に足す。
2. **`Driver::list`**: 挿さっているこの種類の機材（VID:PID、インターフェースのクラス、シリアル番号で）を、それぞれ `<kind>:<address>` で。
3. **`Driver::open`**: アドレスを読み、機材を開き、保持する（USB のインターフェースを claim: `Hold::Lock`）。要るならファームウェアを送る（§3）。機材が自分について言うことを尋ねる。
4. **`Device::caps`**: 線、`stream` と `memory`（線の本数ごとの上限付き）、トリガ、しきい値。`capture` は `caps.check(req)` から始める: 機材を超える要求は何も送る前に断る。
5. **`Device::capture`**: 機材を設定し（速さ、線、しきい値、トリガ）、始め、共通の流れでデータを受ける - `stream::pipe(flow)` が受信の側と読む側を作る。受信の側（USB の IN 転送を多数並べたスレッド。`crates/wireskein-oep/src/transport.rs` と同じ形）が転送ごとのバイトをストリームの位置付きで置く。`stream::collect` が、時間・`Ctl::stop`・on-loss stop の損失まで、`Format::Packed` の `SpoolWorker` に読み込む。機材を止め、機材の facts と流れの報告を `meta.probe` に入れた `Acquired` を返す。機材が誤りで止まったら、損失（`stopped`）を足し、取れた所は残す。
6. **`Device::close`**: 止めて、インターフェースを放す。
7. **実機なしの試験**: §4 の観察の記録に書いた「機材が送るバイト」を与える代わりの受信の側を `stream::pipe` に通す。試しの機材の試験と同じ（上限の中の止まりは何も落とさない、超えたら損失、落とした塊は損失、誤り停止は取れた所を残す）。実機では `meta.probe` の数字と、既知の信号（プローブ自身の出力、信号発生器）で確かめる。試験対象の機材が「送ったつもり」のものを正解にしない。

Python からは、どの Rust のドライバも `wireskein_core.capture_source(source, request, owner)` で呼ぶ。Python の実装のない種類（試しの機材）は、`src/wireskein/sources/` の下にそれを呼ぶモジュールを置く。

## 3. ほかの 2 つの型

**機材のメモリに貯める型**（速く取り、後で読み出す）: `Caps::memory` に、メモリのバイト数、何本の線の 1 サンプルをどう詰めるか（`sample_bits`）、線の本数ごとの速さ、トリガの前を残せるかを書く。`Caps::check` は `samples × segments` が `MemoryCaps::samples_for(lines)` を超える要求と、残せない pretrigger を断る。`capture` は機材を構え、トリガとその後のサンプルを待ち（`Ctl` に `Stage::Taking` を伝え、`Ctl::stop` で早く止める）、それからメモリを同じ流れで読み出す。`Inlet::put` を使い（機材が host を待つので落ちない）、進み具合は `Stage::Reading` で `of_bytes` にメモリの大きさ。試しの機材の `type=memory`（遅い読み出しは `read=毎秒のバイト数`）がこのとおりに動き、その試験がトリガのサンプル、pretrigger、両方の段を確かめる。

**接続のたびにファームウェアや FPGA の構成を送る型**（FX2・FX3、FPGA のロジアナ）: そのファイルはメーカーのもので、WireSkein は同梱しない。ドライバはそれぞれを `Caps::firmware`（`FirmwareNeed`: ファイル名、何か、利用者がどこで手に入れるか、メーカーのアプリが入れる場所）に書き、`open` で `driver::find_firmware(kind, need)` で探す。探す順は `WIRESKEIN_FIRMWARE_<KIND>`（ファイルかそのディレクトリ）、`WIRESKEIN_FIRMWARE_DIR`、WireSkein の種類ごとのディレクトリ（Linux は `~/.config/wireskein/firmware/<kind>/`、macOS は `~/Library/Application Support/...`、Windows は `%APPDATA%\wireskein\firmware\<kind>\`）、最後にメーカーの場所。どこにもなければ、誤りの文言が、ファイルが何か、どこで手に入るか、探した場所の全部、設定する変数を言う。利用者の持つファイルを別のプログラムが送ってよいかは、メーカーの条件で確かめてから頼る。試しの機材の `firmware=FILE` で探し方と誤りの文言を試せる。

## 4. 観察からプロトコルを書く

各社のホストのソフトや、その機材の有志のドライバは GPL のことが多い。WireSkein は MIT: ドライバは、その機材の GPL のコードを **読まず、写さず、見ながら書かず** に作る。プロトコルはベンダーのアプリと機材の通信を観察して知り、ドライバはそのコードを読んでいない者がその記録から書く。

1. **通信を取る**（Linux の usbmon と Wireshark）:
   ```
   sudo modprobe usbmon
   lsusb -d 1a86:5537          # バスと機器の番号（Bus 003 Device 007）
   sudo wireshark -i usbmon3 -k   # または: sudo tshark -i usbmon3 -w run.pcapng
   ```
   機材で絞る（`usb.device_address == 7`）。ベンダーのアプリを動かし（Linux で、または機材を渡した Windows の VM で）、1 度に 1 つのことだけをして、それぞれの時刻を控える: 挿す、開く、設定を 1 つ変える（速さだけ、次に線だけ、しきい値だけ、トリガだけ）、短く取る、止める、閉じる。手順ごとに別のファイルに取る。
2. **記録から読み取る**:
   - **見つける**: VID:PID、シリアルの文字列、インターフェースとエンドポイント（`lsusb -v`）
   - **開く**: control 転送（`bmRequestType`、`bRequest`、`wValue`、`wIndex`、データ）とファームウェアの送り込み（挿した後の長い書き込みの列、機材の再列挙）
   - **設定**: 速さだけ変えたとき、線だけ、しきい値だけのときに変わるバイト。速さの表し方（基準クロックの分周か）
   - **開始と停止**: 命令と機材の答え
   - **データ**: bulk IN のエンドポイント、転送の大きさ、サンプルの詰め方（既知の信号 - 周波数のわかったプローブの方形波を 1 本に、ほかは Low に - を取り、バイトの中にその周期を探す）、転送ごとの見出しやカウンタ（落ちた転送がわかる）、終わりの形
   - **誤り**: host が遅いとき（ディスクを外す、CPU に負荷をかける）に、機材は止まるか、隙間に印を付けるか、黙って落とすか
3. **記録を書く**: `docs/instruments/<機材>-protocol.md` に、メッセージごとの欄、見たこととその確かさ、元にした記録（名前、日付、アプリの版）。生の記録はリポジトリに入れない（送り込むときのベンダーのファームウェアやシリアル番号を含む）: 日付とアプリの版を付けてベンチのデータの置き場に残し、記録に要るバイトだけを引く。
4. **ドライバ** は、ベンダーのコードも GPL のドライバのコードも読んでいない者が、この記録だけから書く。代わりの受信の側の試験（§2 の 7）は記録に書いたバイトを与える。記録で決まらないことは、そうしたコードで調べず、取り直す。
