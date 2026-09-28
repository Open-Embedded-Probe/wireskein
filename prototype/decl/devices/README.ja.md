# デバイスの定義

置き場所は `<方式>/<メーカーまたは分類>/<機種>.toml` です（例: `i2c/sensirion/sht30.toml`、`spi/flash/spi_nor.toml`）。

- 最上位のフォルダ名は方式で、ファイル内の `device.bus` と同じにします。違っていると読み込み時にエラーになります。
- 使う定義は、このフォルダからの相対パスのパターンで選びます。`!` で始まるパターンは除外です。
  - CLI: `ws.py analyze CAPTURE --depth device --devices "i2c/**" "!i2c/qst/**"`
  - ヒント: `{"devices": ["i2c/sensirion/**"]}`
  - 省略すると全部（`**`）を使います。
- 起動時に読むのは各ファイルの `[device]` の見出しだけです。見出しは `prototype/.cache/` に、ファイルの更新時刻をキーに保存します。表の本体は、I²C ならアドレスが観測されたとき、SPI なら SPI のバスが見つかったときに初めて読みます。
- 同じ証拠で識別された定義が複数あるとき（例: 命令と CRC が同じ SHT30 と SHT31）は、`"SHT30 | SHT31"` の1件の主張として返します。
- 証拠がなくアドレスが合うだけの定義は、I2CDeviceDB の名前と一緒に、アドレスごとに1件の候補一覧にまとめます。

型は3つです。`register_map` と `command_table` がこのフォルダの対象で、`message framing` は `analyzers/upper.py` 側にあります。判定の強さは `identified` / `consistent` / `address_only` の3段階です。詳しくは `../../findings.ja.md` を見てください。
