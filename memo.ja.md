# WireSkein Core Specification Draft

## 1. 概要

WireSkein は、デジタル信号を取得・解析し、信号上に存在する通信プロトコルを自動検出・復号・階層化して扱うためのロジックアナライザ基盤である。

従来のロジックアナライザのように、ユーザーが事前に通信方式・通信速度・チャンネル割り当て・上位プロトコルをすべて指定することを前提としない。

WireSkein は入力信号から特徴を抽出し、複数の通信方式について仮説を生成し、それぞれの確度を評価しながら、可能なデコード経路を動的に構築する。

代表的な解析例:

```text
Digital Signal
    ↓
UART
    ↓
Byte Stream
    ↓
ASCII
    ↓
SCPI
```

また、通信条件がキャプチャ途中で変化する場合にも対応する。

```text
UART
  0 ms  - 18 ms : 115200 baud
 18 ms  - 40 ms : 230400 baud
```

WireSkein の中心概念は、単純な Decoder の集合ではなく、

```text
Signal
  ↓
Features
  ↓
Hypotheses
  ↓
Typed Streams
  ↓
Decoder Graph
```

として信号を段階的に解釈することである。

---

# 2. 目的

WireSkein Core は以下を目的とする。

- デジタル信号から通信プロトコルを自動検出する
- 通信パラメータを可能な限り自動推定する
- 通信条件の時間的変化を追跡する
- 複数段のプロトコルデコードを連鎖させる
- 複数の解釈候補を同時に保持する
- 各解釈に信頼度を付与する
- デコーダーを階層的に組み合わせる
- GUI、CLI、その他のフロントエンドから同一の解析結果を利用できる
- キャプチャデバイスやデコーダーを後から追加できる
- 大規模なキャプチャデータを部分的に解析できる
- デコード結果を機械可読なデータとして取得できる

---

# 3. 非目的

Core 自体は以下を直接担当しない。

- GUIの描画
- ウィンドウ管理
- テーマ
- パネル配置
- 波形ビューのスクロール位置
- CLIの表示形式
- 製品固有の操作体系

これらは Core の解析結果を利用するフロントエンド側の責務とする。

---

# 4. 基本概念

WireSkein Core は以下の主要概念から構成される。

```text
Capture
Channel
Sample
Feature
Stream
Event
Detector
Decoder
Hypothesis
Parameter
Parameter Timeline
Decoder Graph
Annotation
```

---

# 5. Capture

Capture は、ある期間に取得された信号データ全体を表す。

Capture は以下の情報を持つ。

```text
Capture
├─ Sample Rate
├─ Start Time
├─ Duration
├─ Channels
└─ Metadata
```

Capture 内の時間位置は、原則として絶対時間ではなく Sample Index を基準として表現する。

例:

```text
sample 0
sample 1
sample 2
...
sample 18472831
```

必要に応じて Sample Index から時間へ変換できる。

---

# 6. Channel

Channel は入力信号源を表す。

例:

```text
CH0
CH1
CH2
CH3
```

Channel は名前や役割が事前に決まっているとは限らない。

WireSkein は解析結果から、以下のような役割を推定できる。

```text
CH0 : UART TX candidate
CH1 : UART RX candidate

CH2 : SPI CLK candidate
CH3 : SPI MOSI candidate
CH4 : SPI MISO candidate
CH5 : SPI CS candidate
```

一つの Channel に複数の候補役割が存在してもよい。

---

# 7. Sample Range

解析対象となる区間は Sample Range として表現する。

```text
[start_sample, end_sample)
```

すべての解析結果は、可能な限り有効範囲を持つ。

例:

```text
UART hypothesis

range:
  sample 120000 .. 980000
```

これにより、同じチャンネルで途中から異なる通信方式になるケースを扱える。

---

# 8. Feature

Feature は Raw Signal から抽出される、プロトコルに依存しない特徴量である。

代表例:

```text
edge positions
edge intervals
pulse widths
duty cycle
idle ratio
periodicity
transition density
burst structure
channel correlation
frequency estimate
```

Feature は Detector がプロトコル候補を判断するために利用する。

同一の Feature を複数の Detector から共有できることを前提とする。

---

# 9. Detector

Detector は入力データから「何のプロトコルである可能性があるか」を判定する。

Detector は確定的なデコード結果ではなく、Hypothesis を生成する。

例:

```text
UART Detector
```

入力:

```text
Digital Signal
```

出力:

