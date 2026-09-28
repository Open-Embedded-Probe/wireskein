# WireSkein — プロトコルの層と積み重なりの調査

2026-09-28 作成。匿名のデジタル記録からプロトコルを推定するにあたり、よく出会う方式を「どの層で何が観測できるか」で整理し、解析器（プラグイン）の段の切り方と共通前処理の範囲を決める材料にする。[プロトタイプ計画](prototype-plan.ja.md) の「解析器の段とスコアのプラグイン化」を補う調査であり、方式の採用を決める文書ではない。

表記の約束:

- 数値は代表値。規格の版や機器で変わるものは「代表」「例」と書く。
- 一次資料で確かめていないものは **（未確認）**、この文書の解釈は **（解釈）** と明記する。
- WCH 関連の実測は、ローカルの解析ノート `/home/mt/dev_wch/wch-protocols`（以下 wch-protocols）と `/home/mt/dev_oep/oep-spec` を根拠にする。

## 1. 推定のための層モデル

通常の OSI 参照モデルは「ロジックアナライザで何が見えるか」とずれる。ここでは観測から上へ積む順に次の 6 層を置く。

| 層 | 決めるもの | 例 | デジタル記録だけで観測できるか |
| --- | --- | --- | --- |
| E 電気・線の形式 | 駆動方式、線の本数と役割、アイドルレベル | プッシュプル、オープンドレイン（ワイヤード AND）、差動、双方向 | アイドルレベル、線の本数、どの線が同時に動くかは見える。駆動方式は**直接は見えない**（下記） |
| C 線符号化 | 1 bit をレベル／幅／遷移のどれで表すか | NRZ、NRZI、マンチェスタ（bi-phase）、パルス幅、パルス間隔、ビットスタッフィング | ラン幅の分布（単位の整数倍か、2 値か、3 値以上か）で推定できる |
| S ビット同期 | 受信側がどこで bit を取るか | 非同期（スタートビットで再同期）、別線クロック、自己同期（遷移に同期） | クロック様の線の有無、データ遷移とクロック端の位相で推定できる |
| F フレーミング | 単位の始まりと終わり | START/STOP 条件、CS、ブレーク、アイドル間隔、SYNC パターン、固定長 | 境界の候補は見える。長さの規則（9 bit 単位、53 clock 等）は復号して初めて確かめられる |
| L リンク／トランザクション | アドレス、方向、応答、誤り検出 | I²C のアドレス＋ACK、SWD の ACK、CAN の ACK スロット、DMI の op/status | 復号後の値の一貫性（アドレスの再出現、ACK の位置、パリティ・CRC）として見える |
| A 上位アプリケーション | 意味 | レジスタマップ、Modbus、PMBus、UDS、RISC-V Debug Module | 固定パターン、チェックサム、既知コマンド表との一致で「支持」として見える |

デジタル記録だけでは分からないこと:

- **電圧・駆動方式。** オープンドレインかプッシュプルかは、立ち上がりの遅さやグリッチでしか推し量れない。アナログ記録か閾値の違う 2 本の取得が要る。I²C と I³C の区別（SDR では SCL がプッシュプル）も、デジタルでは主に速度と規則で行う。
- **差動の 2 線。** CAN_H/CAN_L や USB D+/D− は、トランシーバの後（CAN の RXD 等）か、片側だけを取れば NRZ の 1 本として扱える。USB は D+ と D− の両方が要る（SE0 の検出のため）。
- **誰が駆動しているか。** 双方向線（I²C SDA、SWDIO、1-Wire、SWIO）でホストとターゲットのどちらが出しているかは、位相の規則からの推定になる。
- **搬送波。** IR リモコンは受光モジュールの出力（復調後）を取るのが普通。38 kHz 等の搬送波が残っている場合は、先に包絡を取る前処理が要る。

## 2. よく出会うプロトコルの一覧

凡例: 同期は「非同期」＝スタートビット等で再同期、「同期」＝別線クロック、「自己」＝自己同期。「H/L」はアイドルレベル。

### 2.1 汎用のシリアル・バス

