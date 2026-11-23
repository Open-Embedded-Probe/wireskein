# Protocols of your own

[日本語](protocols.ja.md)

WireSkein decodes UART, SPI, I2C, CAN, LIN and more itself. A protocol that rides on one of them - a device's command frames, a sensor's reports, a node's readings in CAN frames, text commands on a UART pair and their answers - is declared in a table, without code: `wireskein analyze` then shows its messages under the bus, and a test checks them like any other check (§4). A protocol that is a bus of its own, on a clock and data or on one line, is not an extension point: WireSkein's own such protocols are part of its analysis engine, which may change (§5).

## 1. A protocol table

A table is a file `protocols/<name>.toml` in a definitions directory ([extending](extending.md) §2):

```toml
format = "wireskein-protocol/1"
name = "acme.link"          # its owner's namespace and its own (lowercase letters, digits, _)
over = ["uart", "spi"]      # the buses it rides on

[frame]                      # byte frames (§2), or [lines] for commands and answers on a UART pair (§3)
...
```

| `over` | What the table reads (the bus as the [analysis format](analysis-format.md) §4 exports it) |
| --- | --- |
| `"uart"` | The byte stream (`bytes`), searched for frames |
| `"spi"` | Each transfer (`transfers`), on each data line, searched for frames; each message gets the `line` |
| `"i2c"` | Each transaction's bytes (`transactions`), searched; its `addr` is a value a field or a check may use (`@addr`) |
| `"can"`, `"lin"`, `"dmx512"` | Each frame's data (`frames`, `packets`) as one frame of the table, whole; the frame's own values (`@id`, ...) usable. A frame the bus itself got wrong is left out |
| `"uart_pair"` | Two UART lines at one rate whose messages follow one another, their lines of text in time order (§3) |

Every message has its first tick as `"s"`. The checks a protocol's frames pass and fail are its evidence: the analysis gives the bus more confidence the more passed and the fewer failed (as for Modbus RTU), and shows the protocol, as the bus's final reading, only when its checks held. A protocol whose checks never pass shows nothing: without evidence, any bytes parse as anything.

## 2. Frames: `[frame]`

```toml
[frame]
fields = [
    { name = "sof", equals = 0xA5 },                      # a byte that must be 0xA5
    { name = "len", max = 32 },
    { name = "seq" },
    { name = "cmd", mask = 0x7F },                        # the value shown: the bits of the mask
    { name = "reply", of = "cmd", mask = 0x80, as = "bool" },   # another value from a field before
    { name = "payload", size = "len", as = "hex" },       # as many bytes as len says
    { name = "crc", check = { kind = "crc8", over = ["len", "seq", "cmd", "payload"] } },
]
show = ["seq", "cmd", "reply", "payload"]                 # the message's keys (default: every field but checks)
text = "#{seq} cmd {cmd:#x}"                              # optional: what the viewer draws
```

| Field key | Meaning |
| --- | --- |
| `name` | Its name (required) |
| `size` | Bytes: a number (default 1), the name of a field before it (its value), or `"rest"` (up to the fields after it) |
| `endian` | `"big"` (default) or `"little"`, for a field of 2 bytes or more |
| `equals`, `min`, `max` | What the value must be for a frame to be here: where it is not, there is no frame (not a failed check) |
| `mask` | The value shown is these bits, shifted down (`as = "bool"` for one bit) |
| `of` | Read from a field before it rather than from new bytes (with `mask`) |
| `as` | `"hex"` (a byte string), `"bool"`; default a number for 8 bytes or fewer, else hex |
| `check` | This field is a check of the fields named in `over` (their bytes, in that order): `kind`, and its options below |

| Check `kind` | What |
| --- | --- |
| `crc8`, `crc8_maxim`, `crc8_sae_j1850`, `crc16_modbus`, `crc16_ccitt_false`, `crc16_xmodem`, `crc16_kermit`, `crc32` | The CRC by its common name; `poly`, `init`, `refin`, `refout`, `xorout` change one |
| `crc` | A CRC of your own: `width`, `poly`, `init`, `refin`, `refout`, `xorout` |
| `sum8`, `sum16`, `xor8` | The sum (modulo 2⁸, 2¹⁶) or the xor of the bytes, from `init`, then xor `xorout` |

`init` may name a value of the bus's frame (`"@id"`: a CAN or LIN identifier; `"@addr"`: an I2C address).

### 2.1 Where frames are found

