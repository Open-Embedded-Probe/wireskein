# Protocols of your own

[日本語](protocols.ja.md)

WireSkein decodes UART, SPI and I2C itself. A protocol that rides on one of them - a device's command frames, a sensor's reports, a bootloader's packets, text commands on a UART pair and their answers - can be added to the analysis without touching WireSkein: `wireskein analyze` then shows its messages under the bus, and a test checks them like any other check (§1 to §4). A protocol that is a bus of its own, on a clock and data or on one line, is added as a declarative definition (§5).

## 1. A protocol

A protocol is a Python object with

| Attribute | Meaning |
| --- | --- |
| `name` | The name it is shown under: letters, digits and `_`, not one of WireSkein's own layers |
| `over` | The buses it rides on: a tuple of `"uart"`, `"spi"`, `"i2c"`, `"uart_pair"` (two UART lines, §3.1), `"can"`, `"lin"`, `"dmx512"`, or a protocol of your own definition by its name (§3.2) |
| `decode(bus)` | Reads one decoded bus and returns `{"messages": [...], "checks_passed": n, "checks_failed": m}` |

`bus` is the bus as the analysis exports it ([analysis format](analysis-format.md) §4): `roles` (the pin of each role), `tick_hz`, and its layers - for UART `bytes` (`{"s": [tick...], "hex": "..."}`), `chars` (each `{"s", "value", "ok"}`) and `breaks`; for SPI `transfers` (each `{"s", "e", <pin>: hex}`); for I2C `transactions`; for a UART pair both lines' `messages` (§3.1); for CAN, LIN and a definition their `frames` (§3.2). A protocol never sees WireSkein's internal types, so it keeps working as long as the analysis format does.

- **Messages** are plain JSON values. Give each its first tick as `"s"`, so a reader and the viewer find it.
- **Checks** are the evidence: a checksum, a length that matches, a sequence number that counts. A check that passes on random bytes only by chance tells the bus carries this protocol. The analysis gives the bus more confidence by it, the more passed and the fewer failed (as for Modbus RTU), and shows the protocol, as the bus's final reading, only when its checks held. A protocol with no check shows nothing: without evidence, any bytes parse as anything.
- An exception in `decode` is shown with the bus (`error`); the analysis goes on.

## 2. Adding it

- In a script or a test: `wireskein.protocols.register(protocol)`.
- From a package: declare it in the entry point group `wireskein.protocols`, and it is there whenever the package is installed:

```toml
[project.entry-points."wireskein.protocols"]
demo_link = "demo_link:PROTOCOL"
```

## 3. The example: demo_link

`examples/protocols/demo_link.py` is a protocol made up for this guide, in the shape many device protocols have:

```
0xA5 | len | seq | cmd | payload[len] | crc8
```

- `0xA5` starts a frame. A reader that starts mid-stream looks for the next `0xA5` whose frame checks (resynchronizing).
- `len` is the payload's length (0 to 32), `seq` counts frames, `cmd` is the command; a reply sets bit 7 of `cmd` and has the seq of its request.
- `crc8` is CRC-8 (polynomial 0x07, initial 0) of `len`, `seq`, `cmd` and the payload: the check.
- On a UART the frames follow one another in the byte stream; on SPI each CS window carries one frame on MOSI (the request) and one on MISO (a reply). One `decode` reads both, so `over = ("uart", "spi")`.

What it shows in `wireskein analyze`, under the bus's `result`:

```json
"demo_link": {"messages": [{"s": 1210, "seq": 1, "cmd": 16, "reply": false, "payload": "0102"}, ...],
              "checks_passed": 4, "checks_failed": 0}
```

Its tests (`tests/test_protocols.py`) make the signals with WireSkein's own generator, so the truth does not come from a device under test. On hardware, the truth is a signal a probe sends with an established library, not the output of the firmware being tested: `examples/protocols/demo_link_p4.wireskein` is an OEP probe (an ESP32-P4) sending the frames with ESP-IDF's UART driver (115200 8N1) and capturing that pin, with what it sent in `demo_link_p4.truth.json`. Without demo_link the analysis reads that UART as `likely`; with it, `confirmed`, its three frames and their CRCs. The capture also holds the frames on SPI, from the probe's own pin-driving SPI controller; that is not an established library, so it is no truth.

