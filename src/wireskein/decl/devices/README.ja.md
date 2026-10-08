# デバイスの定義

[English](README.md)

置き場所は `<方式>/<メーカーまたは分類>/<機種>.toml` です（例: `i2c/sensirion/sht30.toml`、`spi/flash/spi_nor.toml`）。

- 最上位のフォルダ名は方式で、ファイル内の `device.bus` と同じにします。違っていると読み込み時にエラーになります。
- 使う定義は、このフォルダからの相対パスのパターンで選びます。`!` で始まるパターンは除外です。
  - CLI: `wireskein analyze CAPTURE --depth device --devices "i2c/**" "!i2c/qst/**"`
  - ヒント: `{"devices": ["i2c/sensirion/**"]}`
  - 省略すると全部（`**`）を使います。
- 起動時に読むのは各ファイルの `[device]` の見出しだけです。見出しは `~/.cache/wireskein/device-headers.json`（`XDG_CACHE_HOME` があればその下）に、ファイルの更新時刻をキーに保存します。表の本体は、I²C ならアドレスが観測されたとき、SPI なら SPI のバスが見つかったときに初めて読みます。定義が数千件に増えても、起動と照合の時間を抑えるためです。
- 同じ証拠で識別された定義が複数あるとき（例: 命令と CRC が同じ SHT30 と SHT31）は、`"SHT30 | SHT31"` の 1 件の主張として返します。
- 証拠がなくアドレスが合うだけの定義は、アドレスごとに 1 件の候補一覧にまとめます。

型は 3 つです。`register_map`（レジスタ表）と `command_table`（コマンド表）がこのフォルダの対象で、メッセージの枠（NMEA、Modbus、SCPI など）は `_engine/analyzers/` 側にあります。判定の強さは `identified`（識別子の値や CRC などの検算が一致）/ `consistent`（表には合うが検算がない）/ `address_only`（主張せず、候補の一覧だけ）の 3 段階です。理由は [設計の決定](../../../../docs/design.ja.md) §3.4 にあります。
