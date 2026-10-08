# Device definitions

[日本語](README.ja.md)

A definition lives at `<protocol>/<maker or category>/<model>.toml` (e.g. `i2c/sensirion/sht30.toml`, `spi/flash/spi_nor.toml`).

- The top folder is the protocol and must equal `device.bus` in the file; a mismatch is an error when loading.
- The definitions to use are chosen by path patterns relative to this folder; a pattern starting with `!` excludes.
  - CLI: `wireskein analyze CAPTURE --depth device --devices "i2c/**" "!i2c/qst/**"`
  - Hint: `{"devices": ["i2c/sensirion/**"]}`
  - Without a pattern, all (`**`) are used.
- At start only each file's `[device]` header is read. The headers are cached in `~/.cache/wireskein/device-headers.json` (under `XDG_CACHE_HOME` when set), keyed by the file's modification time. A table itself is read only when it is needed: for I²C when its address is seen, for SPI when an SPI bus is found. This keeps start-up and matching fast with thousands of definitions.
- When several definitions are identified by the same evidence (e.g. SHT30 and SHT31 share their commands and CRC), they are returned as one claim, `"SHT30 | SHT31"`.
- Definitions that only match an address, without evidence, are gathered into one candidate list per address.

There are three types. `register_map` and `command_table` live in this folder; message frames (NMEA, Modbus, SCPI, ...) are in `_engine/analyzers/`. A match has one of three strengths: `identified` (an identifier value or a check such as a CRC matches), `consistent` (fits the table, nothing to check), `address_only` (no claim, only the candidate list). Why: [design decisions](../../../../docs/design.md) §3.4.