| 方式 | 線と役割 | 同期 | アイドル | フレーム単位と bit 数 | 代表レート | 見分けの手掛かり |
| --- | --- | --- | --- | --- | --- | --- |
| UART（TTL） | TX、RX（任意で RTS/CTS） | 非同期 NRZ、LSB first | H | start 0 + data 5〜9 + parity 0/1 + stop 1/1.5/2 | 9600〜1 Mbaud 超、115200 が多い | ラン幅が 1 bit 時間の整数倍。フレームの最後が必ず H。ブレーク＝1 フレーム以上 L |
| RS-232 | 同上（線上は反転・±電圧） | 同上 | 線上は負電圧 | 同上 | 同上 | トランシーバ後に取れば UART と同じ。直接なら極性反転 |
| RS-485 | A/B 差動、DE 制御線 | 同上 | 差動でバイアス依存 | 同上 | 〜10 Mbaud | 半二重。DE 線が送信区間を囲むなら CS 相当の区切り |
| I²C | SCL、SDA（オープンドレイン） | 同期、MSB first | H/H | START（SCL H 中に SDA↓）＋ 9 bit 単位（8 + ACK）＋ STOP（SCL H 中に SDA↑）。先頭は 7 bit アドレス + R/W | 100 k / 400 k / 1 MHz / 3.4 MHz | SDA は SCL L 中だけ変わる（START/STOP と例外を除く）。9 クロック周期のまとまり。クロックストレッチ |
| SMBus | SMBCLK、SMBDAT（＋ SMBALERT#） | I²C と同じ | H/H | I²C と同じ＋ PEC（CRC-8） | 10〜100 kHz（3.0 で 400 k/1 MHz） | I²C より低速寄り。クロック L が 25〜35 ms を超えるとタイムアウト。末尾 PEC |
| I³C | SCL（プッシュプル）、SDA | 同期 | H/H | SDR は 8 data + T bit（奇数パリティ）。ブロードキャスト 0x7E | SDR 12.5 MHz | I²C の START/STOP を共有するが 9 bit 目が ACK ではなくパリティ。I²C より速い |
| SPI | SCLK、MOSI、MISO、CS#（複数可） | 同期、モード 0〜3 | CS H、SCLK は CPOL | CS L 区間内の 8n bit（機器により 16/24 等） | 数 MHz〜数十 MHz | CS 区間ごとにクロック数がそろう。データはサンプル端の逆の端で変わる |
| Dual/Quad SPI（QSPI） | SCLK、CS#、IO0〜IO3 | 同期 | 同上 | 命令は 1 線で送り、アドレス・データを 2/4 線で送る形が多い | 〜100 MHz 超 | 1 線区間と多線区間の切替、ダミークロック。IO0〜3 が同時に変わる |
| 1-Wire | DQ（オープンドレイン） | 自己（時間スロット） | H | リセット（L 480 µs 以上）→ プレゼンス（L 60〜240 µs）→ 60 µs 程度のスロットの列、LSB first | 標準 約 15 kbit/s、オーバードライブあり | 非常に長い L のリセットと直後の応答。書き 1 は L 1〜15 µs、書き 0 は L 60 µs 以上 |
| CAN / CAN FD | CAN_H/L（差動）、ロジックでは RXD/TXD | 自己（NRZ + 5 bit スタッフィング、遷移で再同期） | 劣勢＝H（RXD） | SOF + ID 11/29 + 制御 + data 0〜8（FD は 64 byte）+ CRC-15（FD は 17/21）+ ACK + EOF 7 劣勢 | 125 k〜1 Mbit/s、FD のデータ相 〜5/8 Mbit/s | 同値 6 bit が続かない（スタッフ違反はエラーフレーム）。1 フレーム内で bit 時間が 2 種（FD の BRS） |
| LIN | 1 線（12 V、トランシーバ後は UART 相当） | 非同期 | H | ブレーク（13 bit 以上 L）+ sync 0x55 + PID（6 bit ID + 2 bit パリティ）+ data 1〜8 + checksum | 1〜20 kbit/s（19.2 k が多い） | 長いブレーク＋ 0x55 で始まる UART 列 |