```text
Hypothesis A
protocol: UART
baud: 115200
format: 8N1
confidence: 0.98
```

同時に複数候補を生成してよい。

```text
UART 115200 8N1 : 0.98
UART 57600 8N1  : 0.21
Manchester       : 0.07
```

Detector は最終的な判断を単独で行うものではない。

---

# 10. Hypothesis

Hypothesis は「この範囲の信号を、この条件で解釈できる」という仮説を表す。

Hypothesis は少なくとも以下を持つ。

```text
Protocol
Input
Sample Range
Parameters
Confidence
Evidence
```

概念例:

```text
Hypothesis
  protocol: UART
  channel: CH2

  range:
    120000 .. 950000

  parameters:
    baud: 115200
    data_bits: 8
    parity: none
    stop_bits: 1

  confidence:
    0.982
```

Hypothesis は後続の解析結果によって、

- confidence が上がる
- confidence が下がる
- 有効区間が短くなる
- パラメータが修正される
- 別の Hypothesis に置き換わる

可能性がある。

---

# 11. Confidence

解析結果には可能な限り Confidence を付与する。

概念上は次の範囲で表現する。

```text
0.0 - 1.0
```

例:

```text
UART       0.98
ASCII      0.96
SCPI       0.93
Modbus RTU 0.08
```

Confidence は単純な「成功 / 失敗」を置き換えるためのものである。

WireSkein は、曖昧な入力に対しても複数の可能性を保持できる。

Confidence は UI 上の表示だけではなく、Decoder Graph の探索にも利用される。

---

# 12. Evidence

Hypothesis が成立した根拠を保持できる。

例:

```text
UART hypothesis

evidence:
  stable pulse-width base unit
  valid start bits
  low framing error
  high idle-level consistency
```

上位プロトコルの場合:

```text
SCPI hypothesis

evidence:
  printable ASCII ratio: high
  command terminator detected
  "*IDN?" pattern detected
  hierarchical command syntax detected
```

Evidence は解析結果の説明可能性を高める。

---

# 13. Parameter

Decoder が利用する通信条件は Parameter として扱う。

例:

```text
baud_rate
data_bits
parity
stop_bits

clock_polarity
clock_phase
bit_order

address_width
word_size
```

Parameter は以下の状態を取り得る。

```text
Unknown
Estimated
Detected
User Specified
```

また、値だけでなく推定誤差や Confidence を持つことができる。

例:

```text
baud:
  value: 115180
  uncertainty: ±300
  confidence: 0.94
```

---

# 14. Parameter Timeline

Parameter は Capture 全体で一定とは限らない。

そのため Parameter は時間方向に変化可能である。

例:

```text
baud_rate

sample      0 ───────── 500000
            115200

sample 500001 ──────── 900000
            230400
```

概念上は以下のような区間として保持する。

```text
Parameter Segment

start_sample
end_sample
value
confidence
```

これにより以下を扱える。

- UART baud rate の途中変更
- 動的な clock rate
- 通信モード変更
- bit order 変更
- framing format 変更
- protocol state の変更

---

# 15. Change Point

既存の Parameter や Hypothesis が成立しなくなる地点を Change Point と呼ぶ。

例えば UART 解析中に、

```text
framing error increases
timing mismatch increases
confidence decreases
```

した場合、WireSkein はその周辺を Change Point 候補とする。

Change Point の前後で別の Parameter を探索する。

例:

```text
115200 baud
      ↓
confidence drop
      ↓
change point
      ↓
230400 baud
```

これにより通信途中の設定変更に追従する。

---

# 16. Stream

WireSkein における Decoder の出力は単なる画面表示用 Annotation ではない。

Decoder は Typed Stream を出力する。

例:

```text
logic.digital

serial.bits
serial.bytes
serial.uart.frame

bus.spi.transfer
bus.i2c.transaction
bus.can.frame

text.character
text.line

protocol.scpi.command
protocol.scpi.response
```

Stream は別の Detector / Decoder の入力となる。

---

# 17. Typed Stream

各 Stream には型が存在する。

例:

```text
UART Decoder

input:
  logic.digital

output:
  serial.bytes
```

SCPI Decoder:

```text
input:
  text.line

output:
  protocol.scpi.command
  protocol.scpi.response
```

この型情報により、Core は互換性のある Decoder 同士を接続できる。

---

# 18. Decoder

Decoder は既知または推定された条件に基づき、入力 Stream を別の Stream に変換する。

