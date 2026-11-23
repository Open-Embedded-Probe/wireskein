# Protocols of your own

[日本語](protocols.ja.md)

WireSkein decodes UART, SPI and I2C itself. A protocol that rides on one of them - a device's command frames, a sensor's reports, a bootloader's packets, text commands on a UART pair and their answers - can be added to the analysis without touching WireSkein: `wireskein analyze` then shows its messages under the bus, and a test checks them like any other check (§1 to §4). A protocol that is a bus of its own, on a clock and data or on one line, is not an extension point: WireSkein's own such protocols are part of its analysis engine, which may change (§5).

## 1. A protocol

A protocol is a Python object with

| Attribute | Meaning |
| --- | --- |
| `name` | The name it is shown under: its owner's namespace and its own, `owner.name` (`acme.link`; lowercase letters, digits and `_`), so it never meets a name of WireSkein's |
| `over` | The buses it rides on: a tuple of `"uart"`, `"spi"`, `"i2c"`, `"uart_pair"` (two UART lines, §3.1), `"can"`, `"lin"`, `"dmx512"` |
| `decode(bus)` | Reads one decoded bus and returns `{"messages": [...], "checks_passed": n, "checks_failed": m}` |

`bus` is the bus as the analysis exports it ([analysis format](analysis-format.md) §4): `roles` (the pin of each role), `tick_hz`, and its layers - for UART `bytes` (`{"s": [tick...], "hex": "..."}`), `chars` (each `{"s", "value", "ok"}`) and `breaks`; for SPI `transfers` (each `{"s", "e", <pin>: hex}`); for I2C `transactions`; for a UART pair both lines' `messages` (§3.1); for CAN and LIN their `frames` (§3.2). A protocol never sees WireSkein's internal types, so it keeps working as long as the analysis format does.

- **Messages** are plain JSON values. Give each its first tick as `"s"`, so a reader and the viewer find it.
- **Checks** are the evidence: a checksum, a length that matches, a sequence number that counts. A check that passes on random bytes only by chance tells the bus carries this protocol. The analysis gives the bus more confidence by it, the more passed and the fewer failed (as for Modbus RTU), and shows the protocol, as the bus's final reading, only when its checks held. A protocol with no check shows nothing: without evidence, any bytes parse as anything.
- An exception in `decode` is shown with the bus (`error`); the analysis goes on.

## 2. Adding it

- In a script or a test: `wireskein.plugins.register_protocol(protocol)` (and `unregister_protocol(name)`).
- From a package: declare it in the entry point group `wireskein.protocols`, and it is there whenever the package is installed:

```toml
[project.entry-points."wireskein.protocols"]
"examples.demo_link" = "demo_link:PROTOCOL"
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
"examples.demo_link": {"messages": [{"s": 1210, "seq": 1, "cmd": 16, "reply": false, "payload": "0102"}, ...],
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
{"protocol": "examples.demo_cmd", "roles": {"a": "TX", "b": "RX"}, "score": 0.9375,
 "messages": [{"s": 200, "seq": "1e", "verb": "PING", "args": [], "status": "OK", "text": "#1e PING -> OK", ...}],
 "checks_passed": 4, "checks_failed": 0}
```

Its tests (`tests/test_demo_cmd.py`) put the exchange on two lines with an encoder apart from the reader. On hardware, `examples/protocols/demo_cmd_p4.wireskein` is an OEP probe sending it with two ESP-IDF UART drivers (115200 8N1, host and device on lines of their own), with what each sent in `demo_cmd_p4.truth.json`: without demo_cmd the analysis reads two UARTs and nothing above them; with it, the five exchanges and four answers that belong to their commands.

## 3.2 On frames of CAN or LIN: demo_telemetry

A protocol may ride on the frames the analysis already reads: `"can"`, `"lin"`, `"dmx512"`. `bus` then carries that
one's `frames` (`packets` for DMX512), as the analysis format §3.2 lists them: for CAN `{"s", "id", "ide", "dlc",
"data": hex, "ack", "ok"}`, for LIN `{"s", "id", "data": hex, "ok"}`. That is where CANopen, J1939, OBD-II / UDS or a LIN node's own signals go.

`examples/protocols/demo_telemetry.py` is made up for this guide: each node sends its readings in frames of its own
identifier, the data being `seq | reading... | check`, the check `(identifier + the bytes before it) & 0xFF` xor
`0x5A`. One `decode` reads CAN and LIN alike (`over = ("can", "lin")`): a frame whose check holds is a check passed,
a seq that does not follow its node's last one is shown (`level: "warn"`). Its messages are under the CAN or LIN
bus's `result`, as demo_link's are under a UART's; its tests (`tests/test_demo_telemetry.py`) put the frames on the
lines with encoders of their own. On hardware, `examples/protocols/demo_telemetry_p4_can.wireskein` and `_lin.wireskein` are an OEP probe sending them with ESP-IDF's TWAI driver (self-test, 500 kbit/s) and UART driver (19200 baud, the break sent as a 0x00 at 9/13 of the rate), with what it sent in their `.truth.json`.

## 4. Checking it in a test

```python
from wireskein.runlog import protocol

with rec.section(2, "ping", expect=[protocol("examples.demo_link", "uart", {"data": "PA2"}, baud=115200,
                                             messages=[{"seq": 1, "cmd": 0x10}, {"seq": 1, "cmd": 0x10, "reply": True}])]):
    ...
```

- `bus` is `"uart"`, `"spi"`, `"i2c"`, `"uart_pair"`, `"can"` or `"lin"`, `roles` its pins by role (`{"data": ...}` for a UART, CAN or LIN; `{"clk", "mosi", "miso", "cs"}`; `{"scl", "sda"}`; `{"a": TX, "b": RX}`). On a pair the two lines are read together whatever the pairing's own score: `protocol("examples.demo_cmd", "uart_pair", {"a": "PA2", "b": "PA3"}, baud=115200, messages=[{"verb": "PING", "status": "OK"}])`.
- `messages` are compared in order, each on the keys it gives (`"s"` and others it leaves out are not). Without `messages`, at least one message.
- At most `checks_failed_max` (default 0) of its checks may fail.
- A rule this cannot say is a check of its own ([guide](capture-test-guide.md) §4.10) or a verdict of the test (`rec.result`, guide §3.3).

## 5. Device packs and marker dialects: a definitions directory

What rides on the buses can also be named by tables, without code: the registers and commands of a device on I²C or
SPI (a device pack) and the marker lines of your own test logs (a marker dialect). They read what the analysis
exports, so they do not depend on how the buses are found.

- **A definitions directory** holds `devices/<bus>/...` device packs ([device packs](../src/wireskein/decl/devices/README.md)) and `markers/*.toml` marker dialects ([markers](markers.md) §4). Either may be absent.
- **Adding it:** `wireskein.plugins.add_definitions(path)` in a script or a test, `wireskein --definitions DIR` on the command line, or the entry point group `wireskein.definitions` from a package (`NAME = "module"`: the module's directory holds the definitions; or a callable returning the path).
- **A protocol on the wire itself** (its own framing on a clock and data, or on one line) is not one of these: WireSkein's own such protocols (`decl/*.toml`: I²C, SPI, SWD, CAN ...) are part of its analysis engine, written for that engine's layers, and may change with it. A directory with protocol tables at its top is refused. For a bus WireSkein does not read, propose it.