On a stream (a UART's bytes, an SPI transfer, an I2C transaction) a frame is tried at every byte: where a field's `equals`, `min` or `max` does not hold, or the frame runs past the end, there is none and the next byte is tried. A frame whose check fails is a check failed, and the search goes on from the next byte (resynchronizing). In a CAN, LIN or DMX512 frame the table reads the frame's data whole: it must be exactly one frame of the table.

### 2.2 A counter

```toml
sequence = { field = "seq", per = "@id", modulo = 256, note = " (seq skipped)" }
```

A value that counts up by one (per value of `per`, when given): every message gets `level` `ok`, or `warn` where the count skipped (a frame lost or sent twice), with `note` after its `text`. A skip is shown, not a failed check.

### 2.3 Text for the viewer

`text` is a template: `{name}` a value, `{name:x}` / `{name:#x}` / `{name:02x}` / `{name:d}` formatted, `{name|-}` with `-` for an empty value; `{@id}` a value of the bus's frame.

## 3. Commands and answers: `[lines]`

Many devices take text commands on one UART line and answer on the other (AT commands, a shell, a test fixture's protocol). A table with `[lines]` rides on `"uart_pair"`: the analysis pairs two UART lines at one rate whose messages follow one another, splits their messages into lines (each line's ticks spread over its message), and the table reads them in time order.

```toml
[lines]
command = '^#(?P<seq>[0-9a-f]{2}) (?P<verb>[A-Z][A-Z0-9_]*)(?P<args>(?: \S+)*)$'
answer = '^#(?P<seq>[0-9a-f]{2}) (?P<status>OK|ERR)(?P<values>(?: \S+)*)$'
event = '^!(?P<event>[A-Z][A-Z0-9_]*)(?P<values>(?: \S+)*)$'      # optional: told unasked
pair = "seq"                                  # optional: an answer belongs only with the same seq
words = ["args", "values"]                    # groups split into lists of words
warn = { status = "ERR" }                     # an answer with this value is level warn
answered = "{command} -> {status}{values}"    # a command's text once answered
```

- `command`, `answer` and `event` are regular expressions; their named groups are the message's keys. The line that sends commands is the host's; the other's lines are answers and events.
- **The evidence is the pairing:** an answer to the command just before it (with the same `pair` group, when named) is a check passed; an answer to no command, or a command still unanswered when the next one comes, a check failed. Text without this shape is neither, so on other text the table shows nothing.
- Each message is a command with its answer (`level` `ok`, or `warn` as `warn` says or when unanswered), an event (`ok`), or an answer to nothing (`error`). It shows under the part's `relations` (analysis format §4).

## 4. The examples, and checking one in a test

`examples/` is a definitions directory of three tables, each with a capture an OEP probe sent with ESP-IDF's drivers (the truth is what it sent, in `*.truth.json`, not the output of a device under test):

| Table | Shape | Capture |
| --- | --- | --- |
| `demo_link.toml` | `0xA5 \| len \| seq \| cmd \| payload[len] \| crc8` on UART and SPI; a reply sets bit 7 of `cmd` | `demo_link_p4.wireskein` (UART 115200 8N1) |
| `demo_cmd.toml` | `#<seq> <VERB>[ <arg>...]` answered `#<seq> OK[ <value>...]` or `#<seq> ERR <code>`, and `!<EVENT>` unasked | `demo_cmd_p4.wireskein` (two UARTs) |
| `demo_telemetry.toml` | `seq \| reading... \| check` in CAN and LIN frames, the check `(identifier + the bytes before it) & 0xFF` xor `0x5A`, a counter per identifier | `demo_telemetry_p4_can.wireskein`, `_lin.wireskein` |

```
$ wireskein --definitions examples analyze examples/protocols/demo_link_p4.wireskein
"examples.demo_link": {"messages": [{"s": 563052, "seq": 1, "cmd": 16, "reply": false, "payload": "0102"}, ...],
                       "checks_passed": 3, "checks_failed": 0}
```

In a test:

```python
from wireskein.runlog import protocol

with rec.section(2, "ping", expect=[protocol("examples.demo_link", "uart", {"data": "PA2"}, baud=115200,
                                             messages=[{"seq": 1, "cmd": 0x10}, {"seq": 1, "cmd": 0x10, "reply": True}])]):
    ...
```

- `bus` is `"uart"`, `"spi"`, `"i2c"`, `"uart_pair"`, `"can"` or `"lin"`, `roles` its pins by role (`{"data": ...}` for a UART, CAN or LIN; `{"clk", "mosi", "miso", "cs"}`; `{"scl", "sda"}`; `{"a": TX, "b": RX}`). On a pair the two lines are read together whatever the pairing's own score: `protocol("examples.demo_cmd", "uart_pair", {"a": "PA2", "b": "PA3"}, baud=115200, messages=[{"verb": "PING", "status": "OK"}])`.
- `messages` are compared in order, each on the keys it gives (`"s"` and others it leaves out are not). Without `messages`, at least one message.
- At most `checks_failed_max` (default 0) of its checks may fail.
- The definitions directory is added where the run is verified (`wireskein --definitions DIR verify RUN`, or `add_definitions` in the test).
- A rule this cannot say is a verdict of the test (`rec.result`, [guide](capture-test-guide.md) §3.3).

## 5. What is not a table

- **A protocol on the wire itself** (its own framing on a clock and data, or on one line): WireSkein's own such protocols (`decl/*.toml`: I²C, SPI, SWD, CAN ...) are part of its analysis engine, written for that engine's layers, and may change with it. A definitions directory with protocol tables at its top is refused. For a bus WireSkein does not read, propose it.
- **A protocol a table cannot say** (state carried across frames, frames reassembled from pieces, a calculation of its own): read the analysis's output in your own code (`wireskein analyze FILE`, the bus's layers) and judge it in a test with `rec.result`.
- **Device packs and marker dialects** are tables of their own, in `devices/` and `markers/` of the same directory ([device packs](../src/wireskein/decl/devices/README.md), [markers](markers.md) §4).