## 3.1 Commands and answers on two lines: demo_cmd

Many devices take text commands on one UART line and answer on the other (AT commands, a shell, a test fixture's
protocol). `decode` on `"uart"` sees one line; a protocol that reads a command with its answer rides on
`"uart_pair"` instead. The analysis pairs two UART lines at one rate whose messages follow one another (one side
speaks, then the other), and gives the protocol both lines' messages in time order:

```json
{"roles": {"a": "TX", "b": "RX"}, "tick_hz": 2000000.0,
 "messages": [{"pin": "TX", "s": 200, "e": 1764, "hex": "2331652050494e470a", "text": "#1e PING\n"},
              {"pin": "RX", "s": 2364, "e": 3580, "hex": "...", "text": "#1e OK\n"}, ...]}
```

A message is one line's characters up to a pause of more than 3 characters; split it into lines yourself. Which
line is the host is for the protocol to read: the analysis does not know.

`examples/protocols/demo_cmd.py` is a text protocol made up for this guide:

```
host     #<seq> <VERB>[ <arg>...]          seq: two hex digits, one more each command
device   #<seq> OK[ <value>...]  |  #<seq> ERR <code>  |  !<EVENT>[ <value>...]  (unasked)
```

- Text has no checksum, so the evidence is the pairing: an answer whose `seq` is the command's just before it is a
  check passed (by chance one in 256); an answer to no command, or a command unanswered when the next one comes, a
  check failed. Text without this shape is neither, so on other text it shows nothing.
- Each message is a command with its answer (`verb`, `args`, `status`, `values`) or an event, with `text` and
  `level` (`ok`, `warn` for `ERR` or no answer, `error` for an answer to nothing) for the viewer.
- It shows in `wireskein analyze` under the part's `relations` (analysis format §4), as `scpi` does:

```json
{"protocol": "demo_cmd", "roles": {"a": "TX", "b": "RX"}, "score": 0.9375,
 "messages": [{"s": 200, "seq": "1e", "verb": "PING", "args": [], "status": "OK", "text": "#1e PING -> OK", ...}],
 "checks_passed": 4, "checks_failed": 0}
```

Its tests (`tests/test_demo_cmd.py`) put the exchange on two lines with an encoder apart from the reader. On hardware, `examples/protocols/demo_cmd_p4.wireskein` is an OEP probe sending it with two ESP-IDF UART drivers (115200 8N1, host and device on lines of their own), with what each sent in `demo_cmd_p4.truth.json`: without demo_cmd the analysis reads two UARTs and nothing above them; with it, the five exchanges and four answers that belong to their commands.

## 3.2 On frames of CAN, LIN or your own: demo_telemetry

A protocol may ride on the frames the analysis already reads: `"can"`, `"lin"`, `"dmx512"`, or a protocol of your
own definition (§5) by its name. `bus` then carries that one's `frames` (`packets` for DMX512), as the analysis
format §3.2 lists them: for CAN `{"s", "id", "ide", "dlc", "data": hex, "ack", "ok"}`, for LIN `{"s", "id", "data":
hex, "ok"}`, for a definition its fields. That is where CANopen, J1939, OBD-II / UDS or a LIN node's own signals go.

`examples/protocols/demo_telemetry.py` is made up for this guide: each node sends its readings in frames of its own
identifier, the data being `seq | reading... | check`, the check `(identifier + the bytes before it) & 0xFF` xor
`0x5A`. One `decode` reads CAN and LIN alike (`over = ("can", "lin")`): a frame whose check holds is a check passed,
a seq that does not follow its node's last one is shown (`level: "warn"`). Its messages are under the CAN or LIN
bus's `result`, as demo_link's are under a UART's; its tests (`tests/test_demo_telemetry.py`) put the frames on the
lines with encoders of their own.

## 4. Checking it in a test