例:

```text
logic.digital
    ↓
UART Decoder
    ↓
serial.bytes
```

または:

```text
serial.bytes
    ↓
ASCII Decoder
    ↓
text.line
```

Decoder 自身は、自分より下位の通信方式について知る必要がない。

SCPI Decoder は、

```text
UART
USB
TCP
GPIB
```

など、SCPI がどの物理通信上を流れているかを意識しない。

SCPI Decoder が必要とするのは、自身が要求する Stream 型のみである。

---

# 19. Decoder Graph

Decoder の接続関係全体を Decoder Graph と呼ぶ。

例:

```text
CH2
 │
 ▼
UART
 │
 ▼
Byte Stream
 │
 ▼
ASCII
 │
 ▼
SCPI
```

一つの Stream から複数の Decoder に分岐できる。

```text
UART
 │
 ▼
Byte Stream
 ├────────→ ASCII → SCPI
 │
 └────────→ Modbus RTU
```

また複数チャンネルを入力として一つの Decoder を構成できる。

```text
CH0 ── CLK ──┐
CH1 ─ MOSI ──┼─→ SPI
CH2 ─ MISO ──┤
CH3 ── CS ───┘
```

Decoder Graph は固定ではなく、解析結果に応じて動的に変化する。

---

# 20. Automatic Graph Construction

WireSkein は Detector の結果を基に、Decoder Graph の候補を自動構築する。

例:

```text
Logic
 │
 ├─ UART 0.98
 │    │
 │    └─ ASCII 0.96
 │          │
 │          └─ SCPI 0.93
 │
 ├─ Manchester 0.11
 │
 └─ Other 0.04
```

候補 Graph は複数同時に存在してよい。

Core は Confidence や計算コストを考慮し、探索対象を制御する。

---

# 21. Multi-stage Detection

上位レイヤーでも自動検出を行う。

例:

```text
Raw Logic
   ↓
UART Detector
   ↓
UART
   ↓
Byte Stream
   ↓
Text Detector
   ↓
ASCII
   ↓
SCPI Detector
   ↓
SCPI
```

これにより「UART と指定したら、その上位に存在する SCPI も自動的に検出する」といった解析が可能になる。

---

# 22. Channel Role Detection

WireSkein は必要に応じてチャンネルの役割も推定する。

SPI の例:

```text
CH0
  strong periodicity
  → CLK candidate

CH3
  active around bursts
  → CS candidate

CH1 / CH2
  correlated with CLK
  → data candidate
```

結果:

```text
SPI candidate

CLK  : CH0
MOSI : CH1
MISO : CH2
CS   : CH3

CPOL : 0
CPHA : 0

confidence: 0.91
```

複数の配線候補が存在する場合は複数 Hypothesis として保持する。

---

# 23. Adaptive Decoding

Decoder は Capture 全体を固定条件で解釈することを前提としない。

解析中に、

```text
clock drift
baud drift
jitter
mode change
parameter change
```

を検出した場合、推定値を更新できる。

例えば UART では、単なる公称 baud rate だけでなく、

```text
bit period
phase
timing error
drift
```

を追跡することができる。

表示上は必要に応じて標準値へ丸めてもよい。

例:

```text
estimated:
  115050 baud

display:
  ~115.2 kbaud
```

---

# 24. Event

Decoder によって生成される意味のあるデータ単位を Event と呼ぶ。

例:

UART:

```text
ByteEvent
  value: 0x41
  character: "A"
```

SPI:

```text
TransferEvent
  MOSI: 0x9F
  MISO: 0xEF
```

SCPI:

```text
CommandEvent
  command: "*IDN?"
```

Event は少なくとも以下を持つことができる。

```text
Sample Range
Type
Payload
Confidence
Source Decoder
Metadata
```

---

# 25. Annotation

Annotation は人間向け表示を目的とした解析結果である。

Event と Annotation は分離する。

例:

Event:

```text
value: 0x41
```

Annotation:

```text
0x41
'A'
```

これにより同じ解析結果を、

- GUI
- CLI
- JSON
- CSV
- レポート

など異なる形式で利用できる。

---

# 26. Protocol Layers

WireSkein はプロトコルを特定の固定階層には制限しない。

一例:

```text
Physical Signal
   ↓
Bit Encoding
   ↓
Frame
   ↓
Bus Transaction
   ↓
Byte Stream
   ↓
Text
   ↓
Application Protocol
```

