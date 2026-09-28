# WireSkein — 対応プロトコルの優先度と積み重なりの深さ

2026-09-28 作成。既存の解析器カタログ（sigrok、Saleae、Pico Technology）から**今も普通に使われている方式**を拾い、WireSkein で対応する順番を決める材料にする。方式ごとの線・符号・区切りの詳細は [プロトコルの層と積み重なりの調査](protocol-layers.ja.md)（以下「層の調査」）にあり、ここでは繰り返さない。[プロトタイプ計画](prototype-plan.ja.md) の「解析器の段とスコアのプラグイン化」と、出力の粒度（GUI と CLI で何を返すか）の設計を補う。

表記の約束:

- 一次資料で確かめていないものは **（未確認）**、この文書の解釈は **（解釈）** と明記する。
- 利用状況の高・中・低は、2026 年時点で新規設計・保守の現場で出会う頻度の見立てであり、市場統計ではない（**解釈**）。
- 掲載の略号: **sr** ＝ sigrok（libsigrokdecode）、**SA** ＝ Saleae Logic 2 の標準解析器、**SX** ＝ Saleae の拡張（コミュニティ共有の LLA か共有 HLA）、**P** ＝ PicoScope 7 の標準デコード。

## 1. 資料と取得状況

| 資料 | URL | 取得 | 得たもの |
| --- | --- | --- | --- |
| sigrok Protocol decoders | https://sigrok.org/wiki/Protocol_decoders | 取得できた | 131 種の一覧。積み重なりは wiki の表では一部しか分からないため、[libsigrokdecode](https://github.com/sigrokproject/libsigrokdecode) を取得し、各 `decoders/*/pd.py` の `inputs`/`outputs`/`tags` を機械的に抜き出した（最新コミット 2024-10-01、71f4514） |
| Saleae Protocol Analyzer SDK | https://www.saleae.com/support/extensions-api/protocol-analyzer-sdk/protocol-analyzer-sdk | 取得できた | C++ の LLA（共有ライブラリ）と標準解析器の一覧。Frame 構造は [SampleAnalyzer の Analyzer_API.md](https://github.com/saleae/SampleAnalyzer/blob/master/docs/Analyzer_API.md) で補った |
| Saleae 標準解析器 | https://www.saleae.com/support/protocol-analyzers/supported-protocols/supported-protocols | 取得できた（support.saleae.com から転送） | 標準 23〜24 種 |
| Saleae HLA・FrameV2 | [HLA](https://www.saleae.com/support/extensions-api/extensions/high-level-analyzer-extensions)、[FrameV2](https://www.saleae.com/support/extensions-api/protocol-analyzer-sdk/framev2-hla-support-analyzer-sdk)、[フレーム形式](https://www.saleae.com/support/extensions-api/hla-frame-format-reference/analyzer-frame-types) | 取得できた | HLA の入力と出力、各 LLA のフレーム型 |
| Saleae 拡張 | [共有 HLA](https://www.saleae.com/support/extensions-api/extensions/shared-high-level-analyzers-hlas)、[コミュニティ共有 LLA](https://www.saleae.com/support/community-contact/community-shared-protocols) | 取得できた | 共有 HLA 約 120、共有 LLA 55。アプリ内 Marketplace の全件は Web から見えない（**未確認**） |
| Pico serial decoding | https://www.picotech.com/library/knowledge-bases/oscilloscopes/serial-bus-decoding-protocol-analysis | 取得できた（2026-01-15 更新版） | 対応方式の一覧と「PicoScope 7 serial decoding color key」（2.3 節） |

## 2. 3 つのカタログの要点

### 2.1 sigrok

- 131 種のうち、**線から直接復号するもの（`inputs = ['logic']`）が 67、他の解析器の出力を受けるものが 64**。後者の内訳は spi の上 22、i2c の上 17、uart の上 10、onewire_network の上 3、jtag の上 2、ook の上 2、その他 1 つずつ（onewire_link → onewire_network、microwire → eeprom93xx、mdio → cfp、lfast → sipi、pjon_link → pjon、usb_signalling → usb_packet → usb_request）。
- 上の 64 の大半は**特定 IC 1 種のレジスタ解釈**（ad79x0、lm75、nrf24l01、cc1101、tca6408a 等）で、プロトコルではなく機器ドライバの読み替えである。
- 分類 `tags` は Embedded/industrial、Automotive、Debug/trace、Retro computing 等。Retro computing（z80、mcs48、maple_bus、ieee488）が独立した分類として存在する。
- **無いもの**: I³C、QSPI（spi は 1 bit 幅のみ）、SMBus/PMBus（i2c の上にも無い）、SENT、PSI5、NMEA、HD44780、eSPI、RFFE、SPMI、Manchester 汎用（miller のみ）、CAN の上位（`can` は `outputs = ['can']` を宣言するが、受ける解析器が 1 つも無い）。
- 開発の勢い: 本体リポジトリの最新コミットが 2024-10 であり、新しい方式（I³C、CAN XL、eSPI 等）の追加は止まっている（**解釈**）。WCH RVSWD は本体に無く、第三者の [sigrok-rvswd](https://github.com/perigoso/sigrok-rvswd) がある。

### 2.2 Saleae

- 標準解析器: 1-Wire、Async Serial、Atmel SWI、BISS-C、CAN、DMX512、HD44780、HDLC、HDMI-CEC、I2C、I2S/PCM、JTAG、LIN、Manchester（差動・Bi-Phase Mark/Space）、MDIO、MIDI、Modbus（RTU/ASCII）、PS/2、SMBus（**PMBus と Smart Battery を含む**）、SPI、SWD、Simple/Synchronous Parallel、USB LS/FS、Addressable LEDs（WS2812 系）。I3C は Binho の有償の第三者製。
- **2 段の構成**: 低位解析器（LLA）は C++ の共有ライブラリで、サンプルを読んでフレームを作る（「an experienced C++ developer at least a full day, and possibly up to a week」）。高位解析器（HLA）は Python で、「process the output of the existing protocol analyzers」。HLA は生サンプルを読めない。
- HLA の入力は **LLA 1 つだけ**で、HLA の出力を別の HLA の入力にはできない（[フォーラム](https://discuss.saleae.com/t/hla-as-input-analyzer-for-other-hla/2663)、[機能要望: HLA の入力に HLA](https://ideas.saleae.com/b/feature-requests/hla-support-input-from-another-hla/)、[機能要望: 複数 LLA の入力](https://ideas.saleae.com/b/feature-requests/high-level-analyzer-combine-data-from-multiple-sources/)。最新版で解消したかは**未確認**）。
- 拡張の傾向: 共有 LLA には標準に無い**線の方式**（CAN-FD、CAN-XL、QSPI、SDIO、SD/MMC、S/PDIF、TDM、SENT、ARINC 429、LPC、MIPI RFFE v1/v2、I3C、SDQ、Wiegand、PWM、Quadrature）が並ぶ。共有 HLA は**上位の解釈**（SPI Flash、SDMMC from SPI、EEPROM、I2C Transactions、SPI transaction framer、COBS、TPIU/ITM、LIN Data、SocketCAN、各種 IC）が並ぶ。USB PD（BMC）と NEC IR は、LLA ではなく HLA として作られた例がある（HLA が別の LLA 出力、例えば Manchester や Simple Parallel を入力にする形と思われる。**未確認**）。

### 2.3 Pico Technology

- PicoScope の対応: 1-Wire、ARINC 429、BroadR-Reach（100BASE-T1）、CAN・CAN FD、CAN J1939、CAN XL、DALI、DCC、DMX512、Ethernet 10BASE-T・100BASE-TX、FlexRay、I²C、I²S、I3C、LIN、Manchester、MIL-STD-1553、Modbus（ASCII/RTU）、Parallel Bus、PMBus、PS/2、Quadrature、SENT Fast/Slow、SMBus、SBS Data、SPI-MISO/MOSI、SPI-SDIO、UART（RS-232/422/485）、USB、Wind Sensor。同じ知識ベースの目次には、ほかに PSI5、SENT SPC、isoSPI、10BASE-T1S、NMEA-0183、NMEA-2000、Differential Manchester、Extended UART の記事がある。
- オシロスコープのデコードなので、**車載・航空・物理層寄り**（CAN XL、SENT、PSI5、FlexRay、ARINC、1553、Ethernet PHY）が厚く、デバッグ線（JTAG、SWD）は無い。
- 「SPI-SDIO」は SD カードの SDIO ではなく、データ 1 本を双方向に使う 3 線 SPI を指すと読める（**解釈、未確認**）。

## 3. Pico の色分けと区切り方の分類

Pico の記事にある分類は、厳密には**フレームの区切り方の分類ではなく、デコード結果の「欄（フィールド）」の色分け**（PicoScope 7 serial decoding color key）である。表の「Meaning」列は次の 15 種で、各行の「Packet example」は画像（alt 文）で例示されている。例示の対応は画像の並び順から読んだもの（**解釈**）。

| # | Meaning（原文） | 画像の例示 |
| --- | --- | --- |
| 1 | Header | FlexRay の Header |
| 2 | Payload / Data | Data |
| 3 | Parity | CAN FD の Parity |
| 4 | CRC / Checksum | USB の CRC5 |
| 5 | Bit Stuffing | CAN FD の Stuffed Bit |
| 6 | Start Bit | UART の Start Bit |
| 7 | Stop Bit | UART の Stop Bit |
| 8 | Sync | USB の Sync |
| 9 | Packet Type | I²C の Write |
| 10 | Address | I²C の address |
| 11 | Break | CAN FD の End-of-Frame |
| 12 | Ack | CAN FD の ACK |
| 13 | Reserved / Delimiter | CAN FD の Reserved |
| 14 | Preamble | DCC の Preamble |
| 15 | Control Pair | I²C の Data ACK |

このうち**単位の始まりと終わりを示す欄**（6、7、8、11、13、14、および 5 のスタッフィング違反による終端）を、層の調査の F 層の区切り方へ対応付けると次のようになる。Pico の分類には CS と START/STOP 条件の欄が無く、別線の区切りは色分けの対象外である（**解釈**）。

| WireSkein の区切り型 | 決め手 | Pico の欄 | 方式の例 |
| --- | --- | --- | --- |
| CS 区切り（別線の選択信号） | CS の活動区間 | —（欄なし） | SPI、QSPI、Microwire、eSPI、RS-485 の DE、8080 の CS# |
| START/STOP 条件区切り（クロック H 中のデータ変化） | 線間の遷移の順序 | —（I²C の Address/Packet Type/Ack で間接に表す） | I²C、SMBus、I³C、RVSWD、HDMI の DDC |
| 文字のスタート/ストップビット（非同期） | アイドルからの最初の反転 | Start Bit、Stop Bit | UART、LIN、DMX512、MIDI、Modbus、Profibus、PS/2（同期だが 11 bit 枠） |
| アイドル間隔区切り（無音時間） | 一定時間以上の無変化 | —（Modbus RTU の 3.5 文字は欄として出ない） | Modbus RTU、UART のパケット化、SWIO、WS2812 のリセット |
| ブレーク区切り（規定長以上の優性・L） | 長い L | Break（例示は CAN FD の EOF） | LIN、DMX512、UART ブレーク、1-Wire のリセット、SENT の同期パルス |
| SYNC・プリアンブル（既知パターン）＋固定長／長さ欄 | パターン照合と長さ | Sync、Preamble、Header、Reserved/Delimiter | USB の SYNC＋EOP、MDIO の 32 bit プリアンブル、DCC、FlexRay、CAN の SOF＋DLC、SD の start bit＋48 bit |
| 自己同期のフレーム（スタッフィング・符号違反で区切る） | bit stuffing、符号則の違反 | Bit Stuffing、Reserved/Delimiter（EOF） | CAN、USB（EOP の SE0）、HDLC フラグ 0x7E、DALI の stop（符号違反） |
| 連続（区切りなし、別線の周期信号で枠を作る） | WS/FSYNC の周期 | — | I²S、TDM、PDM（枠なし）、Parallel |
| 状態機械で区切る | 制御線の状態遷移 | — | JTAG（TMS で Shift-IR/DR）、SWD（ラインリセットと固定長要求） |

## 4. 優先度の表

優先度: **A** ＝ 最初に対応（推定の核になる共通段を育てる方式、または WireSkein の用途で必須）、**B** ＝ A の共通段が揃った後に追加、**C** ＝ 要望があれば。★ はプロトタイプで実装済み（[プロトタイプの README](../prototype/README.ja.md)）。

### 4.1 汎用のバス

| 方式 | 分類（線・同期・区切り） | 今の利用状況 | 掲載 | 上位の積み重なり | 優先度 |
| --- | --- | --- | --- | --- | --- |
| UART（RS-232/485 含む）★ | 1 線、非同期 NRZ、スタート/ストップ | 高: MCU のログ・AT・書込みで標準 | sr SA P | テキスト行、Modbus、NMEA、SLIP/COBS、MAVLink、LIN、DMX（層の調査 3.1） | A |
| I²C ★ | 2 線 OD、同期、START/STOP | 高: センサ・EEPROM・PMIC で標準 | sr SA P | レジスタマップ、EEPROM 24xx、SMBus → PMBus | A |
| SMBus / PMBus / SBS | I²C と同じ＋ PEC | 高: サーバ電源・バッテリ・VR | SA（SMBus に同梱） P | I²C トランザクションへの注釈（層の調査 4.1） | A（注釈として） |
| SMBALERT# | 1 線の割込み（レベル） | 中: SMBus に付随 | — | 直後の ARA（0x0C）読み出しとの対応付け | B |
| SPI ★ | 3〜4 線、同期、CS 区切り | 高: フラッシュ・表示・ADC・無線 IC | sr SA P | SPI NOR、SD（SPI）、表示、機器レジスタ | A |
| QSPI / OSPI（Dual/Quad/Octal） | 4〜8 データ線、同期、CS 区切り、線幅が途中で変わる | 高: 外付けフラッシュ・XIP、PSRAM | SX（QSPI） | SPI NOR 命令表（1-1-4、1-4-4、4-4-4、DTR） | B |
| I³C | 2 線（SCL PP）、同期、START/STOP、9 bit 目がパリティ | 中（増加中）: 新しいセンサ・DDR5 SPD ハブ | SA（有償の第三者） SX P | CCC（ブロードキャスト 0x7E）、I²C との同居 | B |
| 1-Wire | 1 線 OD、時間スロット、リセットパルス区切り | 中: DS18B20、認証 IC | sr SA P | ROM コマンド → 機器（DS28EA00 等） | B |
| UNI/O（Microchip 11XX） | 1 線、マンチェスタ、ヘッダ 0x55 | 低: 採用製品が少ない | Saleae 旧 SDK の例のみ（**未確認**） | EEPROM | C |
| Microwire / 93Cxx | SPI 相当、CS 区切り（CS はアクティブ H） | 低: 旧設計の EEPROM | sr | eeprom93xx | C（SPI の変種として） |

### 4.2 デバッグ・書込み

| 方式 | 分類 | 今の利用状況 | 掲載 | 上位の積み重なり | 優先度 |
| --- | --- | --- | --- | --- | --- |
| ARM SWD | 2 線（双方向）、同期、固定長要求＋ACK | 高: Cortex-M の標準 | sr SA | ADIv5/v6 DP → AP → MEM-AP → メモリ | A |
| JTAG（IEEE 1149.1） | 4〜5 線、同期、TMS の状態機械 | 高: FPGA・SoC・RISC-V | sr SA | TAP → IDCODE、ARM JTAG-DP、RISC-V DTM → DM、STM32/EJTAG | A |
| JTAG 境界スキャン | JTAG の上 | 中: 実装検査 | —（sr・SA とも TAP 止まり） | BSDL による端子の意味付け | C |
| cJTAG（IEEE 1149.7） | 2 線（TCKC/TMSC）、同期、スキャン形式 | 低〜中: 一部 SoC、ESP32 系・TI 等（**未確認**） | sr | JTAG へ変換して同じ上位 | C |
| WCH RVSWD ★ | 2 線、同期、START/STOP＋固定長 53/85 | 中（本プロジェクトでは高）: CH32V の書込み | 第三者 sigrok | DMI → RISC-V DM → 文字チャネル（層の調査 3.6） | A |
| WCH SWIO（SDI） ★ | 1 線 OD、パルス幅、アイドル区切り | 中（本プロジェクトでは高）: CH32V003 等 | — | 同上 | A |
| SWO / ITM | 1 線、UART（NRZ）かマンチェスタ | 中: printf・PC サンプリング | sr（arm_tpiu → arm_itm） SX（TPIU/ITMDWT の HLA） | TPIU の枠 → ITM/DWT パケット | B |
| STM8 SWIM | 1 線 OD、パルス幅 | 低: STM8 は新規採用が少ない | sr | STM8 のメモリアクセス | C |

### 4.3 車載・産業・計装

| 方式 | 分類 | 今の利用状況 | 掲載 | 上位の積み重なり | 優先度 |
| --- | --- | --- | --- | --- | --- |
| CAN / CAN FD | 1 線（RXD）、自己同期 NRZ＋スタッフィング、SOF＋長さ | 高: 車載・産業で標準 | sr（FD の高速側は `fast_bitrate`） SA（2.0 のみ） SX（CAN-FD） P | CANopen、J1939、ISO-TP → UDS → OBD-II（sr・SA とも上位は無い） | A |
| CAN XL | CAN FD と同じ系、データ相が PWM 符号化 | 低（立ち上がり中）: 次世代車載 | SX P | CAN と同じ上位＋ SDU 種別 | C |
| LIN ★ | 1 線、UART＋ブレーク＋0x55 | 高: 車載のボディ系 | sr SA P | 診断（LIN TP） | A（実装済み、UART の上） |
| FlexRay | 差動 2 線（RXD）、NRZ、TSS/FSS/BSS | 低: 新規採用は Ethernet と CAN FD へ移行 | sr P | — | C |
| SENT（SAE J2716） | 1 線、立ち下がり間隔、同期パルス区切り | 中: 車載センサ（位置・圧力） | SX P | Slow/Enhanced serial message（複数フレームにまたがる） | B |
| PSI5 | 2 線電流変調（受信後はマンチェスタ） | 中: エアバッグ等の車載センサ | P | — | C |
| SAE J1850 VPW | 1 線、パルス幅 | 低: 2008 年以前の米国車 OBD | sr | — | C |
| Modbus RTU/ASCII | UART の上、アイドル間隔区切り | 高: 産業機器で現役 | sr SA P | 機能コード＋CRC-16 | B（UART の上位） |
| Profibus DP | RS-485、UART 11 bit（偶数パリティ） | 中→低: PROFINET へ移行中 | — | FDL 電文（SD1〜SD4） | C |
| DMX512 ★ | UART 250 kbaud＋ブレーク | 中: 舞台照明で現役 | sr SA P | RDM（E1.20、SX に HLA） | A（実装済み） |
| DALI / DALI-2 | 1 線、マンチェスタ 1200 bit/s | 中: 照明制御 | sr P | コマンド表 | B |
| ARINC 429 | 差動 3 値（RZ 双極） | 低（分野では高）: 航空 | SX P | ラベル 8 bit＋SDI＋データ＋SSM | C |
| MIL-STD-1553 | 差動、マンチェスタ 1 Mbit/s、3 bit 長の同期 | 低（分野では高）: 防衛 | P（sr は「soon」のまま） | コマンド／状態語 | C |
| NMEA 0183 | UART 4800/9600 の上、テキスト行 | 高: GNSS モジュール | P | $GPxxx 文、XOR 検算 | B（UART の上位） |
| Manchester（汎用） | 1 線、自己同期、2 値ラン | 中: 多くの方式の部品 | SA P（sr は miller のみ） | RC5、DALI、1553、SWO、UNI/O の下 | B（共通段として） |

### 4.4 オーディオ・ストレージ・PC・電源

| 方式 | 分類 | 今の利用状況 | 掲載 | 上位の積み重なり | 優先度 |
| --- | --- | --- | --- | --- | --- |
| I²S / TDM | 3 線、同期、連続（WS・FSYNC が枠） | 高: オーディオ codec・MEMS マイク | sr SA P（TDM は sr、SX） | サンプル列（PCM） | B |
| PDM | 2 線、同期、枠なし | 中: MEMS マイク | — | 間引きフィルタで PCM | C |
| S/PDIF | 1 線、bi-phase mark、プリアンブル区切り | 低〜中: 家電の光・同軸 | sr SX | サブフレーム → チャネル状態 | C |
| SD / SDIO / eMMC | CLK＋CMD＋DAT0〜3（〜7）、同期、start bit＋固定長 48 bit | 高: SD カード・Wi-Fi モジュール・eMMC | sr（SD モード） SX（SD/MMC、SDIO） | CMD/応答 → ブロック転送、SDIO の CMD52/53 | B |
| SD（SPI モード） | SPI の上 | 中: MCU からの SD | sr SX（HLA） | 同上 | B（SPI の上位） |
| MDIO（Clause 22/45） | 2 線、同期、プリアンブル区切り | 高: Ethernet PHY | sr SA | PHY レジスタ | B |
| RMII / RGMII | 並列 2〜4 bit、50/125 MHz | 高（ただし観測は難しい） | — | Ethernet フレーム | C（**注**: 一般のロジックアナライザでは速度が足りず、RGMII は DDR。対象外に近い） |
| USB 1.1 LS/FS | D+/D−、NRZI＋スタッフィング、SYNC＋EOP | 中: HID・CDC の機器（HS は速度的に対象外） | sr（signalling → packet → request） SA SX P | パケット → トランザクション → 標準要求・記述子 | B |
| USB PD（CC の BMC） | 1 線、BMC 300 kbit/s、プリアンブル＋SOP 符号 | 高: USB-C 給電で標準 | sr SX（HLA） | 電力交渉メッセージ（Source_Capabilities 等） | B |
| eSPI / LPC | eSPI は SPI 系（CS 区切り、1/2/4 線）、LPC は 4 bit 並列＋LFRAME# | 中（PC の EC・BMC）: LPC は eSPI へ移行 | sr（LPC） SX（LPC） | I/O・メモリ・仮想線、EC のポート 0x80 | C |
| SVID / AVSBus | 3 線（CLK・DATA・ALERT）、同期 | 中（分野では高）: CPU・ASIC の電圧制御 | — | VID 設定・状態 | C |
| MIPI RFFE | 2 線（SCLK・SDATA）、同期、SSC パターン区切り | 中: 携帯の RF フロントエンド | SX | レジスタ書込み | C |
| SPMI | 2 線、同期、SSC 区切り（RFFE と近い） | 中: スマホ・SoC の PMIC | — | PMIC レジスタ | C |
| I²C-HID | I²C の上 | 中: タッチパッド・タッチパネル | — | HID 記述子・入力レポート | C（I²C の上位） |
| Parallel / 8080・6800 バス | 8/16 bit 並列、WR#/RD# ストローブ区切り | 中: 表示モジュール、FPGA 間 | sr SA P | 表示コマンド（ST77xx 等） | B（共通段として） |
| HD44780 | 4/8 bit 並列、E ストローブ | 中: 文字 LCD（趣味・保守） | SA | LCD 命令 | C |

### 4.5 1 線の記号・民生

| 方式 | 分類 | 今の利用状況 | 掲載 | 上位の積み重なり | 優先度 |
| --- | --- | --- | --- | --- | --- |
| WS2812 / SK6812 | 1 線、パルス幅 2 値、リセット（L 50〜280 µs）区切り | 高: アドレサブル LED | sr SA | 画素列（GRB/GRBW） | A（パルス幅段の最初の利用者） |
| IR（NEC / RC5 / SIRC） | 受光後 1 線、パルス間隔・マンチェスタ・パルス幅 | 高: 家電リモコン | sr SX（NEC の HLA と InfraRed LLA） | コマンド表 | B |
| HDMI-CEC | 1 線 OD、パルス幅、ms 単位 | 中: TV・AV 機器 | sr SA | CEC メッセージ（opcode） | C |
| PS/2 | 2 線 OC、機器クロック、11 bit 枠 | 低: レガシー（USB へ移行） | sr SA P | スキャンコード | C |
| MIDI（5 pin DIN） | UART 31.25 kbaud の上 | 中: 楽器（USB-MIDI が増えたが DIN も現役） | sr SA | ステータスバイト・SysEx | B（UART の上位） |

### 4.6 sigrok の一覧で優先度を下げるもの

「今も使われているか」で見て、次は C（または対象外）とする。対象の IC がまだ流通していても、**その解析器の中身が 1 機器のレジスタ表**であり、共通段の設計には効かないものも C にした。

| 解析器 | 理由 |
| --- | --- |
| z80、mcs48、maple_bus、nes_gamepad、ieee488 | sigrok 自身が Retro computing と分類。レトロ機器・旧計測器 |
| ac97 | PC オーディオは HD Audio へ移行済み |
| aud（Renesas AUD）、avr_pdi（ATxmega）、arm_etmv3 | 旧世代のデバッグ・トレース。ETMv3 は ARM7/9・初期 Cortex で、今は ETMv4/CoreSight をトレースプローブで取る |
| sae_j1850_vpw | 2008 年以降の OBD は CAN（ISO 15765）に統一 |
| xfp、cfp | 光トランシーバの旧形式（XFP、CFP）。管理は I²C・MDIO の上の解釈 |
| sda2506、x2444m、sle44xx、eeprom93xx | 旧式のメモリ IC・メモリカード |
| rfm12、nrf905、mrf24j40、pan1321、adns5020、amulet_ascii、max7219 系、st7735、lm75 等 | 1 機器のレジスタ表。現行品もあるが上位の「機器パック」として後で扱う |
| am230x（DHT11/22） | 趣味用途では現役だが方式は独自の 1 線パルス幅。パルス幅段ができれば安く足せる |
| dcf77、morse、caliper、rc_encode（PT2262） | 用途が限られる。rc_encode は 433 MHz の安価なリモコンで現役（**解釈**） |
| lfast、sipi | NXP MPC57xx の MCU 間高速リンク。分野が狭い |
| hdcp、edid | edid（DDC、I²C の上）は現役で B 相当、hdcp は内容が暗号化されていて解析の意味が薄い |

## 5. 積み重なりの深さ

### 5.1 sigrok の実例

`pd.py` の `inputs`/`outputs` から組んだ実際の積み重なり。深さは「線から直接読む解析器」を 1 と数える。

| 積み重なり | 深さ | 中身 |
| --- | --- | --- |
| i2c → eeprom24xx（ほか lm75、ds1307、edid、mcp230xx 等、i2c の上は計 17 種） | 2 | 2 段目は機器のレジスタ解釈 |
| spi → spiflash、sdcard_spi（ほか nrf24l01、cc1101、ad79x0 等、spi の上は計 22 種） | 2 | 同上 |
| uart → modbus、midi、lin、dmx512、sbus_futaba、pan1321、amulet_ascii | 2 | 2 段目が「プロトコル」として働く唯一のまとまり（**解釈**） |
| uart → arm_tpiu → arm_itm / arm_etmv3 | 3 | tpiu は `uart` を受けて `uart` を出す**置き換え**の段（ストリームの ID で振り分け） |
| i2c → i2cfilter → 任意の i2c 上位 | 3 | filter は同じ型を出す**絞り込み**の段 |
| onewire_link → onewire_network → ds28ea00 / ds2408 / ds243x | 3 | リンク（リセット・bit）→ ネットワーク（ROM コマンド）→ 機器 |
| usb_signalling → usb_packet → usb_request | 3 | 線の状態・bit → パケット → トランザクション／要求。**sigrok で最も深い正規の積み重なり** |
| jtag → jtag_stm32 / jtag_ejtag、cjtag → （jtag と同じ出力型） | 2 | cjtag は `jtag` を出すので jtag の上位をそのまま使う（**transport の置き換え**） |
| swd | 1 | `outputs = ['swd']` を宣言するが受ける解析器は無い。ADIv5 の AP・メモリまでは解釈しない |
| can | 1 | 同上（CANopen、J1939、ISO-TP は無い） |
| microwire → eeprom93xx、mdio → cfp、lfast → sipi、pjdl → pjon、ook → ook_oregon | 2 | — |

結論: **sigrok の階層は浅い**という観察は正しい。正規の深さは最大 3（USB、1-Wire）、それ以外は 2 で止まり、しかも 2 段目の大半は機器のレジスタ表である。filter・tpiu のような「同じ型を出す段」を挟むと 3 になるが、意味の層は増えていない。ISO-TP → UDS、ADIv5 → MEM-AP、SMBus → PMBus、RISC-V DMI → DM のような**実務で欲しい 3〜5 段目は、sigrok 本体にほぼ無い**。

### 5.2 Saleae の実例

- 深さは**固定で 2**（LLA → HLA）。HLA の入力は LLA 1 つで、HLA の上に HLA を積めない（2.2 節）。そのため共有 HLA には「I2C Transactions」「SPI transaction framer」「Concatenator」「Time Delimiter Chunking」「Text Messages」のように、**上位の前に区切りを作り直すだけの HLA** が多数あり、利用者はそれを上位の HLA の中に再実装している（**解釈**）。
- 深さの不足は利用者からも要望されている。例: 「[SPI] → [TPM SPI（バス上の単純なレジスタ読み書き）] → [状態・FIFO 水位] → [個々の TPM コマンド]」の 4 段（[機能要望](https://ideas.saleae.com/b/feature-requests/hla-support-input-from-another-hla/)）。
- 旧 SDK は Frame → Packet → Transaction の 3 段の集約を予定していたが実装されず、「in favor of FrameV2 and HLAs」となった（[Analyzer_API.md](https://github.com/saleae/SampleAnalyzer/blob/master/docs/Analyzer_API.md)）。Packet は CSV 出力でのみ使われる（SPI は enable 区間、I²C は START〜STOP、CAN は CAN パケット）。

### 5.3 WireSkein の層の切り方への示唆

| 分け方 | 段 | 方式を問わず共通か |
| --- | --- | --- |
| 線の段（共通前処理） | エッジ、アイドル、ラン幅、クロック様・パルス幅様の判定、区切りの候補（3 節の 9 型） | 共通。**区切りの型が共通化の単位**になる |
| 記号の段（共通カーネル） | 同期ビット列、パルス幅記号列、マンチェスタ/NRZI＋スタッフィング、時刻付きバイト列（RateBlocks → Chars） | 共通。1 つのカーネルを複数の方式が使う（層の調査 4.1） |
| フレーマ（方式固有） | I²C の 9 bit 枠、SPI の CS 区間、CAN の SOF〜EOF、SWD の要求、RVSWD の 53/85、USB の SYNC〜EOP | **方式固有**。ただし出力は「フレーム（時刻・欄・誤り）」の共通型 |
| 集約の段（共通） | トランザクション（番地・方向・値・応答）、パケット化（無音・区切りバイト・長さ欄）、filter・demux | 共通。Saleae の共有 HLA の多くはここにあたる。**深さを増やさず注釈で重ねる** |
| 上位（方式固有・機器固有） | Modbus、PMBus、SPI NOR、ADIv5 AP、RISC-V DM、ISO-TP → UDS、機器のレジスタ表 | 固有。機器表は「機器パック」としてデータで持つ（**解釈**） |

- sigrok の cjtag → jtag、tpiu → uart のように、**同じ出力型を出す transport の置き換え**を許すと、上位を使い回せる。RVSWD・SWIO・JTAG-DTM → DMI はこの形である。
- Saleae の制約（HLA が 1 入力、HLA の上に積めない）は避ける。**任意の深さと複数入力**（TX/RX の組、SMBALERT# と I²C、D/C# 線と SPI）を型で受け渡せるようにする。

## 6. 次の消費者が欲しいもの（出力の粒度）

| 観点 | sigrok | Saleae | Pico |
| --- | --- | --- | --- |
| 人が見る結果 | **注釈**（OUTPUT_ANN）。解析器は注釈クラス（`annotations`）を宣言し、それを**注釈行**（`annotation_rows`）にまとめる。例: i2c は Bits／Address・data／Warnings の 3 行、uart は RX・TX それぞれ bits／data／warnings／breaks／packets | **バブル**（LLA は `GenerateBubbleText` で長短複数の文字列を返し、幅に応じて選ばれる。HLA は `result_types` の format 文字列）。**データ表**は FrameV2 のキーが列になる | In Graph（波形に並ぶバス表示、誤りは赤）と In Table（フレームの一覧） |
| 次の解析器への受け渡し | **OUTPUT_PYTHON**。型の約束は各解析器の docstring（i2c は `['START'…]`、`['ADDRESS READ', addr]`、`['DATA WRITE', byte]` 等の列） | LLA の **FrameV2**（型名＋キーと値）を HLA が `decode(frame)` で受ける。フレーム型は解析器ごとに文書化（I²C: start/address/data/stop、SPI: enable/result/disable/error、Async Serial: data＋error・address） | 無し（デコーダは固定） |
| 最終データの取り出し | **OUTPUT_BINARY**（24 解析器が宣言。uart の rx/tx/rxtx ダンプ、i2c の address/data、spi の MOSI/MISO 等）。sigrok-cli では `-B` で選ぶ | データ表の CSV 出力、HLA から外部へ送る例（SocketCAN、Socket Transport） | 表の表計算形式への書き出し |
| 欄の選択 | sigrok-cli の `-A i2c=data-read:data-write,edid` のように、解析器ごとに注釈クラスか注釈行を選ぶ（行名はクラスの列に展開され、混在可、曖昧ならクラス名が優先。`man sigrok-cli`）。`-M` で OUTPUT_META（統計値、5 解析器） | データ表の列の表示切替、検索 | Fields で列を選ぶ、Filter／Advanced Filter の「Index of」で欄の中の文字位置を抜き出して別列にする、Link file で値を名前へ |
| 旧式の構造 | — | Frame（`mData1`/`mData2` の 64 bit 2 つ、`mType`、`mFlags`。誤り・警告は予約フラグ）。FrameV2 を使わない LLA は HLA から使えない | — |

WireSkein の出力モードへの示唆（**解釈**）:

- 3 つとも、**同じ結果を「人が見る注釈」「次の段への型付きデータ」「最終データの取り出し」の 3 種に分けて出す**点で一致している。WireSkein でも段の出力を 1 つの型付きストリームにし、GUI の行表示・CLI の抽出・上位の入力を、その射影として作る。
- GUI は全段を行として並べる（sigrok の注釈行、Pico の In Graph）。CLI は既定で**最上位の最終データ**（sigrok の `-B` に相当）を出し、`--layer` と欄の指定（sigrok の `-A`、Pico の Fields・Index of）で途中の段や特定の欄を選べるようにする。
- Saleae の FrameV2（型名＋キーと値）は、欄の選択と表への展開がしやすい。フレームの欄に Pico の 15 種の色分け（Header、Address、CRC、Stuffing …）に相当する**欄の役割**を付けておくと、GUI の色分けと、推定の根拠（「CRC が合った」「Ack の位置が合った」）の表示を同じ情報から作れる。
- 誤りは sigrok では別の注釈行、Saleae ではフラグ（バブルの色）であり、どちらも「結果を捨てずに印を付ける」。推定では誤りの密度自体が反証のスコアになるので、段の出力に誤りの印を必ず残す。

## 7. 未確認のまま残した点

- Saleae のアプリ内 Marketplace の全件（Web の共有 HLA 一覧と一致するか）、HLA の上に HLA を積めない制約が最新版で解消されたか。
- USB PD（BMC）の HLA と NEC IR の HLA が、どの LLA の出力を入力にしているか。
- Pico の色分けの「Break」「Control Pair」の正確な意味（画像の例示は CAN FD の EOF と I²C の Data ACK）。「SPI-SDIO」の意味。Pico の USB デコードが LS/FS だけか。
- cJTAG の採用状況、SVID・SPMI の公開仕様の範囲（どちらも仕様は会員向け）。