```python
from wireskein.runlog import protocol

with rec.section(2, "ping", expect=[protocol("demo_link", "uart", {"data": "PA2"}, baud=115200,
                                             messages=[{"seq": 1, "cmd": 0x10}, {"seq": 1, "cmd": 0x10, "reply": True}])]):
    ...
```

- `bus` is `"uart"`, `"spi"`, `"i2c"`, `"uart_pair"`, `"can"`, `"lin"` or a definition's name, `roles` its pins by role (`{"data": ...}` for a UART, CAN or LIN; `{"clk", "mosi", "miso", "cs"}`; `{"scl", "sda"}`; `{"a": TX, "b": RX}`). On a pair the two lines are read together whatever the pairing's own score: `protocol("demo_cmd", "uart_pair", {"a": "PA2", "b": "PA3"}, baud=115200, messages=[{"verb": "PING", "status": "OK"}])`.
- `messages` are compared in order, each on the keys it gives (`"s"` and others it leaves out are not). Without `messages`, at least one message.
- At most `checks_failed_max` (default 0) of its checks may fail.
- A rule this cannot say is a check of its own ([guide](capture-test-guide.md) §4.10) or a verdict of the test (`rec.result`, guide §3.3).

## 5. A protocol on the wire itself: a definition

A protocol that is its own bus - its own framing on a clock and data, or on one NRZ line - is written declaratively in TOML, as WireSkein's own I²C, SPI, SWD and CAN are ([design](design.md) §3.2), and added from outside as a directory of definitions.

- **A definitions directory** is laid out as WireSkein's `decl/`: the `*.toml` at its top are protocols, `devices/<bus>/...` device packs, `markers/*.toml` marker dialects. Each part is optional.
- **Adding it:** `wireskein.protocols.add_definitions(path)` in a script or a test, or the entry point group `wireskein.definitions` from a package (`NAME = "module"`: the module's directory holds the definitions; or a callable returning the path).
- **The bit layer** is one WireSkein has, named by the definition's `input`: `"nrzbits"` (one line, its bit time read from the line, a frame after ten idle bit times or more), or `"frames.select"`, `"frames.gap"`, `"frames.startstop"` (a clock and data, framed by a select line, by gaps, or by START / STOP). A line coding of its own (Manchester and the like) needs a bit layer in WireSkein itself and is not a definition.
- **The analysis** shows it as a protocol of its own, its frames under `result.frames` (a field with `format = "hex"` as hex bytes), and its checks (expected values, CRCs) as the evidence for it.
- **A test** checks its frames with `protocol(NAME, None, {role: pin}, messages=[...])` (`bus` None: the protocol is the bus).
- A name of WireSkein's own protocols is refused.

### 5.1 The example: demo_wire

`examples/definitions/demo_wire.toml` is a single-wire protocol made up for this guide:

```
idle 1 | start 0 | addr 7 | cmd 4 | len 4 | data 8 x len | crc8 | stop 1      (most significant bit first)
```

- The CRC-8 (polynomial 0x07, initial 0) covers the address to the data.
- After five equal bits the sender puts in one of the other value (bit stuffing), from the start bit to the end of the CRC, as CAN does, so a receiver keeps its bit timing on any data.
- Frames are apart by ten idle bit times or more.

```toml
[plugin]
name = "demo_wire"
input = "nrzbits"

[roles]
data = "data"

[[sequence]]
order = "msb"
stuffing = { after = 5, from = "sof", to = "crc" }
fields = [
  { name = "sof",  width = 1, expect = 0, hide = true },
  { name = "addr", width = 7 },
  { name = "cmd",  width = 4 },
  { name = "len",  width = 4 },
  { name = "data", width = "8 * len", format = "hex" },
  { name = "crc",  width = 8, hide = true },
  { name = "eof",  width = 1, expect = 1, hide = true },
]
checks = [
  { kind = "crc", field = "crc", from = "addr", to = "data", poly = 0x07, width = 8 },
]
```

Its tests write the line bit by bit with an encoder of their own (`tests/test_protocols.py`), apart from the definition that reads it.
