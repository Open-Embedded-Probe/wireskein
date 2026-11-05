# Protocols of your own

[日本語](protocols.ja.md)

WireSkein decodes UART, SPI and I2C itself. A protocol that rides on one of them - a device's command frames, a sensor's reports, a bootloader's packets - can be added to the analysis without touching WireSkein: `wireskein analyze` then shows its messages under the bus, and a test checks them like any other check.

## 1. A protocol

A protocol is a Python object with

| Attribute | Meaning |
| --- | --- |
| `name` | The name it is shown under: letters, digits and `_`, not one of WireSkein's own layers |
| `over` | The buses it rides on: a tuple of `"uart"`, `"spi"`, `"i2c"` |
| `decode(bus)` | Reads one decoded bus and returns `{"messages": [...], "checks_passed": n, "checks_failed": m}` |

`bus` is the bus as the analysis exports it ([analysis format](analysis-format.md) §4): `roles` (the pin of each role), `tick_hz`, and its layers - for UART `bytes` (`{"s": [tick...], "hex": "..."}`), `chars` (each `{"s", "value", "ok"}`) and `breaks`; for SPI `transfers` (each `{"s", "e", <pin>: hex}`); for I2C `transactions`. A protocol never sees WireSkein's internal types, so it keeps working as long as the analysis format does.

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

Its tests (`tests/test_protocols.py`) make the signals with WireSkein's own generator, so the truth does not come from a device under test. On hardware, the truth is a signal a probe sends with an established library (the OEP probe's UART or SPI, say), not the output of the firmware being tested.

## 4. Checking it in a test

```python
from wireskein.runlog import protocol

with rec.section(2, "ping", expect=[protocol("demo_link", "uart", {"data": "PA2"}, baud=115200,
                                             messages=[{"seq": 1, "cmd": 0x10}, {"seq": 1, "cmd": 0x10, "reply": True}])]):
    ...
```

- `bus` is `"uart"`, `"spi"` or `"i2c"`, `roles` its pins by role (`{"data": ...}`; `{"clk", "mosi", "miso", "cs"}`; `{"scl", "sda"}`).
- `messages` are compared in order, each on the keys it gives (`"s"` and others it leaves out are not). Without `messages`, at least one message.
- At most `checks_failed_max` (default 0) of its checks may fail.
- A rule this cannot say is a check of its own ([guide](capture-test-guide.md) §4.10) or a verdict of the test (`rec.result`, guide §3.3).