資料: [NXP UM10204 I²C 仕様](https://www.nxp.com/docs/en/user-guide/UM10204.pdf)、[SMBus 3.x 仕様](http://smbus.org/specs/)、[MIPI I3C Basic](https://www.mipi.org/specifications/i3c-sensor-specification)、[Analog Devices: 1-Wire の基礎（AN126）](https://www.analog.com/en/resources/technical-articles/1wire-communication-through-software.html)、[Bosch CAN 2.0 仕様](http://esd.cs.ucr.edu/webres/can20.pdf)、[CiA: CAN FD](https://www.can-cia.org/can-knowledge/can-fd)、[LIN 仕様（ISO 17987 の元になった LIN 2.2A）](https://www.lin-cia.org/standards/)。

### 2.2 デバッグ・書き込み用

| 方式 | 線と役割 | 同期 | アイドル | フレーム単位と bit 数 | 代表レート | 見分けの手掛かり |
| --- | --- | --- | --- | --- | --- | --- |
| JTAG（IEEE 1149.1） | TCK、TMS、TDI、TDO（任意で TRST#） | 同期。TCK↑ で TMS/TDI を取り、TDO は TCK↓ で変わる | TCK 任意、TMS H で Test-Logic-Reset | TMS で 16 状態の TAP を遷移し、Shift-IR/DR 区間で任意長を shift。LSB first | 〜数十 MHz | TMS が 5 回以上 H でリセット。TDI と TDO の 2 本のデータ |
| ARM SWD | SWCLK、SWDIO（双方向） | 同期、LSB first | 線リセット時 SWDIO H | 要求 8 bit（Start 1・APnDP・RnW・A[2:3]・Parity・Stop 0・Park 1）+ turnaround + ACK 3 bit + turnaround + data 32 + parity | 〜数十 MHz | 要求の先頭が常に 1、Stop 0・Park 1。ラインリセット（H 50 clock 以上）と JTAG→SWD 列 0xE79E |
| WCH RVSWD（2 線） | SWCLK、SWDIO（双方向） | 同期、MSB first。SWCLK H 中にサンプル | H/H | START ＋ 53 clock（short 52 bit + 終端）＋ STOP が主。long 85 clock、burst 15 + 38×N clock もある（2.3 節） | 実測 0.47〜2.5 MHz、V203 は 10 MHz 程度 | I²C と同じ形の START/STOP（SCL H 中の SDA 変化）を使う。9 bit 周期が無く、読み出し中は **target が SWCLK H 中に SWDIO を変える**（I²C では STOP/START に見える） |
| WCH SWIO / SDI（1 線） | SWIO（オープンドレイン、プルアップ） | 自己（L パルスの幅） | H | start + addr 7 + R/W + data 32 = 41 パルス、高速読み出しの 33 パルス | 1 bit 約 1.1〜1.3 µs | L 幅が 2 値（1＝約 260 ns、0＝約 860 ns）で周期がほぼ一定 |
| STM8 SWIM | SWIM（オープンドレイン） | 自己（パルス幅、HSI 基準） | H | 入口列（L 16 µs 程度の後の 1 kHz/2 kHz パルス列）の後、start + 3 bit コマンド + parity + ACK。データは 8 bit + parity + ACK | 低速で 1 bit 22 HSI 周期（8 MHz 時 約 2.75 µs） | 0＝L 長／1＝L 短のパルス幅で、周期は一定。入口の特徴的なパルス列（**細部は未確認**） |

資料: [IEEE 1149.1](https://standards.ieee.org/ieee/1149.1/4484/)、[ARM ADIv5.2 仕様（IHI 0031）](https://developer.arm.com/documentation/ihi0031/latest/)、[ST UM0470 STM8 SWIM](https://www.st.com/resource/en/user_manual/um0470-stm8-swim-communication-protocol-and-debug-module-stmicroelectronics.pdf)、WCH は 2.3 節。

### 2.3 オーディオ・ストレージ・その他

| 方式 | 線と役割 | 同期 | アイドル | フレーム単位と bit 数 | 代表レート | 見分けの手掛かり |
| --- | --- | --- | --- | --- | --- | --- |
| USB LS/FS | D+、D−（差動） | 自己（NRZI + 6 bit 1 の後にスタッフィング） | FS は D+ H（J）、LS は D− H | SYNC（KJKJKJKK）+ PID 8 bit（4 bit + 反転）+ … + CRC5/CRC16 + EOP（SE0 2 bit） | 1.5 / 12 Mbit/s | D+ と D− がほぼ常に逆相、SE0（両方 L）がパケット末尾。1 ms ごとの SOF（FS）／keep-alive（LS） |
| I²S | SCK（BCLK）、WS（LRCLK）、SD、任意で MCLK | 同期、MSB first | 常時動作 | WS の 1 周期＝左右 2 スロット。Philips 形式は WS 変化の 1 clock 後から | BCLK＝fs × 32/48/64（例 48 k × 64 = 3.072 MHz） | 連続クロック、WS はその 1/32〜1/64 のデューティ 50% 方形波 |
| TDM | BCLK、FSYNC、SD | 同期 | 常時動作 | FSYNC ごとに 4〜16 スロット | BCLK 〜25 MHz | FSYNC が 1 clock 幅のパルスになることが多い |
| PDM | CLK、DATA | 同期（両端で 2 チャネル） | 常時動作 | 1 bit 列（フレームなし）。立ち上がり・立ち下がりで左右 | 1〜3.072 MHz | データ線の 1 の密度がゆっくり変わる。フレーム境界が無い |
| SD/SDIO | CLK、CMD、DAT0（〜3） | 同期、MSB first | CMD/DAT H | コマンド 48 bit（start 0・dir・index 6・arg 32・CRC7・end 1）、データブロック＋線ごと CRC16 | 初期化 400 kHz 以下、25/50 MHz 以上 | CMD の 48 bit 単位、CRC7 が合う |
| SD（SPI モード） | SCLK、MOSI、MISO、CS | SPI | SPI | 6 byte コマンド（0x40 + index、arg 4、CRC7 と終端 1）、R1 等の応答、データトークン 0xFE | 初期化 400 kHz 以下 | CMD0 = `40 00 00 00 00 95`、応答待ちの 0xFF 列 |
| MDIO（Clause 22） | MDC、MDIO | 同期 | MDIO H | preamble 32 bit の 1 + ST 01 + OP 2 + PHYAD 5 + REGAD 5 + TA 2 + data 16 | 〜2.5 MHz | 32 個の 1 の後に 01。Clause 45 は ST 00 |
| DMX512 | RS-485 差動 | 非同期 250 kbaud 8N2 | H（MARK） | ブレーク（88 µs 以上）+ MAB + start code（照明は 0x00）+ 最大 512 スロット | 250 kbit/s 固定 | 速度が固定、長いブレーク、8N2 |
| MIDI（5 pin DIN） | 電流ループ（受信側フォトカプラの後は UART） | 非同期 31.25 kbaud 8N1 | H | ステータスバイト（bit7=1）+ データバイト（bit7=0）1〜2 個。ランニングステータス、SysEx は F0…F7 | 31.25 kbit/s 固定 | 速度が固定、bit7 による区切り |
| WS2812 系 LED | DIN（単方向、カスケード） | 自己（パルス幅） | L | 1 LED 24 bit（GRB 順が多い）、リセット＝L 50 µs 以上（新しい版は 280 µs） | 800 kHz（1 bit 1.25 µs） | H 幅が 2 値（0＝約 0.4 µs、1＝約 0.8 µs）、周期一定。SK6812 RGBW は 32 bit。APA102 系は CLK + DATA の同期 SPI 形式 |
| IR（NEC） | 受光出力 1 本（復調後はアクティブ L が多い） | 自己（パルス間隔） | H（受光出力） | リーダ 9 ms + 4.5 ms、32 bit（addr、~addr、cmd、~cmd）、LSB first。リピートは 9 ms + 2.25 ms | 搬送波 38 kHz、単位 562.5 µs | mark 幅一定・space 幅 2 値（1:3）。反転バイトの検算 |
| IR（RC5） | 同上 | 自己（マンチェスタ） | 同上 | 14 bit（start 2、toggle、addr 5、cmd 6） | 36 kHz、半 bit 889 µs | ラン幅が 889 µs と 1778 µs の 2 値 |
| IR（SIRC） | 同上 | 自己（パルス幅） | 同上 | ヘッダ 2.4 ms + 12/15/20 bit（cmd 7 + addr 5/8/13）、LSB first | 40 kHz、単位 600 µs | mark 幅 2 値（600 / 1200 µs）、space 一定 |
| PS/2 | CLK、DATA（オープンコレクタ、双方向） | 同期（機器がクロックを出す） | H/H | start 0 + data 8（LSB first）+ 奇数パリティ + stop 1 = 11 bit（ホスト送信時は機器の ACK が付く） | CLK 10〜16.7 kHz | 11 クロックのまとまり。データは CLK H 中に変わり L 中に取る |
| HDMI-CEC | CEC 1 線（オープンドレイン） | 自己（パルス幅） | H | スタートビット（L 3.7 ms + H 0.8 ms）+ 10 bit ブロック（data 8 + EOM + ACK）の列 | 1 bit 2.4 ms（約 400 bit/s） | ms 単位の遅さ、0＝L 1.5 ms、1＝L 0.6 ms |
| SENT（SAE J2716） | 1 線（センサ → ECU） | 自己（立ち下がり間の間隔） | H | 同期／較正パルス 56 tick + ステータス nibble + data nibble 1〜6 + CRC-4 nibble。1 nibble = 12〜27 tick | 1 tick 3〜90 µs（3 µs が多い） | 立ち下がり間隔が 12 + 値 tick。56 tick のパルスがフレームの頭 |
| DALI（IEC 62386） | 2 線バス（ロジックでは送受信 1 本ずつ） | 自己（bi-phase マンチェスタ） | H（バス電圧あり） | 前進フレーム start + 16 bit（DALI-2 は 24 bit も）+ stop 2 bit 以上、後退フレーム 8 bit | 1200 bit/s | 半 bit 約 417 µs の 2 値ラン、後退フレームは前進の直後 |
| 10BASE-T 等 | 差動、マンチェスタ 10 MHz | 自己 | — | — | — | **対象外**。記録の周波数が足りないことが多い。マンチェスタ一般の検出器が扱う範囲として記録する |

資料: [USB 2.0 仕様](https://www.usb.org/document-library/usb-20-specification)、[NXP I²S 仕様](https://www.nxp.com/docs/en/user-manual/UM11732.pdf)、[SD Association 簡易仕様](https://www.sdcard.org/downloads/pls/)、IEEE 802.3 Clause 22/45（MDIO）、[ESTA: ANSI E1.11 DMX512-A](https://tsp.esta.org/tsp/documents/published_docs.php)、[MIDI 1.0 仕様](https://midi.org/midi-1-0-core-specifications)、[Worldsemi WS2812B データシート](https://cdn-shop.adafruit.com/datasheets/WS2812B.pdf)、[SB-Projects: IR プロトコル（NEC/RC5/SIRC）](https://www.sbprojects.net/knowledge/ir/index.php)、[PS/2 プロトコル（Adam Chapweske の解説のアーカイブ）](https://www.burtonsys.com/ps2_chapweske.htm)、HDMI 1.4 仕様付録 CEC（非公開、代表値は解説資料による）、[SAE J2716](https://www.sae.org/standards/content/j2716_201604/)、[DALI Alliance](https://www.dali-alliance.org/dali/)。DMX の MAB 最小値（初版 4 µs、E1.11 で 8 µs）、DALI-2 の 24 bit フレーム、CEC の各幅は解説資料による代表値（**一次資料未確認**）。

### 2.4 形が似ていて取り違えやすい組

| 組 | 共通点 | 区別の手掛かり |
| --- | --- | --- |
| I²C と RVSWD | 2 線、アイドル H/H、START/STOP が同じ形 | RVSWD は 9 bit 周期でなく 53/85 clock 等の固定長、MSB first の 7 bit アドレスが DMI 番地（0x04、0x10、0x11、0x16、0x17…）に集中。読み出し中の SWCLK H 中の SWDIO 変化 |
| I²C と I³C | START/STOP、アドレス 0x7E | 9 bit 目がパリティとして合う、SCL の高速さ |
| I²C と PS/2 | 2 線オープンドレイン、アイドル H/H | PS/2 はデータが CLK H 中に変わる。11 bit 単位で START/STOP が無い |
| SPI と RVSWD／SWD／JTAG | クロック＋データ | CS 線の有無。SWD は双方向で turnaround の空き。JTAG は TMS/TDI/TDO の 3 本 |
| UART と SWIO／WS2812 | 1 線、アイドル H（WS2812 は L） | UART は L 幅が bit 時間の任意の整数倍。パルス幅方式は L（または H）幅が 2 値で周期が一定 |
| UART とクロック線 | 単線で整数倍の幅 | クロックは幅が 1 種類だけでデューティ一定。[プロトタイプ計画](prototype-plan.ja.md) の SCL 誤判定の例 |
| NEC と SIRC と RC5 | 1 線の IR | 幅を変える側（space か mark か）、ラン幅の 2 値の比、ms 単位の単位時間 |
| LIN と DMX512 と UART のブレーク | 長い L の後に UART 列 | 速度（19.2 k / 250 k）、ブレーク直後が 0x55（LIN）か 0x00（DMX） |

## 3. 上位の積み重なり

### 3.1 UART の上

~~~text
UART バイト列（時刻・フレーム誤り・ブレーク付き）
├─ テキスト行（CR/LF）
│   ├─ ログ・実験マーカー
│   ├─ AT コマンド（"AT…\r"、応答 "OK"/"ERROR"）
│   └─ NMEA 0183（$…*hh\r\n、XOR チェックサム、4800/9600 baud が多い）
├─ Modbus RTU（アドレス・機能・データ・CRC-16/MODBUS。3.5 文字の無音でフレームを区切る）
├─ バイト詰めの区切り
│   ├─ SLIP（END 0xC0、ESC 0xDB）
│   ├─ COBS（0x00 区切り）
│   └─ HDLC 風（フラグ 0x7E、エスケープ 0x7D、FCS-16）→ PPP
├─ MAVLink（v1 開始 0xFE、v2 開始 0xFD、CRC-16/MCRF4XX + CRC_EXTRA）
├─ LIN（ブレーク + 0x55 + PID + data + checksum）
├─ DMX512（ブレーク + start code + スロット）
├─ MIDI（31.25 kbaud、ステータス bit7）
└─ WCH の書込み: ISP（factory、0x57 0xAB 等）、IAP（AA 55 … 55 AA、チェックサム）
~~~

資料: [Modbus over Serial Line 1.02](https://modbus.org/docs/Modbus_over_serial_line_V1_02.pdf)、[RFC 1055 SLIP](https://www.rfc-editor.org/rfc/rfc1055)、[RFC 1662 PPP in HDLC-like Framing](https://www.rfc-editor.org/rfc/rfc1662)、[COBS（Cheshire & Baker）](http://www.stuartcheshire.org/papers/COBSforToN.pdf)、[MAVLink シリアル化](https://mavlink.io/en/guide/serialization.html)、WCH ISP/IAP は wch-protocols `protocols/pc-to-device-isp.ja.md`・`protocols/wch-iap.ja.md`。

### 3.2 I²C の上

~~~text
I²C トランザクション（START、アドレス + R/W、ACK/NACK、データ、反復 START、STOP）
├─ レジスタマップ型の機器（[addr W] reg [Sr addr R] data… の形。EEPROM 24Cxx、センサ）
│   └─ 機器固有の CRC（例: Sensirion SHT3x の CRC-8 poly 0x31）
├─ SMBus（Quick/Send/Receive/Read・Write Byte/Word/Block、Process Call、PEC = CRC-8 poly 0x07）
│   ├─ PMBus（PAGE 0x00、VOUT_MODE 0x20、READ_VOUT 0x8B、LINEAR11/LINEAR16 形式）
│   ├─ Smart Battery（SBS、アドレス 0x0B、Voltage 0x09 等）
│   └─ ARP、SMBALERT の応答アドレス 0x0C
├─ HID over I²C（HID 記述子レジスタ、長さ付き入力レポート、INT 線）
└─ I³C はバスの共有相手（I²C 機器は I³C バスに同居できる）
~~~

資料: [SMBus 3.2 仕様](http://smbus.org/specs/SMBus_3_2_20220112.pdf)、[PMBus 仕様](https://pmbus.org/specification-archives/)（Part II は会員向け、コマンド番号は公開資料・データシートの記載による）、[Smart Battery Data Specification 1.1](http://sbs-forum.org/specs/sbdat110.pdf)、[Microsoft HID over I²C](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/hid-over-i2c-guide)。

### 3.3 SPI の上

~~~text
SPI 転送（CS 区間ごとの MOSI/MISO バイト列、モード、bit 順）
├─ SPI NOR フラッシュ（JEDEC 慣習の命令表）
│   ├─ 9F RDID（製造者 ID + 型番）、05 RDSR、06 WREN、03 READ、0B FAST READ、02 PP、20/D8 消去
│   ├─ 5A SFDP（先頭 4 byte が "SFDP" = 50 44 46 53）
│   └─ Quad I/O 読み出し（EB 等）→ QSPI 区間へ
├─ SD カード SPI モード（CMD0/8/55/41、R1、0xFE トークン、CRC16）
├─ 表示コントローラ（D/C# 線でコマンドとデータを分ける。ST77xx の 2A/2B/2C 等）
├─ ADC/DAC・センサ（固定長の 16/24 bit フレーム、読み出しコマンド + 応答）
└─ 無線チップ等のレジスタアクセス（先頭 byte に R/W bit + アドレス）
~~~

資料: [JEDEC JESD216 SFDP](https://www.jedec.org/standards-documents/docs/jesd216b)（命令番号は各社データシートで確認。例 [Winbond W25Q128JV](https://www.winbond.com/hq/product/code-storage-flash-memory/serial-nor-flash/)）、SD 簡易仕様 Physical Layer 7 章（SPI モード）。

### 3.4 CAN の上

~~~text
CAN フレーム（ID 11/29 bit、DLC、data、CRC、ACK）
├─ CANopen（COB-ID = 機能コード + node ID。NMT 0x000、SYNC 0x080、SDO 0x600+/0x580+、心拍 0x700+）
├─ SAE J1939（29 bit ID = 優先度 + PGN + 送信元アドレス、250/500 kbit/s。長いデータは TP）
└─ ISO-TP（ISO 15765-2。PCI 上位 nibble: 0 単一、1 先頭、2 継続、3 フロー制御）
    └─ UDS（ISO 14229。SID 0x10/0x22/0x27/0x2E/0x31/0x34…、肯定応答 SID+0x40、否定応答 0x7F）
        └─ OBD-II（ISO 15031 / SAE J1979。要求 0x7DF、応答 0x7E8〜0x7EF）
~~~

資料: [CiA 301 CANopen](https://www.can-cia.org/can-knowledge/canopen)、[SAE J1939](https://www.sae.org/standards/content/j1939_202210/)、ISO 15765-2 / ISO 14229-1（有償規格、番号は解説資料による）。

### 3.5 JTAG と SWD の上

~~~text
JTAG 信号列
└─ IEEE 1149.1 TAP（IR/DR の shift 列）
    ├─ IDCODE（32 bit、bit0 = 1）、BYPASS、境界スキャン（EXTEST/SAMPLE、BSDL で意味付け）
    ├─ ARM JTAG-DP（IR: ABORT 0x8、DPACC 0xA、APACC 0xB、IDCODE 0xE。DR 35 bit = data 32 + A[3:2] + RnW）
    │   └─ ADIv5 DP/AP（SWD と同じ上位）
    └─ RISC-V DTM（IR: IDCODE 0x01、dtmcs 0x10、dmi 0x11、BYPASS 0x1f。dmi の DR = abits + data 32 + op 2）
        └─ DMI → RISC-V Debug Module（3.6 節と同じ）

ARM SWD パケット（要求 + ACK + data 32 + parity）
└─ ADIv5 / ADIv6 DP（DPIDR 0x0、CTRL/STAT 0x4、SELECT 0x8、RDBUFF 0xC）
    └─ AP（SELECT で AP 番号とバンクを選ぶ）
        └─ MEM-AP（CSW 0x00、TAR 0x04、DRW 0x0C、BD0〜3、IDR 0xFC）
            └─ メモリ・周辺レジスタ（Flash 書込み、CoreSight ROM テーブル、DHCSR 等のデバッグレジスタ）
副経路: semihosting（BKPT 0xAB で停止し、ホストがメモリを読む）、SWO（別ピン。UART か マンチェスタで ITM/DWT パケット）
~~~

資料: IEEE 1149.1、[ARM ADIv5.2（IHI 0031）](https://developer.arm.com/documentation/ihi0031/latest/)、[ARM ADIv6（IHI 0074）](https://developer.arm.com/documentation/ihi0074/latest/)、[RISC-V Debug Spec](https://github.com/riscv/riscv-debug-spec)、[ARM Semihosting](https://github.com/ARM-software/abi-aa/blob/main/semihosting/semihosting.rst)、ITM は [ARMv7-M ARM 付録 D](https://developer.arm.com/documentation/ddi0403/latest/)。

### 3.6 WCH RVSWD / SWIO（SDI）の上

WCH は QingKe コアのデバッグ線を「WCH debug interface protocol」と呼び、1 線（V2A 等）と 2 線を持つ。1 線は WCH の資料と EVT で **SDI**（Serial Debug Interface）とも呼ばれ、ピン名は SWIO。運ぶのはどちらも RISC-V 標準の **DMI トランザクション（7 bit 番地 + 32 bit data + op/status）** であり、その上は RISC-V Debug Spec 0.13 の Debug Module に WCH の拡張を足したものである（[QingKe V2 プロセッサマニュアル](https://www.wch-ic.com/downloads/QingKeV2_Processor_Manual_PDF.html)、[QingKe V2 デバッグマニュアル（ミラー）](http://nic.vajn.icu/PDF/WCH-IC/RISC-V/support/RISC-V_QingKeV2_Microprocessor_Debug_Manual.pdf)、wch-protocols `protocols/link-to-target.ja.md`）。

~~~text
E/C/S  RVSWD: SWCLK + SWDIO、NRZ、SWCLK H 中にサンプル、MSB first
       SWIO : 1 線、L パルス幅（1 ≈ 260 ns、0 ≈ 860 ns、周期 ≈ 1.1 µs）
F      RVSWD: START（SWCLK H 中に SWDIO↓）… STOP（SWCLK H 中に SWDIO↑、その後 H が 2 µs 以上）
         short 53 clock / long 85 clock / burst 15+38×N clock
       SWIO : start + addr7 + R/W + data32 = 41 パルス、高速読み出し 33 パルス
L      DMI トランザクション: (addr 7, data 32, op read/write) → (data 32, status 0 成功/2 失敗/3 busy)
A1     RISC-V Debug Module（標準番地）
         data0 0x04 / data1 0x05 / dmcontrol 0x10 / dmstatus 0x11 / hartinfo 0x12
         abstractcs 0x16 / command 0x17 / abstractauto 0x18 / progbuf0.. 0x20..
         （sbcs 0x38 等のシステムバスアクセスは仕様上あるが、CH32 では使われていない模様（未確認））
       WCH 拡張（仕様で未定義の番地）: 0x7C / 0x7D / 0x7E / 0x7F
         minichlink の名前は DMCPBR / DMCFGR / DMSHDWCFGR / DMCHIPID。
         LinkE は接続時に 0x7E・0x7D へ 0x5AA50400 を書き、0x7F から chip ID を読む
A2     abstract command（レジスタ read/write、postexec）と program buffer
         → メモリ read/write（progbuf の lw/sw + ebreak）、flash 書込み、abstractauto による連続読み出し
A3     DM の data0/data1 を郵便受けにした文字チャネル（下表）
~~~

short 形式（52 bit）の中身は `addr 7 + R/W 1 + parity 1 + aux 5 + data 32 + parity 1 + aux 5` で、aux（park/padding）は run ごとに変わる don't-care。long 形式は `addr 7 + data 32 + op 2 + parity 1`（host）→ `addr 7 + data 32 + status 2 + parity 1`（target）で、RISC-V JTAG DTM の `dmi` DR を 2 線上で前後に時間分割したものと読める（**解釈**、wch-protocols）。long 形式の parity は資料の記述（奇数/偶数）と実測が合わず、parity として働いていない例がある。いずれも wch-protocols の実測（WCH-LinkE 2.22、CH32X035/L103/V203/V003）と第三者実装（[RINS](https://perigoso.github.io/rins/)、[sigrok-rvswd](https://github.com/perigoso/sigrok-rvswd)、[fxsheep の初期解析](https://github-wiki-see.page/m/fxsheep/openocd_wchlink-rv/wiki/WCH-RVSWD-protocol)）による。

**文字チャネル。** 同じ DM の data0/data1 を使うが、エンコードが 3 種ある。どれも「target がメモリ写像された data0/data1 に書き、host が DMI で data0/data1 を polling し、data0 を書き戻して受領を返す」郵便受けで、target は halt せず走ったまま使える。

| 名称 | 出所 | data0 下位 byte | 中身 | 受領 | 備考 |
| --- | --- | --- | --- | --- | --- |
| WCH SDI printf | WCH EVT `SDI_Printf_Enable()`、`debug.c` の `_write` | 文字数 1〜7 | data0 上位 3 byte ＝ 先頭 3 文字、data1 ＝ 残り 4 文字 | host が data0 = 0 を書く。target は data0 ≠ 0 の間待つ | WCH-LinkE が吸い上げて USB の仮想 COM に出す。LinkE は data0 を約 29 µs ごとに読み、4 文字以上なら data1 も読む（L103 実測） |
| minichlink / ch32fun の debugprintf（SerialDMDATA） | [ch32fun `ch32fun.c`](https://github.com/cnlohr/ch32fun/blob/master/ch32fun/ch32fun.c) | `0x80 \| (文字数 + 4)` | 同じ配置（3 + 4 byte） | host が bit7 を落とした値を書く。そこに host → target の入力を載せられる | `minichlink -T` で端末になる。待ちはタイムアウト付き |
| dmseq（SerialDMSeq） | このプロジェクト群の実験仕様（oep-spec `experiments/dm-console-seq/SPEC-draft.md`、2026-09-24 草案 0.3） | bit7 T、bit6 TO、bit5 S、bit4 A、bit3 SYN、bit2..0 N（0〜6） | payload N byte の直後に CRC-8（poly 0x07、init 0xFF） | host 応答は bit7 = 0、K/H の 1 bit 通番と payload 最大 2 byte + CRC | 1 bit の通番で重複・欠落を防ぐ。接続時に data0 に残る 0xFFFFFFFF を N = 7 として弾く |

- target 側の番地（EVT 既定）: V2 系（V003/V006）は data0 `0xE00000F4`・data1 `0xE00000F8`、V20x/V307/X035/L103/V103 は `0xE0000380`/`0xE0000384`、V205/V407/X315/M030 は `0xE0000340`/`0xE0000344`。DMI 側からはどれも番地 0x04/0x05（[openwch ch32v20x `debug.c`](https://github.com/openwch/ch32v20x/blob/main/EVT/EXAM/SRC/Debug/debug.c)、wch-protocols `protocols/serial-and-print.ja.md`）。
- **「DMSEQ」について。** RISC-V Debug Spec にも WCH の資料にもこの名前は無い。この環境では、oep-spec で草案化した **dmseq**（DM の data レジスタ上の通番付きコンソール。ch32rv の `monitor --source dmseq`、target ライブラリ `SerialDMSeq`）を指す。WCH の SDI printf を指して言う場合は「DMDATA 郵便受け」と呼び分けるのがよい。
- 一般の RISC-V の副経路として、semihosting（`slli x0,x0,0x1f; ebreak; srai x0,x0,7` の命令列で停止）と、RAM のリングバッファを読む SEGGER RTT がある。どちらも線上では「メモリ読み出しの DMI 列」として見え、文字チャネルとしての判別は上位でしかできない。
- RVSWD の積み重なりで、線だけから推定できるのは A1 まで（DM の番地と値）。A2 のメモリ読み出しは abstract command の列から番地・値を組み立てれば得られる。A3 は data0 への繰り返しの read と、非 0 のときの write 0 の組で見つけられる。

## 4. WireSkein への示唆

### 4.1 段（プラグインの単位）の切り方

| 共通の段（出力型の案） | 下に来るもの | 上に乗るもの | 理由 |
| --- | --- | --- | --- |
| **同期ビット列**（クロック線、データ線群、サンプル端、区切り） | SPI、QSPI、I²C、SWD、RVSWD、JTAG、MDIO、PS/2、I²S/TDM/PDM、SD | 各方式のフレーマ | 「クロック端でデータを取る」処理は共通で、方式の違いは区切り（CS、START/STOP、TMS、preamble、WS）と bit 順だけ。1 つのカーネルにできる |
| **パルス幅記号列**（L 幅／H 幅／間隔の 2 値〜3 値化） | SWIO、SWIM、WS2812、1-Wire、NEC、SIRC、CEC、SENT | 各方式のフレーマ | 閾値 1 つでシンボル化でき、周期一定・幅 2 値という指紋も共通 |
| **マンチェスタ／NRZI・スタッフィング** | RC5、DALI、USB、CAN | 各方式のフレーマ | 遷移の再同期と bit 抜きが共通 |
| **時刻付きバイト列**（誤り・ブレーク・無音間隔付き） | UART、SPI の MOSI/MISO、I²C のデータ部、CAN の data | テキスト、Modbus、SLIP/COBS、SPI フラッシュ命令 | 上位が見るのはバイトと区切り（無音 3.5 文字、CS、STOP）だけ |
| **トランザクション**（番地、方向、値、応答、状態） | I²C、SMBus、SWD、DMI（RVSWD/SWIO/JTAG-DTM） | レジスタマップ、PMBus、ADIv5 AP、RISC-V DM | 「どこに何を読み書きしたか」で上位を共通化できる。DMI は 3 つの transport で同じ型になる |
| **状態付きセッション** | RISC-V DM、ADIv5 MEM-AP、ISO-TP | メモリ写像、文字チャネル、UDS | 前のトランザクションの値（data1 の番地、SELECT、TAR）を持ち越して意味を作る |

- RVSWD と SWIO は、線の段だけ別プラグインにし、DMI トランザクション以上を共有する。JTAG の RISC-V DTM もこの型へ落とせるので、DM 以上を 3 経路で使い回せる。
- I²C → SMBus → PMBus は「同じトランザクション型への解釈の重ね書き」であり、段を分けるより**トランザクションに注釈を足す解析器**として並べる方が素直（**解釈**）。

### 4.2 共通前処理に置く推定

| 推定 | 使う方式 | 備考 |
| --- | --- | --- |
| アイドルレベルと活動区間 | すべて | 極性（UART の反転、IR 受光のアクティブ L）もここで |
| ラン幅の分布と基本時間単位 | UART、LIN、DMX、MIDI、マンチェスタ系 | 単位の整数倍か 2 値かで C 層の候補を分ける |
| パルス幅の 2 値クラスタと周期の一定性 | パルス幅記号列の段 | 周期一定＋幅 2 値は UART と強く区別できる |
| クロック様の線と周波数、連続か突発か | 同期ビット列の段 | I²S・PDM は連続、SPI・I²C・RVSWD は突発 |
| 線の対とサンプル端（データ遷移とクロック端の位相） | 同期ビット列の段 | 「データは L 中に変わる」（I²C、RVSWD、SPI モード 0）と「H 中に変わる」（PS/2、読み出し中の RVSWD target）の区別 |
| 区切りの候補（START/STOP 様の遷移、CS 様の線、長い L のブレーク・リセット、長い無音） | すべて | ここでは候補を出すだけにし、方式は決めない |
| 語長・フレーム長の分布（区切り間のクロック数） | 同期ビット列の段 | 9 の倍数（I²C）、8n（SPI）、53/85（RVSWD）、41/33（SWIO）、11（PS/2）、64 bit 前後（MDIO） |
| 線の同時変化（群） | QSPI、I²S、USB D+/D− | 常に逆相の 2 線は差動の片側同士 |

### 4.3 下位の仮説を支える強い検査

| 検査 | 支える下位の仮説 |
| --- | --- |
| CRC-16/MODBUS、NMEA の XOR、MAVLink の CRC、HDLC の FCS | UART の速度・bit 順・パリティ |
| SMBus PEC（CRC-8）、Sensirion 等の機器 CRC-8 | I²C の bit 境界、SMBus としての解釈 |
| SD の CRC7/CRC16、SFDP の "SFDP" 署名、JEDEC ID の再出現 | SPI のモードと bit 順、SD の線の割り当て |
| CAN の CRC-15/17/21 とスタッフ規則、ISO-TP の PCI の連続性 | CAN の bit 時間、FD の BRS |
| SWD の要求パリティと ACK 位置、DP の DPIDR の値 | SWD の線の割り当てと turnaround |
| RVSWD short の 2 つの parity、DMI 番地が既知の DM 番地に集中、dmstatus の値の形 | RVSWD の区切りと bit 順（I²C との取り違えの反証） |
| NEC の反転バイト、RC5 の start bit、1-Wire の ROM CRC-8（Dallas） | パルス幅・マンチェスタの閾値 |
| LIN の PID パリティとチェックサム、DMX の start code | UART のブレーク検出と速度 |
| WCH SDI printf の長さ nibble と文字らしさ、dmseq の CRC-8 | DMI の再構成（data0 の読み書きの組） |

固定パターン（I²C の同じアドレスの再出現、MDIO の preamble、JTAG の IDCODE の bit0 = 1、USB の SYNC）は CRC ほど強くないが、仮説の早期打ち切り（[プロトタイプ計画](prototype-plan.ja.md) の窓による判定）に使える。

## 5. 未確認のまま残した点

- STM8 SWIM の入口列と高速モードの細部（UM0470 で要確認）。
- CEC・DMX の MAB・DALI-2 の各時間の規格上の許容幅（一次資料が有償・非公開）。
- CH32 の DM がシステムバスアクセス（sbcs 等）を持つか。wch-protocols の実測では使われていない。
- RVSWD の long / short の選択規則、long 形式の parity の意味、7 bit と 8 bit のどちらが番地幅か。
- WCH 拡張番地 0x7C〜0x7F の各 bit の意味（WCH の一次資料で定義を見つけていない）。