例えば:

```text
logic.digital
   ↓
UART
   ↓
serial.bytes
   ↓
ASCII
   ↓
SCPI
```

あるいは:

```text
logic.digital
   ↓
CAN
   ↓
CAN Frame
   ↓
ISO-TP
   ↓
UDS
```

各 Decoder は隣接する Stream 型だけを理解すればよい。

---

# 27. Protocol Composition

WireSkein は Decoder の組み合わせを固定しない。

例えば Byte Stream から、

```text
Byte Stream
 ├─ ASCII
 ├─ UTF-8
 ├─ Modbus RTU
 ├─ Binary Protocol A
 └─ User Protocol
```

のように複数の解析候補を生成できる。

上位 Decoder が高い Confidence を得た場合、その経路を主要候補として扱える。

---

# 28. Detection Strategy

プロトコル探索は段階的に行う。

概念的には以下の3段階とする。

```text
1. Feature Extraction
2. Candidate Generation
3. Candidate Validation
```

### Feature Extraction

軽量な特徴抽出を広範囲に行う。

例:

```text
edge density
pulse width distribution
periodicity
idle ratio
channel correlation
```

### Candidate Generation

Feature から通信方式候補を生成する。

例:

```text
periodic channel
  → clock candidate

clock-correlated channels
  → SPI candidate
```

### Candidate Validation

候補に対して詳細なデコードを試し、整合性を評価する。

例:

```text
valid frame ratio
framing error rate
checksum validity
syntax validity
timing consistency
```

---

# 29. Search Space Control

WireSkein はすべての Decoder をすべての Channel 組み合わせに対して無制限に試行しない。

探索対象は、

```text
Feature
Confidence
Compatibility
Cost
Previous Results
```

などによって絞り込む。

低 Confidence の候補でも完全に破棄せず、必要に応じて再評価可能とする。

---

# 30. Re-evaluation

新しい解析結果により過去の仮説を再評価できる。

例えば上位プロトコルの解析結果が、下位のパラメータ選択を補強する場合がある。

例:

```text
UART candidate A
115200 8N1
confidence 0.84

UART candidate B
115200 7E1
confidence 0.82
```

上位解析:

```text
A → valid SCPI messages
B → invalid text
```

結果:

```text
UART candidate A
confidence ↑
```

このように上位レイヤーの情報を下位レイヤーの評価に利用できる。

---

# 31. User Guidance

自動解析だけでなく、ユーザーから与えられた情報も Hypothesis の制約として利用できる。

例:

```text
CH3 is UART
```

または:

```text
baud is probably around 1 Mbps
```

または:

```text
decode this as SPI
```

ユーザー指定は、自動推定機能を無効化するのではなく、探索空間を狭めるための情報として利用できる。

---

# 32. Manual Override

すべての自動推定値は必要に応じてユーザーが上書きできる。

例:

```text
Auto:
  baud = 115200

User Override:
  baud = 117187.5
```

上書き値と自動推定値は区別して保持する。

---

# 33. Partial Analysis

Capture 全体を解析する必要はない。

以下の単位で解析可能とする。

```text
selected channels
selected sample range
selected decoder graph
selected protocol
```

例:

```text
samples 10M - 12M
CH2 only
UART detection only
```

これにより大規模 Capture でも必要部分だけを解析できる。

---

# 34. Incremental Analysis

Capture データが増加している最中でも解析可能とする。

例:

```text
Capture
██████████████████░░░░░░░

Analysis
████████████████░░░░░░░░░
```

新しい Sample が追加された場合、既存の解析結果を可能な限り再利用する。

---

# 35. Streaming Decode

リアルタイム取得時には Stream を逐次 Decoder に渡すことができる。

Decoder は必要に応じて内部状態を保持する。

例えば:

```text
UART frame state
SCPI line state
ISO-TP reassembly state
```

Capture 終了後のオフライン解析と、リアルタイム解析の結果モデルは可能な限り共通とする。

---

# 36. Session

Session はユーザーが扱っている解析状態全体を表す。

例:

```text
Session
├─ Capture
├─ Channels
├─ Decoder Graph
├─ Hypotheses
├─ Parameters
├─ Events
├─ Annotations
└─ User Overrides
```

Session は GUI や CLI に依存しない。

---

# 37. Core API Concept

フロントエンドから見た Core は概念的に以下を提供する。

