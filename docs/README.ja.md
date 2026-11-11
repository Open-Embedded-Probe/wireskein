# WireSkein の文書

[English](README.md)

使い方の概要はリポジトリの [README](../README.ja.md) にあります。ここには、仕様と設計の決定を置きます。どの文書にも、同じ内容の英語版（`*.md`）があります。

| 文書 | 内容 |
| --- | --- |
| [目的と範囲](concept.ja.md) | 解決したい課題、大切にする性質、範囲に入れないもの |
| [設計の決定と理由](design.ja.md) | 版と互換性、実装の言語、復号の段の構成と判定、照合の方針、評価 |
| [WireSkein のファイル形式](wireskein-format.ja.md) | `.wireskein` の仕様: 入れ物、`capture.json`、チャンネルの `encoding`、添付・メモ・マーカー・注釈、`.sr` と VCD との関係 |
| [記録の形式](run-format.ja.md) | 試験の記録（`run.json`）と照合の結果の仕様 |
| [分析の形式](analysis-format.ja.md) | `wireskein-analysis/1`: `decode` と `analyze` が書くもの |
| [独自の分析](extending.ja.md) | WireSkein に無いものを足す方法の選び方（プロトコル、定義、デバイスの定義、マーカーの方言、検査、試験の判定）、どの追加も守ること、結果をビューアに出す方法 |
| [独自のプロトコル](protocols.ja.md) | プロトコルを分析と検査に足す: UART、SPI、I2C に乗るもの（Python）、線そのものの上のもの（TOML の定義）。それぞれ見本つき |
| [マーカー](markers.ja.md) | 見出し型のマーカーと区間の規則 |
| [キャプチャで試験する](capture-test-guide.ja.md) | 試験の記録の作り方、キャプチャの窓、検査の一覧、許容誤差の決め方 |
| [WeAct の U3 ロジックアナライザー](instruments/weact-u3-logic-analyzer.ja.md) | WeAct の CH32H417 のロジアナとそのアプリを調べた結果: ソースとライセンス、USB の通信、保存する `.sr`、WireSkein との組み合わせ方 |