```text
open capture
start capture
stop capture

list channels

query waveform
query events

detect protocols

list hypotheses

create decoder
remove decoder

query decoder graph

set parameter
clear override

reanalyze range
```

Core API は UI 固有概念を含まない。

---

# 38. Decoder Extensibility

Decoder は外部から追加可能であることを前提とする。

各 Decoder は概念的に以下を宣言する。

```text
Identifier
Name
Version

Accepted Input Types
Output Types

Supported Parameters

Detection Capability
Decode Capability
```

例:

```text
SCPI Decoder

inputs:
  text.line

outputs:
  protocol.scpi.command
  protocol.scpi.response
```

Core はこの宣言を利用して Decoder Graph を構築できる。

---

# 39. Adapter Extensibility

Capture Device も Core から抽象化する。

Adapter は概念的に、

```text
Device Discovery
Device Information
Capture Configuration
Capture Start
Capture Stop
Sample Delivery
```

を提供する。

Core は特定のロジックアナライザ機種や通信方式を前提としない。

---

# 40. Error Model

解析時の問題は「成功 / 失敗」だけではなく、可能な限り分類する。

例:

```text
Insufficient Data
Timing Uncertain
Framing Error
Checksum Error
Unsupported Feature
Ambiguous Decode
Protocol Violation
Decoder Failure
```

部分的にデコード可能な場合は、正常な Event を維持したまま Error Event を生成できる。

---

# 41. Ambiguous Results

複数の解釈が成立する場合、WireSkein は無理に一つへ確定しない。

例:

```text
Candidate A
UART 8N1
confidence 0.81

Candidate B
UART 7E1
confidence 0.79
```

フロントエンドは必要に応じて複数候補を表示できる。

追加データが得られた場合、Confidence を再評価する。

---

# 42. Provenance

解析結果がどの経路から生成されたか追跡可能にする。

例:

```text
SCPI Command
    ↑
ASCII Decoder
    ↑
UART Decoder
    ↑
CH2
```

Event は自分を生成した Decoder と入力 Stream を追跡可能である。

これにより、

```text
この SCPI コマンドは
どのチャンネルの
どの UART フレームから
生成されたのか
```

を確認できる。

---

# 43. Core Design Principles

WireSkein Core は以下の原則に従う。

## 43.1 Headless

解析ロジックは GUI に依存しない。

## 43.2 Typed

Decoder 間のデータは意味のある型を持つ。

## 43.3 Composable

Decoder は自由に連鎖できる。

## 43.4 Probabilistic

解析結果は二値ではなく Confidence を持てる。

## 43.5 Temporal

パラメータは時間方向に変化できる。

## 43.6 Explainable

解析結果の根拠を可能な限り追跡できる。

## 43.7 Incremental

新しいデータに応じて解析結果を更新できる。

## 43.8 Extensible

Decoder と Adapter を追加できる。

## 43.9 Frontend Independent

GUI、CLI、その他の利用形態で同じ解析結果を共有できる。

---

# 44. WireSkein の特徴

WireSkein が一般的なロジックアナライザと異なる中心的特徴は以下である。

```text
Traditional Logic Analyzer

Signal
  ↓
User selects UART
  ↓
User enters 115200
  ↓
Decode
```

WireSkein:

```text
Signal
  ↓
Feature Analysis
  ↓
UART hypothesis
  ↓
Baud estimation
  ↓
UART decode
  ↓
Byte Stream
  ↓
ASCII hypothesis
  ↓
SCPI hypothesis
  ↓
Protocol Graph
```

さらに Capture 中に通信条件が変化した場合、

```text
UART 115200
      ↓
Change Point
      ↓
UART 230400
```

として同一 Session 内で扱う。

---

# 45. コンセプト要約

WireSkein は、

> 信号を単に表示するのではなく、信号の中に存在する構造を発見し、それらを階層的なプロトコルグラフとして解きほぐすロジックアナライザ

を目指す。

中心となる解析モデルは以下である。

```text
Raw Signal
    ↓
Features
    ↓
Hypotheses
    ↓
Detected Parameters
    ↓
Typed Streams
    ↓
Decoder Graph
    ↓
Events
    ↓
Higher-level Protocols
```

WireSkein は、一つの固定された解釈を押し付けるのではなく、複数の可能性を保持し、データが増えるにつれてそれらを評価・更新していく。

その結果、ユーザーは

```text
「これは UART だろう」
```

から始めるのではなく、

```text
「この信号には何が流れているのか」
```

から解析を始めることができる。
